//! 2027 roster projection: compose a hypothetical N+1 roster per team from
//! N's qualified roster minus departures plus incoming portal transfers,
//! with a separate "uncertain" bucket for declared-but-uncommitted NBA
//! draft entrants so the API can surface floor (all-`?`-leave) and
//! ceiling (all-`?`-return) bounds.
//!
//! This module is data composition only — it does not run inference. The
//! caller (the API route) builds features via
//! [`roster_features::build_roster_features`] over the materialized
//! roster and feeds them to [`Predictor::predict_adj_em`].
//!
//! Honest scope for v1 (frozen-stats, no growth model):
//! - **Returning players**: use their *N* (most recently completed
//!   season) stats verbatim, with their N-season MPG. Real coaches
//!   would reallocate minutes after departures; we don't try to model
//!   that. Teams that lose lots of players will look thinner than they
//!   actually will be — the route surfaces a `roster_size` count so
//!   the UI can flag obviously-incomplete projections.
//! - **Incoming transfers**: use their N stats from their *source*
//!   team. So the incoming row's `mpg` is the role they played at their
//!   old school, not what they'll play at the destination.
//! - **Incoming freshman recruits**: synthesized from `recruits` table
//!   commits via [`synthesize_freshman_row`]. Each commit gets a
//!   PlayerRow built from a tier-average profile keyed on
//!   `composite_rank` — T1 (top-30) / T2 (31-100) / T3 (101-250) /
//!   T4 (251+ or unranked). Profiles are calibrated from class-of-2024
//!   and class-of-2025 recruits joined to their actual freshman cstat
//!   seasons. This is a population-mean heuristic, not a per-player
//!   projection (that's the Phase 6 freshman-impact prior model). High
//!   variance within tier is the expected honesty cost — a 4-star who
//!   busts and a 4-star All-American both get the same projected row.
//! - **Growth**: out of scope. A junior who's about to break out as a
//!   senior is just their junior line in the model's view.
//!
//! Next iteration: Phase 5c trajectory model already projects per-player
//! next-season CamPom for returners; plugging that in upgrades the
//! "frozen-stats" framing for returning players. Phase 6 freshman-impact
//! prior is the upgrade path for recruits.

use crate::freshman_model::{FreshmanFeatureRow, FreshmanPrediction, build_freshman_features};
use crate::inference::Predictor;
use crate::roster_features::{PlayerRow, QUAL_MIN_GAMES_PLAYED, QUAL_MIN_MPG};
use crate::team_name_match::team_match_score;
use crate::trajectory::{
    build_trajectory_features, fetch_player_trajectory_rows, fetch_trajectory_oof,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

/// Which NBA-draft scenario to materialize. The floor / ceiling pair is
/// the API's honesty story: we don't know if a `declared` player will
/// withdraw before the deadline, so we project both bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftScenario {
    /// Treat every roster member flagged `declared` (NBA early entry,
    /// status unresolved) as gone. Conservative projection.
    Floor,
    /// Treat every `declared` flag as a withdrawal — the player stays.
    /// Optimistic projection.
    Ceiling,
}

/// Reason a player is no longer on the projected roster. Stored for
/// auditability in the route response — users want to know *why* a
/// team's projection dropped.
#[derive(Debug, Clone, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DepartureReason {
    /// `class_year = 'Sr'` at season N → graduating.
    GraduatedSenior { player_id: Uuid, name: String },
    /// In the `transfers` table for portal class N, source = this team.
    /// `destination_team_id` carries the **base-season** UUID of the
    /// destination team — `resolve_team_id` runs against the same
    /// `teams` vec the rest of compose_all_projections uses, which is
    /// loaded for `base_season` (N), not the projection target (N+1).
    /// The frontend links `/teams/{destination_team_id}?season={year}`;
    /// the route's `resolve_team_id_for_season` re-maps via `natstat_id`
    /// to the N+1 team, so the link still lands on the right team —
    /// just with one extra resolution hop. None when the destination
    /// string didn't match any base-season D-I team (non-D1
    /// destination, name miss).
    Transferred {
        player_id: Uuid,
        name: String,
        destination: Option<String>,
        destination_team_id: Option<Uuid>,
    },
    /// On the NBA-draft early-entrants list with status `gone` (firm
    /// commitment, not just `declared`). Always counts as departing.
    DraftGone { player_id: Uuid, name: String },
}

/// A declared-but-uncommitted draft entrant. They count as returning in
/// the ceiling scenario and as departing in the floor scenario.
#[derive(Debug, Clone, Serialize)]
pub struct UncertainPlayer {
    pub player_id: Uuid,
    pub name: String,
    /// Free-text reason ("declared for NBA draft", "in portal but
    /// uncommitted", etc.). Keep human-readable for the UI tooltip.
    pub reason: String,
}

/// Audit-trail metadata for one incoming HS recruit. The synthesized
/// PlayerRow lives alongside in [`ProjectedRoster::recruits`]; this
/// struct is the UI-facing display payload (name, stars, rank tier) so
/// users can see *who* the recruits are without dredging through the
/// recruits table separately.
#[derive(Debug, Clone, Serialize)]
pub struct RecruitMeta {
    /// `recruits.id` — opaque per-recruit UUID. Used as the synthetic
    /// PlayerRow's `player_id` so the row is identifiable across the
    /// roster aggregation (no risk of collision with real player UUIDs;
    /// they come from different namespaces).
    pub recruit_id: Uuid,
    pub name: String,
    /// 247 composite national rank; `None` for unranked commits.
    pub composite_rank: Option<i32>,
    /// 1-5 star rating; `None` for unranked.
    pub star_rating: Option<i16>,
    /// Which tier the freshman profile came from — useful for the UI
    /// tooltip ("synthesized from T1 top-30 profile, expect wide
    /// variance").
    pub tier: FreshmanTier,
    /// 247's listed position (e.g. "PG", "SF", "C"). Free-text from the
    /// scouting feed — surface verbatim, don't try to bucket on it.
    pub position: Option<String>,
    /// Lower bound (q10) of the freshman-model projection. `None` when
    /// batch inference failed and we fell back to tier-mean synthesis;
    /// the synthesized PlayerRow's `cam_v3` field still carries the
    /// tier-mean point estimate in that case.
    pub projected_campom_lower: Option<f32>,
    /// Upper bound (q90) of the freshman-model projection. Pairs with
    /// `projected_campom_lower`; both `None` together on fallback.
    pub projected_campom_upper: Option<f32>,
}

