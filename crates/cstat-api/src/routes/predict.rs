use axum::{
    Router,
    extract::{Query, State},
    http::StatusCode,
    response::Json,
    routing::get,
};
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{Value, json};
use sqlx::PgPool;
use std::collections::BTreeMap;
use std::sync::Arc;
use uuid::Uuid;

use cstat_core::features::TeamSeason;
use cstat_core::inference::{FEATURE_META, FEATURE_NAMES, NUM_FEATURES};
use cstat_core::projection::{
    self, Attribution, BlendClock, INVALID_MATCHUP_PREFIX, NO_PREDICTION_DATA_PREFIX,
    ProjectionSummary, Venue,
};
use cstat_core::queries;

use crate::AppState;

/// Which clock the preseason blend should read for this request.
///
/// The engine lives in `cstat-core`, which deliberately cannot see
/// `cstat_ingest::today_utc` (that's where the replay harness's simulated-clock
/// overrides live), so the wall-clock read happens here — at the edge — and
/// travels in as data.
fn blend_clock(as_of_date: Option<NaiveDate>) -> BlendClock {
    match as_of_date {
        Some(d) => BlendClock::AsOf(d),
        None => BlendClock::Live(cstat_ingest::today_utc()),
    }
}

/// Per-matchup projection for the surfaces that don't need the explainability
/// payload — TeamDetail's `Projected` column and the ScoreTicker strip.
///
/// A thin `AppState` adapter over [`projection::predict_projection`]; the
/// arithmetic itself is shared with the nightly `game_projections` writer so a
/// precomputed row and a live call agree exactly.
pub async fn predict_projection(
    state: &Arc<AppState>,
    home_team_id: Uuid,
    away_team_id: Uuid,
    season: i32,
    is_neutral: bool,
    is_conference: bool,
    as_of_date: Option<NaiveDate>,
) -> Result<ProjectionSummary, String> {
    projection::predict_projection(
        &state.db.pool,
        &state.predictor,
        home_team_id,
        away_team_id,
        season,
        is_neutral,
        is_conference,
        blend_clock(as_of_date),
    )
    .await
}

pub fn router() -> Router<Arc<AppState>> {
    Router::new().route("/api/predict", get(predict))
}

/// Map a prediction-engine error string onto an HTTP status.
///
/// Pure and separately tested, because getting it wrong is expensive in a
/// direction that isn't visible from the response: a 5xx here is tapped by
/// `guards.rs` and posted to `#errors-api`, so mis-classifying a client
/// mistake pages a human.
///
/// - [`NO_PREDICTION_DATA_PREFIX`] → **404**. We looked and hold nothing for
///   this team/season. Covers a not-yet-D1 program, a typo, AND the routine
///   ingest-before-compute window. Deliberately never pages: the request path
///   can't reliably tell a typo from a real data outage, so any attempt to
///   alert here false-fires on normal states, DB blips, and bad input.
///   Detecting a genuine data gap (a team that played but lost its stats /
///   roster rows) is the compute pipeline's job — its post-run invariant
///   checks (ROADMAP M5), which have full context and no typo noise.
/// - [`INVALID_MATCHUP_PREFIX`] → **400**. The question itself was malformed
///   (today: point-in-time across two seasons), so nothing was looked up.
/// - anything else → **500**, a genuine server fault, and it pages.
fn predict_error_status(e: &str) -> StatusCode {
    if e.starts_with(NO_PREDICTION_DATA_PREFIX) {
        StatusCode::NOT_FOUND
    } else if e.starts_with(INVALID_MATCHUP_PREFIX) {
        StatusCode::BAD_REQUEST
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    }
}

#[derive(Deserialize)]
struct PredictParams {
    home: String,
    away: String,
    /// New explicit venue selector. If absent, falls back to the legacy
    /// `neutral` boolean (true → Neutral, false/absent → Home).
    venue: Option<Venue>,
    #[serde(default)]
    neutral: bool,
    season: Option<i32>,
    /// Per-side season overrides. Each falls back to `season` when absent, so
    /// a request naming neither is the legacy single-season path byte-for-byte.
    /// Naming two different years is a *what-if* matchup (2015 Kentucky vs
    /// 2026 Duke) — the two sides never met and never could, which is why
    /// several parts of the response below are switched off rather than
    /// computed and returned empty.
    home_season: Option<i32>,
    away_season: Option<i32>,
    /// Optional point-in-time cutoff (`YYYY-MM-DD`). When set, the
    /// prediction is rebuilt from features available *up to and
    /// including* that date — the leak-free path tied to the pit
    /// model bundle. Caller responsibility: pass `game_date - 1 day`
    /// for completed games (so the model sees pre-game state, not the
    /// game itself), or `Today` for live predictions. Omitting it
    /// preserves the legacy end-of-season behavior.
    as_of_date: Option<NaiveDate>,
}

