//! Roster-only AdjEM model: feature builder + transfer-swap helper.
//!
//! Mirrors `training/train_roster_model.py::aggregate_team_season` for the
//! production (box-score-only, `include_impact_features=false`) variant.
//! Two intentional behavioral differences vs the Python aggregator: (1)
//! missing rate stats are emitted as `0.0` (Python lets LightGBM see `NaN`)
//! — for qualified players the gp/mpg gate keeps box stats populated, so it
//! rarely matters, and for archetype shares `0.0` is semantically correct
//! ("no players in this class"); (2) star tie-break uses an explicit loop
//! to match pandas' `idxmax` first-occurrence semantics (Rust's
//! `Iterator::max_by` picks the last tied element).
//!
//! The Python script and this module share one contract via
//! `training/models/roster_model_meta.json` — the loader in `inference.rs`
//! hard-fails if `player_filter`, `include_impact_features`, or feature
//! order drift from what the trained model expects.
//!
//! Swap semantics: see `swap_player` — rank-slot MPG. Ranks the incoming
//! player against the destination roster by CamPom v3 (`cam_v3`), gives
//! them the MPG of the destination slot their rank earns, and shifts
//! every existing player at-or-below that rank down by one slot. The
//! bottom-ranked destination player falls out of the rotation (MPG → 0).
//! Preserves the team's 200-minute envelope by construction.

use sqlx::PgPool;
use uuid::Uuid;

/// Number of input features the roster ONNX model expects.
///
/// Wire-locked to `roster_model_meta.json::features` order. Don't reorder
/// without retraining and bumping the meta JSON.
pub const ROSTER_NUM_FEATURES: usize = 36;

/// 12 D&D-class archetypes in the same order the Python aggregator emits
/// `arch_<class>` columns. Stored lowercase to match the trained feature
/// names; the source-of-truth uppercase values live in `player_archetypes.primary_class`.
pub const ARCHETYPES: [&str; 12] = [
    "Wizard",
    "Sorcerer",
    "Warlock",
    "Bard",
    "Ranger",
    "Barbarian",
    "Paladin",
    "Monk",
    "Cleric",
    "Druid",
    "Rogue",
    "Fighter",
];

/// Feature names in the exact order produced by the ONNX model. Wire-locked
/// to `roster_model_meta.json::features` — `roster_feature_names_match_meta`
/// asserts the two stay in sync.
pub const ROSTER_FEATURE_NAMES: [&str; ROSTER_NUM_FEATURES] = [
    "roster_size",
    "total_minutes",
    "top1_min_share",
    "top5_min_share",
    "minutes_stddev",
    "w_ppg",
    "w_rpg",
    "w_apg",
    "w_spg",
    "w_bpg",
    "w_topg",
    "w_ts",
    "w_efg",
    "w_usg",
    "w_ast_pct",
    "w_tov_pct",
    "w_orb_pct",
    "w_drb_pct",
    "w_stl_pct",
    "w_blk_pct",
    "w_ft_rate",
    "star_ppg",
    "star_ts",
    "star_usg",
    "arch_wizard",
    "arch_sorcerer",
    "arch_warlock",
    "arch_bard",
    "arch_ranger",
    "arch_barbarian",
    "arch_paladin",
    "arch_monk",
    "arch_cleric",
    "arch_druid",
    "arch_rogue",
    "arch_fighter",
];

/// Player qualification gate applied during training. Surfaced as constants so
/// the SQL fetch can apply the same filter and `Predictor::load` can verify
/// the model meta hasn't drifted from these values.
pub const QUAL_MIN_GAMES_PLAYED: i32 = 5;
pub const QUAL_MIN_MPG: f64 = 5.0;

/// String form the meta JSON stores. Compared verbatim at load.
pub const QUAL_FILTER_STRING: &str = "games_played >= 5 AND minutes_per_game >= 5";

