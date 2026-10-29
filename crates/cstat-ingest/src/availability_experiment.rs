//! How should a projected player who will NOT play be priced? (#420)
//!
//! The roster-impact calibrator scores a canonical-minutes weighted mean of the
//! players present (`cam_wmean`). Remove a player and the mean renormalizes
//! over who is left, so on a projected roster shorter than the 13-slot rotation
//! losing a below-average player RAISES the team. The curated absences (#418,
//! #426) made that visible: Michigan +0.7 without Lincoln Cosby.
//!
//! The question has a historical answer. About 20% of the players a projection
//! carries never record a box score in the target season (injury, redshirt,
//! an exit no feed saw). For every team that had one, this rescoring holds the
//! served pipeline fixed — the same ex-ante composition, held-out projected
//! CAM, leave-one-season-out calibrator, blend weight and program anchor as
//! `projections-backtest` — and varies only how those players are priced:
//!
//!   A  count him            the ex-ante roster, unchanged (no knowledge)
//!   B  drop him             renormalize over the rest (today's behaviour)
//!   C  depth stand-in       replace with a row at CAM min(his, -1.45)
//!   D  team-mean stand-in   replace with CAM min(his, the team's wmean without him)
//!
//! and compares each with the AdjEM the team finished with. B, C and D use
//! hindsight about WHO did not play — the same knowledge a curated absence
//! row supplies live — so the comparison among them is the decision; A is the
//! reference for whether that knowledge helps at all.
//!
//! "Did not play" is any player on the ex-ante roster with zero box scores in
//! the target season under his natstat_id or torvik_pid (the cross-season join
//! rule), and any projecting recruit whose box scores never appear.

use anyhow::{Context, Result};
use sqlx::PgPool;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use cstat_core::inference::{Predictor, RosterImpactModel};
use cstat_core::roster_features::PlayerRow;
use cstat_core::roster_impact::{apply_projected_cam_v3, build_roster_impact_features};
use cstat_core::roster_projection::{
    DraftScenario, ProjectedRoster, compose_all_projections, destination_map, fetch_draft_entrants,
    fetch_player_departures, program_anchor, project_returner_cam_v3, shrink_adj_em_weighted,
    transition_shrink_weight,
};

const MIN_QUALIFYING: usize = 7;

/// Variant C's depth level: mean `cam_gbpm_v3_psos` of qualified players at
/// minutes ranks 9-12 on their D-I team, 2022-2026 (about 5,400
/// player-seasons; -1.29 / -1.51 / -1.50 / -1.48 by rank).
const DEPTH_LEVEL_CAM_V3: f64 = -1.45;

/// A stand-in for `absent` with CAM `min(his, level)`: his class year (so
/// rotation size and experience mix are unchanged), no archetype, and an id
/// derived from his so the rotation's tie-break is deterministic.
fn stand_in(absent: &PlayerRow, level: f64) -> PlayerRow {
    let mut r = absent.clone();
    r.player_id =
        Uuid::from_u128(absent.player_id.as_u128() ^ 0x5245_504c_4143_454d_454e_545f_524f_5721);
    r.primary_class = None;
    r.secondary_class = None;
    r.cam_v3 = Some(absent.cam_v3.map_or(level, |c| c.min(level)));
    r
}
const VARIANTS: [&str; 4] = ["A count", "B drop", "C depth", "D team-mean"];

/// One affected team-season: the served-path prediction under each variant.
struct Row {
    season: i32,
    actual: f64,
    served: [f64; 4],
    raw: [f64; 4],
    /// How many projected players did not play, and their summed projected CAM.
    n_out: usize,
    out_cam: f64,
    /// Whether any of them was a recruit (the redshirt population).
    recruit_out: bool,
    /// Whether any of them projected above the team's weighted mean without
    /// him — the players whose absence the model can feel.
    star_out: bool,
}

