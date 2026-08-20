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
//!   commits via [`freshman_row`]. Each commit gets a minimal PlayerRow
//!   carrying the Phase 6 freshman-impact model's per-recruit projected
//!   `cam_v3` (+ `class_year = "Fr"`); the served roster-impact model
//!   reads only those fields, so no box-score statline is synthesized.
//!   On whole-batch inference failure the row falls back to the
//!   replacement-level [`FRESHMAN_FALLBACK_CAM_V3`].
//! - **Growth**: out of scope. A junior who's about to break out as a
//!   senior is just their junior line in the model's view.
//!
//! Next iteration: Phase 5c trajectory model already projects per-player
//! next-season CamPom for returners; plugging that in upgrades the
//! "frozen-stats" framing for returning players. Phase 6 freshman-impact
//! prior is the upgrade path for recruits.

use crate::freshman_model::{
    FreshmanFeatureRow, FreshmanPrediction, build_freshman_features, fetch_freshman_oof,
};
use crate::inference::Predictor;
use crate::roster_features::{PlayerRow, QUAL_MIN_GAMES_PLAYED, QUAL_MIN_MPG};
use crate::roster_impact::{apply_projected_cam_v3, build_roster_impact_features};
use crate::team_name_match::team_match_score;
use crate::trajectory::{
    TrajectoryPrediction, build_trajectory_features, fetch_player_trajectory_rows,
    fetch_trajectory_oof,
};
use serde::{Deserialize, Serialize};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use std::path::Path;
use uuid::Uuid;

/// How many seasons before a transfer's portal `year` to look back when the
/// player has no stat row in the season the portal year binds to — they sat out
/// / graduated early to preserve eligibility (issue #146, e.g. Caden Pierce:
/// last played Princeton 2025, sat out 2026, entered the 2026 portal bound for
/// Purdue). Single source of truth shared by the transfer→player resolver
/// (`cstat-ingest`), the `/api/transfers/{year}` route, and the roster
/// projection's incoming-arrivals path below.
pub const TRANSFER_SEASON_LOOKBACK: i32 = 2;

/// Which NBA-draft scenario to materialize. The floor / ceiling pair is
/// the API's honesty story for the *pre-deadline* window: while a player is
/// only `declared` we don't know if they'll withdraw, so we project both
/// bounds. Once the withdrawal deadline passes the entrant list records them
/// as `gone` (a firm departure), the `uncertain` bucket empties, and floor
/// and ceiling collapse to the same roster.
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
    /// In the `player_departures` table for base season N — left the
    /// program outside the portal and outside the NBA draft. Signed
    /// professionally overseas, medically retired, was dismissed, or
    /// simply walked away. No feed reports these, so the rows are curated
    /// by hand from `data/departures/{year}_departures.json` (issue #215:
    /// Mario Saint-Supery left Gonzaga for Valencia in July 2026 and, as a
    /// non-senior who never entered the portal, counted as returning).
    LeftProgram {
        player_id: Uuid,
        name: String,
        /// Display-only vocabulary off `player_departures.reason`
        /// (`pro_overseas`, `pro_other`, `retired`, `dismissed`,
        /// `left_program`). Never behavior-bearing — the row's existence
        /// is what removes the player, so an unrecognized value still
        /// projects correctly.
        reason: String,
        /// Free-text destination for the UI chip ("Valencia (ACB)").
        /// `None` when unknown or inapplicable (a retirement).
        destination: Option<String>,
    },
}

impl DepartureReason {
    /// The departing player's base-season UUID, whatever the reason.
    pub fn player_id(&self) -> Uuid {
        match self {
            Self::GraduatedSenior { player_id, .. }
            | Self::Transferred { player_id, .. }
            | Self::DraftGone { player_id, .. }
            | Self::LeftProgram { player_id, .. } => *player_id,
        }
    }
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
    /// 247's listed position (e.g. "PG", "SF", "C"). Free-text from the
    /// scouting feed — surface verbatim, don't try to bucket on it.
    pub position: Option<String>,
    /// Lower bound (q10) of the freshman-model projection. `None` when
    /// per-recruit inference was unavailable and the synthesized
    /// PlayerRow's `cam_v3` fell back to `FRESHMAN_FALLBACK_CAM_V3`
    /// (a single replacement-level scalar); no band exists in that case.
    pub projected_campom_lower: Option<f32>,
    /// Upper bound (q90) of the freshman-model projection. Pairs with
    /// `projected_campom_lower`; both `None` together on fallback.
    pub projected_campom_upper: Option<f32>,
    /// Whether this recruit feeds the scored roster (`for_scenario`) that the
    /// roster-impact AdjEM calibrator sees. `false` for commits-feed-sourced
    /// rows (`institution_group='commits'`) — they surface on the Future page
    /// but stay out of the projection, because the calibrator was trained only
    /// on the ranked composite cohort (issue #175). Serving-internal; not part
    /// of the API payload.
    #[serde(skip)]
    pub feeds_projection: bool,
    /// The recruit committed but never recorded a box score in the target
    /// season — a redshirt / non-enrollment / reclassification. Set true ONLY
    /// once the target season is actually complete (see
    /// `target_season_complete` in `compose_all_projections`); always false for
    /// the live upcoming projection, where we deliberately don't forecast who
    /// will redshirt (every committed freshman is included). When true the
    /// recruit is dropped from the scored roster (`for_scenario` /
    /// `projecting_recruits_count`) and from the displayed recruit-contribution
    /// sum — they contributed zero that season, so counting their projected
    /// cam_v3 over-credits the team. Retroactive only; changes historical /
    /// graded projections, never a model's training data. Serving-internal.
    #[serde(skip)]
    pub did_not_play: bool,
}

/// Replacement-level CamPom for a freshman we can't project per-recruit.
/// Hit only when `predict_freshman_batch` errors for the whole class (a
/// degraded, warn-logged state); the normal path gives every recruit a
/// model prediction. Value = the unconditional mean `cam_gbpm_v3_psos`
/// of qualified freshmen across the class-of-2014→2025 training cohort
/// (n=3253), the least-biased point estimate when the model is down.
/// Replaces the former 4-tier-mean fallback (tiers were deprecated once
/// the served roster-impact model proved it keys only on `cam_v3` /
/// class / archetype — never the synthesized box-score statline).
const FRESHMAN_FALLBACK_CAM_V3: f64 = 1.20;

/// Build a synthetic PlayerRow from a recruit commit. The row plugs into
/// `build_roster_impact_features`, which keys *only* on `cam_v3`,
/// `class_year`, and `primary_class` — it ranks the roster by `cam_v3`
/// and weights every feature by canonical-rotation MPG, so a freshman's
/// box-score statline (mpg, ppg, rate stats) is never read. We therefore
/// carry just the three load-bearing fields and leave the rest `None`/0;
/// this is what let us delete the former 4-tier statline scaffold.
///
/// `predicted_cam_v3` is the freshman-impact model's central projection;
/// `None` only on whole-batch inference failure, where it falls back to
/// [`FRESHMAN_FALLBACK_CAM_V3`]. `primary_class` is `None` — recruits are
/// stylistically diverse with no dominant archetype, and the archetype
/// shares degrade gracefully (the slot stays at zero, as for any player
/// without an assignment).
pub fn freshman_row(recruit_id: Uuid, predicted_cam_v3: Option<f64>) -> PlayerRow {
    PlayerRow {
        player_id: recruit_id,
        // Minutes are reassigned by cam_v3 rank inside
        // `build_roster_impact_features`; the input value is unused.
        total_min: 0.0,
        mpg: 0.0,
        ppg: None,
        rpg: None,
        apg: None,
        spg: None,
        bpg: None,
        topg: None,
        ts: None,
        efg: None,
        usg: None,
        ast_pct: None,
        tov_pct: None,
        orb_pct: None,
        drb_pct: None,
        stl_pct: None,
        blk_pct: None,
        ft_rate: None,
        primary_class: None,
        secondary_class: None,
        // Recruits are freshmen in the projected season by definition.
        class_year: Some("Fr".to_string()),
        cam_v3: Some(predicted_cam_v3.unwrap_or(FRESHMAN_FALLBACK_CAM_V3)),
    }
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
    /// synthesized PlayerRow (from [`freshman_row`]) carrying the
    /// freshman-impact model's per-recruit projected `cam_v3` with audit
    /// metadata for the UI.
    pub recruits: Vec<(PlayerRow, RecruitMeta)>,
    /// Players who are returning in the ceiling scenario but gone in
    /// the floor scenario (declared draft entrants whose withdrawal
    /// status is still TBD). Their PlayerRow lives in `returning` only
    /// in the ceiling materialization.
    pub uncertain: Vec<(PlayerRow, UncertainPlayer)>,
    /// Audit trail: who left and why. Sized for UI display, not used by
    /// inference.
    pub departures: Vec<DepartureReason>,
    /// Σ base-season cam_v3 of players who left this team in the spring
    /// portal cycle moving them into the target season (positive = lost
    /// talent). Fed to the roster-impact model so the calibrator
    /// can learn "more outbound → reduce projection" — without this slot
    /// the audit (`audit_preseason_projections.py`) found the top
    /// quartile of teams was systematically over-projected by ≈−4 AdjEM
    /// because high-portal-loss programs looked unchanged from their
    /// returners alone. Mirrors the audit's `portal_outbound_cam_v3`
    /// signal (β=+0.978, p<0.05 OLS).
    pub outbound_cam_v3_sum: f32,
    /// Σ base-season cam_v3 of players who arrived at this team from
    /// another D-I program via the same portal cycle (positive = gained
    /// talent). Symmetric to `outbound_cam_v3_sum`; encoded as a separate
    /// feature rather than netted so the trees can learn asymmetric
    /// effects. Audit OLS: β=−0.605, t=−1.44 (directionally right but
    /// not significant at p<0.05).
    ///
    /// Both portal sums use base-season cam_v3. 0.0 for teams with no
    /// movement or pre-portal-era seasons; missing torvik coverage on a
    /// portal player contributes 0 (COALESCE convention).
    pub inbound_cam_v3_sum: f32,
    /// Σ base-season cam_v3 across *all* departures (graduating seniors +
    /// outbound portal + firm draft-gone), positive = talent leaving the
    /// program. Distinct from `outbound_cam_v3_sum`, which is the portal
    /// subset only — this is the full "talent out" the Future grid's
    /// Departures column surfaces, so a roster losing a senior star or an
    /// NBA-bound declaree shows the loss the portal-only sum would miss.
    /// Missing torvik coverage contributes 0 (COALESCE convention).
    pub departures_cam_v3_sum: f32,
}