/// One player's contribution to a roster. Pulled from `player_season_stats`
/// joined with `player_archetypes`; box-score-only (no Torvik impact
/// features) since the production model is the `include_impact_features=false`
/// variant.
///
/// `Option`s mirror the underlying nullable columns. Aggregation treats
/// `None` as missing (excluded from the minutes-weighted denominator) so
/// rosters with one player missing a single rate stat don't NaN-poison the
/// whole feature.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct PlayerRow {
    pub player_id: Uuid,
    /// `minutes_per_game * games_played`. The minutes-weighted aggregator's
    /// weight column. The rank-slot `swap_player` back-derives games_played
    /// as `total_min / mpg` (safe because the `mpg >= 5` gate keeps mpg
    /// strictly positive).
    pub total_min: f64,
    pub mpg: f64,
    pub ppg: Option<f64>,
    pub rpg: Option<f64>,
    pub apg: Option<f64>,
    pub spg: Option<f64>,
    pub bpg: Option<f64>,
    pub topg: Option<f64>,
    pub ts: Option<f64>,
    pub efg: Option<f64>,
    pub usg: Option<f64>,
    pub ast_pct: Option<f64>,
    pub tov_pct: Option<f64>,
    pub orb_pct: Option<f64>,
    pub drb_pct: Option<f64>,
    pub stl_pct: Option<f64>,
    pub blk_pct: Option<f64>,
    pub ft_rate: Option<f64>,
    pub primary_class: Option<String>,
    /// `torvik_player_stats.cam_gbpm_v3_psos` — the production CamPom composite,
    /// used by `swap_player` to rank the incoming player against the destination
    /// roster and pick their post-swap MPG slot. Not consumed by
    /// `build_roster_features` (the model is trained on the box-score-only
    /// variant; including a CamPom-derived feature would collapse the model
    /// toward the player-impact identity per the train script's design
    /// comment). Nullable — players without Torvik coverage will produce
    /// `None` here; the API surfaces `delta_adjem = null` for those.
    pub cam_v3: Option<f64>,
}

/// Fetch every qualified player on the given (team_id, season) roster.
///
/// Filter matches `roster_model_meta.json::player_filter` exactly — drift
/// here breaks train/serve parity. `Predictor::load` enforces the meta
/// JSON's filter string equals `QUAL_FILTER_STRING`.
pub async fn fetch_roster(
    pool: &PgPool,
    team_id: Uuid,
    season: i32,
) -> Result<Vec<PlayerRow>, sqlx::Error> {
    sqlx::query_as::<_, PlayerRow>(
        r#"
        SELECT
            pss.player_id,
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
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
        LEFT JOIN torvik_player_stats tps
            ON tps.player_id = pss.player_id AND tps.season = pss.season
        WHERE pss.team_id = $1
          AND pss.season = $2
          AND COALESCE(pss.games_played, 0) >= $3
          AND COALESCE(pss.minutes_per_game, 0) >= $4
        "#,
    )
    .bind(team_id)
    .bind(season)
    .bind(QUAL_MIN_GAMES_PLAYED)
    .bind(QUAL_MIN_MPG)
    .fetch_all(pool)
    .await
}

/// Minutes-weighted mean over `Option<f64>` values. Mirrors
/// `weighted_mean` in `train_roster_model.py`: rows where either value or
/// weight is missing (or weight <= 0) are dropped from numerator and
/// denominator alike. Returns `None` when no row contributes — surfaced
/// as `0.0` in the final feature vector to match Python's `np.nan → 0`
/// path the model was trained on.
fn weighted_mean(values: &[Option<f64>], weights: &[f64]) -> Option<f64> {
    let mut num = 0.0;
    let mut den = 0.0;
    for (v, w) in values.iter().zip(weights.iter()) {
        if let (Some(v), w) = (v, *w)
            && w > 0.0
            && v.is_finite()
        {
            num += v * w;
            den += w;
        }
    }
    if den > 0.0 { Some(num / den) } else { None }
}