async fn adj_em(pool: &PgPool, season: i32) -> Result<HashMap<Uuid, f64>> {
    Ok(sqlx::query_as::<_, (Uuid, f64)>(
        "SELECT team_id, adj_efficiency_margin FROM team_season_stats
         WHERE season = $1 AND adj_efficiency_margin IS NOT NULL",
    )
    .bind(season)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect())
}

async fn actual_adj_em(pool: &PgPool, base: i32, target: i32) -> Result<HashMap<Uuid, f64>> {
    Ok(sqlx::query_as::<_, (Uuid, f64)>(
        "SELECT t_base.id, tss.adj_efficiency_margin
         FROM teams t_base
         JOIN teams t_tgt ON t_tgt.natstat_id = t_base.natstat_id AND t_tgt.season = $2
         JOIN team_season_stats tss ON tss.team_id = t_tgt.id AND tss.season = $2
         WHERE t_base.season = $1 AND tss.adj_efficiency_margin IS NOT NULL",
    )
    .bind(base)
    .bind(target)
    .fetch_all(pool)
    .await?
    .into_iter()
    .collect())
}

/// Ids (player or recruit) on the ex-ante rosters that never played `target`.
async fn did_not_play(pool: &PgPool, ids: &[Uuid], target: i32) -> Result<HashSet<Uuid>> {
    // A player id: played if any target-season player row sharing his
    // natstat_id or torvik_pid has a box score. A recruit id: played if his
    // resolved cstat player has one.
    let played: Vec<Uuid> = sqlx::query_scalar(
        r#"
        WITH me AS (
            SELECT p.id, p.natstat_id, tp.torvik_pid
            FROM players p
            LEFT JOIN torvik_player_stats tp ON tp.player_id = p.id AND tp.season = p.season
            WHERE p.id = ANY($1)
            UNION ALL
            SELECT r.id, p.natstat_id, tp.torvik_pid
            FROM recruits r
            JOIN players p ON p.id = r.cstat_player_id
            LEFT JOIN torvik_player_stats tp ON tp.player_id = p.id AND tp.season = p.season
            WHERE r.id = ANY($1)
        ),
        target AS (
            SELECT p.natstat_id, tp.torvik_pid
            FROM players p
            LEFT JOIN torvik_player_stats tp ON tp.player_id = p.id AND tp.season = p.season
            WHERE p.season = $2
              AND EXISTS (SELECT 1 FROM player_game_stats g WHERE g.player_id = p.id)
        )
        SELECT DISTINCT me.id FROM me
        WHERE EXISTS (
            SELECT 1 FROM target t
            WHERE t.natstat_id = me.natstat_id
               OR (me.torvik_pid IS NOT NULL AND t.torvik_pid = me.torvik_pid)
        )
        "#,
    )
    .bind(ids)
    .bind(target)
    .fetch_all(pool)
    .await?;
    let played: HashSet<Uuid> = played.into_iter().collect();
    Ok(ids
        .iter()
        .filter(|i| !played.contains(i))
        .copied()
        .collect())
}

fn score(
    model: &RosterImpactModel,
    p: &ProjectedRoster,
    roster: &[PlayerRow],
    baseline: f64,
) -> Result<(f64, f64)> {
    let f = build_roster_impact_features(roster, p.outbound_cam_v3_sum, p.inbound_cam_v3_sum);
    let raw = model
        .predict(&f)
        .map_err(|e| anyhow::anyhow!("predict {}: {e}", p.team_name))?;
    let anchor = program_anchor(Some(baseline as f32), p.program_level, raw);
    let served = shrink_adj_em_weighted(raw, anchor, transition_shrink_weight(p));
    Ok((f64::from(raw), f64::from(served)))
}