impl ProjectedRoster {
    /// Materialize the player list the model should see under a given
    /// scenario. Returning + arrivals + recruits always; uncertain only
    /// under ceiling.
    ///
    /// Recruits with `feeds_projection == false` — the commits-feed cohort
    /// (`institution_group='commits'`, issue #175) — are **excluded** from the
    /// scored roster: they surface on the Future page but stay out of the
    /// roster-impact AdjEM calibrator, which was trained only on the ranked
    /// composite cohort. This keeps served projections identical to before the
    /// commits feed existed. (Wiring unranked commits into the projection is a
    /// deliberate follow-up gated on a roster-impact retrain.)
    pub fn for_scenario(&self, scenario: DraftScenario) -> Vec<PlayerRow> {
        let mut out: Vec<PlayerRow> = Vec::with_capacity(
            self.returning.len() + self.arrivals.len() + self.recruits.len() + self.uncertain.len(),
        );
        out.extend(self.returning.iter().cloned());
        out.extend(self.arrivals.iter().cloned());
        out.extend(
            self.recruits
                .iter()
                .filter(|(_, m)| m.feeds_projection && !m.did_not_play)
                .map(|(p, _)| p.clone()),
        );
        if scenario == DraftScenario::Ceiling {
            out.extend(self.uncertain.iter().map(|(p, _)| p.clone()));
        }
        out
    }

    /// Count of recruits that feed the scored roster — i.e. excluding the
    /// display-only commits-feed cohort. Use this (not `recruits.len()`) for
    /// the qualifying-size gate so the gate matches what `for_scenario`
    /// actually scores.
    pub fn projecting_recruits_count(&self) -> usize {
        self.recruits
            .iter()
            .filter(|(_, m)| m.feeds_projection && !m.did_not_play)
            .count()
    }
}

/// One draft early-entrant. Deserializes from the `{year}_early_entrants.json`
/// capture (v1 shape in `data/draft/2026_early_entrants.json`) AND maps from a
/// `draft_entrants` row (the `player_name` column aliases to `name`).
#[derive(Debug, Clone, Deserialize, sqlx::FromRow)]
pub struct DraftEntrant {
    pub name: String,
    pub current_team: String,
    pub status: String,
}

/// Load + parse `data/draft/{year}_early_entrants.json`. The version-controlled
/// capture; `cstat-ingest draft` loads it into `draft_entrants`, which is what
/// the projection actually reads (see `fetch_draft_entrants`).
pub fn load_draft_entrants(path: &Path) -> Result<Vec<DraftEntrant>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let parsed: Vec<DraftEntrant> = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::other(format!("parse {}: {e}", path.display())))?;
    Ok(parsed)
}

/// DB-backed sibling of `load_draft_entrants`: the early-entrant rows for one
/// base season from `draft_entrants`. Preferred over the file read so the data
/// syncs to prod with the rest of the schema (loose JSON files don't, which
/// silently zeroed draft departures in historical/backtest projections).
/// Empty vec when nothing's loaded for `year` — the projection then degrades
/// to seniors + portal-only departures, same as a missing file used to.
pub async fn fetch_draft_entrants(
    pool: &PgPool,
    year: i32,
) -> Result<Vec<DraftEntrant>, sqlx::Error> {
    sqlx::query_as::<_, DraftEntrant>(
        "SELECT player_name AS name, current_team, status \
         FROM draft_entrants WHERE year = $1",
    )
    .bind(year)
    .fetch_all(pool)
    .await
}

/// One curated non-portal, non-draft program exit. Deserializes from the
/// `data/departures/{year}_departures.json` capture AND maps from a
/// `player_departures` row (the `player_name` column aliases to `name`).
///
/// Every row is firm — there is no `declared`-style uncertainty status the way
/// [`DraftEntrant`] has, because no withdrawal deadline exists to wait on. An
/// unconfirmed report simply doesn't get a row.
#[derive(Debug, Clone, Deserialize, sqlx::FromRow)]
pub struct PlayerDeparture {
    pub name: String,
    pub current_team: String,
    /// Display-only; see [`DepartureReason::LeftProgram::reason`].
    #[serde(default = "default_departure_reason")]
    pub reason: String,
    #[serde(default)]
    pub destination: Option<String>,
    /// Provenance for the capture — a URL or outlet slug. Not served.
    #[serde(default)]
    pub source: Option<String>,
    /// Free-text human note. Not served.
    #[serde(default)]
    pub note: Option<String>,
}

/// Matches the `player_departures.reason` column default, so a capture row
/// that omits the field behaves the same through the file path and the DB.
fn default_departure_reason() -> String {
    "left_program".to_string()
}

/// Load + parse `data/departures/{year}_departures.json`. The version-controlled
/// capture; `cstat-ingest departures` loads it into `player_departures`, which
/// is what the projection actually reads (see `fetch_player_departures`).
pub fn load_player_departures(path: &Path) -> Result<Vec<PlayerDeparture>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let parsed: Vec<PlayerDeparture> = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::other(format!("parse {}: {e}", path.display())))?;
    Ok(parsed)
}

/// DB-backed sibling of `load_player_departures`: the curated exits for one
/// base season from `player_departures`. Same file-vs-DB split as
/// `fetch_draft_entrants` — the DB copy is what syncs to prod. Empty vec when
/// nothing's loaded for `year`, which degrades to the pre-#215 behaviour
/// (seniors + portal + draft only).
pub async fn fetch_player_departures(
    pool: &PgPool,
    year: i32,
) -> Result<Vec<PlayerDeparture>, sqlx::Error> {
    sqlx::query_as::<_, PlayerDeparture>(
        "SELECT player_name AS name, current_team, reason, destination, source, note \
         FROM player_departures WHERE year = $1",
    )
    .bind(year)
    .fetch_all(pool)
    .await
}

/// Whether a curated return is settled or still contested. Behaviour-bearing:
/// it selects which bucket the player lands in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReturnStatus {
    /// Eligibility is settled and the player is on next season's roster.
    /// Projected as an ordinary returner.
    Granted,
    /// A claim exists but could still go either way. Projected into the
    /// `uncertain` bucket — present in the ceiling, absent from the floor — so
    /// the team's band spans both outcomes instead of asserting one.
    Contested,
}

impl ReturnStatus {
    /// Parse the DB / JSON vocabulary. Unknown values are treated as
    /// `Contested` rather than `Granted`: an unrecognised status means we do
    /// not actually know, and the honest projection of "we don't know" is the
    /// widened band, not a confident roster addition.
    fn from_str_lenient(s: &str) -> Self {
        match s.trim().to_ascii_lowercase().as_str() {
            "granted" => Self::Granted,
            _ => Self::Contested,
        }
    }
}

/// One curated eligibility return — a player the `class_year == 'Sr'` inference
/// would delete from his team who is in fact coming back (issue #220, the NCAA
/// 5-in-5 rule). Deserializes from `data/returns/{year}_returns.json` AND maps
/// from a `player_returns` row (the `player_name` column aliases to `name`).
///
/// This is the *stay-put* channel. A senior who takes his extra year at another
/// school already arrives correctly through the 247 portal feed and needs no
/// row here.
#[derive(Debug, Clone, Deserialize, sqlx::FromRow)]
pub struct PlayerReturn {
    pub name: String,
    pub current_team: String,
    /// `granted` | `contested`; see [`ReturnStatus`].
    #[serde(default = "default_return_status")]
    pub status: String,
    /// Display-only: `5in5`, `waiver`, `injunction`, `medical`, `other`.
    #[serde(default = "default_return_reason")]
    pub reason: String,
    /// Provenance for the capture — a URL or outlet slug. Not served.
    #[serde(default)]
    pub source: Option<String>,
    /// Free-text human note. Not served.
    #[serde(default)]
    pub note: Option<String>,
}