/// Sample standard deviation of `mpg` across the roster (ddof=1, matching
/// pandas' default). Returns `0.0` for rosters with fewer than two players —
/// same fallback `aggregate_team_season` uses.
fn mpg_stddev(roster: &[PlayerRow]) -> f64 {
    if roster.len() < 2 {
        return 0.0;
    }
    let n = roster.len() as f64;
    let mean = roster.iter().map(|p| p.mpg).sum::<f64>() / n;
    let var = roster.iter().map(|p| (p.mpg - mean).powi(2)).sum::<f64>() / (n - 1.0);
    var.sqrt()
}

/// Aggregate one team-season's qualified player rows into the 36-feature
/// vector consumed by `roster_model.onnx`.
///
/// Feature order is locked to `ROSTER_FEATURE_NAMES`. Missing values become
/// `0.0` (matches Python training's `np.nan → 0` LightGBM behavior — LightGBM
/// natively handles NaN, but ONNX export squashes to zero, and the Rust path
/// must match that to stay consistent with what the model learned).
///
/// Empty rosters return all zeros. `Predictor::predict_adj_em` will still
/// produce a number, but the result is meaningless — callers should
/// short-circuit on `roster.is_empty()` before invoking inference.
pub fn build_roster_features(roster: &[PlayerRow]) -> [f32; ROSTER_NUM_FEATURES] {
    let mut out = [0.0_f32; ROSTER_NUM_FEATURES];
    if roster.is_empty() {
        return out;
    }

    let total: f64 = roster.iter().map(|p| p.total_min).sum();
    let weights: Vec<f64> = roster.iter().map(|p| p.total_min).collect();

    let mut sorted_min: Vec<f64> = weights.clone();
    sorted_min.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

    let top1 = if total > 0.0 {
        sorted_min[0] / total
    } else {
        0.0
    };
    let top5 = if total > 0.0 {
        sorted_min.iter().take(5).sum::<f64>() / total
    } else {
        0.0
    };

    // Star = top player by minutes (box-score-only variant; the impact-features
    // variant uses CamPom for the star pick — see Python script). Match
    // pandas `idxmax` semantics: on ties, the *first* occurrence wins.
    // `Iterator::max_by` returns the last maximum (per its spec), which
    // would silently drift from the trained Python aggregator on ties.
    let star_idx = {
        let mut best = 0_usize;
        for (i, p) in roster.iter().enumerate().skip(1) {
            if p.total_min > roster[best].total_min {
                best = i;
            }
        }
        best
    };
    let star = &roster[star_idx];

    let wm = |sel: fn(&PlayerRow) -> Option<f64>| -> f32 {
        let values: Vec<Option<f64>> = roster.iter().map(sel).collect();
        weighted_mean(&values, &weights).unwrap_or(0.0) as f32
    };

    out[0] = roster.len() as f32;
    out[1] = total as f32;
    out[2] = top1 as f32;
    out[3] = top5 as f32;
    out[4] = mpg_stddev(roster) as f32;
    out[5] = wm(|p| p.ppg);
    out[6] = wm(|p| p.rpg);
    out[7] = wm(|p| p.apg);
    out[8] = wm(|p| p.spg);
    out[9] = wm(|p| p.bpg);
    out[10] = wm(|p| p.topg);
    out[11] = wm(|p| p.ts);
    out[12] = wm(|p| p.efg);
    out[13] = wm(|p| p.usg);
    out[14] = wm(|p| p.ast_pct);
    out[15] = wm(|p| p.tov_pct);
    out[16] = wm(|p| p.orb_pct);
    out[17] = wm(|p| p.drb_pct);
    out[18] = wm(|p| p.stl_pct);
    out[19] = wm(|p| p.blk_pct);
    out[20] = wm(|p| p.ft_rate);
    out[21] = star.ppg.unwrap_or(0.0) as f32;
    out[22] = star.ts.unwrap_or(0.0) as f32;
    out[23] = star.usg.unwrap_or(0.0) as f32;

    // Archetype minutes shares. The 12 columns are *always* present even
    // when no player on the roster matched the archetype (zero share).
    let mut arch_min: [f64; 12] = [0.0; 12];
    for p in roster {
        if let Some(cls) = p.primary_class.as_deref()
            && let Some(i) = ARCHETYPES.iter().position(|a| *a == cls)
        {
            arch_min[i] += p.total_min;
        }
    }
    for (i, m) in arch_min.iter().enumerate() {
        out[24 + i] = if total > 0.0 {
            (*m / total) as f32
        } else {
            0.0
        };
    }

    out
}