/// Coarse freshman-impact buckets. We map `composite_rank` → tier and
/// look up a tier-mean profile to synthesize the recruit's PlayerRow.
/// 4-tier scheme calibrated from the empirical (class-of-2024,
/// class-of-2025) × freshman-season-stats join — see
/// `docs/projections_methodology.md` for the calibration query and the
/// per-tier sample sizes (T1 n=52, T2 n=114, T3 n=201, T4 n=185).
///
/// The CamPom monotonicity is clean across tiers (T1 +8.97, T2 +2.41,
/// T3 +0.70, T4 −0.57) so the tiering is real signal, not noise.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FreshmanTier {
    /// `composite_rank` ∈ 1..=30. Elite recruits; "5-star tier".
    T1,
    /// `composite_rank` ∈ 31..=100. Top high-major recruits; mostly
    /// 4-star and high 3-star.
    T2,
    /// `composite_rank` ∈ 101..=250. Lower 4-star and mid 3-star.
    T3,
    /// `composite_rank` ≥ 251 OR unranked. Includes walk-on equivalents,
    /// late bloomers, internationals not in 247's database. Profile is
    /// empirically very close to T3 (composite rank stops being a strong
    /// signal past ~100) so don't claim more precision than warranted.
    T4,
}

impl FreshmanTier {
    /// Bucket a recruit by their composite_rank. `None` (unranked) → T4.
    pub fn from_rank(rank: Option<i32>) -> Self {
        match rank {
            Some(r) if r <= 30 => Self::T1,
            Some(r) if r <= 100 => Self::T2,
            Some(r) if r <= 250 => Self::T3,
            _ => Self::T4,
        }
    }
}

/// Per-tier averages. Each field is the mean across the empirical join
/// described above. Persisting as `f64` constants (not a config file)
/// because (a) the calibration is a one-time exercise to be redone
/// when more paired classes ingest, (b) compile-time access keeps the
/// hot path alloc-free, and (c) the next-level upgrade is a *prior
/// model*, not a richer tier table — so investing in JSON config
/// scaffolding would be churn.
///
/// Calibration query lives in `docs/projections_methodology.md` so the
/// numbers can be re-derived against fresh data. Re-run when adding a
/// new class year.
struct FreshmanProfile {
    mpg: f64,
    gp: f64,
    ppg: f64,
    rpg: f64,
    apg: f64,
    spg: f64,
    bpg: f64,
    topg: f64,
    ts: f64,
    efg: f64,
    usg: f64,
    ast_pct: f64,
    tov_pct: f64,
    orb_pct: f64,
    drb_pct: f64,
    stl_pct: f64,
    blk_pct: f64,
    ft_rate: f64,
    cam_v3: f64,
}

const T1_PROFILE: FreshmanProfile = FreshmanProfile {
    mpg: 24.0,
    gp: 31.1,
    ppg: 11.80,
    rpg: 4.39,
    apg: 1.82,
    spg: 0.80,
    bpg: 0.53,
    topg: 1.41,
    ts: 0.573,
    efg: 0.534,
    usg: 0.232,
    ast_pct: 0.133,
    tov_pct: 0.124,
    orb_pct: 6.308,
    drb_pct: 15.392,
    stl_pct: 1.792,
    blk_pct: 2.400,
    ft_rate: 0.361,
    cam_v3: 8.97,
};
const T2_PROFILE: FreshmanProfile = FreshmanProfile {
    mpg: 14.4,
    gp: 23.3,
    ppg: 5.48,
    rpg: 2.37,
    apg: 1.03,
    spg: 0.52,
    bpg: 0.32,
    topg: 0.80,
    ts: 0.537,
    efg: 0.509,
    usg: 0.194,
    ast_pct: 0.107,
    tov_pct: 0.158,
    orb_pct: 6.630,
    drb_pct: 14.246,
    stl_pct: 1.947,
    blk_pct: 2.512,
    ft_rate: 0.363,
    cam_v3: 2.41,
};
const T3_PROFILE: FreshmanProfile = FreshmanProfile {
    mpg: 12.7,
    gp: 21.9,
    ppg: 4.24,
    rpg: 2.15,
    apg: 0.84,
    spg: 0.44,
    bpg: 0.24,
    topg: 0.68,
    ts: 0.524,
    efg: 0.494,
    usg: 0.175,
    ast_pct: 0.108,
    tov_pct: 0.154,
    orb_pct: 8.095,
    drb_pct: 13.988,
    stl_pct: 2.005,
    blk_pct: 2.396,
    ft_rate: 0.416,
    cam_v3: 0.70,
};
const T4_PROFILE: FreshmanProfile = FreshmanProfile {
    mpg: 14.1,
    gp: 22.4,
    ppg: 4.91,
    rpg: 2.21,
    apg: 0.95,
    spg: 0.48,
    bpg: 0.21,
    topg: 0.78,
    ts: 0.514,
    efg: 0.484,
    usg: 0.184,
    ast_pct: 0.110,
    tov_pct: 0.151,
    orb_pct: 6.459,
    drb_pct: 13.465,
    stl_pct: 2.108,
    blk_pct: 2.111,
    ft_rate: 0.381,
    cam_v3: -0.57,
};

fn profile_for(tier: FreshmanTier) -> &'static FreshmanProfile {
    match tier {
        FreshmanTier::T1 => &T1_PROFILE,
        FreshmanTier::T2 => &T2_PROFILE,
        FreshmanTier::T3 => &T3_PROFILE,
        FreshmanTier::T4 => &T4_PROFILE,
    }
}