impl PredictParams {
    fn resolved_venue(&self) -> Venue {
        self.venue.unwrap_or(if self.neutral {
            Venue::Neutral
        } else {
            Venue::Home
        })
    }

    /// `(home_season, away_season)`. Both fall back to `season`, which falls
    /// back to the site default — so the three-way absence is the legacy path.
    ///
    /// Note the fallback runs through `season` rather than straight to the
    /// default: `?season=2015&home_season=2026` means "2026 Duke visiting the
    /// 2015 field", not "2026 Duke visiting the 2026 field".
    fn resolved_seasons(&self, fallback: i32) -> (i32, i32) {
        let season = self.season.unwrap_or(fallback);
        (
            self.home_season.unwrap_or(season),
            self.away_season.unwrap_or(season),
        )
    }
}

/// Validate `as_of_date` against the resolved matchup, returning the
/// user-facing 400 message on rejection.
///
/// Pure — the clock and the two seasons travel in — because each rejection is
/// a message the user has to act on, and getting the wording right matters
/// more than it looks: the alternative to every one of these is a
/// confidently-labelled garbage forecast, not an error.
///
/// - **Cross-era.** The point-in-time cohort (`features.rs`
///   `build_all_features_pit`) is built for exactly one season and cannot
///   straddle two. `predict_matchup` carries a backstop for the same
///   combination, but it can only name the seasons; here we can name the query
///   param to drop.
/// - **Future.** No data exists yet, so no answer can be honest.
/// - **Not a real season.** The floor below is built from `home_season`, which
///   is an unvalidated query param whose range is wider than chrono's year
///   range. Constructing it fallibly is what keeps a bad query string from
///   panicking the handler; see the comment at the check itself.
/// - **Before the season opens.** Produces an empty pit cohort that the model
///   silently dilutes into a degenerate "bias-only" prediction, labelled as
///   honest. Seasons use end-year numbering (2026 = the 2025-26 season), so
///   the floor is Sep 1 of the prior calendar year — early enough to probe
///   preseason and opening night, late enough to catch a date that plainly
///   belongs to a different season.
fn validate_as_of_date(
    as_of_date: Option<NaiveDate>,
    home_season: i32,
    away_season: i32,
    today: NaiveDate,
) -> Result<(), String> {
    let Some(d) = as_of_date else {
        return Ok(());
    };

    // Checked first: a cross-era request is malformed whatever the date is,
    // and the bounds below have no single season to measure against.
    if home_season != away_season {
        return Err(format!(
            "as_of_date is single-season and this matchup spans two \
             (home {home_season}, away {away_season}); point-in-time state \
             is computed within one season, so drop as_of_date for a \
             cross-year matchup or make the two seasons match"
        ));
    }

    if d > today {
        return Err(format!(
            "as_of_date {d} is in the future; honest predictions can only \
             reflect data through today ({today})"
        ));
    }

    // Fallibly, not with an `expect`: `home_season` is an unvalidated query
    // param, and chrono's year range is narrower than i32's, so
    // `?season=300000&as_of_date=2026-01-01` used to panic the handler here.
    // A panic is the worst available outcome for a bad query string — the
    // `guards.rs` hook turns it into a 500 AND posts it to #errors-api, which
    // is the exact false-fire [`predict_error_status`] exists to avoid.
    let Some(earliest) = home_season
        .checked_sub(1)
        .and_then(|y| NaiveDate::from_ymd_opt(y, 9, 1))
    else {
        return Err(format!(
            "season {home_season} is not a real season; pick one the site has \
             data for"
        ));
    };
    if d < earliest {
        return Err(format!(
            "as_of_date {d} is before season {home_season} starts ({earliest}); \
             pick a date in this season or change the season parameter"
        ));
    }

    Ok(())
}

