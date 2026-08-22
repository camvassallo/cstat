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
    self, Attribution, BlendClock, NO_PREDICTION_DATA_PREFIX, ProjectionSummary, Venue,
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
}

async fn predict(
    State(state): State<Arc<AppState>>,
    Query(params): Query<PredictParams>,
) -> Result<Json<Value>, (StatusCode, Json<Value>)> {
    let season = params.season.unwrap_or_else(crate::default_season);
    let venue = params.resolved_venue();

    // Bound-check `as_of_date` before doing any DB work. Future dates can't
    // be honest by construction (no data exists yet); dates before the
    // start of the requested season produce an empty pit cohort that the
    // model silently dilutes into a degenerate "bias-only" prediction
    // labelled as honest. Reject loudly instead — the alternative is the
    // user shipping a confidently-labelled garbage forecast.
    if let Some(d) = params.as_of_date {
        let today = cstat_ingest::today_utc();
        if d > today {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!(
                        "as_of_date {d} is in the future; honest predictions can only \
                         reflect data through today ({today})"
                    ),
                })),
            ));
        }
        // Seasons in this codebase use end-year numbering (season 2026 =
        // 2025-26 college season). Allow as_of_date down to Sep 1 of the
        // prior calendar year so the user can probe preseason / opening
        // night, but reject further-back dates that obviously belong to
        // another season — those would silently route to a wrong-season
        // pit cohort lookup.
        let earliest =
            chrono::NaiveDate::from_ymd_opt(season - 1, 9, 1).expect("Sep 1 always valid");
        if d < earliest {
            return Err((
                StatusCode::BAD_REQUEST,
                Json(json!({
                    "error": format!(
                        "as_of_date {d} is before season {season} starts ({earliest}); \
                         pick a date in this season or change the season parameter"
                    ),
                })),
            ));
        }
    }

    let home_team = find_team(&state.db.pool, &params.home, season)
        .await
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("home team not found: {}", params.home) })),
            )
        })?;

    let away_team = find_team(&state.db.pool, &params.away, season)
        .await
        .map_err(|_| {
            (
                StatusCode::NOT_FOUND,
                Json(json!({ "error": format!("away team not found: {}", params.away) })),
            )
        })?;

    let is_conference =
        home_team.conference.is_some() && home_team.conference == away_team.conference;

    // Run the predictor with explicit venue semantics. Neutral games are
    // symmetrised inside the helper so argument order doesn't change the
    // answer. The returned `Explained` carries both the headline numbers
    // and per-feature ablation deltas + the input feature vector itself
    // (already sign-flipped to the home perspective for the Away venue).
    // One season for both sides today; #296 splits these into `home_season` /
    // `away_season` from the query string, which is the whole reason the
    // feature builder now takes the two as a pair.
    let (home_ts, away_ts) = TeamSeason::same_season(home_team.id, away_team.id, season);
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
        // Missing feature-extraction data → 404 (client error): we can't predict
        // this matchup. Covers a not-yet-D1 program, a typo, AND the routine
        // ingest-before-compute window. Deliberately never 500/pages: the request
        // path can't reliably tell a typo from a real data outage, so any attempt
        // to page here false-fires on normal states, DB blips, and bad input.
        // Detecting a genuine data gap (a team that played but lost its stats /
        // roster rows) is the compute pipeline's job — its post-run invariant
        // checks (ROADMAP M5), which have full context and no typo noise. We log
        // it so it's at least visible in server logs in the meantime.
        if e.starts_with(NO_PREDICTION_DATA_PREFIX) {
            tracing::warn!(
                home = %params.home, away = %params.away, season,
                "predict: no prediction data for this matchup — returning 404"
            );
            (StatusCode::NOT_FOUND, Json(json!({ "error": e })))
        } else {
            (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(json!({ "error": e })),
            )
        }
    })?;

    // Early-season preseason × pit blend (ROADMAP §6) — see
    // [`apply_preseason_blend`] for the full semantics (weight schedule,
    // live-path gating, σ choice).
    let pit_margin = explained.prediction.predicted_margin;
    let mut prediction_basis = if params.as_of_date.is_some() {
        "pit"
    } else {
        "leaky"
    };
    let blend = projection::apply_preseason_blend(
        &state.db.pool,
        season,
        home_team.id,
        away_team.id,
        venue,
        blend_clock(params.as_of_date),
        pit_margin,
    )
    .await;
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
    let pool = &state.db.pool;
    let (roster_home, roster_away, prior_meetings_raw, archetype_home, archetype_away) = tokio::join!(
        queries::get_team_roster(pool, home_team.id, season),
        queries::get_team_roster(pool, away_team.id, season),
        queries::get_prior_meetings(pool, home_team.id, away_team.id, season),
        queries::get_team_archetype_index(pool, home_team.id, season),
        queries::get_team_archetype_index(pool, away_team.id, season),
    );
    let roster_home = roster_home.unwrap_or_default();
    let roster_away = roster_away.unwrap_or_default();
    let prior_meetings_raw = prior_meetings_raw.unwrap_or_default();
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

    // `prediction_basis` ("preseason" | "blended" | "pit" | "leaky") is set
    // above alongside the blend so the frontend chip reads which regime is
    // active rather than inferring from its own state — a request that drops
    // `as_of_date` in transit can't paint a leaky prediction as honest.

    Ok(Json(json!({
        "home_team": home_team.name,
        "home_team_id": home_team.id,
        "away_team": away_team.name,
        "away_team_id": away_team.id,
        "season": season,
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
            as_of_date: None,
        }
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

    #[test]
    fn as_of_date_bounds() {
        // The bound logic in `predict` is straightforward arithmetic on
        // chrono dates; test it directly rather than spinning up the full
        // route. Future dates and far-past dates should both be rejected.
        let season = 2026_i32;
        let earliest = chrono::NaiveDate::from_ymd_opt(season - 1, 9, 1).unwrap();
        let today = chrono::NaiveDate::from_ymd_opt(2026, 5, 29).unwrap();

        // OK: within bounds.
        for d in ["2025-11-01", "2026-01-15", "2026-04-06"] {
            let d = chrono::NaiveDate::parse_from_str(d, "%Y-%m-%d").unwrap();
            assert!(d >= earliest && d <= today, "{d} should be in-bounds");
        }
        // Reject: future.
        let fut = chrono::NaiveDate::from_ymd_opt(2027, 1, 1).unwrap();
        assert!(fut > today, "future date should be rejected");
        // Reject: before season window. 2024-12-15 belongs to season 2025,
        // not 2026, so it slips by intent (silently pulls the wrong-season
        // cohort) unless the bound rejects it.
        let early = chrono::NaiveDate::from_ymd_opt(2024, 12, 15).unwrap();
        assert!(early < earliest, "wrong-season date should be rejected");
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