async fn year_rows(
    pool: &PgPool,
    predictor: &Predictor,
    model_dir: &std::path::Path,
    year: i32,
) -> Result<Vec<Row>> {
    let base = year - 1;
    let model = RosterImpactModel::load_loso(model_dir, year)
        .with_context(|| format!("LOSO roster-impact model {year}"))?;
    let entrants = fetch_draft_entrants(pool, base).await?;
    let departures = fetch_player_departures(pool, base).await?;
    // Ex-ante, exactly as `projections-backtest` composes it: recruits who
    // went on to redshirt are still on the roster.
    let projections = compose_all_projections(pool, base, &entrants, &departures, predictor, false)
        .await
        .with_context(|| format!("compose base {base}"))?;
    let baseline_map = adj_em(pool, base).await?;
    let actual_map = actual_adj_em(pool, base, year).await?;

    // One "did he play?" lookup per season over every roster, not per team.
    let all_ids: Vec<Uuid> = projections
        .iter()
        .flat_map(|p| p.for_scenario(DraftScenario::Ceiling))
        .map(|r| r.player_id)
        .collect();
    let gone_all = did_not_play(pool, &all_ids, year).await?;

    let mut out = Vec::new();
    for p in &projections {
        if p.returning.len() + p.arrivals.len() + p.recruits.len() < MIN_QUALIFYING {
            continue;
        }
        let (Some(&actual), Some(&baseline)) =
            (actual_map.get(&p.team_id), baseline_map.get(&p.team_id))
        else {
            continue;
        };
        let mut roster = p.for_scenario(DraftScenario::Ceiling);
        let gone: HashSet<Uuid> = roster
            .iter()
            .map(|r| r.player_id)
            .filter(|id| gone_all.contains(id))
            .collect();
        if gone.is_empty() {
            continue;
        }

        let traj_ids: Vec<Uuid> = p
            .returning
            .iter()
            .chain(p.arrivals.iter())
            .map(|r| r.player_id)
            .chain(p.uncertain.iter().map(|(r, _)| r.player_id))
            .collect();
        let destinations = destination_map(std::slice::from_ref(p), |t| {
            baseline_map.get(&t).map(|v| *v as f32)
        });
        let projected =
            project_returner_cam_v3(pool, predictor, &traj_ids, year, &destinations).await?;
        apply_projected_cam_v3(&mut roster, &projected);

        let (absent, present): (Vec<PlayerRow>, Vec<PlayerRow>) = roster
            .iter()
            .cloned()
            .partition(|r| gone.contains(&r.player_id));
        let wmean_without = f64::from(
            build_roster_impact_features(&present, p.outbound_cam_v3_sum, p.inbound_cam_v3_sum)[1],
        );

        let mut depth = present.clone();
        depth.extend(absent.iter().map(|a| stand_in(a, DEPTH_LEVEL_CAM_V3)));
        let mut team_mean = present.clone();
        team_mean.extend(absent.iter().map(|a| stand_in(a, wmean_without)));

        let variants = [&roster, &present, &depth, &team_mean];
        let mut served = [0.0; 4];
        let mut raw = [0.0; 4];
        for (i, v) in variants.iter().enumerate() {
            let (r, s) = score(&model, p, v, baseline)?;
            raw[i] = r;
            served[i] = s;
        }
        let recruit_ids: HashSet<Uuid> = p.recruits.iter().map(|(_, m)| m.recruit_id).collect();
        out.push(Row {
            season: year,
            actual,
            served,
            raw,
            n_out: absent.len(),
            out_cam: absent.iter().filter_map(|a| a.cam_v3).sum(),
            recruit_out: absent.iter().any(|a| recruit_ids.contains(&a.player_id)),
            star_out: absent
                .iter()
                .any(|a| a.cam_v3.is_some_and(|c| c > wmean_without)),
        });
    }
    println!("  {year}: {} affected teams", out.len());
    Ok(out)
}