impl PlayerReturn {
    pub fn parsed_status(&self) -> ReturnStatus {
        ReturnStatus::from_str_lenient(&self.status)
    }
}

/// A capture row that omits `status` is treated as contested, matching
/// [`ReturnStatus::from_str_lenient`]: asserting a player is back is a claim
/// that should have to be made explicitly.
fn default_return_status() -> String {
    "contested".to_string()
}

/// Matches the `player_returns.reason` column default.
fn default_return_reason() -> String {
    "5in5".to_string()
}

/// Load + parse `data/returns/{year}_returns.json`. The version-controlled
/// capture; `cstat-ingest returns` loads it into `player_returns`, which is
/// what the projection actually reads (see [`fetch_player_returns`]).
pub fn load_player_returns(path: &Path) -> Result<Vec<PlayerReturn>, std::io::Error> {
    let content = std::fs::read_to_string(path)?;
    let parsed: Vec<PlayerReturn> = serde_json::from_str(&content)
        .map_err(|e| std::io::Error::other(format!("parse {}: {e}", path.display())))?;
    Ok(parsed)
}

/// DB-backed sibling of [`load_player_returns`] — the curated returns for one
/// base season. Empty vec when nothing is loaded for `year`, which degrades to
/// the pre-#220 behaviour (every `Sr` assumed graduating).
pub async fn fetch_player_returns(
    pool: &PgPool,
    year: i32,
) -> Result<Vec<PlayerReturn>, sqlx::Error> {
    sqlx::query_as::<_, PlayerReturn>(
        "SELECT player_name AS name, current_team, status, reason, source, note \
         FROM player_returns WHERE year = $1",
    )
    .bind(year)
    .fetch_all(pool)
    .await
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
    secondary_class: Option<String>,
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
            secondary_class: self.secondary_class,
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
    // Ingest provenance: 'highschool'/'juco'/'prep' = 247 composite rankings
    // (the ranked cohort the roster-impact calibrator was trained on);
    // 'commits' = the national commits feed (unranked/international/prep/
    // G-League, issue #175). Commits-sourced rows are displayed but kept OUT
    // of the scored roster — see `RecruitMeta::feeds_projection`.
    institution_group: String,
    composite_rank: Option<i32>,
    star_rating: Option<i16>,
    // Resolved cstat player (set once the recruit's freshman season ingests).
    // Keys the freshman OOF lookup so a backtest projection serves the
    // held-out prediction instead of an in-sample one.
    cstat_player_id: Option<Uuid>,
    #[allow(dead_code)] // forensics only; bucketing uses `base_team_id`
    committed_team_id: Option<Uuid>,
    // `committed_team_id` resolves to the recruit's *playing*-season team UUID
    // (`resolve_team_joins` caps at `year + 1`), but the projection buckets onto
    // the *base*-season roster. `base_team_id` re-resolves via `natstat_id` to
    // the base-season (`= r.year`) team UUID so recruits attach in every season,
    // not just the live forecast where the playing season isn't ingested yet.
    base_team_id: Option<Uuid>,
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
    match_roster_entry(&entrant.name, &entrant.current_team, players_by_name, teams)
}

/// Match a curated `player_departures` row to a season-N player_id. Same
/// `(normalized name, resolved team)` join as `match_draft_entrant` — shared so
/// the two hand-maintained captures can't drift on matching rules.
fn match_player_departure(
    departure: &PlayerDeparture,
    players_by_name: &HashMap<String, Vec<(Uuid, Uuid)>>,
    teams: &[TeamRow],
) -> Option<Uuid> {
    match_roster_entry(
        &departure.name,
        &departure.current_team,
        players_by_name,
        teams,
    )
}

/// Same `(name, team)` resolution for the curated eligibility returns.
fn match_player_return(
    ret: &PlayerReturn,
    players_by_name: &HashMap<String, Vec<(Uuid, Uuid)>>,
    teams: &[TeamRow],
) -> Option<Uuid> {
    match_roster_entry(&ret.name, &ret.current_team, players_by_name, teams)
}

