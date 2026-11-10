//! Retroactive picks for past seasons (#447): every played game's projection,
//! its lines, what the model would have taken on each market, and how it came
//! out, graded by exactly the rules the Schedule tab's live record uses
//! (`cstat_core::record`).
//!
//! Two outputs. A per-season record, printed, in the same shape as the record
//! panel. And a per-game CSV that carries the raw inputs beside each pick (the
//! projection, every line and price, the edge each pick was made on), which is
//! what pick-selection research needs: a rule other than the served one can be
//! re-run over the file without touching the database.
//!
//! **Which projection a game is graded on.** `pregame_projections` where the
//! game has one: frozen before tipoff, the honest record. Otherwise the stored
//! `game_projections` row, and that is honest only for a season the model that
//! wrote it never trained on. The all-seasons bundle has seen every season it
//! lists (#468), so its stored rows grade at a leaked ~58% against the spread;
//! a past season is fit to grade only after `game-projections` has been re-run
//! with the held-out (walk-forward) bundles. Nothing in a stored row says which
//! bundle wrote it, so the `source` column names the table and the caller has
//! to know which bundle last swept it.
//!
//! **Lines** are the record panel's: `espn_game_lines` first, `game_lines` by
//! market (`queries::LineSources::resolved`). Neither covers every past game:
//! the second source lists no line for many low-major games before 2024.

use anyhow::{Context, Result};
use chrono::NaiveDate;
use cstat_core::queries::{LineSources, line_columns, night_sql};
use cstat_core::record::{self, GradedGame, Outcome, Pick, Side};
use sqlx::{FromRow, PgPool};
use std::io::Write;
use uuid::Uuid;

#[derive(Debug, Clone, FromRow)]
pub struct BacktestRow {
    pub season: i32,
    pub game_id: Uuid,
    pub game_date: NaiveDate,
    /// The night the game is filed under (`queries::night_sql`), whose month
    /// decides the early-season spread handicap.
    pub night: NaiveDate,
    pub home_team: String,
    pub away_team: String,
    pub is_neutral: bool,
    /// `pregame` (frozen before tipoff) or `stored` (`game_projections`).
    pub source: String,
    pub projected_margin: f64,
    pub home_win_prob: f64,
    pub projected_total: Option<f64>,
    pub is_preseason: bool,
    pub actual_margin: f64,
    pub actual_total: f64,
    /// Whether each market's line came from the primary source.
    pub espn_spread_line: bool,
    pub espn_total_line: bool,
    #[sqlx(flatten)]
    pub lines: LineSources,
}

impl BacktestRow {
    pub fn graded(&self) -> GradedGame {
        let l = self.lines.resolved();
        GradedGame {
            night: self.night,
            projected_margin: self.projected_margin,
            home_win_prob: self.home_win_prob,
            projected_total: self.projected_total,
            is_preseason: self.is_preseason,
            actual_margin: self.actual_margin,
            actual_total: self.actual_total,
            home_spread: l.home_spread,
            home_spread_odds: l.home_spread_odds,
            away_spread_odds: l.away_spread_odds,
            home_moneyline: l.home_moneyline,
            away_moneyline: l.away_moneyline,
            total_line: l.total,
            over_odds: l.over_odds,
            under_odds: l.under_odds,
        }
    }
}