/// Pick the tier whose mean CamPom is closest to the model's per-recruit
/// projection. The freshman model predicts CamPom directly; we use it to
/// reassign the recruit to a *predicted-impact* tier (rather than the
/// rank-derived one), then synthesise the PlayerRow from that tier's
/// profile. Cheap path per ROADMAP §6 — keeps the tier-mean per-stat
/// *shape* (e.g. T1's higher mpg/usg, T4's lower) while letting the
/// model's prediction drive which shape we pull.
///
/// Closest-tier (min absolute distance) instead of linear scaling because
/// the four tier centroids span both signs (T1 +8.97 down to T4 −0.57)
/// and naive ratio scaling sign-flips when predicted and tier mean
/// disagree. The 4-bucket discretisation loses some resolution at the
/// extremes — predicted +12 and +6 both land in T1 — but the
/// per-recruit CamPom is also surfaced as `cam_v3` on the row, so
/// downstream consumers that look at impact directly (e.g.
/// `roster_features::normalize_rotation`) still see the continuous
/// signal. Multi-output per-stat regression would replace this
/// heuristic; tracked as the principled-path follow-up in ROADMAP.
pub fn tier_from_predicted_campom(predicted: f64) -> FreshmanTier {
    let candidates = [
        (FreshmanTier::T1, T1_PROFILE.cam_v3),
        (FreshmanTier::T2, T2_PROFILE.cam_v3),
        (FreshmanTier::T3, T3_PROFILE.cam_v3),
        (FreshmanTier::T4, T4_PROFILE.cam_v3),
    ];
    candidates
        .iter()
        .min_by(|(_, a), (_, b)| {
            (predicted - a)
                .abs()
                .partial_cmp(&(predicted - b).abs())
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .map(|(t, _)| *t)
        .unwrap_or(FreshmanTier::T4)
}

/// Build a synthetic PlayerRow from a recruit commit. The row plugs
/// straight into `build_roster_features` so the projection model sees
/// the freshman cohort just like it sees returning + arriving players.
///
/// `rank_tier` is the recruit's 247-derived tier — used as the fallback
/// when no model prediction is available. `predicted_campom` is the
/// freshman-impact model's central projection; when present, the row is
/// synthesised from the *predicted-impact* tier (closest-centroid match)
/// and `cam_v3` is set to the prediction rather than the tier mean. The
/// returned `FreshmanTier` is the one actually used for the profile (=
/// predicted-impact tier when available, else `rank_tier`); caller stores
/// it in `RecruitMeta` so the UI can label the team's class breakdown by
/// projected impact rather than 247 rank.
///
/// `primary_class` is set to `None` — the empirical join showed no
/// dominant archetype per tier (freshmen are stylistically diverse even
/// within rank tier), and the roster model's archetype-share features
/// degrade gracefully when None (the slot just stays at zero, same as
/// for any player without an archetype assignment).
pub fn synthesize_freshman_row(
    recruit_id: Uuid,
    rank_tier: FreshmanTier,
    predicted_campom: Option<f64>,
) -> (PlayerRow, FreshmanTier) {
    let chosen_tier = predicted_campom
        .map(tier_from_predicted_campom)
        .unwrap_or(rank_tier);
    let p = profile_for(chosen_tier);
    // `cam_v3` carries the per-recruit prediction when available — the
    // tier profile's `cam_v3` is the cohort mean, which is correct as a
    // last-resort fallback but loses the model's resolution. Downstream
    // rotation-normalisation (if/when the projection page starts using
    // it) reads `cam_v3` directly, so surfacing the continuous prediction
    // here is the load-bearing piece.
    let cam_v3 = Some(predicted_campom.unwrap_or(p.cam_v3));
    let row = PlayerRow {
        player_id: recruit_id,
        total_min: p.mpg * p.gp,
        mpg: p.mpg,
        ppg: Some(p.ppg),
        rpg: Some(p.rpg),
        apg: Some(p.apg),
        spg: Some(p.spg),
        bpg: Some(p.bpg),
        topg: Some(p.topg),
        ts: Some(p.ts),
        efg: Some(p.efg),
        usg: Some(p.usg),
        ast_pct: Some(p.ast_pct),
        tov_pct: Some(p.tov_pct),
        orb_pct: Some(p.orb_pct),
        drb_pct: Some(p.drb_pct),
        stl_pct: Some(p.stl_pct),
        blk_pct: Some(p.blk_pct),
        ft_rate: Some(p.ft_rate),
        primary_class: None,
        // Recruits are freshmen in the projected season by definition.
        class_year: Some("Fr".to_string()),
        cam_v3,
    };
    (row, chosen_tier)
}

/// One team's projected N+1 roster. The caller picks `Floor` or
/// `Ceiling` via [`Self::for_scenario`] to materialize the player rows
/// fed into the model.
#[derive(Debug, Clone, Serialize)]
pub struct ProjectedRoster {
    /// The season-N team UUID. We project AGAINST this team's identity;
    /// no season-(N+1) `teams` row exists yet because the upcoming
    /// season hasn't been ingested.
    pub team_id: Uuid,
    /// Torvik short name or NatStat short_name — whichever is populated.
    pub team_name: String,
    /// Full NatStat name; used for UI links.
    pub team_full_name: String,
    /// Returning players who are firmly back (not Sr, not in portal,
    /// not draft-gone, not draft-declared). Carry their season-N
    /// PlayerRow verbatim.
    pub returning: Vec<PlayerRow>,
    /// Incoming portal transfers committed to this team. Carry their
    /// season-N PlayerRow from their *source* team.
    pub arrivals: Vec<PlayerRow>,
    /// Incoming HS recruits committed to this team. Each pairs a
    /// synthesized PlayerRow (from [`synthesize_freshman_row`]) with
    /// audit metadata for the UI. Population-mean profile, not a
    /// per-player projection — see the `FreshmanTier` doc.
    pub recruits: Vec<(PlayerRow, RecruitMeta)>,
    /// Players who are returning in the ceiling scenario but gone in
    /// the floor scenario (declared draft entrants whose withdrawal
    /// status is still TBD). Their PlayerRow lives in `returning` only
    /// in the ceiling materialization.
    pub uncertain: Vec<(PlayerRow, UncertainPlayer)>,
    /// Audit trail: who left and why. Sized for UI display, not used by
    /// inference.
    pub departures: Vec<DepartureReason>,
}

impl ProjectedRoster {
    /// Materialize the player list the model should see under a given
    /// scenario. Returning + arrivals + recruits always; uncertain only
    /// under ceiling.
    pub fn for_scenario(&self, scenario: DraftScenario) -> Vec<PlayerRow> {
        let mut out: Vec<PlayerRow> = Vec::with_capacity(
            self.returning.len() + self.arrivals.len() + self.recruits.len() + self.uncertain.len(),
        );
        out.extend(self.returning.iter().cloned());
        out.extend(self.arrivals.iter().cloned());
        out.extend(self.recruits.iter().map(|(p, _)| p.clone()));
        if scenario == DraftScenario::Ceiling {
            out.extend(self.uncertain.iter().map(|(p, _)| p.clone()));
        }
        out
    }
}

/// One row of the draft early-entrants JSON. Fields match the v1 shape
/// described in `data/draft/2026_early_entrants.json`.
#[derive(Debug, Clone, Deserialize)]
pub struct DraftEntrant {
    pub name: String,
    pub current_team: String,
    pub status: String,
}

/// Load + parse `data/draft/{year}_early_entrants.json`. Caller is the
/// route handler; the route holds `season` and constructs the path.
pub fn load_draft_entrants(path: &Path) -> Result<Vec<DraftEntrant>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let parsed: Vec<DraftEntrant> = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::other(format!("parse {}: {e}", path.display())))?;
    Ok(parsed)
}

/// One pick from the Tankathon mock draft. The API surfaces this on
/// uncertain (`?`) draft entrants — players who've declared but haven't
/// withdrawn — so users can eyeball "is this player projected to be
/// drafted high enough to stay gone, or are they in the gray zone."
/// Strictly informational in Phase 1: no auto-promotion to `gone`.
#[derive(Debug, Clone, Deserialize)]
pub struct MockPick {
    pub pick: i32,
    pub name: String,
    pub team: String,
    pub school: String,
    pub position: String,
}

/// Top-level shape of `data/draft/{year}_mock_draft.json`. Produced by
/// `scripts/draft/parse_tankathon_mock.py` from the raw Tankathon paste.
#[derive(Debug, Clone, Deserialize)]
pub struct MockDraft {
    pub meta: serde_json::Value,
    pub picks: Vec<MockPick>,
}

