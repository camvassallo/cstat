use super::team_aliases;
use super::utils::get_f64_from;
use crate::NatStatClient;
use crate::client::NatStatError;
use crate::extract_results;
use serde_json::Value;
use sqlx::PgPool;
use tracing::{info, warn};
use uuid::Uuid;

/// Ingest all MBB teams for a given season from teamcodes.
pub async fn ingest_teams(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    let pages = client
        .get_all_pages("teamcodes", Some(&season.to_string()), None)
        .await?;

    let mut count = 0u64;
    let mut skipped_departed = 0u64;
    for page in &pages {
        let teams = extract_results(page);
        for team in teams {
            if skip_departed_program(team, season) {
                skipped_departed += 1;
                continue;
            }
            if upsert_team(team, pool, season).await? {
                count += 1;
            }
        }
    }

    info!(count, skipped_departed, season, "teams ingested");
    Ok(count)
}

/// Should this `teamcodes` row be skipped for this season because the program
/// has left Division I?
///
/// `teamcodes` publishes an `active` flag that `ingest_teams` used to ignore,
/// so every bootstrap of a not-yet-played season minted rows for programs that
/// no longer exist. Three today: Hartford and Saint Francis (NY), gone after
/// 2022-23, and Saint Francis (PA), whose last Division I season was 2025-26.
/// They have no games, so the projection skips them as too-thin and they never
/// reach the Future board — but they do reach team lists, search, and any
/// `teams`-keyed join, as programs that do not play.
///
/// **Gated on the season, and that gate is the whole correctness of this.**
/// `active` is a statement about *now* with no season dimension, while
/// `teamcodes` itself is season-agnostic — `--year` selects nothing, so the
/// same current flag is served whatever season is asked for. Skipping on the
/// flag alone would therefore erase history: `ingest_teams` is the ONLY
/// production writer of `teams` rows (`team_id_by_code_and_season` is a pure
/// lookup, and `games.rs` SKIPS a game whose team does not resolve, counting
/// it as `unresolved_team`). So a from-scratch `season --year 2023` would
/// create no Hartford row and then silently drop all 31 of its games.
///
/// A departed program cannot be entering a season that has not happened, but
/// its past seasons are real. Comparing against the current season is what
/// separates those two, and it needs no per-team departure date.
fn skip_departed_program(team: &Value, season: i32) -> bool {
    let inactive = team.get("active").and_then(|a| a.as_str()) == Some("N");
    inactive && season >= crate::current_natstat_season()
}

/// Pick the best human-readable name for a team JSON blob from NatStat.
///
/// NatStat's `/teamcodes` endpoint returns `"name": {}` (an empty object,
/// not a string) for some teams with `&` in the name (ALAM, CWM, FAMU,
/// NCAT, TAMC, TAMU, TMCM). `Value::as_str()` returns None on non-string
/// values, so the chain falls through to `full_name`, then the bundled
/// alias map, then the raw natstat code as a last resort.
///
/// The alias-map fallback writes the *short* name (e.g. "Texas A&M")
/// into what becomes the `name` column. Cosmetic — `name` is rendered
/// through `COALESCE(short_name, name)` everywhere user-facing, so the
/// user sees the short_name regardless. Pulled out of `upsert_team` so
/// the empty-object regression can be unit-tested without a DB.
fn pick_team_name<'a>(team: &'a Value, natstat_id: &'a str) -> &'a str {
    team.get("name")
        .and_then(|n| n.as_str())
        .or_else(|| team.get("full_name").and_then(|n| n.as_str()))
        .or_else(|| team_aliases::short_name(natstat_id))
        .unwrap_or(natstat_id)
}

