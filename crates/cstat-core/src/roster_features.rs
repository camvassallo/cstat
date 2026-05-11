//! Roster-only AdjEM model: feature builder + transfer-swap helper.
//!
//! Mirrors `training/train_roster_model.py::aggregate_team_season` exactly.
//! The Python script and this module share one contract via
//! `training/models/roster_model_meta.json` — the loader in `inference.rs`
//! hard-fails if `player_filter` or `include_impact_features` drift from
//! what the trained model expects.
//!
//! Swap semantics: see `swap_player` — proportionally shrinks the existing
//! roster's per-player `total_min` to keep team total minutes invariant
//! after injecting an incoming player at a given MPG.

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
    /// weight column — kept as a field rather than recomputed so the swap
    /// helper can scale it directly without re-deriving from mpg/gp.
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
            pa.primary_class
        FROM player_season_stats pss
        LEFT JOIN player_archetypes pa
            ON pa.player_id = pss.player_id AND pa.season = pss.season
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
    // variant uses CamPom for the star pick — see Python script). `idxmax` on
    // ties picks the first occurrence; we do the same.
    let (star_idx, _) = roster
        .iter()
        .enumerate()
        .max_by(|(_, a), (_, b)| {
            a.total_min
                .partial_cmp(&b.total_min)
                .unwrap_or(std::cmp::Ordering::Equal)
        })
        .expect("non-empty roster");
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

/// Produce a swap-modified roster: existing players' minutes scaled down
/// to make room for the incoming player at `incoming.mpg`.
///
/// Method: a team plays ~200 minutes/game. Reduce every existing player's
/// `mpg` and `total_min` uniformly by `(200 - incoming.mpg) / 200`, then
/// inject the incoming player. The incoming player's `total_min` is
/// rewritten by this function (the caller's value is ignored) to keep the
/// team's `total_minutes` invariant — without this rewrite, the model's
/// level-sensitive `total_minutes` feature would jump arbitrarily based on
/// where the incoming player came from. Concretely:
///   `incoming.total_min := mpg * (old_total / 200)`
///   `new_total = old_total * (200 - mpg)/200 + mpg * (old_total / 200) = old_total`.
///
/// Because every existing player's stats are unchanged (only weights
/// scale uniformly), all minutes-weighted means are stable except for the
/// new player's contribution — which is the entire point of the Δ engine.
///
/// Bounds: `incoming.mpg` clamped to `[0, 40]` (40 is a hard ceiling — a
/// player can't outplay regulation time on average; OT pushes a tiny tail
/// above but treating it as a swap-cap is fine).
pub fn swap_player(roster: &[PlayerRow], mut incoming: PlayerRow) -> Vec<PlayerRow> {
    const TEAM_MINUTES_PER_GAME: f64 = 200.0;
    let mpg = incoming.mpg.clamp(0.0, 40.0);
    incoming.mpg = mpg;
    let scale = ((TEAM_MINUTES_PER_GAME - mpg) / TEAM_MINUTES_PER_GAME).max(0.0);

    // Replace the incoming player's caller-supplied total_min with one
    // consistent with the destination's minutes envelope. Excludes the
    // incoming player from `old_total` if they're already on the roster
    // so the invariant still holds in the dedup case.
    let old_total: f64 = roster
        .iter()
        .filter(|p| p.player_id != incoming.player_id)
        .map(|p| p.total_min)
        .sum();
    incoming.total_min = mpg * old_total / TEAM_MINUTES_PER_GAME;

    let mut out: Vec<PlayerRow> = Vec::with_capacity(roster.len() + 1);
    for p in roster {
        // Drop the incoming player from the destination roster if they're
        // already on it — otherwise the swap would double-count them.
        if p.player_id == incoming.player_id {
            continue;
        }
        let mut q = p.clone();
        q.mpg *= scale;
        q.total_min *= scale;
        out.push(q);
    }
    out.push(incoming);
    out
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
        }
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
    fn swap_preserves_total_minutes() {
        let roster = vec![
            mk(Uuid::new_v4(), 30.0, 30.0, Some("Wizard")),
            mk(Uuid::new_v4(), 20.0, 30.0, Some("Bard")),
            mk(Uuid::new_v4(), 15.0, 30.0, Some("Ranger")),
        ];
        let orig_total: f64 = roster.iter().map(|p| p.total_min).sum();

        // Caller sets mpg only — swap_player rewrites total_min to keep
        // the team's minutes envelope invariant.
        let incoming = mk(Uuid::new_v4(), 25.0, 30.0, Some("Sorcerer"));
        let swapped = swap_player(&roster, incoming);
        let new_total: f64 = swapped.iter().map(|p| p.total_min).sum();
        assert!(
            (new_total - orig_total).abs() < 1e-6,
            "swap broke total_minutes invariant: {orig_total} -> {new_total}",
        );
    }

    #[test]
    fn swap_injects_incoming_minutes_share() {
        // 200 mpg ÷ 200 team-min/game = 1.0 of a team-game per incoming player
        // wouldn't be physical; clamp pulls 25 mpg through unchanged.
        let roster = vec![mk(Uuid::new_v4(), 40.0, 30.0, Some("Wizard"))];
        let orig_total = roster[0].total_min; // 1200
        let incoming = mk(Uuid::new_v4(), 25.0, 30.0, Some("Sorcerer"));
        let swapped = swap_player(&roster, incoming);
        // Incoming total_min = 25 * 1200 / 200 = 150
        let inc = swapped
            .iter()
            .find(|p| p.primary_class.as_deref() == Some("Sorcerer"))
            .unwrap();
        assert!(
            (inc.total_min - 150.0).abs() < 1e-6,
            "incoming total_min {} ≠ expected 150 (25 * old_total/200)",
            inc.total_min,
        );
        // Existing player scaled by (200-25)/200 = 0.875: 1200 * 0.875 = 1050
        let existing = swapped
            .iter()
            .find(|p| p.primary_class.as_deref() == Some("Wizard"))
            .unwrap();
        assert!(
            (existing.total_min - 1050.0).abs() < 1e-6,
            "existing total_min {} ≠ 1050 (1200 * 0.875)",
            existing.total_min,
        );
        // Invariant: 1050 + 150 = 1200
        let new_total: f64 = swapped.iter().map(|p| p.total_min).sum();
        assert!((new_total - orig_total).abs() < 1e-6);
    }

    #[test]
    fn swap_dedups_if_incoming_already_on_roster() {
        let existing_id = Uuid::new_v4();
        let roster = vec![
            mk(existing_id, 30.0, 30.0, Some("Wizard")),
            mk(Uuid::new_v4(), 20.0, 30.0, Some("Bard")),
        ];
        let incoming = mk(existing_id, 25.0, 30.0, Some("Sorcerer"));
        let swapped = swap_player(&roster, incoming);
        assert_eq!(swapped.len(), 2, "duplicate player_id should be replaced");
        // The remaining holder of `existing_id` is the incoming player
        // (Sorcerer), not the original Wizard.
        let dup = swapped.iter().find(|p| p.player_id == existing_id).unwrap();
        assert_eq!(dup.primary_class.as_deref(), Some("Sorcerer"));
    }
}
