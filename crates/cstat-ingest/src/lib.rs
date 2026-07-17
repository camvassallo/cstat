pub mod cache;
pub mod campom_parity;
pub mod client;
pub mod compute_projections;
pub mod ingest;
pub mod measure_blend_accuracy;
pub mod notify;
pub mod preflight;
pub mod projections_backtest;
pub mod rate_limiter;
pub mod run_ledger;
pub mod simulate;
pub mod tfs;
pub mod tfs_recruits;
pub mod torvik;

pub use client::{NatStatClient, rate_budget_from_env};
pub use tfs::{AuthProbe, TfsClient, TfsError};
pub use tfs_recruits::{InstitutionGroup, Recruit247Client, RecruitError};
pub use torvik::TorkvikClient;

use chrono::{Datelike, NaiveDate, Utc};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::atomic::{AtomicI32, Ordering};
use tracing::warn;
use uuid::Uuid;

/// Process-wide simulated-date override, as days-from-CE (0 = unset). Set by
/// the `simulate` replay driver between windows; wins over the env var so a
/// single process can advance the clock without racy mid-run env mutation.
static SIMULATED_TODAY: AtomicI32 = AtomicI32::new(0);

/// Override "today" for every date-sensitive default in this process
/// (season resolution, the nightly window, the predict future-check).
/// `None` restores the real clock / env-var behavior.
pub fn set_simulated_today(date: Option<NaiveDate>) {
    SIMULATED_TODAY.store(
        date.map(|d| d.num_days_from_ce()).unwrap_or(0),
        Ordering::Relaxed,
    );
}

/// The `CSTAT_SIMULATED_DATE` env override, if it is set to a **valid**
/// `YYYY-MM-DD` date. This is the single parse every consumer must share —
/// the boot warnings and the nightly's degraded-marking key off the same
/// predicate as [`today_utc`], so a present-but-empty value (Railway injects
/// empty strings for unset config) or an unparsable typo can never be
/// *ignored* by the clock yet *alerted on* as if it pinned the window.
/// Empty/whitespace values are silently absent; a non-empty unparsable value
/// warns and counts as absent.
pub fn env_simulated_date() -> Option<NaiveDate> {
    let s = std::env::var("CSTAT_SIMULATED_DATE").ok()?;
    let s = s.trim();
    if s.is_empty() {
        return None;
    }
    match NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        Ok(d) => Some(d),
        Err(_) => {
            // Warn once per process, not per call: `today_utc` runs this on
            // every miss, and `today_utc` is on the API's per-request path
            // (default season resolution), so an unthrottled warn on a
            // persistently-malformed value would flood the logs.
            static WARNED_INVALID: std::sync::Once = std::sync::Once::new();
            WARNED_INVALID.call_once(|| {
                warn!(
                    value = %s,
                    "CSTAT_SIMULATED_DATE is not a valid YYYY-MM-DD date; using the real clock"
                );
            });
            None
        }
    }
}

/// The simulated date, if either override is active — the in-process
/// [`set_simulated_today`] first, then `CSTAT_SIMULATED_DATE`.
///
/// [`today_utc`] answers "what date is it"; this answers "is the clock fake at
/// all", which callers need when they reason about the *time of day* (something
/// the overrides don't model — they inject a date, not an instant). Silent by
/// design: [`today_utc`] owns the warn-once for a lingering override, and this
/// must be callable without doubling that log.
pub fn simulated_today() -> Option<NaiveDate> {
    let days = SIMULATED_TODAY.load(Ordering::Relaxed);
    if days != 0
        && let Some(d) = NaiveDate::from_num_days_from_ce_opt(days)
    {
        return Some(d);
    }
    env_simulated_date()
}

/// Hour (UTC) by which a game date's slate has reliably finished. A US
/// college-basketball date D's games tip ~D 23:00 UTC and the latest end around
/// D+1 07:30 UTC, so a run after ~08:00 UTC on D+1 sees D final.
///
/// The production cron fires 09:30 UTC (`railway.cron.json`), clearing this by
/// ~2h. `SeasonIngester::nightly` checks it rather than assuming it: the
/// coverage clamp's correctness depends on the schedule, and nothing else ties
/// the two together.
pub const GAMES_SETTLE_HOUR_UTC: u32 = 8;

/// Today's date (UTC) — the single wall-clock read for the pipeline.
///
/// Honors two overrides so the whole pipeline can run "as if today is
/// 2025-12-15" (season-simulation harness, M4): the in-process
/// [`set_simulated_today`] override first, then the `CSTAT_SIMULATED_DATE`
/// env var (via [`env_simulated_date`]).
pub fn today_utc() -> NaiveDate {
    let days = SIMULATED_TODAY.load(Ordering::Relaxed);
    if days != 0
        && let Some(d) = NaiveDate::from_num_days_from_ce_opt(days)
    {
        return d;
    }
    if let Some(d) = env_simulated_date() {
        // Warn once per process: a *lingering* override on a real service
        // (API or cron) is the dangerous case, and a silent one would never
        // be caught.
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            warn!(
                simulated_date = %d,
                "CSTAT_SIMULATED_DATE override active — all date-sensitive behavior \
                 uses the simulated clock"
            );
        });
        return d;
    }
    Utc::now().naive_utc().date()
}