/// Load + parse `data/draft/{year}_mock_draft.json`. Returns Err on
/// missing or malformed file — callers should `.map(...).unwrap_or_default()`
/// the lookup hashmap derived from the result, not the MockDraft itself,
/// to degrade gracefully when the snapshot isn't available. Same pattern
/// as `load_draft_entrants`. Phase 1 use is purely additive UI; the
/// projection still composes correctly without a mock-draft file.
pub fn load_mock_draft(path: &Path) -> Result<MockDraft, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let parsed: MockDraft = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::other(format!("parse {}: {e}", path.display())))?;
    Ok(parsed)
}

/// Helper struct for the batch roster fetch. We pull every qualified
/// player on every team for the base season in one query; partition by
/// team_id in Rust. PlayerRow fields plus team_id, full name (for
/// audit), class_year (for senior detection).
#[derive(sqlx::FromRow, Clone)]
struct RosterRow {
    player_id: Uuid,
    player_name: String,
    team_id: Uuid,
    class_year: Option<String>,
    total_min: f64,
    mpg: f64,
    ppg: Option<f64>,
    rpg: Option<f64>,
    apg: Option<f64>,
    spg: Option<f64>,
    bpg: Option<f64>,
    topg: Option<f64>,
    ts: Option<f64>,
    efg: Option<f64>,
    usg: Option<f64>,
    ast_pct: Option<f64>,
    tov_pct: Option<f64>,
    orb_pct: Option<f64>,
    drb_pct: Option<f64>,
    stl_pct: Option<f64>,
    blk_pct: Option<f64>,
    ft_rate: Option<f64>,
    primary_class: Option<String>,
    cam_v3: Option<f64>,
}

/// Advance a class year by one season for forward projection. A returner
/// who was a `Jr` in the base season is a `Sr` in the projected season;
/// arrivals (transfers, who played last season) age up the same way.
/// Tolerates the inconsistently-stored vocab (`Sr` / `Senior` / `SR`).
/// A returning `Sr` (a 5th-year / grad returner — base-season graduating
/// seniors are filtered to `departures` before this is reached, but
/// grad-transfer *arrivals* can carry `Sr`) stays `Sr`. Unrecognized or
/// missing values pass through unchanged.
fn age_up_class_year(cy: Option<String>) -> Option<String> {
    let lc = cy.as_deref()?.to_ascii_lowercase();
    let next = if lc.starts_with("fr") {
        "So"
    } else if lc.starts_with("so") {
        "Jr"
    } else if lc.starts_with("jr")
        || lc.starts_with("ju")
        || lc.starts_with("sr")
        || lc.starts_with("se")
    {
        // Jr → Sr; a returning Sr (5th-year / grad transfer) stays Sr.
        "Sr"
    } else {
        return cy; // unknown vocab — pass through untouched
    };
    Some(next.to_string())
}

impl RosterRow {
    fn into_player_row(self) -> PlayerRow {
        PlayerRow {
            player_id: self.player_id,
            total_min: self.total_min,
            mpg: self.mpg,
            ppg: self.ppg,
            rpg: self.rpg,
            apg: self.apg,
            spg: self.spg,
            bpg: self.bpg,
            topg: self.topg,
            ts: self.ts,
            efg: self.efg,
            usg: self.usg,
            ast_pct: self.ast_pct,
            tov_pct: self.tov_pct,
            orb_pct: self.orb_pct,
            drb_pct: self.drb_pct,
            stl_pct: self.stl_pct,
            blk_pct: self.blk_pct,
            ft_rate: self.ft_rate,
            primary_class: self.primary_class,
            // Aged up one season — this PlayerRow is materialized into a
            // *projected* (next-season) roster.
            class_year: age_up_class_year(self.class_year),
            cam_v3: self.cam_v3,
        }
    }
}

/// One row from `teams` for the base season — minimum we need for
/// 247-side name resolution.
#[derive(sqlx::FromRow, Clone)]
struct TeamRow {
    id: Uuid,
    name: String,
    short_name: Option<String>,
}

/// One row from `transfers` for the base year. We only need the
/// resolved `cstat_player_id` (per the ingest resolver) and the raw
/// 247 destination name; the source team is back-derived from the
/// player's PSS row, and the audit-trail name comes from the same
/// `roster_rows` query that built the rest of the projection.
#[derive(sqlx::FromRow)]
struct TransferLink {
    cstat_player_id: Option<Uuid>,
    destination_institution: Option<String>,
}

/// One row from `recruits` for the base year's HS class. We only need
/// the team destination + the rank/star fields driving the freshman
/// profile lookup; everything else (position, height, hometown) is
/// audit metadata served on the route response separately.
#[derive(sqlx::FromRow)]
struct RecruitRow {
    recruit_id: Uuid,
    full_name: String,
    composite_rank: Option<i32>,
    star_rating: Option<i16>,
    committed_team_id: Option<Uuid>,
    #[allow(dead_code)] // kept for forensics; row-filter is in SQL
    commit_status: Option<String>,
    // Freshman-impact prior model inputs (Phase 6). Same join chain as
    // `freshman_model::fetch_freshman_features` and the recruits route —
    // pulled here so a single SQL fetch covers both the audit metadata
    // and the model features. NULL for solo signings / defunct programs;
    // sentinel-encoded inside `build_freshman_features`.
    composite_rating: Option<f32>,
    position_rank: Option<i32>,
    previous_rank: Option<i32>,
    height: Option<String>,
    weight: Option<i32>,
    position: Option<String>,
    recruit_year: Option<i32>,
    committed_team_prior_adjem: Option<f64>,
    peer_class_strength: Option<f64>,
}

/// Resolve a 247 short name to a team_id at the given season by best
/// match score across the supplied teams. `None` when no team matches.
fn resolve_team_id(teams: &[TeamRow], short: &str) -> Option<Uuid> {
    teams
        .iter()
        .filter_map(|t| {
            team_match_score(t.short_name.as_deref(), &t.name, short).map(|s| (s, t.id))
        })
        .min_by_key(|(s, _)| *s)
        .map(|(_, id)| id)
}

/// Match a draft entrant `(name, current_team)` to a season-N player_id
/// by normalized name + team-id resolution. Returns `None` when the
/// player isn't on a cstat-known D-I roster (e.g., walk-ons, foreign
/// transfers we haven't ingested).
fn match_draft_entrant(
    entrant: &DraftEntrant,
    players_by_name: &HashMap<String, Vec<(Uuid, Uuid)>>, // norm_name → [(player_id, team_id)]
    teams: &[TeamRow],
) -> Option<Uuid> {
    let key = normalize_player_name(&entrant.name);
    let candidates = players_by_name.get(&key)?;
    let want_team_id = resolve_team_id(teams, &entrant.current_team)?;
    candidates
        .iter()
        .find(|(_, tid)| *tid == want_team_id)
        .map(|(pid, _)| *pid)
}

