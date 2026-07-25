pub mod cache;
pub mod campom_parity;
pub mod client;
pub mod compute_projections;
pub mod departures_audit;
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

/// The override precedence, as a pure function of the two sources: the
/// in-process [`set_simulated_today`] atomic (as days-from-CE, `0` = unset)
/// wins, then the `CSTAT_SIMULATED_DATE` env value. An `atomic_days` that isn't
/// a representable date falls through to `env` rather than pinning a bogus day.
///
/// **Both** [`today_utc`] and [`simulated_today`] resolve through this, so they
/// cannot disagree about whether the clock is faked — the guard in
/// `SeasonIngester::nightly` skips on `simulated_today()` while inspecting a
/// window built from `today_utc()`, and a divergence would have it compare a
/// real wall-clock hour against a simulated date. Sharing one resolver makes
/// that structurally impossible instead of merely tested: the env arm can't be
/// exercised through the public fns without `set_var`, which is `unsafe` in
/// edition 2024 and races the parallel test runner.
fn simulated_override(atomic_days: i32, env: Option<NaiveDate>) -> Option<NaiveDate> {
    if atomic_days != 0
        && let Some(d) = NaiveDate::from_num_days_from_ce_opt(atomic_days)
    {
        return Some(d);
    }
    env
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
    simulated_override(
        SIMULATED_TODAY.load(Ordering::Relaxed),
        env_simulated_date(),
    )
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
    let env = env_simulated_date();
    let Some(d) = simulated_override(days, env) else {
        return Utc::now().naive_utc().date();
    };
    // Warn once per process, and only when the *env var* is what pinned us: a
    // lingering override on a real service (API or cron) is the dangerous case,
    // and a silent one would never be caught. The in-process override is the
    // replay harness deliberately driving its own clock, so it stays quiet.
    // `simulated_override` prefers the atomic, so "the env won" is exactly "the
    // atomic arm yielded nothing".
    if simulated_override(days, None).is_none() {
        static WARNED: std::sync::Once = std::sync::Once::new();
        WARNED.call_once(|| {
            warn!(
                simulated_date = %d,
                "CSTAT_SIMULATED_DATE override active — all date-sensitive behavior \
                 uses the simulated clock"
            );
        });
    }
    d
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

/// True when the college-basketball calendar says games are actively being
/// played — November through March, plus April 1–15 (through the Final Four).
/// Mirrors `scripts/sync_to_prod.sh::in_season_now`. Respects the simulated
/// clock via [`today_utc`], so the replay harness sees the right regime.
///
/// Used to gate the archetype nightly's **live newcomer inference** (tier 3):
/// inferring an archetype from a player's thin, partial current-season sample
/// only makes sense while that season is still accumulating games. Off-season
/// and historical recomputes leave sub-gate newcomers unlabelled instead.
pub fn in_season_now() -> bool {
    in_season_on(today_utc())
}

/// [`in_season_now`]'s date rule, factored out for testing.
fn in_season_on(today: NaiveDate) -> bool {
    match today.month() {
        11 | 12 | 1 | 2 | 3 => true,
        4 => today.day() <= 15,
        _ => false,
    }
}

/// Whether an archetype recompute of `season` should run tier-3 live newcomer
/// inference: true only when `season` is the current NatStat season AND the
/// calendar says games are being played. The single gate every compute path
/// uses (nightly / `update` / bootstrap), so a manual in-season recompute never
/// clobbers the tier-3 rows the nightly wrote. Respects the simulated clock, so
/// the replay harness exercises tier-3 when its simulated date is in-season.
pub fn should_infer_newcomers(season: i32) -> bool {
    in_season_now() && season == current_natstat_season()
}

/// Whether `target_season` is *fully over* — safe to retroactively exclude
/// no-shows (redshirts / non-enrollments) from that season's graded projection.
/// True for any season strictly before the current one, and for the current
/// season once the calendar leaves the playing window (the offseason after it
/// ends). **Always false for a season still being played or a future/upcoming
/// one**, so the live projection never retro-excludes a not-yet-debuted
/// freshman — a game-volume proxy can't make that distinction (it flips true in
/// the final weeks of an in-progress season). Respects the simulated clock via
/// [`today_utc`].
pub fn target_season_retro_complete(target_season: i32) -> bool {
    retro_complete_on(target_season, today_utc())
}

/// [`target_season_retro_complete`]'s date rule, factored out for testing.
fn retro_complete_on(target_season: i32, today: NaiveDate) -> bool {
    let current = season_for_date(today);
    target_season < current || (target_season == current && !in_season_on(today))
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
    fn test_in_season_on() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        assert!(in_season_on(d(2025, 11, 1))); // opening week
        assert!(in_season_on(d(2026, 1, 15))); // deep winter
        assert!(in_season_on(d(2026, 3, 31))); // tournament
        assert!(in_season_on(d(2026, 4, 15))); // Final Four edge (inclusive)
        assert!(!in_season_on(d(2026, 4, 16))); // day after the cutoff
        assert!(!in_season_on(d(2026, 7, 13))); // deep off-season
        assert!(!in_season_on(d(2026, 10, 31))); // eve of the season
    }

    #[test]
    fn test_retro_complete_on() {
        let d = |y, m, day| NaiveDate::from_ymd_opt(y, m, day).unwrap();
        // A season strictly in the past is complete regardless of the date.
        assert!(retro_complete_on(2025, d(2026, 2, 1))); // 2025 done, mid-2026-season
        assert!(retro_complete_on(2025, d(2026, 7, 1))); // 2025 done, off-season
        // The just-ended season becomes complete once we leave the playing
        // window (off-season), so the graded report card corrects immediately.
        assert!(retro_complete_on(2026, d(2026, 7, 13))); // 2026 done, summer
        assert!(retro_complete_on(2026, d(2026, 10, 31))); // 2026 done, pre-tipoff
        // The current, still-in-progress season is NOT complete — this is the
        // finding-2 guard: a game-volume proxy would wrongly flip true in the
        // final weeks. current_natstat_season(Feb 2027) == 2027.
        assert!(!retro_complete_on(2027, d(2027, 2, 15))); // 2027 being played
        assert!(!retro_complete_on(2027, d(2027, 3, 31))); // 2027 tournament
        // A future/upcoming season is never complete.
        assert!(!retro_complete_on(2027, d(2026, 7, 1))); // 2027 upcoming, off-season
        assert!(!retro_complete_on(2028, d(2027, 2, 1))); // far future
    }

    #[test]
    fn test_simulated_today_override_wins_and_clears() {
        let sim = NaiveDate::from_ymd_opt(2025, 12, 15).unwrap();
        set_simulated_today(Some(sim));
        assert_eq!(today_utc(), sim);
        assert_eq!(current_natstat_season(), 2026);
        // Both public fns route through `simulated_override`, so this only has
        // to confirm the atomic reaches them; the precedence itself is pinned by
        // `simulated_override_precedence` below.
        assert_eq!(simulated_today(), Some(sim));
        set_simulated_today(None);
        // Real clock again — just sanity-check it's nowhere near the override.
        assert_ne!(today_utc(), sim);
    }

    #[test]
    fn simulated_override_precedence() {
        // Tested on the pure resolver, not through `simulated_today()`, because
        // reaching the env arm through the public fn needs `set_var` — `unsafe`
        // in edition 2024 and racy under the parallel test runner. Going direct
        // covers the arm that a real cron actually hits (a lingering
        // CSTAT_SIMULATED_DATE) and that an atomic-only test can never reach.
        let atomic = NaiveDate::from_ymd_opt(2025, 12, 15).unwrap();
        let env = NaiveDate::from_ymd_opt(2024, 1, 1).unwrap();

        // In-process override wins over the env var.
        assert_eq!(
            simulated_override(atomic.num_days_from_ce(), Some(env)),
            Some(atomic)
        );
        // Env var is the fallback when the atomic is unset — drop this arm and
        // a lingering override on a real service stops being visible to
        // `simulated_today`, silently un-skipping the settle guard.
        assert_eq!(simulated_override(0, Some(env)), Some(env));
        // Neither set => not simulated => callers use the real clock.
        assert_eq!(simulated_override(0, None), None);
        // A stored value that isn't a representable date must fall through to
        // env rather than pin a bogus day.
        assert_eq!(simulated_override(i32::MAX, Some(env)), Some(env));
        assert_eq!(simulated_override(i32::MAX, None), None);
    }
}