/// Produce a swap-modified roster: insert the incoming player into the
/// destination's MPG rotation at the rank their CamPom v3 earns them.
///
/// Method (rank-slot MPG): rank every player by `cam_v3` descending. The
/// incoming player slots in at rank `k` = (count of destination players
/// strictly better than incoming) + 1. They take the *MPG slot* at rank
/// `k` — i.e., the MPG of the destination player currently at rank `k`,
/// from the destination's actual observed MPG distribution. Every
/// destination player at rank ≥ `k` shifts down one slot and inherits the
/// MPG one slot below them; the bottom-ranked destination player drops
/// out of the rotation (MPG → 0). Existing players' rate stats are
/// preserved; only their `mpg` and `total_min` change.
///
/// Why this and not constant-scale: the role structure of a real D-I team
/// is rank-based, not proportional. A 5-star addition doesn't trim every
/// existing player's minutes by 12%; it bumps a bench player out of the
/// rotation and shifts the rotation order. The rank-slot version makes
/// the post-swap roster look like a real team's rotation, which is the
/// distribution the model was trained on.
///
/// Minutes-envelope invariant: the set of MPG slots is unchanged (just
/// reassigned), so `Σ mpg_new = Σ mpg_old`. `total_min` is recomputed per
/// player from `new_mpg × old_games_played`, which means the model's
/// level-sensitive `total_minutes` feature is preserved exactly only when
/// every roster slot has the same games_played; in real data
/// games_played varies by ~3-5 games across the rotation (injuries,
/// redshirts), so `Σ total_min` drifts by single-digit percent. The Δ
/// signal — what this engine actually surfaces — cancels out the drift
/// since both baseline and swap predictions inherit the same skew.
///
/// Fallback: if `incoming.cam_v3` is `None`, the player slots at the
/// bottom of the rotation and effectively gets ~0 MPG. Callers should
/// detect this case and surface `delta_adjem = null` rather than report a
/// near-zero Δ that's an artifact of missing data.
///
/// Dedup: if the incoming player_id matches an existing roster slot, the
/// existing entry is dropped before ranking (avoids double-counting and
/// keeps the slot count constant).
///
/// Bounds: `incoming.mpg` is no longer load-bearing for this function
/// (the rank-slot logic ignores it), but we still clamp to `[0, 40]` on
/// the output to defend against malformed input downstream.
pub fn swap_player(roster: &[PlayerRow], mut incoming: PlayerRow) -> Vec<PlayerRow> {
    // Strip any existing copy of the incoming player; their old slot
    // shouldn't compete with their incoming slot in the ranking.
    let dest: Vec<PlayerRow> = roster
        .iter()
        .filter(|p| p.player_id != incoming.player_id)
        .cloned()
        .collect();

    if dest.is_empty() {
        // Pathological: no destination roster to rank against. Return the
        // incoming player alone with whatever MPG they came in with.
        incoming.mpg = incoming.mpg.clamp(0.0, 40.0);
        // No team to attach to → keep total_min as-is.
        return vec![incoming];
    }

    // Snapshot the destination's MPG slots in descending order. These are
    // the MPG values that will be redistributed among the post-swap
    // roster; the count stays constant.
    let mut mpg_slots: Vec<f64> = dest.iter().map(|p| p.mpg).collect();
    mpg_slots.sort_by(|a, b| b.partial_cmp(a).unwrap_or(std::cmp::Ordering::Equal));

    // Incoming rank = 1 + (number of destination players strictly better
    // by CamPom v3). Players without cam_v3 are treated as -inf for
    // ranking (they sink to the bottom); the incoming player with
    // None.cam_v3 likewise sinks below everyone who has a value, so they
    // get the worst slot.
    let incoming_q = incoming.cam_v3.unwrap_or(f64::NEG_INFINITY);
    let better_count = dest
        .iter()
        .filter(|p| p.cam_v3.unwrap_or(f64::NEG_INFINITY) > incoming_q)
        .count();
    let incoming_rank = better_count; // 0-indexed: slot index in mpg_slots

    // Sort destination by cam_v3 desc (NaNs/None go last) so we can walk
    // them in rank order. Stable ordering within ties is fine — the slot
    // assignment is the same.
    let mut dest_by_rank: Vec<PlayerRow> = dest;
    dest_by_rank.sort_by(|a, b| {
        let aq = a.cam_v3.unwrap_or(f64::NEG_INFINITY);
        let bq = b.cam_v3.unwrap_or(f64::NEG_INFINITY);
        bq.partial_cmp(&aq).unwrap_or(std::cmp::Ordering::Equal)
    });

    // Assign new MPG to each existing player by their post-swap rank.
    // - Players at original rank r < incoming_rank: keep slot r (their
    //   own MPG, unchanged).
    // - Players at original rank r >= incoming_rank: shift down by 1, get
    //   the MPG at slot r+1. The player at the bottom of dest_by_rank
    //   has no slot below them → MPG goes to 0 (effectively benched).
    let mut out: Vec<PlayerRow> = Vec::with_capacity(dest_by_rank.len() + 1);
    for (r, p) in dest_by_rank.into_iter().enumerate() {
        let mut q = p.clone();
        let new_slot = if r < incoming_rank { r } else { r + 1 };
        let new_mpg = mpg_slots.get(new_slot).copied().unwrap_or(0.0);
        // Recompute total_min from new_mpg holding the player's
        // games_played fixed. We don't store games_played directly, so
        // back it out as old_total_min / old_mpg (safe: every roster row
        // came from `mpg >= QUAL_MIN_MPG = 5`, so old_mpg > 0).
        let games_played = if p.mpg > 0.0 {
            p.total_min / p.mpg
        } else {
            0.0
        };
        q.mpg = new_mpg.clamp(0.0, 40.0);
        q.total_min = q.mpg * games_played;
        out.push(q);
    }

    // Incoming player takes the slot at incoming_rank.
    let incoming_mpg = mpg_slots
        .get(incoming_rank)
        .copied()
        .unwrap_or(0.0)
        .clamp(0.0, 40.0);
    // For total_min, use the same games_played the destination's
    // rank-incoming_rank player would have played — best available
    // proxy for "how many games would they play if added here." Falls
    // back to the destination's median if the slot is somehow empty.
    let games_for_team = median_games_played(roster).max(1.0);
    incoming.mpg = incoming_mpg;
    incoming.total_min = incoming_mpg * games_for_team;
    out.push(incoming);
    out
}