/// Player-name normalization for cross-source joins. Lowercases,
/// accent-folds the diacritics we actually see, drops all
/// non-alphabetic characters (so `V.J.` → `vj`), and strips
/// generational suffixes (`Jr`, `Sr`, `II`–`V`, `lll` — the last one
/// is the typo'd lowercase-L variant of `III` we've seen in our
/// `players` table for a handful of rows). Same logic the transfers
/// route inlines in `crates/cstat-api/src/routes/transfers.rs`; the
/// recruit resolver uses this function too.
///
/// `pub` because the recruit-ingest resolver (a different crate)
/// needs the exact same normalization to match `recruits.full_name`
/// (247-side, e.g. `"V.J. Edgecombe"` or `"Mikel Brown Jr."`) against
/// the cstat `players.name` (e.g. `"VJ Edgecombe"` / `"Mikel Brown"`).
/// Three call sites is the right point to promote — two we tolerate.
pub fn normalize_player_name(name: &str) -> String {
    let folded: String = name
        .chars()
        .flat_map(|c| match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'Á' | 'À' | 'Â' | 'Ä' | 'Ã' | 'Å' => {
                Some('a')
            }
            'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => Some('e'),
            'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => Some('i'),
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'Ó' | 'Ò' | 'Ô' | 'Ö' | 'Õ' => Some('o'),
            'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => Some('u'),
            'ñ' | 'Ñ' => Some('n'),
            'ç' | 'Ç' => Some('c'),
            _ if c.is_alphabetic() || c.is_whitespace() => Some(c.to_ascii_lowercase()),
            _ => None,
        })
        .collect();
    folded
        .split_whitespace()
        .filter(|w| !matches!(*w, "jr" | "sr" | "ii" | "iii" | "iv" | "v" | "lll"))
        .collect::<Vec<_>>()
        .join(" ")
}