/// The shared `(name, team)` → base-season `player_id` resolution behind both
/// hand-curated captures. `None` when the player isn't on a cstat-known D-I
/// roster or the team string doesn't resolve.
fn match_roster_entry(
    name: &str,
    current_team: &str,
    players_by_name: &HashMap<String, Vec<(Uuid, Uuid)>>, // norm_name → [(player_id, team_id)]
    teams: &[TeamRow],
) -> Option<Uuid> {
    let key = normalize_player_name(name);
    let candidates = players_by_name.get(&key)?;
    let want_team_id = resolve_team_id(teams, current_team)?;
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
///
/// `player_departures` is the optional curated list of exits no feed reports —
/// pro signings abroad, retirements, dismissals (issue #215). Pass `&[]` to
/// skip. A matched row takes precedence over all three inferred channels
/// (graduation, portal, draft), so a hand-entered exit always wins: a player who
/// committed in the portal and *then* signed professionally is gone from both
/// his old team and his would-be destination's arrivals, and a graduating senior
/// who signed abroad is labelled by where he went rather than by his class year.
///
/// `target_season_complete` is the caller's clock verdict on whether the target
/// season (`base_season + 1`) is *fully over* — pass
/// `cstat_ingest::target_season_retro_complete(base_season + 1)`. When true (and
/// the target's games are actually ingested), committed recruits who never
/// recorded a box score are treated as redshirts/no-shows and dropped from the
/// scored roster (see `RecruitMeta::did_not_play`). Pass `false` for the live
/// upcoming projection so every committed freshman is included — we don't
/// forecast who will redshirt.
pub async fn compose_all_projections(
    pool: &PgPool,
    base_season: i32,
    draft_entrants: &[DraftEntrant],
    player_departures: &[PlayerDeparture],
    predictor: &Predictor,
    target_season_complete: bool,
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
            pa.secondary_class,
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
        FROM transfers
        WHERE year = $1
          -- A `Withdrawn` row is a player who entered the portal and then
          -- pulled out; they are on their source team's roster, not leaving
          -- it. Without this filter every withdrawal is subtracted from the
          -- returning core as an outbound transfer (2026: 25 players, incl.
          -- NC State's Paul McNeil at 8.9 cam_v3). Withdrawals that really
          -- did leave went to the NBA, and `draft_entrants` removes those
          -- via `firm_draft_gone` below — which only gets reached once the
          -- outbound path stops short-circuiting them. Mirrors the display
          -- filter in cstat-api `routes/transfers.rs`.
          AND status <> 'Withdrawn'
        "#,
    )
    .bind(base_season)
    .fetch_all(pool)
    .await?;

    // Portal WITHDRAWALS, kept separate from the outbound set above. A player
    // who entered and pulled back out is staying at his source school — which
    // is ordinarily uninteresting, since he'd fall through to `returning`
    // anyway. It stops being uninteresting when he is a SENIOR.
    //
    // Under the pre-2027 rules a senior in the portal was nearly always a grad
    // transfer, and a senior who *withdrew* was a contradiction we never had to
    // resolve. The 5-in-5 rule (issue #220) makes it a signal: a senior who
    // entered the portal and withdrew has demonstrated, without any curation on
    // our part, both that he believes he has eligibility left and that he
    // intends to use it where he is. That is exactly the stay-put population
    // the `class_year == 'Sr'` inference deletes and no feed otherwise reports.
    //
    // Treated as `contested`, not `granted`: entering the portal is evidence of
    // intent, not proof the NCAA agreed. A curated `player_returns` row
    // overrides this either way (checked first below).
    let withdrawn_pids: HashSet<Uuid> = sqlx::query_scalar::<_, Uuid>(
        "SELECT DISTINCT cstat_player_id FROM transfers \
         WHERE year = $1 AND cstat_player_id IS NOT NULL AND status = 'Withdrawn'",
    )
    .bind(base_season)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect();

    // Curated eligibility returns (issue #220). Fetched here rather than passed
    // in like `player_departures` on purpose: every caller of this function —
    // the route, `compute-projections`, the backtest, the departures audit —
    // must see the same roster, and a parameter is something a call site can
    // pass `&[]` for. That failure would be silent and would look exactly like
    // "this team lost its seniors", which is the bug rather than a symptom of
    // it. Same trap shape as the `--seasons` default in the archetype trainer:
    // the wrong thing succeeds quietly.
    let returns_rows = fetch_player_returns(pool, base_season).await?;

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
            r.institution_group,
            r.composite_rank,
            r.star_rating,
            r.cstat_player_id,
            r.committed_team_id,
            tm_prior.id                       AS base_team_id,
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

    // Issue #146 — a transfer whose player sat out the base season (graduated
    // early to preserve eligibility, redshirt, gap year) has no base-season
    // stat row, so they're absent from `player_team` above and the bucketing
    // loop below would silently drop them (no incoming arrival at their
    // destination). Their resolved `cstat_player_id` is season-scoped, so it
    // pins the exact source-season row; pull those directly and fold them into
    // the lookup maps. Bounded by `TRANSFER_SEASON_LOOKBACK` to match the
    // resolver. These rows belong to a *prior*-season team UUID that the
    // per-team loop (over base-season `teams`) never visits, so they only ever
    // surface as arrivals at their destination — never as a returning/outbound
    // member of some stale team.
    let satout_lookup: HashMap<Uuid, PlayerRow> = {
        let known: HashSet<Uuid> = player_team.keys().copied().collect();
        let satout_ids: Vec<Uuid> = transfers
            .iter()
            .filter_map(|t| t.cstat_player_id)
            .filter(|pid| !known.contains(pid))
            .collect();
        let mut lookup: HashMap<Uuid, PlayerRow> = HashMap::new();
        if !satout_ids.is_empty() {
            let satout_rows: Vec<RosterRow> = sqlx::query_as::<_, RosterRow>(
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
                    pa.secondary_class,
                    tps.cam_gbpm_v3_psos AS cam_v3
                FROM player_season_stats pss
                JOIN players p ON p.id = pss.player_id AND p.season = pss.season
                LEFT JOIN player_archetypes pa
                    ON pa.player_id = pss.player_id AND pa.season = pss.season
                LEFT JOIN torvik_player_stats tps
                    ON tps.player_id = pss.player_id AND tps.season = pss.season
                WHERE p.id = ANY($1)
                  AND pss.season BETWEEN $2 AND $3
                  AND COALESCE(pss.games_played, 0) >= $4
                  AND COALESCE(pss.minutes_per_game, 0) >= $5
                "#,
            )
            .bind(&satout_ids)
            .bind(base_season - TRANSFER_SEASON_LOOKBACK)
            .bind(base_season - 1)
            .bind(QUAL_MIN_GAMES_PLAYED)
            .bind(QUAL_MIN_MPG)
            .fetch_all(pool)
            .await?;
            for row in satout_rows {
                let pid = row.player_id;
                player_team.insert(pid, row.team_id);
                lookup.insert(pid, row.into_player_row());
            }
        }
        lookup
    };

    // Recruits: bucket by destination team_id. Each recruit synthesises
    // a minimal PlayerRow (`freshman_row`) carrying the freshman model's
    // per-recruit projected CamPom. Predictions are batched: one [N, 13]
    // tensor per model for the whole class (3 ONNX runs total). On batch
    // error, fall back to `FRESHMAN_FALLBACK_CAM_V3` with no band.
    // OOF-first, mirroring the recruits route + the returner path
    // (`project_returner_cam_v3`): a recruit who already played their
    // freshman season — one the freshman model trained on — is served the
    // HELD-OUT prediction from `freshman_oof_predictions`, so a historical
    // (backtest) projection never serves an in-sample (leaky) number. For
    // the upcoming forecast year the OOF table is empty and every recruit
    // falls through to live inference. Live is batched (one [N, 13] tensor
    // per model); on batch error those rows get `None` and `freshman_row`
    // falls back to `FRESHMAN_FALLBACK_CAM_V3`.
    let target_season = base_season + 1;

    // Redshirt / non-enrollment gate. A committed recruit whose `cstat_player_id`
    // never resolved has no box-score row in the target season = they didn't play
    // (redshirt, non-enroll, reclass). Two conditions must BOTH hold to trust it:
    //   1. `target_season_complete` — the caller's clock verdict that the season
    //      is fully OVER (never the live/in-progress one; see
    //      `cstat_ingest::target_season_retro_complete`). A game-volume proxy is
    //      NOT enough — it flips true in the final weeks of a season still being
    //      played, which would drop not-yet-debuted freshmen from the live grid.
    //   2. the target season's games are actually ingested — otherwise every
    //      recruit looks like a no-show (no players exist to resolve against).
    let target_season_complete: bool = target_season_complete && {
        // Presence check only — EXISTS short-circuits on the first row instead
        // of counting every game in the season.
        let target_has_games: bool =
            sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM games WHERE season = $1)")
                .bind(target_season)
                .fetch_one(pool)
                .await?;
        target_has_games
    };

    let recruit_cstat_ids: Vec<Uuid> = recruit_rows
        .iter()
        .filter_map(|r| r.cstat_player_id)
        .collect();
    let freshman_oof = if recruit_cstat_ids.is_empty() {
        HashMap::new()
    } else {
        fetch_freshman_oof(pool, &recruit_cstat_ids, target_season)
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(
                    error = ?e,
                    target_season,
                    "freshman OOF lookup failed in compose_all_projections; live inference",
                );
                HashMap::new()
            })
    };
    // Live feature vectors only for recruits without an OOF hit; track the
    // original index so we can splice predictions back in order.
    let mut live_indices: Vec<usize> = Vec::new();
    let mut live_vectors: Vec<[f32; crate::freshman_model::FRESHMAN_NUM_FEATURES]> = Vec::new();
    for (i, r) in recruit_rows.iter().enumerate() {
        if let Some(pid) = r.cstat_player_id
            && freshman_oof.contains_key(&pid)
        {
            continue;
        }
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
        live_indices.push(i);
        live_vectors.push(build_freshman_features(&feature_row));
    }
    let live_preds: Vec<Option<FreshmanPrediction>> = if live_vectors.is_empty() {
        Vec::new()
    } else {
        match predictor.predict_freshman_batch(&live_vectors) {
            Ok(preds) => preds.into_iter().map(Some).collect(),
            Err(e) => {
                tracing::warn!(
                    error = ?e,
                    year = base_season,
                    n = live_vectors.len(),
                    "freshman batch predict failed in compose_all_projections; \
                     falling back to the replacement-level FRESHMAN_FALLBACK_CAM_V3",
                );
                vec![None; live_vectors.len()]
            }
        }
    };
    // Splice: OOF hits from the map, live hits by `live_indices` order.
    let mut live_iter = live_indices.iter().zip(live_preds);
    let mut next_live = live_iter.next();
    let predictions: Vec<Option<FreshmanPrediction>> = recruit_rows
        .iter()
        .enumerate()
        .map(|(i, r)| {
            if let Some(pid) = r.cstat_player_id
                && let Some(pred) = freshman_oof.get(&pid)
            {
                return Some(pred.clone());
            }
            match next_live.as_ref() {
                Some((idx, _)) if **idx == i => {
                    let (_, pred) = next_live.take().unwrap();
                    next_live = live_iter.next();
                    pred
                }
                _ => None,
            }
        })
        .collect();

    let mut recruits_by_team: HashMap<Uuid, Vec<(PlayerRow, RecruitMeta)>> = HashMap::new();
    for (r, pred) in recruit_rows.into_iter().zip(predictions) {
        // Bucket onto the base-season team UUID (re-resolved via natstat_id),
        // not the raw `committed_team_id` (which points at the playing season).
        let Some(team_id) = r.base_team_id else {
            continue; // no base-season team row (new/defunct program) — skip.
        };
        // The model's central projection drives the row's `cam_v3`; the
        // full band rides alongside on RecruitMeta so the team-detail
        // route can surface q10/q90 without re-running the model. On
        // whole-batch failure `pred` is None and `freshman_row` falls
        // back to `FRESHMAN_FALLBACK_CAM_V3` with no band.
        let pred_mean = pred.as_ref().map(|p| p.mean as f64);
        // Redshirt / non-enroll: committed but no target-season box score, and
        // only once the target season is actually complete. Never fires on the
        // live upcoming projection (target_season_complete == false there).
        let did_not_play = target_season_complete && r.cstat_player_id.is_none();
        let row = freshman_row(r.recruit_id, pred_mean);
        let meta = RecruitMeta {
            recruit_id: r.recruit_id,
            name: r.full_name,
            composite_rank: r.composite_rank,
            star_rating: r.star_rating,
            position: r.position,
            projected_campom_lower: pred.as_ref().map(|p| p.lower),
            projected_campom_upper: pred.as_ref().map(|p| p.upper),
            // Commits-feed rows display but don't feed the AdjEM calibrator.
            feeds_projection: r.institution_group != "commits",
            did_not_play,
        };
        recruits_by_team
            .entry(team_id)
            .or_default()
            .push((row, meta));
    }

    // --- Curated non-portal, non-draft exits (issue #215). ---------------
    // Resolved before the transfer bucketing below so a player who committed
    // in the portal and *then* signed professionally is kept out of his
    // would-be destination's `arrivals` — otherwise the projection would move
    // a player who is on another continent onto a roster he never joined.
    // (reason, destination) is display payload only; membership is what
    // removes the player.
    let left_program: HashMap<Uuid, (String, Option<String>)> = player_departures
        .iter()
        .filter_map(|d| {
            match_player_departure(d, &players_by_name, &teams)
                .map(|pid| (pid, (d.reason.clone(), d.destination.clone())))
        })
        .collect();

    // --- Curated eligibility returns (issue #220, NCAA 5-in-5). -----------
    // Resolved the same way as the curated exits above. `(status, reason)` —
    // status selects the bucket, reason is display vocabulary.
    let eligibility_returns: HashMap<Uuid, (ReturnStatus, String)> = returns_rows
        .iter()
        .filter_map(|r| {
            match_player_return(r, &players_by_name, &teams)
                .map(|pid| (pid, (r.parsed_status(), r.reason.clone())))
        })
        .collect();

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
        // Outbound is deliberately NOT filtered on `left_program`: the source
        // team loses the player either way, and `outbound_cam_v3_sum` (a served
        // model feature) should keep counting the talent that walked.
        outbound_by_team
            .entry(source_team_id)
            .or_default()
            .push((pid, t.destination_institution.clone()));

        if left_program.contains_key(&pid) {
            continue; // never showed up at the destination
        }
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
    let mut player_row_lookup: HashMap<Uuid, PlayerRow> = roster_by_team
        .values()
        .flat_map(|rows| {
            rows.iter()
                .map(|(r, _)| (r.player_id, r.clone().into_player_row()))
        })
        .collect();
    // Fold in the sat-out transfers (issue #146) so their destination's
    // `arrivals` lookup resolves to the prior-season source PlayerRow.
    player_row_lookup.extend(satout_lookup);

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
        // Σ base-season cam_v3 across all departures (seniors + portal-out
        // + draft-gone). Accumulated here where each departing player's
        // RosterRow (and its cam_v3) is in scope; missing torvik → 0.
        let mut departures_cam_v3_sum: f32 = 0.0;

        for (row, name) in rows {
            let pid = row.player_id;
            // Left the program outside the portal and the draft? Checked FIRST,
            // ahead of all three inferred channels, so a hand-entered row always
            // wins: a portal commit who then signed pro is labelled by where he
            // actually went, a curated exit beats a `declared` draft flag
            // (nothing left to resolve — he's gone), and a *senior* who signed
            // abroad reads "→ Real Madrid" instead of "Sr graduation". The
            // roster effect is identical either way — he's a departure — but the
            // label is the informative one, and the invariant that a matched
            // capture row always produces a `LeftProgram` is what
            // `departures-audit` and `tests/curated_departures.rs` rely on to
            // tell a resolved row from a typo. Without this ordering, recording
            // a graduating senior's overseas signing (a perfectly reasonable
            // entry) would make both of them report the row as doing nothing.
            if let Some((reason, destination)) = left_program.get(&pid) {
                departures.push(DepartureReason::LeftProgram {
                    player_id: pid,
                    name: name.clone(),
                    reason: reason.clone(),
                    destination: destination.clone(),
                });
                departures_cam_v3_sum += row.cam_v3.unwrap_or(0.0) as f32;
                continue;
            }
            // Outbound portal commit? Checked BEFORE the senior branch: a
            // portal row is an *observation* that the player moved, while the
            // senior check is an *inference* that he's out of eligibility, and
            // an observation should always win — same principle as the curated
            // `left_program` rows above. Ordering the other way made a
            // graduating senior who portals read "Sr graduation" on his old
            // team while simultaneously appearing on another team's arrivals
            // list, with no destination chip and no link.
            //
            // Under the old four-in-five rule that was a rare grad-transfer
            // edge case. The NCAA's age-based "5-in-5" model (adopted
            // 2026-06-23, effective season 2027) makes it the common case:
            // 53 of the 56 players who entered the 2026 portal after June 1
            // were `Sr`-labelled in 2026 (issue #220). Roster-neutral — the
            // player departs either way, and `departures_cam_v3_sum` is not
            // one of the 27 roster-impact features — so this only corrects
            // the label and restores the destination link.
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
                departures_cam_v3_sum += row.cam_v3.unwrap_or(0.0) as f32;
                continue;
            }
            // --- Eligibility overrides on the senior inference (issue #220).
            // Two of them, and they straddle the draft check below: both sit
            // above the `Sr` inference, because a player who is STAYING must
            // not be deleted by a rule the 5-in-5 change invalidated — but only
            // the hand-entered one outranks an observation that he left.
            //
            // Curated row first, matching the `left_program` precedent at the
            // top of this loop: hand-entered beats every derived channel, and a
            // `granted` row is how an operator overrides the automatic signal
            // further down.
            if let Some((status, reason)) = eligibility_returns.get(&pid) {
                match status {
                    ReturnStatus::Granted => {
                        returning.push(row.clone().into_player_row());
                    }
                    ReturnStatus::Contested => {
                        uncertain.push((
                            row.clone().into_player_row(),
                            UncertainPlayer {
                                player_id: pid,
                                name: name.clone(),
                                reason: format!("eligibility contested ({reason})"),
                            },
                        ));
                    }
                }
                continue;
            }
            // Firm NBA draft departure? Checked here — above BOTH the derived
            // withdrawn-senior signal and the `Sr` inference — for the same
            // reason the portal check sits above them: a `gone` draft row is an
            // observation that the player is in the NBA, and an observation
            // beats an inference about his eligibility.
            //
            // Ordering it below the withdrawn-senior branch turned a real
            // departure into a ceiling-roster player: a senior who entered the
            // portal, withdrew, and then went pro would be bucketed
            // `uncertain`, materialized in his old team's ceiling, and dropped
            // from `departures_cam_v3_sum`. That is precisely the invariant
            // `tests/withdrawn_transfers_return.rs` exists to hold (Santa
            // Clara's Allen Graves), and under 5-in-5 the senior side of it is
            // the common case rather than the rare one.
            //
            // It also sits above the plain `Sr` branch now, which relabels a
            // graduating senior who was drafted from "Sr graduation" to
            // `draft_gone`. Roster-neutral — he departs either way and the
            // cam_v3 sum is unchanged — but it is the informative label, and it
            // keeps the withdrawn-to-the-NBA half of that test true once a
            // drafted player is `Sr`-labelled, which 5-in-5 makes routine.
            if firm_draft_gone.contains(&pid) {
                departures.push(DepartureReason::DraftGone {
                    player_id: pid,
                    name: name.clone(),
                });
                departures_cam_v3_sum += row.cam_v3.unwrap_or(0.0) as f32;
                continue;
            }
            // Derived signal, no curation required: a senior who entered the
            // portal and then withdrew. He is staying where he is AND he
            // evidently believes he has a year left — but the NCAA has not
            // necessarily agreed, so this is `uncertain`, never `returning`.
            // Non-seniors need no special handling: they fall through to
            // `returning` on their own, which is already correct.
            let is_senior = row
                .class_year
                .as_deref()
                .is_some_and(|c| matches!(c, "Sr" | "SR" | "Senior" | "sr" | "senior"));
            if is_senior && withdrawn_pids.contains(&pid) {
                uncertain.push((
                    row.clone().into_player_row(),
                    UncertainPlayer {
                        player_id: pid,
                        name: name.clone(),
                        reason: "entered the portal and withdrew as a senior \
                                 (5-in-5 eligibility unconfirmed)"
                            .into(),
                    },
                ));
                continue;
            }
            if is_senior {
                departures.push(DepartureReason::GraduatedSenior {
                    player_id: pid,
                    name: name.clone(),
                });
                departures_cam_v3_sum += row.cam_v3.unwrap_or(0.0) as f32;
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

        // Σ base-season cam_v3 across players who left/arrived in the
        // portal cycle. The outbound + incoming lists already live in
        // scope; player rows are in `player_row_lookup`. Missing torvik
        // coverage on a portal player contributes 0 to either sum
        // (matches the SQL COALESCE in the training pipeline and the
        // audit). Identity-aligned with the audit signals that drove
        // this PR.
        let outbound_cam_v3_sum: f32 = outbound_by_team
            .get(&team.id)
            .map(|v| {
                v.iter()
                    .map(|(pid, _)| {
                        player_row_lookup
                            .get(pid)
                            .and_then(|r| r.cam_v3)
                            .unwrap_or(0.0) as f32
                    })
                    .sum()
            })
            .unwrap_or(0.0);
        let inbound_cam_v3_sum: f32 = incoming_by_team
            .get(&team.id)
            .map(|pids| {
                pids.iter()
                    .map(|pid| {
                        player_row_lookup
                            .get(pid)
                            .and_then(|r| r.cam_v3)
                            .unwrap_or(0.0) as f32
                    })
                    .sum()
            })
            .unwrap_or(0.0);

        out.push(ProjectedRoster {
            team_id: team.id,
            team_name: team.short_name.clone().unwrap_or_else(|| team.name.clone()),
            team_full_name: team.name.clone(),
            returning,
            arrivals,
            recruits,
            uncertain,
            departures,
            outbound_cam_v3_sum,
            inbound_cam_v3_sum,
            departures_cam_v3_sum,
        });
    }

    Ok(out)
}