fn summarize(label: &str, rows: &[&Row], pick: impl Fn(&Row) -> [f64; 4]) {
    if rows.is_empty() {
        return;
    }
    let n = rows.len() as f64;
    println!("\n  {label} (n={})", rows.len());
    println!(
        "    {:<12} {:>7} {:>7} {:>16}",
        "variant", "MAE", "bias", "vs B (z)"
    );
    for (i, name) in VARIANTS.iter().enumerate() {
        let errs: Vec<f64> = rows.iter().map(|r| pick(r)[i] - r.actual).collect();
        let mae = errs.iter().map(|e| e.abs()).sum::<f64>() / n;
        let bias = errs.iter().sum::<f64>() / n;
        let d: Vec<f64> = rows
            .iter()
            .map(|r| (pick(r)[i] - r.actual).abs() - (pick(r)[1] - r.actual).abs())
            .collect();
        let dm = d.iter().sum::<f64>() / n;
        let sd = (d.iter().map(|x| (x - dm).powi(2)).sum::<f64>() / (n - 1.0).max(1.0)).sqrt();
        let z = if sd > 0.0 { dm / (sd / n.sqrt()) } else { 0.0 };
        let cmp = if i == 1 {
            "—".to_string()
        } else {
            format!("{dm:+.3} ({z:+.1})")
        };
        println!("    {name:<12} {mae:7.3} {bias:+7.3} {cmp:>16}");
    }
}

pub async fn run(
    pool: &PgPool,
    predictor: &Predictor,
    model_dir: &std::path::Path,
    years: &[i32],
) -> Result<()> {
    println!("availability-convention rescoring (#420) — target seasons {years:?}");
    let mut all = Vec::new();
    for &y in years {
        all.extend(year_rows(pool, predictor, model_dir, y).await?);
    }
    let every: Vec<&Row> = all.iter().collect();
    let stars: Vec<&Row> = all.iter().filter(|r| r.star_out).collect();
    let non_stars: Vec<&Row> = all.iter().filter(|r| !r.star_out).collect();
    let recruits: Vec<&Row> = all.iter().filter(|r| r.recruit_out).collect();
    let heavy: Vec<&Row> = all.iter().filter(|r| r.out_cam >= 5.0).collect();
    println!("\n== SERVED (blend + program anchor, weight held at the ex-ante value) ==");
    summarize("every affected team", &every, |r| r.served);
    summarize("lost a player above the team mean", &stars, |r| r.served);
    summarize(
        "lost only below-mean players (the #420 case)",
        &non_stars,
        |r| r.served,
    );
    summarize("lost a recruit (redshirt / no-show)", &recruits, |r| {
        r.served
    });
    summarize("lost >= 5.0 projected CAM", &heavy, |r| r.served);
    println!("\n== RAW calibrator ==");
    summarize("every affected team", &every, |r| r.raw);
    summarize("lost only below-mean players", &non_stars, |r| r.raw);

    println!("\n== per season, served MAE (A / B / C / D) and best ==");
    let mut seasons: Vec<i32> = all.iter().map(|r| r.season).collect();
    seasons.sort_unstable();
    seasons.dedup();
    for s in seasons {
        let rs: Vec<&Row> = all.iter().filter(|r| r.season == s).collect();
        let n = rs.len() as f64;
        let m: Vec<f64> = (0..4)
            .map(|i| {
                rs.iter()
                    .map(|r| (r.served[i] - r.actual).abs())
                    .sum::<f64>()
                    / n
            })
            .collect();
        let best = (0..4).min_by(|a, b| m[*a].total_cmp(&m[*b])).unwrap();
        println!(
            "  {s}: n={:<4} {:.3} / {:.3} / {:.3} / {:.3}   best {}",
            rs.len(),
            m[0],
            m[1],
            m[2],
            m[3],
            VARIANTS[best]
        );
    }
    let n_players: usize = all.iter().map(|r| r.n_out).sum();
    println!(
        "\n  {} affected team-seasons, {n_players} projected players who did not play",
        all.len()
    );
    Ok(())
}