async fn upsert_team(team: &Value, pool: &PgPool, season: i32) -> Result<bool, NatStatError> {
    let natstat_id = match team.get("code").and_then(|c| c.as_str()) {
        Some(id) => id,
        None => return Ok(false),
    };

    let name = pick_team_name(team, natstat_id);

    // Prefer the bundled Torvik-style short name; fall back to whatever NatStat
    // returns. Re-ingest is therefore idempotent and won't clobber the mapping.
    let short_name = team_aliases::short_name(natstat_id)
        .or_else(|| team.get("short_name").and_then(|n| n.as_str()));
    if short_name.is_none() {
        // Not merely cosmetic, which is why this is louder than a debug line.
        // `data/conference_realignment.json` is keyed by `short_name`, so a
        // NULL cannot be matched by the realignment capture at all — the
        // lookup does not fail, it silently finds nothing, and the team keeps
        // last season's conference with no signal that a verdict was missed.
        // The site also renders `COALESCE(short_name, name)`, so the row shows
        // its full NatStat name where every neighbour shows a short one.
        //
        // The map is bundled (`include_str!`), so the fix is an entry in
        // `data/team_short_names.json` plus a REDEPLOY — a sync will not carry
        // it. West Florida hit this as the first genuinely new Division I
        // program since the map was built (#385).
        warn!(
            natstat_id,
            season, "team has no short_name; realignment lookups key on it and will not match"
        );
    }
    let conference = team
        .get("conference")
        .or_else(|| team.get("league"))
        .and_then(|c| c.as_str());
    let division = team.get("division").and_then(|d| d.as_str());

    // **Fill-only on the three nullable columns** (#384). This is the
    // `teamcodes` path, and a `teamcodes` row carries only `code` / `name` /
    // `active` — no conference, no division, and a `short_name` only for the
    // teams the bundled alias map covers. So all three reads above are
    // routinely `None`, and writing `EXCLUDED` unconditionally does not
    // "refresh" them, it blanks them.
    //
    // That is harmless on a first bootstrap, where the columns are NULL
    // anyway, and destructive on every run after. It matters because
    // `teams.conference` is written from NatStat here and then **corrected by
    // Torvik** in `compute_all` (`TORVIK_CONF_TO_CSTAT`) — the corrected value
    // is the authoritative one, and an unconditional write silently discards
    // it, taking the Conf column, the conference filter and conference search
    // with it until the next `compute_all` repairs it. #245's checklist says
    // to re-run `teams --year 2027` weekly on the strength of this command
    // being idempotent; with an unconditional write it is not.
    //
    // `COALESCE` keeps a real incoming value winning — an alias-map fix or a
    // genuine conference from a richer payload still lands — and only
    // preserves the stored value when the incoming one is NULL. Clearing a
    // conference deliberately is not this path's job: `ingest_single_team_details`
    // writes one when the `/teams` detail payload has it, and `compute_all`'s
    // Torvik pass is what moves a realigned team.
    //
    // `name` is deliberately NOT guarded: `pick_team_name` never returns NULL
    // (it falls through to the raw code), so there is nothing for COALESCE to
    // catch, and no team in twelve ingested seasons has `name = natstat_id`,
    // so the degraded fallback has never actually fired.
    sqlx::query(
        "INSERT INTO teams (id, natstat_id, name, short_name, conference, division, season)
         VALUES ($1, $2, $3, $4, $5, $6, $7)
         ON CONFLICT (natstat_id, season) DO UPDATE
         SET name = EXCLUDED.name,
             short_name = COALESCE(EXCLUDED.short_name, teams.short_name),
             conference = COALESCE(EXCLUDED.conference, teams.conference),
             division = COALESCE(EXCLUDED.division, teams.division),
             updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(natstat_id)
    .bind(name)
    .bind(short_name)
    .bind(conference)
    .bind(division)
    .bind(season)
    .execute(pool)
    .await?;

    Ok(true)
}

/// Ingest detailed team data (TCR, ELO) from the /teams endpoint.
/// Fetches `/teams/mbb/{TEAMCODE}` for each team in the DB.
pub async fn ingest_team_details(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
) -> Result<u64, NatStatError> {
    let teams: Vec<(Uuid, String)> =
        sqlx::query_as("SELECT id, natstat_id FROM teams WHERE season = $1 ORDER BY natstat_id")
            .bind(season)
            .fetch_all(pool)
            .await?;

    let mut count = 0u64;
    for (team_id, team_code) in &teams {
        match ingest_single_team_details(client, pool, season, team_id, team_code).await {
            Ok(true) => count += 1,
            Ok(false) => {}
            Err(e) => warn!(team_code, error = %e, "failed to ingest team details, skipping"),
        }
    }

    info!(count, season, "team details ingested");
    Ok(count)
}

/// Ingest details for a single team.
pub async fn ingest_single_team_details(
    client: &NatStatClient,
    pool: &PgPool,
    season: i32,
    team_id: &Uuid,
    team_code: &str,
) -> Result<bool, NatStatError> {
    let response = client.get("teams", Some(team_code), None, None).await?;
    let results = extract_results(&response);

    let Some(team) = results.first() else {
        return Ok(false);
    };

    // Extract ELO — NatStat only provides elo.rank (ordinal ranking), not the actual rating.
    // We also check for elo.rating in case NatStat adds it in the future.
    let elo_rating = team
        .get("elo")
        .and_then(|e| {
            e.get("rating")
                .or_else(|| e.get("value"))
                .or_else(|| e.get("elo"))
        })
        .and_then(|r| {
            r.as_str()
                .and_then(|s| s.parse::<f64>().ok())
                .or(r.as_f64())
        });
    let elo_rank = team.get("elo").and_then(|e| e.get("rank")).and_then(|r| {
        r.as_str()
            .and_then(|s| s.parse::<f64>().ok())
            .or(r.as_f64())
    });
    // Prefer actual rating; fall back to rank (which the column currently stores)
    let elo_value = elo_rating.or(elo_rank);

    // Find the current season competition entry
    let season_key = format!("season_{season}");
    let competition = team.get(&season_key).and_then(|s| s.get("competition_0"));

    let (wins, losses, conference) = if let Some(comp) = competition {
        let w = comp
            .get("wins")
            .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or(v.as_i64()))
            .map(|v| v as i32);
        let l = comp
            .get("losses")
            .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or(v.as_i64()))
            .map(|v| v as i32);
        let conf = comp
            .get("league")
            .or_else(|| comp.get("conference"))
            .and_then(|c| c.as_str());
        (w, l, conf)
    } else {
        (None, None, None)
    };

    // Extract TCR (Team Composite Rating)
    let tcr = competition.and_then(|c| c.get("tcr"));
    let tcr_rank = tcr
        .and_then(|t| t.get("tcrrank"))
        .and_then(|v| v.as_str().and_then(|s| s.parse().ok()).or(v.as_i64()))
        .map(|v| v as i32);
    let tcr_points = get_f64_from(tcr, "tcrpoints");
    let tcr_adjusted = get_f64_from(tcr, "tcradjusted");
    let efficiency = get_f64_from(tcr, "efficiency");
    let defense = get_f64_from(tcr, "defense");
    let point_diff = get_f64_from(tcr, "pointdiff");
    let pythag_win_pct = get_f64_from(tcr, "pythagwinpct");
    let luck = get_f64_from(tcr, "luck");
    let opp_win_pct = get_f64_from(tcr, "oppwinpct");
    let opp_opp_win_pct = get_f64_from(tcr, "oppoppwinpct");
    let road_win_pct = get_f64_from(tcr, "roadwinpct");

    // Update conference on the team record if we got it
    if let Some(conf) = conference {
        sqlx::query("UPDATE teams SET conference = $1, updated_at = now() WHERE id = $2")
            .bind(conf)
            .bind(team_id)
            .execute(pool)
            .await?;
    }

    sqlx::query(
        "INSERT INTO team_season_stats (id, team_id, season, wins, losses, elo,
         tcr_rank, tcr_points, tcr_adjusted, efficiency, defense, point_diff,
         pythag_win_pct, luck, opp_win_pct, opp_opp_win_pct, road_win_pct, conference)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14, $15, $16, $17, $18)
         ON CONFLICT (team_id, season) DO UPDATE
         SET wins = COALESCE(EXCLUDED.wins, team_season_stats.wins),
             losses = COALESCE(EXCLUDED.losses, team_season_stats.losses),
             elo = COALESCE(EXCLUDED.elo, team_season_stats.elo),
             tcr_rank = COALESCE(EXCLUDED.tcr_rank, team_season_stats.tcr_rank),
             tcr_points = COALESCE(EXCLUDED.tcr_points, team_season_stats.tcr_points),
             tcr_adjusted = COALESCE(EXCLUDED.tcr_adjusted, team_season_stats.tcr_adjusted),
             efficiency = COALESCE(EXCLUDED.efficiency, team_season_stats.efficiency),
             defense = COALESCE(EXCLUDED.defense, team_season_stats.defense),
             point_diff = COALESCE(EXCLUDED.point_diff, team_season_stats.point_diff),
             pythag_win_pct = COALESCE(EXCLUDED.pythag_win_pct, team_season_stats.pythag_win_pct),
             luck = COALESCE(EXCLUDED.luck, team_season_stats.luck),
             opp_win_pct = COALESCE(EXCLUDED.opp_win_pct, team_season_stats.opp_win_pct),
             opp_opp_win_pct = COALESCE(EXCLUDED.opp_opp_win_pct, team_season_stats.opp_opp_win_pct),
             road_win_pct = COALESCE(EXCLUDED.road_win_pct, team_season_stats.road_win_pct),
             conference = COALESCE(EXCLUDED.conference, team_season_stats.conference),
             updated_at = now()",
    )
    .bind(Uuid::new_v4())
    .bind(team_id)
    .bind(season)
    .bind(wins.unwrap_or(0))
    .bind(losses.unwrap_or(0))
    .bind(elo_value)
    .bind(tcr_rank)
    .bind(tcr_points)
    .bind(tcr_adjusted)
    .bind(efficiency)
    .bind(defense)
    .bind(point_diff)
    .bind(pythag_win_pct)
    .bind(luck)
    .bind(opp_win_pct)
    .bind(opp_opp_win_pct)
    .bind(road_win_pct)
    .bind(conference)
    .execute(pool)
    .await?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;

    /// A TEMP `teams` that shadows the real table through `search_path`
    /// (`pg_temp` precedes `public`), on a pinned single-connection pool so
    /// the temp table survives for the whole test and dies with it.
    ///
    /// Same isolation discipline as `tests/ledger_write_failures.rs`, and for
    /// the same reason: the thing under test is an `UPDATE` against a table a
    /// developer's `DATABASE_URL` really has, and getting the shadowing wrong
    /// would not fail — it would quietly blank conferences in their database,
    /// which is precisely the bug. So the promise is asserted rather than
    /// assumed: resolve `teams` and refuse to run unless it is a temp schema.
    ///
    /// Only the columns `upsert_team` binds; no foreign keys, so nothing else
    /// in the schema is involved.
    const TEMP_TEAMS_DDL: &str = "
        CREATE TEMP TABLE teams (
            id          uuid PRIMARY KEY,
            natstat_id  text        NOT NULL,
            name        text        NOT NULL,
            short_name  text,
            conference  text,
            division    text,
            season      integer     NOT NULL,
            created_at  timestamp   NOT NULL DEFAULT now(),
            updated_at  timestamp   NOT NULL DEFAULT now(),
            UNIQUE (natstat_id, season)
        )";

    async fn temp_teams_pool() -> Option<PgPool> {
        let url = std::env::var("DATABASE_URL").ok()?;
        let pool = PgPoolOptions::new()
            .max_connections(1)
            .min_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect(&url)
            .await
            .expect("connect");
        sqlx::query(TEMP_TEAMS_DDL)
            .execute(&pool)
            .await
            .expect("create temp teams");

        let schema: String = sqlx::query_scalar(
            "SELECT n.nspname FROM pg_class c \
             JOIN pg_namespace n ON n.oid = c.relnamespace \
             WHERE c.oid = 'teams'::regclass",
        )
        .fetch_one(&pool)
        .await
        .expect("resolve teams");
        assert!(
            schema.starts_with("pg_temp"),
            "refusing to run: `teams` resolved to schema `{schema}`, not a temp schema — \
             this test would blank conferences in the REAL database in DATABASE_URL"
        );

        Some(pool)
    }

    async fn stored(pool: &PgPool, code: &str) -> (String, Option<String>, Option<String>) {
        sqlx::query_as("SELECT name, conference, division FROM teams WHERE natstat_id = $1")
            .bind(code)
            .fetch_one(pool)
            .await
            .expect("read back")
    }

    #[tokio::test]
    async fn re_ingest_does_not_blank_a_stored_conference() {
        let Some(pool) = temp_teams_pool().await else {
            eprintln!("DATABASE_URL unset — skipping");
            return;
        };

        // A `teamcodes` row, which is all this path ever sees: code, name,
        // active. No conference, no division, no short_name.
        let teamcode_row = json!({"code": "DUKE", "name": "Duke Blue Devils", "active": "Y"});

        // First bootstrap: nothing to preserve, the columns land NULL.
        upsert_team(&teamcode_row, &pool, 2027)
            .await
            .expect("insert");
        let (_, conf, div) = stored(&pool, "DUKE").await;
        assert_eq!(
            (conf, div),
            (None, None),
            "teamcodes carries neither column"
        );

        // What `compute_all`'s Torvik pass then writes — the authoritative
        // value, and the one #384 was discarding.
        sqlx::query(
            "UPDATE teams SET conference = 'ACC', division = 'D1' WHERE natstat_id = 'DUKE'",
        )
        .execute(&pool)
        .await
        .expect("torvik correction");

        // The weekly re-run #245's checklist asks for.
        upsert_team(&teamcode_row, &pool, 2027)
            .await
            .expect("re-ingest");

        let (name, conf, div) = stored(&pool, "DUKE").await;
        assert_eq!(
            conf.as_deref(),
            Some("ACC"),
            "a re-run must not blank the Torvik-corrected conference"
        );
        assert_eq!(div.as_deref(), Some("D1"), "same for division");
        assert_eq!(name, "Duke Blue Devils", "name still refreshes");
    }

    #[tokio::test]
    async fn a_real_incoming_value_still_wins_over_the_stored_one() {
        let Some(pool) = temp_teams_pool().await else {
            eprintln!("DATABASE_URL unset — skipping");
            return;
        };

        // The other half of COALESCE, and the reason it is the right tool
        // rather than "never update these columns": a payload that DOES carry
        // a conference must still be able to move a realigned team. Only a
        // NULL is treated as "no opinion".
        sqlx::query(
            "INSERT INTO teams (id, natstat_id, name, conference, season) \
             VALUES (gen_random_uuid(), 'SDST', 'San Diego State', 'MWC', 2027)",
        )
        .execute(&pool)
        .await
        .expect("seed");

        let richer = json!({"code": "SDST", "name": "San Diego State", "conference": "Pac-12"});
        upsert_team(&richer, &pool, 2027).await.expect("upsert");

        let (_, conf, _) = stored(&pool, "SDST").await;
        assert_eq!(
            conf.as_deref(),
            Some("Pac-12"),
            "a real value must still land"
        );
    }

    #[test]
    fn a_departed_program_is_skipped_only_for_seasons_that_have_not_happened() {
        let current = crate::current_natstat_season();
        let hartford = json!({"code": "HART", "name": "Hartford Hawks", "active": "N"});

        // The bug: a bootstrap of a not-yet-played season mints a row for a
        // program that left Division I.
        assert!(skip_departed_program(&hartford, current));
        assert!(skip_departed_program(&hartford, current + 1));

        // THE REGRESSION THIS GATE PREVENTS, and it would be severe.
        // `ingest_teams` is the only production writer of `teams` rows —
        // `team_id_by_code_and_season` is a pure lookup and `games.rs` skips a
        // game whose team does not resolve. Skipping on the flag alone would
        // make `season --year 2023` create no Hartford row and then silently
        // drop all 31 of its games as `unresolved_team`.
        for past in [2015, 2020, current - 1] {
            assert!(
                !skip_departed_program(&hartford, past),
                "season {past} is history and must still be ingestable"
            );
        }
    }

    #[test]
    fn an_active_program_is_never_skipped() {
        let duke = json!({"code": "DUKE", "name": "Duke Blue Devils", "active": "Y"});
        for season in [2015, crate::current_natstat_season(), 2027, 2030] {
            assert!(!skip_departed_program(&duke, season));
        }
        // A row with no `active` key at all must not be treated as departed:
        // the flag is NatStat's to send, and absence is not a claim.
        let no_flag = json!({"code": "DUKE", "name": "Duke Blue Devils"});
        assert!(!skip_departed_program(&no_flag, 2027));
    }

    #[test]
    fn every_active_program_in_the_current_feed_has_a_short_name() {
        // A NULL `short_name` is a silent key miss for
        // `data/conference_realignment.json`, which is keyed on it — the
        // lookup finds nothing rather than failing. West Florida was the
        // first genuinely new Division I program since the map was built and
        // slipped through exactly that way (#385).
        //
        // Asserted against the bundled alias map rather than a live fetch, so
        // it runs offline: the codes below are the ones the 2027 feed
        // carries. A new member added to `teamcodes` still needs the warning
        // in `upsert_team` to surface it — this pins the ones we know about.
        for code in ["WFLA", "LEMN", "DUKE", "SFPA"] {
            assert!(
                team_aliases::short_name(code).is_some(),
                "{code} has no short_name; realignment lookups key on it"
            );
        }
    }

    #[test]
    fn pick_team_name_uses_string_name_when_present() {
        let team = json!({"code": "DUKE", "name": "Duke Blue Devils"});
        assert_eq!(pick_team_name(&team, "DUKE"), "Duke Blue Devils");
    }

    #[test]
    fn pick_team_name_falls_back_to_alias_when_name_is_empty_object() {
        // Regression: NatStat's /teamcodes returns `"name": {}` for the
        // seven `&`-name teams. The fallback chain must land on the
        // alias-map short_name, NOT the raw natstat_id.
        let team = json!({"code": "ALAM", "name": {}});
        assert_eq!(pick_team_name(&team, "ALAM"), "Alabama A&M");
    }

    #[test]
    fn pick_team_name_falls_back_to_full_name_when_no_name() {
        let team = json!({"code": "DUKE", "full_name": "Duke University Blue Devils"});
        assert_eq!(pick_team_name(&team, "DUKE"), "Duke University Blue Devils");
    }

    #[test]
    fn pick_team_name_falls_back_to_code_for_unknown_team() {
        // Unaliased, no name fields at all — last-resort raw code.
        let team = json!({"code": "XYZ"});
        assert_eq!(pick_team_name(&team, "XYZ"), "XYZ");
    }
}