async fn predict(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PredictParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let (home_season, away_season) = params.resolved_seasons(crate::default_season());
    let venue = params.resolved_venue();

    // A matchup whose two sides come from different years is a what-if that
    // never happened and could not have. Everything below that assumes the two
    // teams shared a league — the conference flag, prior meetings, the
    // preseason blend, the point-in-time cohort — is switched off from this
    // one predicate rather than being left to return something empty or
    // coincidentally-true.
    let cross_era = home_season != away_season;

    // Bound-check `as_of_date` before doing any DB work; see
    // [`validate_as_of_date`] for why each rejection exists.
    validate_as_of_date(
        params.as_of_date,
        home_season,
        away_season,
        cstat_ingest::today_utc(),
    )
    .map_err(|error| (StatusCode::BAD_REQUEST, Json(json!({ "error": error }))))?;

    // Each side resolves in its own year. `find_team` is already
    // name-and-season scoped, so this needs no new query — but the 404 has to
    // name both the side and the year: "not in Division I in 2015" is a
    // routine outcome on a cross-year surface, not a typo, and a message that
    // only echoes the team name leaves the user with no idea which of the two
    // slots to change.
    let home_team = find_team(&state.db.pool, &params.home, home_season)
        .await
        .map_err(|_| team_not_found("home", &params.home, home_season))?;

    let away_team = find_team(&state.db.pool, &params.away, away_season)
        .await
        .map_err(|_| team_not_found("away", &params.away, away_season))?;

    // Conference membership is a within-season fact. Across years the string
    // match is nonsense in both directions — the 2015 Big East is not the 2026
    // Big East, and Duke 2024 vs Duke 2026 would sail in as a conference game
    // — so the flag is forced off and the model sees `is_conference_game = 0`.
    let is_conference = !cross_era
        && home_team.conference.is_some()
        && home_team.conference == away_team.conference;

    // Run the predictor with explicit venue semantics. Neutral games are
    // symmetrised inside the helper so argument order doesn't change the
    // answer. The returned `Explained` carries both the headline numbers
    // and per-feature ablation deltas + the input feature vector itself
    // (already sign-flipped to the home perspective for the Away venue).
    // Each side's season is bound to its team id, which is what keeps the
    // venue swap inside `predict_with_venue` from pairing each team with the
    // other's year.
    let home_ts = TeamSeason::new(home_team.id, home_season);
    let away_ts = TeamSeason::new(away_team.id, away_season);
    let explained = projection::predict_with_venue(
        &state.db.pool,
        &state.predictor,
        home_ts,
        away_ts,
        venue,
        is_conference,
        params.as_of_date,
        // The Keys panel renders these — the one surface that does.
        Attribution::Shap,
    )
    .await
    .map_err(|e| {
        let status = predict_error_status(&e);
        if status != StatusCode::INTERNAL_SERVER_ERROR {
            tracing::warn!(
                home = %params.home, away = %params.away,
                home_season, away_season, %status,
                "predict: client-side prediction failure — {e}"
            );
        }
        (status, Json(json!({ "error": e })))
    })?;

    // Early-season preseason × pit blend (ROADMAP §6) — see
    // [`apply_preseason_blend`] for the full semantics (weight schedule,
    // live-path gating, σ choice).
    //
    // Cross-era skips it outright. `apply_preseason_blend` takes a single
    // season and `blend_weight` already decays to 0 for any past one, so for
    // most cross-era pairs this is a guard rather than new math. It is load-
    // bearing for the pair it isn't: when one slot is the in-progress season,
    // the weight is non-zero and the helper would look up BOTH teams'
    // `team_preseason_projection` rows in that one season — silently pulling
    // the other side's current-year forecast in place of the past-year team
    // the user actually asked for.
    let pit_margin = explained.prediction.predicted_margin;
    let mut prediction_basis = if cross_era {
        // Its own label. The existing four all describe *how much of the
        // season the number saw*, which is not the axis a cross-year what-if
        // varies on; reusing "leaky" in particular would read as an accuracy
        // warning on a surface where the whole point is that the matchup is
        // hypothetical.
        "cross_era"
    } else if params.as_of_date.is_some() {
        "pit"
    } else {
        "leaky"
    };
    let blend = if cross_era {
        None
    } else {
        projection::apply_preseason_blend(
            &state.db.pool,
            home_season,
            home_team.id,
            away_team.id,
            venue,
            blend_clock(params.as_of_date),
            pit_margin,
        )
        .await
    };
    let blended_margin = blend.map(|b| b.margin).unwrap_or(pit_margin);
    if let Some(b) = blend {
        // Peak weight is 0.70 (never pure preseason), so the chip labels the
        // *dominant* leg: "preseason" while the preseason weight is the majority
        // (the first ~12 days), "blended" through the decay tail to pure pit.
        prediction_basis = if b.weight >= 0.5 {
            "preseason"
        } else {
            "blended"
        };
    }
    let blended_win_prob = match blend {
        Some(b) => b.win_prob,
        None => explained.prediction.home_win_probability,
    };

    let predicted_winner = if blended_margin > 0.0 {
        &home_team.name
    } else {
        &away_team.name
    };

    let venue_str = match venue {
        Venue::Home => "home",
        Venue::Away => "away",
        Venue::Neutral => "neutral",
    };

    let (feature_contributions, contributions_by_group) =
        build_contribution_payload(&explained.feature_values, &explained.contributions);

    // Roster summaries + prior meetings travel in the same response so the
    // Predict page stays a one-round-trip view. Both run in parallel with
    // each other (the prediction has already resolved before this point —
    // it's the slowest step and gates the response shape via venue+team
    // perspective). Failures here downgrade to empty arrays rather than
    // tanking the prediction; the page degrades gracefully.
    //
    // Each roster/archetype fetch takes its own side's season — that is the
    // whole content of the cross-era case here. Prior meetings, by contrast,
    // are skipped rather than queried: two teams from different years never
    // played, by construction, so the query is guaranteed empty and its only
    // possible non-empty answer would be wrong.
    let pool = &state.db.pool;
    let prior_meetings_fut = async {
        if cross_era {
            Vec::new()
        } else {
            queries::get_prior_meetings(pool, home_team.id, away_team.id, home_season)
                .await
                .unwrap_or_default()
        }
    };
    let (roster_home, roster_away, prior_meetings_raw, archetype_home, archetype_away) = tokio::join!(
        queries::get_team_roster(pool, home_team.id, home_season),
        queries::get_team_roster(pool, away_team.id, away_season),
        prior_meetings_fut,
        queries::get_team_archetype_index(pool, home_team.id, home_season),
        queries::get_team_archetype_index(pool, away_team.id, away_season),
    );
    let roster_home = roster_home.unwrap_or_default();
    let roster_away = roster_away.unwrap_or_default();
    let archetype_home = archetype_home.unwrap_or_default();
    let archetype_away = archetype_away.unwrap_or_default();

    // Box score data: only fetch when there's at least one prior meeting.
    // Saves two empty-array round-trips on the common case (no rematch yet).
    let prior_meetings = if prior_meetings_raw.is_empty() {
        Vec::new()
    } else {
        let game_ids: Vec<Uuid> = prior_meetings_raw.iter().map(|m| m.game_id).collect();
        let (team_boxes, player_boxes) = tokio::join!(
            queries::get_team_game_boxes(pool, &game_ids),
            queries::get_player_game_boxes(pool, &game_ids),
        );
        let team_boxes = team_boxes.unwrap_or_default();
        let player_boxes = player_boxes.unwrap_or_default();

        prior_meetings_raw
            .into_iter()
            .map(|m| {
                let team_box: Vec<&queries::TeamGameBox> = team_boxes
                    .iter()
                    .filter(|b| b.game_id == m.game_id)
                    .collect();
                let player_box: Vec<&queries::PlayerGameBox> = player_boxes
                    .iter()
                    .filter(|b| b.game_id == m.game_id)
                    .collect();
                json!({
                    "headline": m,
                    "team_box": team_box,
                    "player_box": player_box,
                })
            })
            .collect::<Vec<_>>()
    };

    // Derive integer team scores from (total ± margin) / 2. Rounded
    // independently — `home + away` may differ from `round(total)` by
    // ±1 in edge cases where (total ± margin) / 2 lands on .5 (e.g.
    // total=146.0, margin=3.0 → 75-72, sum 147 ≠ round(total) 146).
    // We accept this because `predicted_total` isn't currently
    // displayed in the UI; only the integer scores and the
    // 1-decimal `predicted_margin` are. If the totals number ever
    // gets surfaced alongside the score pair, switch to
    // `away_score = round(total) - home_score` for sum reconciliation.
    // Scores derive from the *blended* margin so they stay consistent with
    // the headline (total stays pit — preseason has no totals model).
    let total = explained.prediction.predicted_total as f64;
    let margin = blended_margin as f64;
    let predicted_home_score = ((total + margin) / 2.0).round() as i32;
    let predicted_away_score = ((total - margin) / 2.0).round() as i32;

    // `prediction_basis` ("preseason" | "blended" | "pit" | "leaky" |
    // "cross_era") is set above alongside the blend so the frontend chip reads
    // which regime is active rather than inferring from its own state — a
    // request that drops `as_of_date` in transit can't paint a leaky
    // prediction as honest.

    Ok(Json(json!({
        "home_team": home_team.name,
        "home_team_id": home_team.id,
        "away_team": away_team.name,
        "away_team_id": away_team.id,
        // The shared season, and the field every legacy caller already reads.
        // Cross-era it degrades to the home side's year; the two seasons are
        // deliberately not echoed as separate fields, so that a request naming
        // neither new param gets a byte-identical response. A cross-era caller
        // named both seasons itself, and both team ids are season-scoped.
        "season": home_season,
        "venue": venue_str,
        "as_of_date": params.as_of_date,
        "prediction_basis": prediction_basis,
        "predicted_margin": (blended_margin as f64 * 10.0).round() / 10.0,
        "home_win_probability": (blended_win_prob * 1000.0).round() / 1000.0,
        "predicted_total": (total * 10.0).round() / 10.0,
        "predicted_home_score": predicted_home_score,
        "predicted_away_score": predicted_away_score,
        "predicted_winner": predicted_winner,
        "feature_contributions": feature_contributions,
        "contributions_by_group": contributions_by_group,
        "roster_home": roster_home,
        "roster_away": roster_away,
        "archetype_distribution_home": archetype_home,
        "archetype_distribution_away": archetype_away,
        "prior_meetings": prior_meetings,
    })))
}