/// Compose every team's projected N+1 roster from base-season-N data.
/// One DB round-trip per source table (teams, players-with-stats,
/// transfers); the partitioning happens in Rust.
///
/// `base_season` = N (the most recently completed season; for cstat's
/// 2026 = 2025-26 college season, the 2026 portal class moves players
/// from N=2026 into N+1=2027). `draft_entrants` is the optional
/// declared/gone list — pass `&[]` to skip draft-cohort handling
/// entirely (every player who isn't a Sr or in the portal is treated
/// as returning).
pub async fn compose_all_projections(
    pool: &PgPool,
    base_season: i32,
    draft_entrants: &[DraftEntrant],
    predictor: &Predictor,
) -> Result<Vec<ProjectedRoster>, sqlx::Error> {
    // --- Pull every input table in one shot. ----------------------------
    let teams: Vec<TeamRow> =
        sqlx::query_as::<_, TeamRow>(r#"SELECT id, name, short_name FROM teams WHERE season = $1"#)
            .bind(base_season)
            .fetch_all(pool)
            .await?;

    let roster_rows: Vec<RosterRow> = sqlx::query_as::<_, RosterRow>(
        r#"
        SELECT
            p.id   AS player_id,
            p.name AS player_name,
            pss.team_id,
            p.class_year,
            (COALESCE(pss.minutes_per_game, 0) * COALESCE(pss.games_played, 0))::float8 AS total_min,
            COALESCE(pss.minutes_per_game, 0)::float8 AS mpg,
            pss.ppg, pss.rpg, pss.apg, pss.spg, pss.bpg, pss.topg,
            pss.true_shooting_pct AS ts,
            pss.effective_fg_pct  AS efg,
            pss.usage_rate        AS usg,
            pss.ast_pct, pss.tov_pct, pss.orb_pct, pss.drb_pct,
            pss.stl_pct, pss.blk_pct, pss.ft_rate,
            pa.primary_class,
            tps.cam_gbpm_v3_psos AS cam_v3
        FROM player_season_stats pss
        JOIN players p ON p.id = pss.player_id AND p.season = pss.season
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = pss.player_id AND tps.season = pss.season
        WHERE pss.season = $1
          AND COALESCE(pss.games_played, 0) >= $2
          AND COALESCE(pss.minutes_per_game, 0) >= $3
        "#,
    )
    .bind(base_season)
    .bind(QUAL_MIN_GAMES_PLAYED)
    .bind(QUAL_MIN_MPG)
    .fetch_all(pool)
    .await?;

    let transfers: Vec<TransferLink> = sqlx::query_as::<_, TransferLink>(
        r#"
        SELECT cstat_player_id, destination_institution
        FROM transfers WHERE year = $1
        "#,
    )
    .bind(base_season)
    .fetch_all(pool)
    .await?;

    // Recruits: HS class year = `base_season` (the spring of HS
    // graduation; class-of-2026 first plays in cstat-season 2027 = N+1).
    // We only consume rows with a resolved `committed_team_id`; uncommitted
    // recruits don't have a destination so they can't be assigned. Status
    // filter excludes "Uncommitted" to be defensive — the resolver may
    // have backfilled `committed_team_id` for previously-committed-then-
    // decommitted entries that 247 marked Uncommitted.
    // Pulls freshman-model features alongside audit metadata in one
    // query — mirrors the recruits-route SQL. The two LEFT JOINs (teams
    // → tm_prior via natstat_id, then team_season_stats for the season
    // BEFORE the recruit arrived) carry the school-context inputs the
    // model needs; the peer subquery aggregates per-(year, committed_team)
    // mean composite_rating for the peer-class-strength feature. Each
    // freshman gets a per-recruit prediction below.
    let recruit_rows: Vec<RecruitRow> = sqlx::query_as::<_, RecruitRow>(
        r#"
        SELECT
            r.id            AS recruit_id,
            r.full_name,
            r.composite_rank,
            r.star_rating,
            r.committed_team_id,
            r.commit_status,
            r.composite_rating,
            r.position_rank,
            r.previous_rank,
            r.height,
            r.weight,
            r.position,
            r.year                            AS recruit_year,
            adjem.adj_efficiency_margin       AS committed_team_prior_adjem,
            peer.mean_rating                  AS peer_class_strength
        FROM recruits r
        LEFT JOIN teams t
            ON t.id = r.committed_team_id
        LEFT JOIN teams tm_prior
            ON tm_prior.natstat_id = t.natstat_id AND tm_prior.season = r.year
        LEFT JOIN team_season_stats adjem
            ON adjem.team_id = tm_prior.id AND adjem.season = r.year
        LEFT JOIN (
            SELECT year, committed_team_id, AVG(composite_rating) AS mean_rating
            FROM recruits
            WHERE composite_rating IS NOT NULL AND committed_team_id IS NOT NULL
            GROUP BY year, committed_team_id
        ) peer
            ON peer.year = r.year AND peer.committed_team_id = r.committed_team_id
        WHERE r.year = $1
          AND r.committed_team_id IS NOT NULL
          AND COALESCE(r.commit_status, '') <> 'Uncommitted'
        "#,
    )
    .bind(base_season)
    .fetch_all(pool)
    .await?;

    // --- Bucket the inputs by team_id. ----------------------------------
    // Roster + audit metadata per team. The String alongside RosterRow
    // is a clone of the player's cstat name — used downstream for
    // DepartureReason audit messages without re-borrowing the row.
    let mut roster_by_team: HashMap<Uuid, Vec<(RosterRow, String)>> = HashMap::new();
    // Normalized-name → [(player_id, team_id)] for draft-entrant matching.
    let mut players_by_name: HashMap<String, Vec<(Uuid, Uuid)>> = HashMap::new();
    // player_id → source_team_id for outbound transfer attribution.
    let mut player_team: HashMap<Uuid, Uuid> = HashMap::new();
    for row in roster_rows {
        let pid = row.player_id;
        let name = row.player_name.clone();
        let team_id = row.team_id;
        players_by_name
            .entry(normalize_player_name(&name))
            .or_default()
            .push((pid, team_id));
        player_team.insert(pid, team_id);
        roster_by_team.entry(team_id).or_default().push((row, name));
    }

    // Recruits: bucket by destination team_id. Each recruit synthesises
    // a PlayerRow from the closest-impact tier profile per the freshman
    // model's CamPom projection (was tier-mean by 247 rank before
    // Phase 6). Predictions are batched: one [N, 13] tensor per model
    // for the whole class (3 ONNX runs total). On batch error, fall
    // back to rank-tier synthesis with no predicted CamPom — same
    // behaviour as before this PR.
    let feature_vectors: Vec<[f32; crate::freshman_model::FRESHMAN_NUM_FEATURES]> = recruit_rows
        .iter()
        .map(|r| {
            let feature_row = FreshmanFeatureRow {
                composite_rank: r.composite_rank,
                composite_rating: r.composite_rating,
                star_rating: r.star_rating,
                position_rank: r.position_rank,
                previous_rank: r.previous_rank,
                height: r.height.clone(),
                weight: r.weight,
                position: r.position.clone(),
                year: r.recruit_year,
                committed_team_prior_adjem: r.committed_team_prior_adjem,
                peer_class_strength: r.peer_class_strength,
            };
            build_freshman_features(&feature_row)
        })
        .collect();
    let predictions: Vec<Option<FreshmanPrediction>> =
        match predictor.predict_freshman_batch(&feature_vectors) {
            Ok(preds) => preds.into_iter().map(Some).collect(),
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    year = base_season,
                    n = recruit_rows.len(),
                    "freshman batch predict failed in compose_all_projections; \
                     falling back to rank-tier synthesis",
                );
                vec![None; recruit_rows.len()]
            }
        };

    let mut recruits_by_team: HashMap<Uuid, Vec<(PlayerRow, RecruitMeta)>> = HashMap::new();
    for (r, pred) in recruit_rows.into_iter().zip(predictions) {
        let Some(team_id) = r.committed_team_id else {
            continue; // SQL gate already filters; defensive guard.
        };
        let rank_tier = FreshmanTier::from_rank(r.composite_rank);
        // `synthesize_freshman_row` only needs the mean for tier
        // reassignment; the full band rides alongside on RecruitMeta so
        // the team-detail route can surface q10/q90 without re-running
        // the model. Fallback path keeps both None — the synthesized
        // PlayerRow's `cam_v3` is the tier-mean and surfaces alone.
        let pred_mean = pred.as_ref().map(|p| p.mean as f64);
        let (row, chosen_tier) = synthesize_freshman_row(r.recruit_id, rank_tier, pred_mean);
        let meta = RecruitMeta {
            recruit_id: r.recruit_id,
            name: r.full_name,
            composite_rank: r.composite_rank,
            star_rating: r.star_rating,
            tier: chosen_tier,
            position: r.position,
            projected_campom_lower: pred.as_ref().map(|p| p.lower),
            projected_campom_upper: pred.as_ref().map(|p| p.upper),
        };
        recruits_by_team
            .entry(team_id)
            .or_default()
            .push((row, meta));
    }

    // Transfers: bucket outbound by source_team_id (= which team is
    // losing a player) and incoming by destination_team_id (= which
    // team is gaining one). The route's existing ingestion populated
    // `cstat_player_id` per row; we use it both as the "outbound
    // player to remove from returning" identity AND as the PlayerRow
    // key to clone into the destination's `arrivals`. Audit display
    // names come from `roster_by_team` (cstat-canonical), not from
    // the 247-side string on the transfer row.
    let mut outbound_by_team: HashMap<Uuid, Vec<(Uuid, Option<String>)>> = HashMap::new();
    let mut incoming_by_team: HashMap<Uuid, Vec<Uuid>> = HashMap::new();
    for t in &transfers {
        let Some(pid) = t.cstat_player_id else {
            continue;
        };
        let Some(&source_team_id) = player_team.get(&pid) else {
            continue; // resolved cstat_player_id but the player no longer in our roster fetch
        };
        outbound_by_team
            .entry(source_team_id)
            .or_default()
            .push((pid, t.destination_institution.clone()));

        if let Some(dest_str) = t.destination_institution.as_deref()
            && let Some(dest_team_id) = resolve_team_id(&teams, dest_str)
        {
            incoming_by_team.entry(dest_team_id).or_default().push(pid);
        }
    }

    // --- Draft entrants: status → action mapping. -----------------------
    // `gone` ⇒ unconditional departure (audit reason DraftGone).
    // `declared` ⇒ uncertain; included in ceiling, excluded in floor.
    // Anything else (`staying`, `withdrawn`) ⇒ no effect (player returns
    // through the normal returning path).
    let mut firm_draft_gone: HashSet<Uuid> = HashSet::new();
    let mut declared_draft: HashSet<Uuid> = HashSet::new();
    for entrant in draft_entrants {
        let Some(pid) = match_draft_entrant(entrant, &players_by_name, &teams) else {
            continue;
        };
        match entrant.status.as_str() {
            "gone" => {
                firm_draft_gone.insert(pid);
            }
            "declared" => {
                declared_draft.insert(pid);
            }
            _ => {} // "staying", "withdrawn", unknown — no roster impact
        }
    }

    // --- Per-team composition. ------------------------------------------
    // PlayerRow lookup so the incoming-portal arrivals can pull the
    // source-team PlayerRow without re-querying.
    let player_row_lookup: HashMap<Uuid, PlayerRow> = roster_by_team
        .values()
        .flat_map(|rows| {
            rows.iter()
                .map(|(r, _)| (r.player_id, r.clone().into_player_row()))
        })
        .collect();

    let mut out: Vec<ProjectedRoster> = Vec::with_capacity(teams.len());
    for team in &teams {
        let Some(rows) = roster_by_team.get(&team.id) else {
            // Team with no qualified players in the gate — skip rather
            // than emit a zero-feature projection that the model can't
            // sensibly score.
            continue;
        };
        let outbound_pids: HashSet<Uuid> = outbound_by_team
            .get(&team.id)
            .map(|v| v.iter().map(|(p, _)| *p).collect())
            .unwrap_or_default();

        let mut returning: Vec<PlayerRow> = Vec::new();
        let mut uncertain: Vec<(PlayerRow, UncertainPlayer)> = Vec::new();
        let mut departures: Vec<DepartureReason> = Vec::new();

        for (row, name) in rows {
            let pid = row.player_id;
            // Senior graduating? class_year fits {'Sr', 'SR', 'Senior'};
            // cstat normalizes to 'Sr' but tolerate variants.
            let is_senior = row
                .class_year
                .as_deref()
                .is_some_and(|c| matches!(c, "Sr" | "SR" | "Senior" | "sr" | "senior"));
            if is_senior {
                departures.push(DepartureReason::GraduatedSenior {
                    player_id: pid,
                    name: name.clone(),
                });
                continue;
            }
            // Outbound portal commit?
            if outbound_pids.contains(&pid) {
                let dest = outbound_by_team.get(&team.id).and_then(|v| {
                    v.iter()
                        .find(|(p, _)| *p == pid)
                        .and_then(|(_, d)| d.clone())
                });
                let dest_team_id = dest.as_deref().and_then(|d| resolve_team_id(&teams, d));
                departures.push(DepartureReason::Transferred {
                    player_id: pid,
                    name: name.clone(),
                    destination: dest,
                    destination_team_id: dest_team_id,
                });
                continue;
            }
            // Firm NBA draft departure?
            if firm_draft_gone.contains(&pid) {
                departures.push(DepartureReason::DraftGone {
                    player_id: pid,
                    name: name.clone(),
                });
                continue;
            }
            // Declared (uncertain) → bucket separately so the route can
            // surface floor/ceiling. Player row carried so ceiling
            // materialization includes them.
            if declared_draft.contains(&pid) {
                uncertain.push((
                    row.clone().into_player_row(),
                    UncertainPlayer {
                        player_id: pid,
                        name: name.clone(),
                        reason: "declared for NBA draft (status pending)".into(),
                    },
                ));
                continue;
            }
            // Otherwise: returning.
            returning.push(row.clone().into_player_row());
        }

        // Incoming portal arrivals (their season-N source-team PlayerRow).
        let arrivals: Vec<PlayerRow> = incoming_by_team
            .get(&team.id)
            .map(|pids| {
                pids.iter()
                    .filter_map(|p| player_row_lookup.get(p).cloned())
                    .collect()
            })
            .unwrap_or_default();

        let recruits: Vec<(PlayerRow, RecruitMeta)> =
            recruits_by_team.remove(&team.id).unwrap_or_default();

        out.push(ProjectedRoster {
            team_id: team.id,
            team_name: team.short_name.clone().unwrap_or_else(|| team.name.clone()),
            team_full_name: team.name.clone(),
            returning,
            arrivals,
            recruits,
            uncertain,
            departures,
        });
    }

    Ok(out)
}