/// Median of `(total_min / mpg)` across the roster — a proxy for the
/// team's per-player games-played count, used to project the incoming
/// player's `total_min` when their old team's schedule shouldn't bleed
/// into the destination's projection.
fn median_games_played(roster: &[PlayerRow]) -> f64 {
    let mut gps: Vec<f64> = roster
        .iter()
        .filter_map(|p| {
            if p.mpg > 0.0 {
                Some(p.total_min / p.mpg)
            } else {
                None
            }
        })
        .collect();
    if gps.is_empty() {
        return 0.0;
    }
    gps.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let mid = gps.len() / 2;
    if gps.len().is_multiple_of(2) {
        (gps[mid - 1] + gps[mid]) / 2.0
    } else {
        gps[mid]
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn model_meta_path() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../training/models/roster_model_meta.json")
    }

    fn mk(player_id: Uuid, mpg: f64, gp: f64, class: Option<&str>) -> PlayerRow {
        PlayerRow {
            player_id,
            total_min: mpg * gp,
            mpg,
            ppg: Some(10.0),
            rpg: Some(5.0),
            apg: Some(2.0),
            spg: Some(1.0),
            bpg: Some(0.5),
            topg: Some(1.5),
            ts: Some(0.55),
            efg: Some(0.52),
            usg: Some(20.0),
            ast_pct: Some(15.0),
            tov_pct: Some(15.0),
            orb_pct: Some(5.0),
            drb_pct: Some(15.0),
            stl_pct: Some(2.0),
            blk_pct: Some(1.5),
            ft_rate: Some(0.35),
            primary_class: class.map(str::to_string),
            cam_v3: None,
        }
    }

    /// Same as `mk` but with a CamPom v3 value attached. Used by swap
    /// tests where rank order is what's under test.
    fn mk_q(player_id: Uuid, mpg: f64, gp: f64, class: Option<&str>, cam_v3: f64) -> PlayerRow {
        let mut p = mk(player_id, mpg, gp, class);
        p.cam_v3 = Some(cam_v3);
        p
    }

    #[test]
    fn feature_names_match_meta_json() {
        let content = match std::fs::read_to_string(model_meta_path()) {
            Ok(c) => c,
            Err(_) => {
                eprintln!("skipping: roster_model_meta.json not found");
                return;
            }
        };
        let meta: serde_json::Value = serde_json::from_str(&content).unwrap();
        let meta_features: Vec<String> = meta["features"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| v.as_str().unwrap().to_string())
            .collect();
        assert_eq!(meta_features.len(), ROSTER_NUM_FEATURES);
        for (i, (expected, actual)) in meta_features
            .iter()
            .zip(ROSTER_FEATURE_NAMES.iter())
            .enumerate()
        {
            assert_eq!(expected, actual, "roster feature mismatch at index {i}");
        }
    }

    #[test]
    fn meta_player_filter_unchanged() {
        let content = match std::fs::read_to_string(model_meta_path()) {
            Ok(c) => c,
            Err(_) => return,
        };
        let meta: serde_json::Value = serde_json::from_str(&content).unwrap();
        // The Rust path applies QUAL_MIN_GAMES_PLAYED + QUAL_MIN_MPG. If the
        // training script gates differently, train/serve features drift in
        // ways the model can't be evaluated against. Lock the contract.
        assert_eq!(
            meta["player_filter"].as_str().unwrap(),
            QUAL_FILTER_STRING,
            "training filter drifted from Rust constants — retrain or update QUAL_* consts",
        );
        assert!(
            !meta["include_impact_features"].as_bool().unwrap_or(true),
            "current Rust path is box-score-only; meta says include_impact_features=true",
        );
    }

    #[test]
    fn empty_roster_returns_zeros() {
        let v = build_roster_features(&[]);
        assert_eq!(v, [0.0; ROSTER_NUM_FEATURES]);
    }

    #[test]
    fn weighted_mean_drops_none_and_zero_weight() {
        let v = weighted_mean(&[Some(10.0), None, Some(20.0)], &[1.0, 5.0, 1.0]);
        assert!((v.unwrap() - 15.0).abs() < 1e-9);
        let z = weighted_mean(&[Some(1.0)], &[0.0]);
        assert!(z.is_none());
    }

    #[test]
    fn star_picks_max_minutes() {
        let bench = mk(Uuid::new_v4(), 10.0, 30.0, Some("Wizard"));
        let mut star = mk(Uuid::new_v4(), 30.0, 30.0, Some("Sorcerer"));
        star.ppg = Some(22.0);
        star.usg = Some(28.0);
        let feats = build_roster_features(&[bench, star]);
        // star_ppg is index 21
        assert!((feats[21] - 22.0).abs() < 1e-3);
        // star_usg is index 23
        assert!((feats[23] - 28.0).abs() < 1e-3);
    }

    /// On total_min ties, the *first* occurrence wins (matches Python
    /// pandas `idxmax`). Rust's `Iterator::max_by` returns the last
    /// element on ties, which would silently drift from the trained
    /// aggregator. Pin the first-occurrence behavior here.
    #[test]
    fn star_breaks_ties_by_first_occurrence() {
        let mut first = mk(Uuid::new_v4(), 28.0, 30.0, Some("Wizard"));
        first.ppg = Some(18.0);
        let mut second = mk(Uuid::new_v4(), 28.0, 30.0, Some("Sorcerer"));
        second.ppg = Some(22.0);
        let feats = build_roster_features(&[first, second]);
        // Both have total_min = 840. Python `idxmax` returns the first
        // index → first.ppg = 18.0. If we accidentally used Rust max_by,
        // we'd see second.ppg = 22.0.
        assert!(
            (feats[21] - 18.0).abs() < 1e-3,
            "star_ppg should be first-occurrence's ppg (18.0), got {}",
            feats[21],
        );
    }

    #[test]
    fn archetype_shares_sum_to_one_minus_unknown() {
        // All 8 players have a class → shares should sum to 1.0.
        let players: Vec<PlayerRow> = [
            "Wizard", "Wizard", "Sorcerer", "Bard", "Ranger", "Monk", "Cleric", "Druid",
        ]
        .iter()
        .map(|c| mk(Uuid::new_v4(), 20.0, 30.0, Some(c)))
        .collect();
        let feats = build_roster_features(&players);
        let arch_sum: f32 = feats[24..36].iter().sum();
        assert!(
            (arch_sum - 1.0).abs() < 1e-4,
            "archetype shares sum to {arch_sum}, expected 1.0",
        );
        // Wizard share = 2/8 = 0.25
        assert!((feats[24] - 0.25).abs() < 1e-4);
    }

    #[test]
    fn swap_preserves_mpg_slots() {
        // Destination roster, MPG slots descending: [30, 26, 22, 18, 14, 10, 8, 6].
        // Incoming with CamPom = 4.0 ranks 3rd (better than the 4 below
        // cam_v3 < 4.0, worse than the 2 above). They take slot index 2 → 22 mpg.
        let roster = vec![
            mk_q(Uuid::new_v4(), 30.0, 30.0, Some("Wizard"), 8.0),
            mk_q(Uuid::new_v4(), 26.0, 30.0, Some("Sorcerer"), 6.0),
            mk_q(Uuid::new_v4(), 22.0, 30.0, Some("Bard"), 3.0),
            mk_q(Uuid::new_v4(), 18.0, 30.0, Some("Ranger"), 2.0),
            mk_q(Uuid::new_v4(), 14.0, 30.0, Some("Cleric"), 1.0),
            mk_q(Uuid::new_v4(), 10.0, 30.0, Some("Monk"), 0.5),
            mk_q(Uuid::new_v4(), 8.0, 30.0, Some("Druid"), 0.0),
            mk_q(Uuid::new_v4(), 6.0, 30.0, Some("Rogue"), -0.5),
        ];
        let orig_mpgs: f64 = roster.iter().map(|p| p.mpg).sum();

        let incoming_id = Uuid::new_v4();
        let incoming = mk_q(incoming_id, 0.0, 30.0, Some("Paladin"), 4.0);
        let swapped = swap_player(&roster, incoming);

        // Set of MPG slots is preserved (just reassigned). Excludes the
        // 6-mpg slot that fell off the bottom — that's now 0 on the
        // displaced player. Verify total is unchanged.
        let new_mpgs: f64 = swapped.iter().map(|p| p.mpg).sum();
        assert!(
            (new_mpgs - orig_mpgs).abs() < 1e-6,
            "Σ mpg drifted: {orig_mpgs} -> {new_mpgs}",
        );

        // Incoming gets slot 2 (22 mpg).
        let inc = swapped.iter().find(|p| p.player_id == incoming_id).unwrap();
        assert!(
            (inc.mpg - 22.0).abs() < 1e-6,
            "incoming took slot mpg {} ≠ 22",
            inc.mpg,
        );
        // total_min should be mpg × games_played (30 here) = 660.
        assert!((inc.total_min - 660.0).abs() < 1e-6);
    }

    #[test]
    fn swap_top_ranked_incoming_displaces_bottom() {
        // Incoming with CamPom higher than everyone takes slot 0 (top MPG).
        let roster = vec![
            mk_q(Uuid::new_v4(), 30.0, 30.0, Some("Wizard"), 5.0),
            mk_q(Uuid::new_v4(), 20.0, 30.0, Some("Bard"), 3.0),
            mk_q(Uuid::new_v4(), 10.0, 30.0, Some("Cleric"), 1.0),
        ];
        let incoming_id = Uuid::new_v4();
        let incoming = mk_q(incoming_id, 0.0, 30.0, Some("Sorcerer"), 9.0);
        let swapped = swap_player(&roster, incoming);

        let inc = swapped.iter().find(|p| p.player_id == incoming_id).unwrap();
        assert!(
            (inc.mpg - 30.0).abs() < 1e-6,
            "incoming should get top slot"
        );

        // Original Wizard (was slot 0, 30 mpg) gets bumped to slot 1 (20 mpg).
        let wizard = swapped
            .iter()
            .find(|p| p.primary_class.as_deref() == Some("Wizard"))
            .unwrap();
        assert!(
            (wizard.mpg - 20.0).abs() < 1e-6,
            "Wizard shifted to wrong slot mpg {}",
            wizard.mpg,
        );

        // Original Cleric (was slot 2, 10 mpg) falls off the bottom.
        let cleric = swapped
            .iter()
            .find(|p| p.primary_class.as_deref() == Some("Cleric"))
            .unwrap();
        assert!(
            cleric.mpg.abs() < 1e-6,
            "Cleric should have fallen out, got mpg {}",
            cleric.mpg,
        );
    }

    #[test]
    fn swap_no_cam_v3_falls_below_rotation() {
        let roster = vec![
            mk_q(Uuid::new_v4(), 30.0, 30.0, Some("Wizard"), 5.0),
            mk_q(Uuid::new_v4(), 20.0, 30.0, Some("Bard"), 3.0),
        ];
        // Incoming has no cam_v3 → treated as NEG_INFINITY → ranks below
        // everyone with a value. Their target slot is past the end of
        // the destination's MPG array (no slot N+1 exists), so they get
        // 0 MPG. The API surfaces `delta_adjem = null` in this case
        // rather than treat a near-zero Δ as a real prediction.
        let incoming_id = Uuid::new_v4();
        let mut incoming = mk(incoming_id, 0.0, 30.0, Some("Sorcerer"));
        incoming.cam_v3 = None;
        let swapped = swap_player(&roster, incoming);

        let inc = swapped.iter().find(|p| p.player_id == incoming_id).unwrap();
        assert!(
            inc.mpg.abs() < 1e-6,
            "incoming with no cam_v3 should fall below rotation (0 mpg), got {}",
            inc.mpg,
        );
        // Existing rotation is undisturbed — Wizard keeps 30, Bard keeps 20.
        let wizard = swapped
            .iter()
            .find(|p| p.primary_class.as_deref() == Some("Wizard"))
            .unwrap();
        assert!((wizard.mpg - 30.0).abs() < 1e-6);
        let bard = swapped
            .iter()
            .find(|p| p.primary_class.as_deref() == Some("Bard"))
            .unwrap();
        assert!((bard.mpg - 20.0).abs() < 1e-6);
    }

    #[test]
    fn swap_dedups_if_incoming_already_on_roster() {
        let existing_id = Uuid::new_v4();
        let roster = vec![
            mk_q(existing_id, 30.0, 30.0, Some("Wizard"), 5.0),
            mk_q(Uuid::new_v4(), 20.0, 30.0, Some("Bard"), 3.0),
        ];
        // Incoming with same ID as the Wizard — destination's Wizard
        // entry should be dropped before ranking, so the only existing
        // player is Bard (20 mpg). Incoming with CamPom=9 ranks above
        // Bard, takes the top slot (20 mpg).
        let incoming = mk_q(existing_id, 0.0, 30.0, Some("Sorcerer"), 9.0);
        let swapped = swap_player(&roster, incoming);
        assert_eq!(swapped.len(), 2, "duplicate player_id should be replaced");
        // The remaining holder of `existing_id` is the incoming player.
        let dup = swapped.iter().find(|p| p.player_id == existing_id).unwrap();
        assert_eq!(dup.primary_class.as_deref(), Some("Sorcerer"));
        assert!(
            (dup.mpg - 20.0).abs() < 1e-6,
            "incoming should take Bard's old slot"
        );
    }
}