/// Every played game of `season` with a projection, in the game's own home
/// frame. A projection row keeps the home and away it was made with, and a
/// game `correct_swapped_games` relabelled since is flipped rather than
/// dropped, as `queries::get_pregame_results` does.
pub async fn load(pool: &PgPool, season: i32) -> Result<Vec<BacktestRow>> {
    let columns = line_columns("pr.home_team_id");
    let night = night_sql("g", "b");
    let query = format!(
        r#"
        WITH projected AS (
            SELECT pp.game_id, pp.home_team_id, pp.away_team_id, 'pregame' AS source,
                   pp.projected_margin, pp.home_win_prob,
                   (pp.projected_home_score + pp.projected_away_score)::float8
                       AS projected_total,
                   pp.basis = 'preseason' AS is_preseason
            FROM pregame_projections pp
            WHERE pp.season = $1
            UNION ALL
            SELECT gp.game_id, gp.home_team_id, gp.away_team_id, 'stored',
                   gp.projected_margin, gp.home_win_prob,
                   (gp.projected_home_score + gp.projected_away_score)::float8,
                   false
            FROM game_projections gp
            WHERE gp.season = $1
              AND NOT EXISTS (SELECT 1 FROM pregame_projections pp
                               WHERE pp.game_id = gp.game_id)
        ),
        pr AS (
            SELECT g.season, g.id AS game_id, g.game_date, {night} AS night,
                   g.home_team_id, g.is_neutral_site AS is_neutral,
                   COALESCE(ht.short_name, ht.name) AS home_team,
                   COALESCE(at.short_name, at.name) AS away_team,
                   CASE WHEN p.home_team_id = g.home_team_id THEN 1.0 ELSE -1.0 END AS dir,
                   p.source, p.projected_margin, p.home_win_prob, p.projected_total,
                   p.is_preseason,
                   (g.home_score - g.away_score)::float8 AS actual_margin,
                   (g.home_score + g.away_score)::float8 AS actual_total
            FROM projected p
            JOIN games g ON g.id = p.game_id
            JOIN teams ht ON ht.id = g.home_team_id
            JOIN teams at ON at.id = g.away_team_id
            LEFT JOIN game_broadcasts b ON b.game_id = g.id
            WHERE g.home_score IS NOT NULL AND g.away_score IS NOT NULL
              AND ((p.home_team_id = g.home_team_id AND p.away_team_id = g.away_team_id)
                OR (p.home_team_id = g.away_team_id AND p.away_team_id = g.home_team_id))
        )
        SELECT pr.season, pr.game_id, pr.game_date, pr.night, pr.home_team, pr.away_team,
               pr.is_neutral, pr.source,
               pr.dir * pr.projected_margin AS projected_margin,
               CASE WHEN pr.dir > 0 THEN pr.home_win_prob ELSE 1 - pr.home_win_prob END
                   AS home_win_prob,
               pr.projected_total, pr.is_preseason, pr.actual_margin, pr.actual_total,
               e.home_spread IS NOT NULL AS espn_spread_line,
               e.total IS NOT NULL AS espn_total_line,
               {columns}
        FROM pr
        LEFT JOIN game_lines l ON l.game_id = pr.game_id
        LEFT JOIN espn_game_lines e ON e.game_id = pr.game_id
        ORDER BY pr.game_date, pr.game_id
        "#
    );
    sqlx::query_as::<_, BacktestRow>(&query)
        .bind(season)
        .fetch_all(pool)
        .await
        .with_context(|| format!("loading backtest rows for {season}"))
}

fn side(p: &Option<Pick>) -> &'static str {
    match p.and_then(|p| p.side) {
        None if p.is_none() => "",
        None => "pass",
        Some(Side::Home) => "home",
        Some(Side::Away) => "away",
        Some(Side::Over) => "over",
        Some(Side::Under) => "under",
    }
}

fn outcome(p: &Option<Pick>) -> &'static str {
    match p.and_then(|p| p.outcome) {
        None => "",
        Some(Outcome::Win) => "win",
        Some(Outcome::Loss) => "loss",
        Some(Outcome::Push) => "push",
    }
}

fn opt<T: ToString>(v: Option<T>) -> String {
    v.map(|v| v.to_string()).unwrap_or_default()
}

const HEADER: &[&str] = &[
    "season",
    "game_id",
    "game_date",
    "night",
    "home_team",
    "away_team",
    "is_neutral",
    "source",
    "is_preseason",
    "projected_margin",
    "home_win_prob",
    "projected_total",
    "actual_margin",
    "actual_total",
    "home_spread",
    "home_spread_odds",
    "away_spread_odds",
    "spread_source",
    "home_moneyline",
    "away_moneyline",
    "total_line",
    "over_odds",
    "under_odds",
    "total_source",
    "straight_up_correct",
    "spread_pick",
    "spread_price",
    "spread_outcome",
    "spread_units",
    "moneyline_pick",
    "moneyline_price",
    "moneyline_outcome",
    "moneyline_units",
    "total_pick",
    "total_price",
    "total_outcome",
    "total_units",
];

/// One CSV record: the inputs, then each market's served pick and result.
fn record_fields(r: &BacktestRow) -> Vec<String> {
    let g = r.graded();
    let l = g.lines();
    let home = record::picks_home(g.projected_margin, g.home_win_prob);
    let p = record::picks(
        g.projected_margin,
        g.home_win_prob,
        g.projected_total,
        &l,
        g.night,
    )
    .settled(home, g.actual_margin, g.actual_total, &l);
    let source = |line: Option<f64>, espn: bool| match (line, espn) {
        (None, _) => "",
        (Some(_), true) => "espn",
        (Some(_), false) => "feed",
    };
    let price = |p: &Option<Pick>| opt(p.and_then(|p| p.price));
    let units = |p: &Option<Pick>| p.map(|p| format!("{:.4}", p.units)).unwrap_or_default();
    vec![
        r.season.to_string(),
        r.game_id.to_string(),
        r.game_date.to_string(),
        r.night.to_string(),
        r.home_team.clone(),
        r.away_team.clone(),
        r.is_neutral.to_string(),
        r.source.clone(),
        r.is_preseason.to_string(),
        // Full precision: a rule re-run over the file must land on the same
        // side of every threshold the grader did.
        g.projected_margin.to_string(),
        g.home_win_prob.to_string(),
        opt(g.projected_total),
        g.actual_margin.to_string(),
        g.actual_total.to_string(),
        opt(l.home_spread),
        opt(l.home_spread_odds),
        opt(l.away_spread_odds),
        source(l.home_spread, r.espn_spread_line).to_string(),
        opt(l.home_moneyline),
        opt(l.away_moneyline),
        opt(l.total),
        opt(l.over_odds),
        opt(l.under_odds),
        source(l.total, r.espn_total_line).to_string(),
        opt(p.straight_up),
        side(&p.spread).to_string(),
        price(&p.spread),
        outcome(&p.spread).to_string(),
        units(&p.spread),
        side(&p.moneyline).to_string(),
        price(&p.moneyline),
        outcome(&p.moneyline).to_string(),
        units(&p.moneyline),
        side(&p.total).to_string(),
        price(&p.total),
        outcome(&p.total).to_string(),
        units(&p.total),
    ]
}