/// Forward-project next-season cam_v3 for a set of returning / arriving
/// players (the Phase B impact-aggregation pipeline's per-player input).
///
/// OOF-first: for a historical `target_season` the trajectory model
/// trained on, `trajectory_oof_predictions` holds leave-one-pair-out
/// predictions — honest, not in-sample. Players without an OOF row
/// (the live forward year, or transitions the model didn't train on)
/// fall through to live trajectory inference off their `base_season`
/// line.
///
/// Returns a `player_id → projected cam_v3` map. Players the trajectory
/// model can't score (no qualifying prior season) are simply absent;
/// `roster_impact::apply_projected_cam_v3` then leaves their existing
/// cam_v3 in place (a "no growth projected" fallback). Recruits are not
/// passed here — their cam_v3 already carries the freshman model's
/// prediction from `synthesize_freshman_row`.
pub async fn project_returner_cam_v3(
    pool: &PgPool,
    predictor: &Predictor,
    player_ids: &[Uuid],
    base_season: i32,
    target_season: i32,
) -> Result<HashMap<Uuid, f64>, sqlx::Error> {
    if player_ids.is_empty() {
        return Ok(HashMap::new());
    }
    // OOF held-out predictions for the historical target seasons.
    let mut out: HashMap<Uuid, f64> = fetch_trajectory_oof(pool, player_ids, target_season)
        .await?
        .into_iter()
        .map(|(pid, p)| (pid, p.mean as f64))
        .collect();

    // Live inference for everyone without an OOF row.
    let need_live: Vec<Uuid> = player_ids
        .iter()
        .filter(|pid| !out.contains_key(*pid))
        .copied()
        .collect();
    if !need_live.is_empty() {
        let row_map = fetch_player_trajectory_rows(pool, &need_live, base_season).await?;
        let mut ids: Vec<Uuid> = Vec::with_capacity(row_map.len());
        let mut feats = Vec::with_capacity(row_map.len());
        for (pid, row) in row_map {
            ids.push(pid);
            feats.push(build_trajectory_features(&row, base_season));
        }
        if !feats.is_empty() {
            match predictor.predict_trajectory_batch(&feats) {
                Ok(preds) => {
                    for (pid, pred) in ids.into_iter().zip(preds) {
                        out.insert(pid, pred.mean as f64);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        n = feats.len(),
                        "trajectory batch predict failed in project_returner_cam_v3; \
                         affected players keep their current cam_v3",
                    );
                }
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pr(mpg: f64, cam_v3: Option<f64>) -> PlayerRow {
        PlayerRow {
            player_id: Uuid::new_v4(),
            total_min: mpg * 30.0,
            mpg,
            ppg: Some(10.0),
            rpg: None,
            apg: None,
            spg: None,
            bpg: None,
            topg: None,
            ts: Some(0.55),
            efg: None,
            usg: Some(20.0),
            ast_pct: None,
            tov_pct: None,
            orb_pct: None,
            drb_pct: None,
            stl_pct: None,
            blk_pct: None,
            ft_rate: None,
            primary_class: Some("Wizard".into()),
            class_year: None,
            cam_v3,
        }
    }

    #[test]
    fn for_scenario_includes_uncertain_only_in_ceiling() {
        let returning = vec![pr(30.0, Some(5.0))];
        let arrivals = vec![pr(25.0, Some(4.0))];
        let uncertain = vec![(
            pr(20.0, Some(3.0)),
            UncertainPlayer {
                player_id: Uuid::new_v4(),
                name: "X".into(),
                reason: "draft".into(),
            },
        )];
        let r = ProjectedRoster {
            team_id: Uuid::new_v4(),
            team_name: "Foo".into(),
            team_full_name: "Foo Bar".into(),
            returning,
            arrivals,
            recruits: vec![],
            uncertain,
            departures: vec![],
        };
        assert_eq!(r.for_scenario(DraftScenario::Floor).len(), 2);
        assert_eq!(r.for_scenario(DraftScenario::Ceiling).len(), 3);
    }

    #[test]
    fn for_scenario_includes_recruits_in_both_floor_and_ceiling() {
        // Returning + arrivals + 2 recruits in both scenarios; the
        // uncertain bucket lives in ceiling only.
        let returning = vec![pr(28.0, Some(4.0))];
        let arrivals = vec![pr(20.0, Some(2.0))];
        let recruits = vec![
            (
                synthesize_freshman_row(Uuid::new_v4(), FreshmanTier::T1, None).0,
                RecruitMeta {
                    recruit_id: Uuid::new_v4(),
                    name: "5★".into(),
                    composite_rank: Some(5),
                    star_rating: Some(5),
                    tier: FreshmanTier::T1,
                    position: None,
                    projected_campom_lower: None,
                    projected_campom_upper: None,
                },
            ),
            (
                synthesize_freshman_row(Uuid::new_v4(), FreshmanTier::T3, None).0,
                RecruitMeta {
                    recruit_id: Uuid::new_v4(),
                    name: "3★".into(),
                    composite_rank: Some(180),
                    star_rating: Some(3),
                    tier: FreshmanTier::T3,
                    position: None,
                    projected_campom_lower: None,
                    projected_campom_upper: None,
                },
            ),
        ];
        let uncertain = vec![(
            pr(20.0, Some(3.0)),
            UncertainPlayer {
                player_id: Uuid::new_v4(),
                name: "X".into(),
                reason: "draft".into(),
            },
        )];
        let r = ProjectedRoster {
            team_id: Uuid::new_v4(),
            team_name: "Foo".into(),
            team_full_name: "Foo Bar".into(),
            returning,
            arrivals,
            recruits,
            uncertain,
            departures: vec![],
        };
        // Floor: 1 returning + 1 arrival + 2 recruits = 4
        assert_eq!(r.for_scenario(DraftScenario::Floor).len(), 4);
        // Ceiling: 4 + 1 uncertain = 5
        assert_eq!(r.for_scenario(DraftScenario::Ceiling).len(), 5);
    }

    #[test]
    fn freshman_tier_buckets_match_rank() {
        assert_eq!(FreshmanTier::from_rank(Some(1)), FreshmanTier::T1);
        assert_eq!(FreshmanTier::from_rank(Some(30)), FreshmanTier::T1);
        assert_eq!(FreshmanTier::from_rank(Some(31)), FreshmanTier::T2);
        assert_eq!(FreshmanTier::from_rank(Some(100)), FreshmanTier::T2);
        assert_eq!(FreshmanTier::from_rank(Some(101)), FreshmanTier::T3);
        assert_eq!(FreshmanTier::from_rank(Some(250)), FreshmanTier::T3);
        assert_eq!(FreshmanTier::from_rank(Some(251)), FreshmanTier::T4);
        assert_eq!(FreshmanTier::from_rank(Some(9999)), FreshmanTier::T4);
        assert_eq!(FreshmanTier::from_rank(None), FreshmanTier::T4);
    }

    #[test]
    fn synthesize_freshman_row_uses_tier_profile() {
        // Pre-Phase-6 fallback path: no predicted CamPom → use the
        // rank-tier profile. T1 elite carries cam_v3 +8.97 by construction.
        let (row, chosen) = synthesize_freshman_row(Uuid::new_v4(), FreshmanTier::T1, None);
        assert_eq!(chosen, FreshmanTier::T1);
        assert_eq!(row.cam_v3, Some(8.97));
        assert_eq!(row.mpg, 24.0);
        assert!((row.total_min - 24.0 * 31.1).abs() < 1e-6);
        // T4 (unranked) is far below T1 on cam_v3.
        let (unranked, _) = synthesize_freshman_row(Uuid::new_v4(), FreshmanTier::T4, None);
        assert!(unranked.cam_v3.unwrap() < row.cam_v3.unwrap());
        // No archetype — synthesised row leaves the slot empty.
        assert_eq!(row.primary_class, None);
    }

    #[test]
    fn synthesize_freshman_row_reassigns_tier_from_prediction() {
        // Low rank (T4) but high model prediction → closest-impact tier
        // is T1; synthesised PlayerRow takes T1's profile (mpg, usg, etc.)
        // and `cam_v3` carries the per-recruit prediction, not the
        // tier mean. Mirrors the Wagler-style "model overrules rank" case.
        let (row, chosen) = synthesize_freshman_row(Uuid::new_v4(), FreshmanTier::T4, Some(8.0));
        assert_eq!(chosen, FreshmanTier::T1);
        assert_eq!(row.mpg, T1_PROFILE.mpg);
        assert_eq!(row.usg, Some(T1_PROFILE.usg));
        assert_eq!(row.cam_v3, Some(8.0));

        // Conversely: high rank (T1) but bearish model prediction → tier
        // reassigned downward. cam_v3 carries the prediction.
        let (bearish, bearish_tier) =
            synthesize_freshman_row(Uuid::new_v4(), FreshmanTier::T1, Some(-0.5));
        assert_eq!(bearish_tier, FreshmanTier::T4);
        assert_eq!(bearish.mpg, T4_PROFILE.mpg);
        assert_eq!(bearish.cam_v3, Some(-0.5));
    }

    #[test]
    fn tier_from_predicted_campom_picks_closest_centroid() {
        // Tier means: T1 +8.97, T2 +2.41, T3 +0.70, T4 -0.57. Midpoint
        // boundaries are (8.97+2.41)/2 = 5.69, (2.41+0.70)/2 = 1.555,
        // (0.70-0.57)/2 = 0.065. Pick test values clearly inside each
        // bucket so the boundary semantics aren't load-bearing.
        assert_eq!(tier_from_predicted_campom(20.0), FreshmanTier::T1);
        assert_eq!(tier_from_predicted_campom(8.97), FreshmanTier::T1);
        assert_eq!(tier_from_predicted_campom(7.0), FreshmanTier::T1);
        assert_eq!(tier_from_predicted_campom(2.41), FreshmanTier::T2);
        assert_eq!(tier_from_predicted_campom(3.0), FreshmanTier::T2);
        assert_eq!(tier_from_predicted_campom(0.70), FreshmanTier::T3);
        assert_eq!(tier_from_predicted_campom(1.0), FreshmanTier::T3);
        assert_eq!(tier_from_predicted_campom(-0.57), FreshmanTier::T4);
        assert_eq!(tier_from_predicted_campom(-2.0), FreshmanTier::T4);
        assert_eq!(tier_from_predicted_campom(-50.0), FreshmanTier::T4);
    }

    #[test]
    fn normalize_player_name_strips_suffix_and_case() {
        assert_eq!(
            normalize_player_name("Christian Anderson Jr."),
            "christian anderson",
        );
        assert_eq!(normalize_player_name("Cooper Flagg"), "cooper flagg");
    }
}