/// Build the JSON-shaped contribution panel from raw ablation deltas.
///
/// Returns `(feature_contributions, by_group)`. `feature_contributions`
/// lists every feature (all NUM_FEATURES of them) with name, label, group,
/// raw value, and ablation contribution — sorted by |contribution| desc.
/// The frontend slices for top-N display and aggregates per-group as
/// needed; returning the full list lets the keys panel mix the model's
/// importance with the data-side stat direction without needing a separate
/// per-feature endpoint. `by_group` is the model's signed sum per group,
/// kept around for any future "raw model breakdown" surface but currently
/// unused on the frontend (keys recompute their own group sums to flip
/// the direction sign onto the data-faithful axis).
fn build_contribution_payload(
    feature_values: &[f32; NUM_FEATURES],
    contributions: &[f32; NUM_FEATURES],
) -> (Vec<Value>, Vec<Value>) {
    // Per-feature details, sorted by |contribution| desc.
    let mut details: Vec<(usize, f32)> = contributions
        .iter()
        .enumerate()
        .map(|(i, c)| (i, *c))
        .collect();
    details.sort_by(|a, b| {
        b.1.abs()
            .partial_cmp(&a.1.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let feature_contributions = details
        .iter()
        .map(|(i, c)| {
            json!({
                "name": FEATURE_NAMES[*i],
                "label": FEATURE_META[*i].label,
                "group": FEATURE_META[*i].group,
                // Round value to 3 decimals — fraction-scaled features
                // (AST%, eFG%, TOV%, FT rate) routinely have diffs in
                // the 0.01–0.05 range, and rounding to 1 decimal would
                // collapse them all to 0.0 and obscure real direction.
                "value": round3(feature_values[*i] as f64),
                "contribution": round1(*c as f64),
            })
        })
        .collect::<Vec<_>>();

    // Group totals.
    let mut group_sums: BTreeMap<&'static str, (f32, usize)> = BTreeMap::new();
    for (i, c) in contributions.iter().enumerate() {
        let g = FEATURE_META[i].group;
        let entry = group_sums.entry(g).or_insert((0.0, 0));
        entry.0 += c;
        entry.1 += 1;
    }
    let mut group_vec: Vec<(&'static str, f32, usize)> = group_sums
        .into_iter()
        .map(|(g, (sum, n))| (g, sum, n))
        .collect();
    group_vec.sort_by(|a, b| {
        b.1.abs()
            .partial_cmp(&a.1.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    let by_group = group_vec
        .into_iter()
        .map(|(g, sum, n)| {
            json!({
                "group": g,
                "contribution": round1(sum as f64),
                "feature_count": n,
            })
        })
        .collect::<Vec<_>>();

    (feature_contributions, by_group)
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

fn round1(x: f64) -> f64 {
    (x * 10.0).round() / 10.0
}

/// The per-side 404 for a team that has no row in the season its slot asked
/// for.
///
/// Names the side and the year because on a cross-year surface this is a
/// routine outcome rather than a typo — plenty of programs simply were not
/// Division I in an older season — and the user has two slots to choose
/// between when deciding what to change.
fn team_not_found(side: &str, query: &str, season: i32) -> (StatusCode, Json<Value>) {
    (
        StatusCode::NOT_FOUND,
        Json(json!({
            "error": format!("{side} team not found: {query} (season {season})"),
        })),
    )
}

#[derive(sqlx::FromRow)]
struct TeamLookup {
    id: Uuid,
    name: String,
    conference: Option<String>,
}

async fn find_team(pool: &PgPool, query: &str, season: i32) -> Result<TeamLookup, sqlx::Error> {
    if let Ok(id) = query.parse::<Uuid>() {
        return sqlx::query_as::<_, TeamLookup>(
            "SELECT id, COALESCE(short_name, name) AS name, conference FROM teams WHERE id = $1 AND season = $2",
        )
        .bind(id)
        .bind(season)
        .fetch_one(pool)
        .await;
    }

    // Exact match against either the Torvik short_name ("Duke") or the full
    // NatStat name ("Duke Blue Devils"). short_name is the canonical input
    // surface; the full name is kept for backwards compat with old links.
    if let Ok(team) = sqlx::query_as::<_, TeamLookup>(
        "SELECT id, COALESCE(short_name, name) AS name, conference
         FROM teams
         WHERE (LOWER(short_name) = LOWER($1) OR LOWER(name) = LOWER($1))
           AND season = $2",
    )
    .bind(query)
    .bind(season)
    .fetch_one(pool)
    .await
    {
        return Ok(team);
    }

    sqlx::query_as::<_, TeamLookup>(
        "SELECT id, COALESCE(short_name, name) AS name, conference
         FROM teams
         WHERE (LOWER(short_name) LIKE LOWER($1) || '%' OR LOWER(name) LIKE LOWER($1) || '%')
           AND season = $2
         ORDER BY LENGTH(COALESCE(short_name, name))
         LIMIT 1",
    )
    .bind(query)
    .bind(season)
    .fetch_one(pool)
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(venue: Option<&str>, neutral: bool) -> PredictParams {
        let venue = venue.map(|v| match v {
            "home" => Venue::Home,
            "away" => Venue::Away,
            "neutral" => Venue::Neutral,
            _ => panic!("bad venue"),
        });
        PredictParams {
            home: "A".into(),
            away: "B".into(),
            venue,
            neutral,
            season: None,
            home_season: None,
            away_season: None,
            as_of_date: None,
        }
    }

    #[test]
    fn prediction_errors_map_to_the_right_status_and_only_real_faults_page() {
        // Missing data → 404. Never a 5xx: the `guards.rs` tap posts every 5xx
        // to #errors-api, and a typo'd team name must not page anyone.
        assert_eq!(
            predict_error_status(&format!(
                "{NO_PREDICTION_DATA_PREFIX}: one or both teams have no data for season 2021"
            )),
            StatusCode::NOT_FOUND
        );
        // Malformed question → 400. Same no-paging requirement, different
        // cause: nothing was looked up, so 404 would misdescribe it.
        assert_eq!(
            predict_error_status(&format!(
                "{INVALID_MATCHUP_PREFIX}: point-in-time predictions are single-season; \
                 got home 2015 vs away 2026"
            )),
            StatusCode::BAD_REQUEST
        );
        // Anything unrecognised stays a 500 — a genuine fault SHOULD page.
        assert_eq!(
            predict_error_status("feature extraction failed: pool timed out"),
            StatusCode::INTERNAL_SERVER_ERROR
        );
        // The two tagged prefixes must stay distinguishable from each other;
        // if one ever became a prefix of the other this classifier would
        // silently collapse two outcomes into one.
        assert!(!NO_PREDICTION_DATA_PREFIX.starts_with(INVALID_MATCHUP_PREFIX));
        assert!(!INVALID_MATCHUP_PREFIX.starts_with(NO_PREDICTION_DATA_PREFIX));
    }

    #[test]
    fn parses_venue_from_url_query_string() {
        // This is the actual deserialization path axum uses for `Query<T>` —
        // if this fails, the route would silently fall through to the
        // legacy `neutral` default and every venue would look identical.
        let cases = [
            ("home=A&away=B&venue=home", Venue::Home),
            ("home=A&away=B&venue=away", Venue::Away),
            ("home=A&away=B&venue=neutral", Venue::Neutral),
        ];
        for (q, expected) in cases {
            let p: PredictParams = serde_urlencoded::from_str(q)
                .unwrap_or_else(|e| panic!("failed to parse {q:?}: {e}"));
            assert_eq!(p.resolved_venue(), expected, "query string: {q}");
        }

        // No venue param → falls back to neutral=false default.
        let p: PredictParams = serde_urlencoded::from_str("home=A&away=B").unwrap();
        assert_eq!(p.resolved_venue(), Venue::Home);

        // Legacy neutral=true still works when venue is absent.
        let p: PredictParams = serde_urlencoded::from_str("home=A&away=B&neutral=true").unwrap();
        assert_eq!(p.resolved_venue(), Venue::Neutral);
    }

    fn date(s: &str) -> NaiveDate {
        NaiveDate::parse_from_str(s, "%Y-%m-%d").unwrap()
    }

    #[test]
    fn as_of_date_bounds() {
        let today = date("2026-05-29");

        // OK: inside season 2026's window, on or before today.
        for d in ["2025-09-01", "2025-11-01", "2026-01-15", "2026-04-06"] {
            assert!(
                validate_as_of_date(Some(date(d)), 2026, 2026, today).is_ok(),
                "{d} should be in-bounds"
            );
        }
        // Absent as_of_date is always fine — that is the legacy path.
        assert!(validate_as_of_date(None, 2026, 2026, today).is_ok());

        // Reject: future. Nothing exists to be honest about yet.
        let err = validate_as_of_date(Some(date("2027-01-01")), 2026, 2026, today).unwrap_err();
        assert!(err.contains("in the future"), "{err}");

        // Reject: before the season window. 2024-12-15 belongs to season
        // 2025, not 2026, so it slips by intent (silently pulls the
        // wrong-season cohort) unless the bound rejects it.
        let err = validate_as_of_date(Some(date("2024-12-15")), 2026, 2026, today).unwrap_err();
        assert!(err.contains("before season 2026 starts"), "{err}");

        // The floor tracks the slot's own season, not the site default: a
        // request that pins both sides to 2016 must be able to probe 2016
        // dates. Measuring against the default season instead would reject
        // every honest date in every past season.
        assert!(validate_as_of_date(Some(date("2016-01-15")), 2016, 2016, today).is_ok());
    }

    #[test]
    fn an_out_of_range_season_is_a_400_not_a_panic() {
        let today = date("2026-05-29");

        // chrono's year range is narrower than i32's, and `season` is an
        // unvalidated query param, so building the Sep-1 floor with an
        // `expect` panicked the handler on `?season=300000&as_of_date=...`.
        // A panic is the worst outcome available for a bad query string: the
        // `guards.rs` hook answers 500 *and* posts to #errors-api, paging a
        // human for a typo.
        for season in [300_000, -300_000, i32::MAX, i32::MIN] {
            let err = validate_as_of_date(Some(date("2026-01-01")), season, season, today)
                .expect_err("out-of-range season should be rejected, not accepted");
            assert!(err.contains("not a real season"), "season {season}: {err}");
        }

        // ...without dragging any plausible season down with it. The site
        // holds 2015 onward; the check must not become a de-facto season
        // allowlist that goes stale.
        for season in [1900, 2015, 2026, 2100] {
            assert!(
                validate_as_of_date(None, season, season, today).is_ok(),
                "season {season} should still be constructible"
            );
        }
    }

    #[test]
    fn as_of_date_is_rejected_across_two_seasons() {
        let today = date("2026-05-29");

        // The pit cohort is built for exactly one season and cannot straddle
        // two. `predict_matchup` carries the same guard as a backstop, but by
        // then the message can only name seasons — this one names the param.
        let err = validate_as_of_date(Some(date("2026-01-15")), 2015, 2026, today).unwrap_err();
        assert!(err.contains("as_of_date"), "{err}");
        assert!(err.contains("2015") && err.contains("2026"), "{err}");

        // Checked before the bounds, so a cross-era request gets the reason it
        // can act on rather than a season-relative complaint about a date that
        // is fine for one of its two slots.
        let err = validate_as_of_date(Some(date("2027-01-01")), 2015, 2026, today).unwrap_err();
        assert!(!err.contains("in the future"), "{err}");
    }

    #[test]
    fn resolves_a_season_per_side_with_fallbacks() {
        let fallback = 2026_i32;
        let seasons = |q: &str| {
            let p: PredictParams = serde_urlencoded::from_str(q)
                .unwrap_or_else(|e| panic!("failed to parse {q:?}: {e}"));
            p.resolved_seasons(fallback)
        };

        // Legacy: neither new param. Both sides take the site default, and
        // `season` alone still moves both — this is the byte-identical path.
        assert_eq!(seasons("home=A&away=B"), (2026, 2026));
        assert_eq!(seasons("home=A&away=B&season=2019"), (2019, 2019));

        // One side pinned; the other falls back through `season`, NOT straight
        // to the default. `season=2015&home_season=2026` is "2026 Duke
        // visiting the 2015 field", so the away side must read 2015.
        assert_eq!(seasons("home=A&away=B&home_season=2015"), (2015, 2026));
        assert_eq!(seasons("home=A&away=B&away_season=2015"), (2026, 2015));
        assert_eq!(
            seasons("home=A&away=B&season=2015&home_season=2026"),
            (2026, 2015)
        );

        // Both pinned, and pinned to the same year: not cross-era, so every
        // guard stays off and this must behave as an ordinary single-season
        // request in 2015 rather than in the default season.
        let (h, a) = seasons("home=A&away=B&home_season=2015&away_season=2015");
        assert_eq!((h, a), (2015, 2015));
        assert!(h == a, "same year on both sides is not a cross-era matchup");

        // Both pinned to different years: the cross-era case.
        let (h, a) = seasons("home=A&away=B&home_season=2015&away_season=2026");
        assert_eq!((h, a), (2015, 2026));
        assert!(h != a);
    }

    #[test]
    fn parses_as_of_date_from_url_query_string() {
        // The audit's R5 plumbing rides on this serialize round-trip — a
        // typo'd field name or a mis-quoted serde rename would silently
        // produce `None` and the leaky model would always win.
        let p: PredictParams =
            serde_urlencoded::from_str("home=A&away=B&as_of_date=2026-02-14").unwrap();
        assert_eq!(p.as_of_date, NaiveDate::from_ymd_opt(2026, 2, 14));

        // Absent param defaults to None (legacy end-of-season path).
        let p: PredictParams = serde_urlencoded::from_str("home=A&away=B").unwrap();
        assert_eq!(p.as_of_date, None);

        // Malformed dates surface as a deserialize error rather than silent
        // None — caller gets a 400 instead of a leaky prediction labelled
        // as honest. axum's Query handler maps this to a 400 automatically.
        let err: Result<PredictParams, _> =
            serde_urlencoded::from_str("home=A&away=B&as_of_date=not-a-date");
        assert!(err.is_err(), "malformed date should fail deserialization");
    }

    #[test]
    fn venue_explicit_overrides_legacy_neutral() {
        // Explicit venue always wins, even if `neutral=true` is set.
        assert_eq!(params(Some("home"), true).resolved_venue(), Venue::Home);
        assert_eq!(params(Some("away"), false).resolved_venue(), Venue::Away);
        assert_eq!(
            params(Some("neutral"), false).resolved_venue(),
            Venue::Neutral
        );
    }

    #[test]
    fn legacy_neutral_bool_falls_through() {
        // When venue is absent, fall back to the legacy boolean.
        assert_eq!(params(None, false).resolved_venue(), Venue::Home);
        assert_eq!(params(None, true).resolved_venue(), Venue::Neutral);
    }
}