/// Write every row to `out` as CSV.
pub fn write_csv(rows: &[BacktestRow], out: impl Write) -> Result<()> {
    let mut w = csv::Writer::from_writer(out);
    w.write_record(HEADER)?;
    for r in rows {
        w.write_record(record_fields(r))?;
    }
    w.flush()?;
    Ok(())
}

/// One line per season, in the record panel's terms.
pub fn summary(season: i32, rows: &[BacktestRow]) -> String {
    let games: Vec<GradedGame> = rows.iter().map(BacktestRow::graded).collect();
    let r = record::grade(&games);
    let stored = rows.iter().filter(|r| r.source == "stored").count();
    let pct = |w: u32, l: u32| {
        if w + l == 0 {
            "-".to_string()
        } else {
            format!("{:.1}%", 100.0 * w as f64 / (w + l) as f64)
        }
    };
    let f = |v: Option<f64>| v.map(|v| format!("{v:.2}")).unwrap_or("-".into());
    format!(
        "{season}: {} games ({stored} stored, {} frozen), MAE {} | lined {} (model {} vs line {}) \
         | SU {} | ATS {}-{}-{} {} {:+.1}u ({} pass) | ML {}-{} {:+.1}u ({} pass) \
         | totals {}-{}-{} {} {:+.1}u ({} pass)",
        r.games,
        rows.len() - stored,
        f(r.mae),
        r.lined_games,
        f(r.mae_lined),
        f(r.line_mae),
        pct(r.straight_up.wins, r.straight_up.losses),
        r.ats.wins,
        r.ats.losses,
        r.ats.pushes,
        pct(r.ats.wins, r.ats.losses),
        r.ats.units,
        r.ats.passes,
        r.moneyline.wins,
        r.moneyline.losses,
        r.moneyline.units,
        r.moneyline.passes,
        r.totals.wins,
        r.totals.losses,
        r.totals.pushes,
        pct(r.totals.wins, r.totals.losses),
        r.totals.units,
        r.totals.passes,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(margin: f64, spread: Option<f64>, total: Option<f64>) -> BacktestRow {
        BacktestRow {
            season: 2024,
            game_id: Uuid::nil(),
            game_date: NaiveDate::from_ymd_opt(2024, 1, 10).unwrap(),
            night: NaiveDate::from_ymd_opt(2024, 1, 10).unwrap(),
            home_team: "Home".into(),
            away_team: "Away".into(),
            is_neutral: false,
            source: "stored".into(),
            projected_margin: margin,
            home_win_prob: 0.7,
            projected_total: Some(150.0),
            is_preseason: false,
            actual_margin: 10.0,
            actual_total: 140.0,
            espn_spread_line: spread.is_some(),
            espn_total_line: false,
            lines: LineSources {
                espn_spread: spread,
                total_line: total,
                ..Default::default()
            },
        }
    }

    #[test]
    fn a_row_carries_its_picks_and_results() {
        // Model +8 against home -3: takes the home side, which covers by 7.
        // Projected 150 against 144.5: takes the over, which loses at 140.
        let f = record_fields(&row(8.0, Some(-3.0), Some(144.5)));
        let col = |name: &str| f[HEADER.iter().position(|h| *h == name).unwrap()].clone();
        assert_eq!(f.len(), HEADER.len());
        assert_eq!(col("spread_pick"), "home");
        assert_eq!(col("spread_outcome"), "win");
        assert_eq!(col("spread_source"), "espn");
        assert_eq!(col("total_pick"), "over");
        assert_eq!(col("total_outcome"), "loss");
        assert_eq!(col("total_source"), "feed");
        assert_eq!(col("total_units"), "-1.0000");
        assert_eq!(col("moneyline_pick"), "");
        assert_eq!(col("straight_up_correct"), "true");
    }

    #[test]
    fn no_edge_is_a_pass_and_no_line_is_blank() {
        let f = record_fields(&row(3.0, Some(-3.0), None));
        let col = |name: &str| f[HEADER.iter().position(|h| *h == name).unwrap()].clone();
        assert_eq!(col("spread_pick"), "pass");
        assert_eq!(col("spread_outcome"), "");
        assert_eq!(col("total_pick"), "");
        assert_eq!(col("total_source"), "");
    }
}