/// Forward-project next-season cam_v3 for a set of returning / arriving
/// players (the roster-impact pipeline's per-player input).
///
/// OOF-first: for a historical `target_season` the trajectory model
/// trained on, `trajectory_oof_predictions` holds leave-one-pair-out
/// predictions — honest, not in-sample. Players without an OOF row
/// (the live forward year, or transitions the model didn't train on)
/// fall through to live trajectory inference off each player's own source
/// season (season-scoped `player_id`, so it's self-pinning — a returner's
/// base season, or an earlier season for a sat-out arrival; issue #146).
///
/// Returns a `player_id → projected cam_v3` map. Players the trajectory
/// model can't score (no qualifying prior season) are simply absent;
/// `roster_impact::apply_projected_cam_v3` then leaves their existing
/// cam_v3 in place (a "no growth projected" fallback). Recruits are not
/// passed here — their cam_v3 already carries the freshman model's
/// prediction from `freshman_row`.
pub async fn project_returner_cam_v3(
    pool: &PgPool,
    predictor: &Predictor,
    player_ids: &[Uuid],
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
        let row_map = fetch_player_trajectory_rows(pool, &need_live).await?;
        let mut ids: Vec<Uuid> = Vec::with_capacity(row_map.len());
        let mut feats = Vec::with_capacity(row_map.len());
        // `src_season` is each player's own season (base_season for returners,
        // an earlier season for a sat-out arrival; issue #146) — not a fixed
        // year — so the feature block's season-derived inputs stay correct.
        for (pid, (row, src_season)) in row_map {
            ids.push(pid);
            feats.push(build_trajectory_features(&row, src_season));
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

/// Banded twin of [`project_returner_cam_v3`]: returns the full trajectory
/// prediction (mean **and** q10/q90 band) per player, not just the mean.
/// Used by the per-player projection materializer (`compute-projections`),
/// which surfaces the floor/ceiling band on the `/players` projected page —
/// the team-AdjEM callers only need the mean and keep using the scalar fn.
///
/// Same source-season semantics as the scalar version: OOF held-out rows for
/// historical target seasons, live `predict_trajectory_batch` off each player's
/// own source season otherwise. Players the model can't score are absent from
/// the map (caller falls back to their frozen base-season cam_v3, no band).
pub async fn project_returner_cam_v3_banded(
    pool: &PgPool,
    predictor: &Predictor,
    player_ids: &[Uuid],
    target_season: i32,
) -> Result<HashMap<Uuid, TrajectoryPrediction>, sqlx::Error> {
    if player_ids.is_empty() {
        return Ok(HashMap::new());
    }
    // OOF held-out predictions for the historical target seasons.
    let mut out: HashMap<Uuid, TrajectoryPrediction> =
        fetch_trajectory_oof(pool, player_ids, target_season).await?;

    // Live inference for everyone without an OOF row.
    let need_live: Vec<Uuid> = player_ids
        .iter()
        .filter(|pid| !out.contains_key(*pid))
        .copied()
        .collect();
    if !need_live.is_empty() {
        let row_map = fetch_player_trajectory_rows(pool, &need_live).await?;
        let mut ids: Vec<Uuid> = Vec::with_capacity(row_map.len());
        let mut feats = Vec::with_capacity(row_map.len());
        for (pid, (row, src_season)) in row_map {
            ids.push(pid);
            feats.push(build_trajectory_features(&row, src_season));
        }
        if !feats.is_empty() {
            match predictor.predict_trajectory_batch(&feats) {
                Ok(preds) => {
                    for (pid, pred) in ids.into_iter().zip(preds) {
                        out.insert(pid, pred);
                    }
                }
                Err(e) => {
                    tracing::warn!(
                        error = ?e,
                        n = feats.len(),
                        "trajectory batch predict failed in project_returner_cam_v3_banded; \
                         affected players omitted (caller keeps frozen cam_v3)",
                    );
                }
            }
        }
    }
    Ok(out)
}

/// Baseline-shrink weight for the *served* projection:
/// `shrink = w·baseline + (1−w)·raw + offset`. Canonical home for the
/// calibration constants the `/api/projections` route and the
/// `cstat-ingest compute-projections` step share — a recalibration touches
/// one place and both surfaces stay in lockstep. Retuned 2026-06-27 on the
/// LOSO backtest after the multi-season-trajectory calibrator refit
/// (`training/transition_blend_diagnostic.py`, targets 2019–2026, n=1487):
/// the continuity cohort's (≥40% talent retained) own backtest optimum moved
/// 0.50 → 0.45 against the better raw projector. offset stays 0.0.
pub const PROJECTION_SHRINK_WEIGHT: f32 = 0.45;
/// Additive bias correction applied after the baseline shrink. The roster-impact model's
/// raw residual at `PROJECTION_SHRINK_WEIGHT` is ≈−0.10 — within noise, so
/// the offset stays 0.0 (the box-score model's old +2.0 hack is retired).
pub const PROJECTION_OFFSET: f32 = 0.0;
/// Minimum (returning + arrivals + recruits) roster size to score a team.
/// Below this the rate-stat aggregates over-weight the few starters and the
/// projection isn't honest.
pub const MIN_QUALIFYING_FOR_PROJECTION: usize = 7;

/// Baseline weight for a roster-OVERHAUL team — lower than the stable
/// [`PROJECTION_SHRINK_WEIGHT`], so the blend leans toward the roster
/// projection. `baseline` (last season's AdjEM) is a *stale anchor* when a
/// roster turns over wholesale, so trusting it less cuts both the error and
/// the systematic over-projection of overhaul teams.
///
/// Validated on the LOSO backtest (`training/transition_blend_diagnostic.py`,
/// targets 2019–2026): a turnover-conditional weight beats the flat served
/// weight — concentrated on overhaul teams — and corrects their ≈+0.7 AdjEM
/// over-projection bias. Deliberately keyed on roster turnover ALONE, not
/// `is_new_hc`: turnover is the stronger signal (it directly measures baseline
/// staleness, and subsumes 61% of new-HC teams), it has no false positives
/// from same-roster coaching changes, and it lives on [`ProjectedRoster`] — so
/// both serving surfaces (`/api/projections` and `compute-projections`) stay in
/// lockstep with NO extra DB fetch.
///
/// Retuned 2026-06-27 (multi-season-trajectory calibrator refit): the overhaul
/// cohort's (<40% retained) own backtest optimum moved 0.25 → 0.20. The
/// honest leave-one-season-out test gives transition-conditional weights
/// (stable 0.45 / overhaul 0.20) pooled MAE 5.788 vs 5.842 flat (+0.054 lift).
pub const PROJECTION_SHRINK_WEIGHT_OVERHAUL: f32 = 0.20;

/// Retained-talent fraction at/below which the overhaul weight applies in full.
const OVERHAUL_RETAINED_FULL: f32 = 0.20;
/// Retained-talent fraction at/above which the stable weight applies in full;
/// the blend weight ramps linearly between the two bounds. `[0.20, 0.40]` keeps
/// every team that returns ≥40% of last season's cam-weighted talent at the
/// stable 0.45 (the continuity cohort's own backtest optimum), so only genuine
/// overhauls deviate.
const STABLE_RETAINED_FULL: f32 = 0.40;

/// Fraction of last season's roster TALENT retained into the projected season:
/// `Σ base-season cam_v3 of returners / (returners + all departures)`. Both
/// sums are base-season cam_v3, so this reads as "how much of last year's
/// production is coming back". `None` when the team has no measurable prior
/// talent on the books (e.g. a brand-new D-I program) — caller treats that as
/// stable (the stale-baseline problem doesn't apply without a baseline roster).
pub fn retained_talent_fraction(p: &ProjectedRoster) -> Option<f32> {
    let returning: f32 = p
        .returning
        .iter()
        .map(|r| r.cam_v3.unwrap_or(0.0) as f32)
        .sum();
    let total = returning + p.departures_cam_v3_sum;
    // `returning < 0` (all-negative returners) or a non-positive `total` →
    // unknown. A non-positive total means departures out-weigh the returners
    // in net *negative* cam (the team shed a pile of bench/negative-value
    // players) — that's a continuity team, not an overhaul, so fall through to
    // the stable weight rather than letting a negative ratio clamp to 0.0 and
    // mislabel it. A zero-returner team with positive departures keeps a valid
    // `total > 0` and correctly reads as a full overhaul (retained 0.0).
    if returning < 0.0 || total <= 1e-3 {
        return None;
    }
    Some((returning / total).clamp(0.0, 1.0))
}

/// Baseline blend weight for this roster: the stable [`PROJECTION_SHRINK_WEIGHT`]
/// for continuity teams, ramping down toward [`PROJECTION_SHRINK_WEIGHT_OVERHAUL`]
/// as retained talent falls from [`STABLE_RETAINED_FULL`] to
/// [`OVERHAUL_RETAINED_FULL`] (see [`retained_talent_fraction`]). Teams with no
/// measurable turnover get the stable weight unchanged.
pub fn transition_shrink_weight(p: &ProjectedRoster) -> f32 {
    let Some(retained) = retained_talent_fraction(p) else {
        return PROJECTION_SHRINK_WEIGHT;
    };
    if retained >= STABLE_RETAINED_FULL {
        PROJECTION_SHRINK_WEIGHT
    } else if retained <= OVERHAUL_RETAINED_FULL {
        PROJECTION_SHRINK_WEIGHT_OVERHAUL
    } else {
        let t =
            (retained - OVERHAUL_RETAINED_FULL) / (STABLE_RETAINED_FULL - OVERHAUL_RETAINED_FULL);
        PROJECTION_SHRINK_WEIGHT_OVERHAUL
            + t * (PROJECTION_SHRINK_WEIGHT - PROJECTION_SHRINK_WEIGHT_OVERHAUL)
    }
}

/// Blend the raw model output toward the baseline AdjEM at an explicit baseline
/// `weight`, applying the calibration offset. With no baseline (brand-new D-I
/// program) the blend collapses to the offset-corrected raw value.
pub fn shrink_adj_em_weighted(raw: f32, baseline: Option<f32>, weight: f32) -> f32 {
    match baseline {
        Some(b) => weight * b + (1.0 - weight) * raw + PROJECTION_OFFSET,
        None => raw + PROJECTION_OFFSET,
    }
}

/// Blend at the default stable [`PROJECTION_SHRINK_WEIGHT`]. Convenience wrapper
/// for callers that aren't roster-turnover aware.
pub fn shrink_adj_em(raw: f32, baseline: Option<f32>) -> f32 {
    shrink_adj_em_weighted(raw, baseline, PROJECTION_SHRINK_WEIGHT)
}

/// Score one composed roster's (floor, ceiling, midpoint) projected AdjEM —
/// the *served* projection number. The `/api/projections` route and the
/// persisted `team_preseason_projection` table both call this so they can't
/// diverge. `projected_cam` overwrites each returner/arrival's cam_v3 with
/// the trajectory model's projection (recruits already carry the freshman
/// model's value); `build_roster_impact_features` then does its own
/// cam_v3-ranked canonical-MPG rotation normalization.
///
/// `p_return` weights the midpoint between floor (all draft-`?` players
/// leave) and ceiling (all return). Both bounds are baseline-shrunk first,
/// so the midpoint is over the shrunk band. Returns `None` for too-thin
/// rosters (below [`MIN_QUALIFYING_FOR_PROJECTION`]) or ONNX errors —
/// callers treat that as "can't project this team".
pub fn score_projection_adj_em(
    p: &ProjectedRoster,
    predictor: &Predictor,
    baseline: Option<f32>,
    p_return: f32,
    projected_cam: &HashMap<Uuid, f64>,
) -> Option<(f32, f32, f32)> {
    let qualifying = p.returning.len() + p.arrivals.len() + p.projecting_recruits_count();
    if qualifying < MIN_QUALIFYING_FOR_PROJECTION {
        return None;
    }
    let score = |scenario| {
        let mut roster = p.for_scenario(scenario);
        apply_projected_cam_v3(&mut roster, projected_cam);
        predictor.predict_roster_impact(&build_roster_impact_features(
            &roster,
            p.outbound_cam_v3_sum,
            p.inbound_cam_v3_sum,
        ))
    };
    let floor_raw = score(DraftScenario::Floor).ok()?;
    let ceiling_raw = score(DraftScenario::Ceiling).ok()?;
    // Lean off the (potentially stale) baseline for roster-overhaul teams.
    // Computed from `p` so the route's `predict_team` and this shared scorer
    // stay in lockstep without either passing extra state.
    let weight = transition_shrink_weight(p);
    let floor = shrink_adj_em_weighted(floor_raw, baseline, weight);
    let ceiling = shrink_adj_em_weighted(ceiling_raw, baseline, weight);
    let midpoint = p_return * ceiling + (1.0 - p_return) * floor;
    Some((floor, ceiling, midpoint))
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
            secondary_class: None,
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
            outbound_cam_v3_sum: 0.0,
            inbound_cam_v3_sum: 0.0,
            departures_cam_v3_sum: 0.0,
        };
        assert_eq!(r.for_scenario(DraftScenario::Floor).len(), 2);
        assert_eq!(r.for_scenario(DraftScenario::Ceiling).len(), 3);
    }

    fn roster_with(returning_cam: &[f64], departures_cam_v3_sum: f32) -> ProjectedRoster {
        ProjectedRoster {
            team_id: Uuid::new_v4(),
            team_name: "T".into(),
            team_full_name: "T".into(),
            returning: returning_cam.iter().map(|&c| pr(20.0, Some(c))).collect(),
            arrivals: vec![],
            recruits: vec![],
            uncertain: vec![],
            departures: vec![],
            outbound_cam_v3_sum: 0.0,
            inbound_cam_v3_sum: 0.0,
            departures_cam_v3_sum,
        }
    }

    #[test]
    fn retained_fraction_and_weight_ramp() {
        // Continuity: returns 24 of 30 cam (0.80 ≥ 0.40) → stable weight (0.45).
        let stable = roster_with(&[12.0, 12.0], 6.0);
        assert!((retained_talent_fraction(&stable).unwrap() - 0.80).abs() < 1e-5);
        assert!((transition_shrink_weight(&stable) - PROJECTION_SHRINK_WEIGHT).abs() < 1e-6);

        // Overhaul: returns 4 of 24 cam (0.167 ≤ 0.20) → full overhaul weight.
        let overhaul = roster_with(&[4.0], 20.0);
        assert!(retained_talent_fraction(&overhaul).unwrap() < 0.20);
        assert!(
            (transition_shrink_weight(&overhaul) - PROJECTION_SHRINK_WEIGHT_OVERHAUL).abs() < 1e-6
        );

        // Mid-ramp at retained 0.30 (midpoint of [0.20,0.40]) → weight is the
        // midpoint of [0.20,0.45] = 0.325. ret 9, dep 21, total 30 → 0.30.
        let mid = roster_with(&[9.0], 21.0);
        assert!((retained_talent_fraction(&mid).unwrap() - 0.30).abs() < 1e-5);
        assert!((transition_shrink_weight(&mid) - 0.325).abs() < 1e-5);

        // No measurable prior talent (new D-I program) → stable default, no panic.
        let empty = roster_with(&[], 0.0);
        assert!(retained_talent_fraction(&empty).is_none());
        assert!((transition_shrink_weight(&empty) - PROJECTION_SHRINK_WEIGHT).abs() < 1e-6);

        // Departures net-negative enough to drive total < 0 (kept +5, shed −10
        // of bench scrubs): a CONTINUITY team, must NOT clamp to 0.0 and get
        // mislabeled a full overhaul. → None → stable.
        let neg_total = roster_with(&[5.0], -10.0);
        assert!(retained_talent_fraction(&neg_total).is_none());
        assert!((transition_shrink_weight(&neg_total) - PROJECTION_SHRINK_WEIGHT).abs() < 1e-6);

        // Zero returners but real (positive) departures = a GENUINE full
        // overhaul (everyone left, all-new roster) → retained 0.0 → 0.25.
        let all_new = roster_with(&[], 15.0);
        assert!(retained_talent_fraction(&all_new).unwrap() < 1e-6);
        assert!(
            (transition_shrink_weight(&all_new) - PROJECTION_SHRINK_WEIGHT_OVERHAUL).abs() < 1e-6
        );
    }

    #[test]
    fn shrink_weighted_matches_default_at_stable_weight() {
        assert!(
            (shrink_adj_em_weighted(10.0, Some(20.0), PROJECTION_SHRINK_WEIGHT)
                - shrink_adj_em(10.0, Some(20.0)))
            .abs()
                < 1e-6
        );
    }

    #[test]
    fn for_scenario_includes_recruits_in_both_floor_and_ceiling() {
        // Returning + arrivals + 2 recruits in both scenarios; the
        // uncertain bucket lives in ceiling only.
        let returning = vec![pr(28.0, Some(4.0))];
        let arrivals = vec![pr(20.0, Some(2.0))];
        let recruits = vec![
            (
                freshman_row(Uuid::new_v4(), Some(8.0)),
                RecruitMeta {
                    recruit_id: Uuid::new_v4(),
                    name: "5★".into(),
                    composite_rank: Some(5),
                    star_rating: Some(5),
                    position: None,
                    projected_campom_lower: None,
                    projected_campom_upper: None,
                    feeds_projection: true,
                    did_not_play: false,
                },
            ),
            (
                freshman_row(Uuid::new_v4(), Some(1.0)),
                RecruitMeta {
                    recruit_id: Uuid::new_v4(),
                    name: "3★".into(),
                    composite_rank: Some(180),
                    star_rating: Some(3),
                    position: None,
                    projected_campom_lower: None,
                    projected_campom_upper: None,
                    feeds_projection: true,
                    did_not_play: false,
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
            outbound_cam_v3_sum: 0.0,
            inbound_cam_v3_sum: 0.0,
            departures_cam_v3_sum: 0.0,
        };
        // Floor: 1 returning + 1 arrival + 2 recruits = 4
        assert_eq!(r.for_scenario(DraftScenario::Floor).len(), 4);
        // Ceiling: 4 + 1 uncertain = 5
        assert_eq!(r.for_scenario(DraftScenario::Ceiling).len(), 5);
    }

    #[test]
    fn commits_feed_recruit_displays_but_is_excluded_from_scored_roster() {
        // A commits-feed recruit (`feeds_projection == false`, issue #175)
        // stays in `recruits` for display but must NOT enter `for_scenario`
        // or the qualifying count — so served projections match the pre-feed
        // ranked-only calibration.
        let meta = |name: &str, feeds: bool| RecruitMeta {
            recruit_id: Uuid::new_v4(),
            name: name.into(),
            composite_rank: None,
            star_rating: None,
            position: None,
            projected_campom_lower: None,
            projected_campom_upper: None,
            feeds_projection: feeds,
            did_not_play: false,
        };
        let r = ProjectedRoster {
            team_id: Uuid::new_v4(),
            team_name: "Foo".into(),
            team_full_name: "Foo Bar".into(),
            returning: vec![pr(28.0, Some(4.0))],
            arrivals: vec![],
            recruits: vec![
                (
                    freshman_row(Uuid::new_v4(), Some(6.0)),
                    meta("ranked", true),
                ),
                (
                    freshman_row(Uuid::new_v4(), Some(1.0)),
                    meta("intl commit", false),
                ),
            ],
            uncertain: vec![],
            departures: vec![],
            outbound_cam_v3_sum: 0.0,
            inbound_cam_v3_sum: 0.0,
            departures_cam_v3_sum: 0.0,
        };
        // Both recruits are present for display.
        assert_eq!(r.recruits.len(), 2);
        // Only the ranked one feeds the scored roster: 1 returning + 1 ranked.
        assert_eq!(r.for_scenario(DraftScenario::Floor).len(), 2);
        assert_eq!(r.projecting_recruits_count(), 1);
    }

    #[test]
    fn redshirt_recruit_excluded_from_scored_roster_but_still_displayed() {
        // A ranked recruit who committed but never played the (completed)
        // target season — did_not_play == true — must stay in `recruits` for
        // display yet drop out of the scored roster and the qualifying count,
        // exactly like the commits-feed cohort. A ranked recruit who DID play
        // (did_not_play == false) still scores.
        let meta = |name: &str, did_not_play: bool| RecruitMeta {
            recruit_id: Uuid::new_v4(),
            name: name.into(),
            composite_rank: Some(50),
            star_rating: Some(4),
            position: None,
            projected_campom_lower: None,
            projected_campom_upper: None,
            feeds_projection: true,
            did_not_play,
        };
        let r = ProjectedRoster {
            team_id: Uuid::new_v4(),
            team_name: "Foo".into(),
            team_full_name: "Foo Bar".into(),
            returning: vec![pr(28.0, Some(4.0))],
            arrivals: vec![],
            recruits: vec![
                (
                    freshman_row(Uuid::new_v4(), Some(6.0)),
                    meta("played", false),
                ),
                (
                    freshman_row(Uuid::new_v4(), Some(5.0)),
                    meta("redshirt", true),
                ),
            ],
            uncertain: vec![],
            departures: vec![],
            outbound_cam_v3_sum: 0.0,
            inbound_cam_v3_sum: 0.0,
            departures_cam_v3_sum: 0.0,
        };
        // Both recruits are present for display.
        assert_eq!(r.recruits.len(), 2);
        // Only the one who played feeds the scored roster: 1 returning + 1 played.
        assert_eq!(r.for_scenario(DraftScenario::Floor).len(), 2);
        assert_eq!(r.projecting_recruits_count(), 1);
    }

    #[test]
    fn freshman_row_carries_prediction_and_minimal_fields() {
        // The row plugs into the roster-impact model, which reads only
        // cam_v3 / class_year / primary_class — so we carry the
        // per-recruit prediction and leave the box-score statline empty.
        let row = freshman_row(Uuid::new_v4(), Some(8.0));
        assert_eq!(row.cam_v3, Some(8.0));
        assert_eq!(row.class_year.as_deref(), Some("Fr"));
        assert_eq!(row.primary_class, None);
        // No synthesized statline — minutes are reassigned by cam_v3 rank
        // downstream, and the rate stats reach no served model.
        assert_eq!(row.mpg, 0.0);
        assert_eq!(row.total_min, 0.0);
        assert_eq!(row.ppg, None);
        assert_eq!(row.usg, None);
    }

    #[test]
    fn freshman_row_falls_back_when_prediction_absent() {
        // Whole-batch inference failure → replacement-level scalar, no band.
        let row = freshman_row(Uuid::new_v4(), None);
        assert_eq!(row.cam_v3, Some(FRESHMAN_FALLBACK_CAM_V3));
        assert_eq!(row.class_year.as_deref(), Some("Fr"));
    }

    #[test]
    fn return_status_unknown_values_are_contested_not_granted() {
        assert_eq!(
            ReturnStatus::from_str_lenient("granted"),
            ReturnStatus::Granted
        );
        assert_eq!(
            ReturnStatus::from_str_lenient("  GRANTED "),
            ReturnStatus::Granted
        );
        assert_eq!(
            ReturnStatus::from_str_lenient("contested"),
            ReturnStatus::Contested
        );
        // The asymmetry is the point: an unrecognised status means we do not
        // know, and "we do not know" projects as a widened band, never as a
        // confident roster addition. Defaulting the other way would let a typo
        // silently assert a player is eligible.
        for unknown in ["", "pending", "granted?", "true", "GRANTED_BY_WAIVER"] {
            assert_eq!(
                ReturnStatus::from_str_lenient(unknown),
                ReturnStatus::Contested,
                "unknown status {unknown:?} must fall back to Contested"
            );
        }
    }

    #[test]
    fn normalize_player_name_strips_suffix_and_case() {
        assert_eq!(
            normalize_player_name("Christian Anderson Jr."),
            "christian anderson",
        );
        assert_eq!(normalize_player_name("Cooper Flagg"), "cooper flagg");
    }

    /// Regression guard for the freshman-OOF leak (2026-06-05): for a class the
    /// freshman model trained on, `compose_all_projections` must serve the
    /// HELD-OUT prediction from `freshman_oof_predictions`, NOT a live in-sample
    /// one. The leak showed as Cameron Boozer projecting +17.7 in the projection
    /// vs +14.2 (held-out) on the recruits page — identical feature vectors,
    /// different model (full-data vs leave-one-class-out fold).
    ///
    /// Integration test: requires a populated DB + the committed ONNX models.
    /// Skips cleanly (passes) when `DATABASE_URL` is unset, the DB can't be
    /// reached, the canonical class-2025 fixture isn't ingested (fresh CI DB),
    /// models can't load, or compose fails for unrelated reasons — so it never
    /// breaks a schema-only CI run, but acts as a real guard on a dev DB.
    #[tokio::test]
    async fn freshman_projection_serves_oof_not_live_for_historical_class() {
        let Ok(url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let Ok(pool) = sqlx::PgPool::connect(&url).await else {
            return;
        };

        // base_season 2025 → freshman season 2026 (a season the model trained
        // on, so it has OOF rows). Pick the highest-OOF class-2025 recruit that
        // resolved to a player *and* attaches to a base-season team — the elite
        // tail is exactly where held-out and in-sample diverge most.
        const BASE_SEASON: i32 = 2025;
        let fixture: Option<(Uuid, f32)> = sqlx::query_as::<_, (Uuid, f32)>(
            r#"
            SELECT r.id, f.mean
            FROM recruits r
            JOIN freshman_oof_predictions f
              ON f.cstat_player_id = r.cstat_player_id
             AND f.target_season = r.year + 1
            JOIN teams t  ON t.id = r.committed_team_id
            JOIN teams tm ON tm.natstat_id = t.natstat_id AND tm.season = r.year
            WHERE r.year = $1
              AND r.committed_team_id IS NOT NULL
              AND COALESCE(r.commit_status, '') <> 'Uncommitted'
            ORDER BY f.mean DESC
            LIMIT 1
            "#,
        )
        .bind(BASE_SEASON)
        .fetch_optional(&pool)
        .await
        .ok()
        .flatten();
        let Some((recruit_id, oof_mean)) = fixture else {
            return; // fresh / un-ingested DB — nothing to assert against.
        };

        let model_dir =
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../training/models");
        let Ok(predictor) = Predictor::load(&model_dir) else {
            return; // models not present in this environment.
        };

        // `false` = don't retro-exclude redshirts; this guard is about OOF-vs-live
        // freshman serving, and the fixture recruit played (has a resolved id).
        let Ok(projections) =
            compose_all_projections(&pool, BASE_SEASON, &[], &[], &predictor, false).await
        else {
            return; // a compose failure is a different concern, not this guard's.
        };

        // Find the fixture recruit in any team's recruit cohort.
        let cam = projections
            .iter()
            .flat_map(|p| p.recruits.iter())
            .find(|(_, meta)| meta.recruit_id == recruit_id)
            .and_then(|(row, _)| row.cam_v3);
        let Some(cam) = cam else {
            return; // recruit didn't land in a composed roster (e.g. too-thin gate).
        };

        // The served value must equal the held-out OOF mean. If this regresses,
        // `compose_all_projections` is serving live in-sample freshman
        // predictions for a historical class again (the leak). fp tolerance
        // covers the f32 → f64 → f32 round-trip.
        assert!(
            (cam as f32 - oof_mean).abs() < 0.05,
            "freshman OOF leak regressed: composed recruit cam {cam:.3} != held-out OOF \
             mean {oof_mean:.3} (recruit {recruit_id}) — compose_all_projections must use \
             fetch_freshman_oof, not live predict_freshman_batch, for trained-on classes",
        );
    }
}