/// Season the NCAA basketball calendar is currently in. November rolls
/// forward to the next year's season (e.g. November 2025 → 2026 season).
/// Used as the default for CLI commands so the binary doesn't go stale at
/// season rollover. Respects the simulated-date overrides via [`today_utc`].
pub fn current_natstat_season() -> i32 {
    season_for_date(today_utc())
}

/// [`current_natstat_season`]'s date→season rule, factored out for testing.
fn season_for_date(today: NaiveDate) -> i32 {
    if today.month() >= 11 {
        today.year() + 1
    } else {
        today.year()
    }
}

/// Resolve a NatStat team code to its cstat `teams.id` for a specific season.
/// Returns `None` if the code is missing or the team isn't in the DB for that
/// season. Centralized so every ingest path uses the same `(natstat_id, season)`
/// lookup instead of inlining the SQL.
pub async fn team_id_by_code_and_season(
    pool: &PgPool,
    code: Option<&str>,
    season: i32,
) -> Result<Option<Uuid>, sqlx::Error> {
    let Some(code) = code else { return Ok(None) };
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM teams WHERE natstat_id = $1 AND season = $2")
            .bind(code)
            .bind(season)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(id,)| id))
}

/// Extract the data payload from a NatStat API response.
///
/// NatStat v4 puts results under endpoint-specific keys (e.g., "teamcodes", "games", "players")
/// rather than a generic "results" key. This finds the first non-metadata key.
pub fn extract_results(page: &Value) -> Vec<&Value> {
    const META_KEYS: &[&str] = &["meta", "user", "query", "success", "error", "warnings"];
    if let Some(obj) = page.as_object() {
        for (key, value) in obj {
            if META_KEYS.contains(&key.as_str()) {
                continue;
            }
            return match value {
                Value::Array(arr) => arr.iter().collect(),
                Value::Object(inner) => inner.values().collect(),
                _ => vec![],
            };
        }
    }
    vec![]
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_extract_results_from_teamcodes_object() {
        let response = json!({
            "meta": {"results-total": 2},
            "success": "1",
            "user": {"ratelimit": 500},
            "query": {},
            "teamcodes": {
                "team_224": {"code": "KU", "name": "Kansas Jayhawks"},
                "team_236": {"code": "DUKE", "name": "Duke Blue Devils"}
            }
        });
        let results = extract_results(&response);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_extract_results_from_games_object() {
        let response = json!({
            "meta": {},
            "success": "1",
            "user": {},
            "query": {},
            "games": {
                "game_123": {"id": "123", "gameday": "2026-03-15"},
                "game_456": {"id": "456", "gameday": "2026-03-16"}
            }
        });
        let results = extract_results(&response);
        assert_eq!(results.len(), 2);
    }

    #[test]
    fn test_extract_results_from_array() {
        let response = json!({
            "meta": {},
            "success": "1",
            "data": [{"id": 1}, {"id": 2}, {"id": 3}]
        });
        let results = extract_results(&response);
        assert_eq!(results.len(), 3);
    }

    #[test]
    fn test_extract_results_skips_metadata() {
        let response = json!({
            "meta": {"page": 1},
            "success": "1",
            "user": {"account": "test"},
            "query": {"endpoint": "teams"},
            "error": "",
            "warnings": ""
        });
        // All keys are metadata — no data payload
        let results = extract_results(&response);
        assert!(results.is_empty());
    }

    #[test]
    fn test_extract_results_empty_response() {
        let response = json!(null);
        let results = extract_results(&response);
        assert!(results.is_empty());
    }

    #[test]
    fn test_season_for_date_rolls_forward_in_november() {
        let nov = NaiveDate::from_ymd_opt(2025, 11, 3).unwrap();
        assert_eq!(season_for_date(nov), 2026);
        let mar = NaiveDate::from_ymd_opt(2026, 3, 15).unwrap();
        assert_eq!(season_for_date(mar), 2026);
        let jul = NaiveDate::from_ymd_opt(2026, 7, 13).unwrap();
        assert_eq!(season_for_date(jul), 2026);
    }

    #[test]
    fn test_simulated_today_override_wins_and_clears() {
        let sim = NaiveDate::from_ymd_opt(2025, 12, 15).unwrap();
        set_simulated_today(Some(sim));
        assert_eq!(today_utc(), sim);
        assert_eq!(current_natstat_season(), 2026);
        set_simulated_today(None);
        // Real clock again — just sanity-check it's nowhere near the override.
        assert_ne!(today_utc(), sim);
    }
}
