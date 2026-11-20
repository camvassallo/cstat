//! CollegeBasketballData (CBBD) API client and response types.
//!
//! CBBD is the planned replacement for NatStat's game data
//! (`docs/natstat_migration_plan.md`). Its ids lead back to ESPN: a game's
//! `sourceId` is the ESPN event id, a player's `athleteSourceId` the ESPN
//! athlete id, which is what joins it to `game_broadcasts`.
//!
//! **The quota is monthly, not hourly.** Every key has a fixed number of calls
//! a month (1,000 on the free tier), shared with CollegeFootballData, and a 429
//! means it is spent until the 1st. So a 429 is never retried: the client
//! records it and every later call this run fails at once without a request.
//! A 401 or 403 (a bad or revoked key) is handled the same way. The remaining
//! count comes back on every response as `x-calllimit-remaining` and is kept
//! for the run's log.
//!
//! Responses can be cached in `api_cache` under a `cbbd/` endpoint prefix, so a
//! re-run inside the TTL spends no quota. `/plays/date` is never cached: one
//! night is ~60 MB, which does not belong in a jsonb row.

use crate::cache::ApiCache;
use reqwest::{Client, StatusCode};
use serde::Deserialize;
use serde::de::{DeserializeOwned, IgnoredAny};
use serde_json::Value;
use sqlx::PgPool;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicI64, Ordering};
use std::time::Duration;
use thiserror::Error;
use tracing::{info, warn};

const BASE_URL: &str = "https://api.collegebasketballdata.com";

/// Retries for a network error or a 5xx. Each retry is another call against
/// the month's quota, so this stays small.
const MAX_RETRIES: u32 = 2;

#[derive(Debug, Error)]
pub enum CbbdError {
    #[error("CBBD request failed: {0}")]
    Http(#[from] reqwest::Error),
    #[error("CBBD returned HTTP {status} for {path}: {body}")]
    Status {
        status: u16,
        path: String,
        body: String,
    },
    /// The key cannot be used for the rest of the run: the month's quota is
    /// spent (429) or the key was rejected (401/403).
    #[error("CBBD refused the key ({0}); skipping CBBD for the rest of the run")]
    Refused(String),
    #[error("CBBD response did not parse for {path}: {source}")]
    Json {
        path: String,
        source: serde_json::Error,
    },
    #[error("cache error: {0}")]
    Cache(#[from] sqlx::Error),
}

pub struct CbbdClient {
    http: Client,
    api_key: String,
    cache: ApiCache,
    /// Set by the first 429/401/403; later calls return it without a request.
    refused: OnceLock<String>,
    /// The last `x-calllimit-remaining` seen, or -1 before any response.
    remaining: AtomicI64,
}

impl CbbdClient {
    pub fn new(pool: PgPool, api_key: String) -> Self {
        let http = Client::builder()
            // `/plays/date` returns tens of megabytes.
            .timeout(Duration::from_secs(180))
            .user_agent("cstat-ingest")
            .build()
            .expect("reqwest client builds");
        Self {
            http,
            api_key,
            cache: ApiCache::new(pool),
            refused: OnceLock::new(),
            remaining: AtomicI64::new(-1),
        }
    }

    /// A client from `CBBD_API_KEY`, or `None` when it is unset or empty.
    pub fn from_env(pool: PgPool) -> Option<Self> {
        let key = std::env::var("CBBD_API_KEY").ok()?;
        let key = key.trim();
        if key.is_empty() {
            return None;
        }
        Some(Self::new(pool, key.to_string()))
    }

    /// Calls left this month as of the last response, if one has arrived.
    pub fn calls_remaining(&self) -> Option<i64> {
        let n = self.remaining.load(Ordering::Relaxed);
        (n >= 0).then_some(n)
    }

    /// GET `path` with `query`, deserialized. With `cache_ttl`, a cached copy
    /// inside the TTL is returned without a call, and a fresh response is
    /// stored.
    pub async fn get<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, String)],
        cache_ttl: Option<i64>,
    ) -> Result<T, CbbdError> {
        let (endpoint, params) = cache_key(path, query);
        let parse = |v: Value| {
            serde_json::from_value(v).map_err(|source| CbbdError::Json {
                path: path.to_string(),
                source,
            })
        };
        if cache_ttl.is_some()
            && let Some(cached) = self.cache.get(&endpoint, &params).await?
        {
            info!(path, params = %params, "CBBD cache hit");
            return parse(cached);
        }
        let bytes = self.fetch(path, query).await?;
        match cache_ttl {
            Some(ttl) => {
                let value: Value =
                    serde_json::from_slice(&bytes).map_err(|source| CbbdError::Json {
                        path: path.to_string(),
                        source,
                    })?;
                self.cache.set(&endpoint, &params, &value, ttl).await?;
                parse(value)
            }
            None => serde_json::from_slice(&bytes).map_err(|source| CbbdError::Json {
                path: path.to_string(),
                source,
            }),
        }
    }

    /// GET `path` with `query` and return the raw body, uncached. For
    /// `/plays/date`, whose ~60 MB body a caller may want to keep as a file
    /// (see `cbbd_plays`), and which is parsed straight into typed rows.
    pub async fn get_bytes(
        &self,
        path: &str,
        query: &[(&str, String)],
    ) -> Result<Vec<u8>, CbbdError> {
        self.fetch(path, query).await
    }

    async fn fetch(&self, path: &str, query: &[(&str, String)]) -> Result<Vec<u8>, CbbdError> {
        if let Some(why) = self.refused.get() {
            return Err(CbbdError::Refused(why.clone()));
        }
        let url = format!("{BASE_URL}{path}");
        let mut attempt = 0;
        loop {
            info!(path, attempt, "fetching from CBBD");
            let result = self
                .http
                .get(&url)
                .query(query)
                .bearer_auth(&self.api_key)
                .send()
                .await;
            let response = match result {
                Ok(r) => r,
                Err(e) if attempt < MAX_RETRIES => {
                    warn!(path, attempt, error = %e, "CBBD network error; retrying");
                    attempt += 1;
                    tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                    continue;
                }
                Err(e) => return Err(e.into()),
            };
            if let Some(n) = response
                .headers()
                .get("x-calllimit-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|s| s.parse::<i64>().ok())
            {
                self.remaining.store(n, Ordering::Relaxed);
            }
            let status = response.status();
            if let Some(why) = refusal(status) {
                let body = response.text().await.unwrap_or_default();
                let why = format!("{why}: {}", body.chars().take(200).collect::<String>());
                warn!(path, status = status.as_u16(), reason = %why, "CBBD refused the key");
                let _ = self.refused.set(why.clone());
                return Err(CbbdError::Refused(why));
            }
            if status.is_server_error() && attempt < MAX_RETRIES {
                warn!(
                    path,
                    attempt,
                    status = status.as_u16(),
                    "CBBD server error; retrying"
                );
                attempt += 1;
                tokio::time::sleep(Duration::from_secs(2u64.pow(attempt))).await;
                continue;
            }
            if !status.is_success() {
                let body = response.text().await.unwrap_or_default();
                return Err(CbbdError::Status {
                    status: status.as_u16(),
                    path: path.to_string(),
                    body: body.chars().take(500).collect(),
                });
            }
            return Ok(response.bytes().await?.to_vec());
        }
    }
}

/// The `api_cache` key `CbbdClient::get` stores a response under.
fn cache_key(path: &str, query: &[(&str, String)]) -> (String, String) {
    let params = query
        .iter()
        .map(|(k, v)| format!("{k}={v}"))
        .collect::<Vec<_>>()
        .join("&");
    (format!("cbbd{path}"), params)
}

/// CBBD's `/teams` list as the last [`CbbdClient::get`] of it cached it, or
/// `None` when no unexpired copy is cached. Makes no call, so it needs no key
/// and spends no quota: the nightly's `espn_ids` step refreshes the copy (one
/// call a month) before the gap-fill reads it.
pub async fn cached_teams(pool: &PgPool) -> Result<Option<Vec<TeamInfo>>, CbbdError> {
    let (endpoint, params) = cache_key("/teams", &[]);
    match ApiCache::new(pool.clone()).get(&endpoint, &params).await? {
        Some(v) => serde_json::from_value(v)
            .map(Some)
            .map_err(|source| CbbdError::Json {
                path: "/teams".to_string(),
                source,
            }),
        None => Ok(None),
    }
}

/// Why `status` refuses the key for the rest of the run, if it does.
fn refusal(status: StatusCode) -> Option<&'static str> {
    match status.as_u16() {
        429 => Some("monthly call limit reached"),
        401 | 403 => Some("key rejected"),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// Response types: only the fields the shadow ingest reads. Unknown fields are
// ignored, so CBBD adding one does not break a run.
// ---------------------------------------------------------------------------

/// One row of `/games`.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Game {
    pub id: i32,
    pub source_id: Option<String>,
    pub season: i32,
    pub start_date: chrono::DateTime<chrono::Utc>,
    pub status: Option<String>,
    pub neutral_site: Option<bool>,
    pub conference_game: Option<bool>,
    pub home_team_id: Option<i32>,
    pub home_team: Option<String>,
    pub home_points: Option<i32>,
    pub home_team_elo_start: Option<i32>,
    pub home_team_elo_end: Option<i32>,
    pub away_team_id: Option<i32>,
    pub away_team: Option<String>,
    pub away_points: Option<i32>,
    pub away_team_elo_start: Option<i32>,
    pub away_team_elo_end: Option<i32>,
}

/// One row of `/teams`. `sourceId` is ESPN's team id, the only place CBBD
/// gives it: `/games` and the box endpoints carry CBBD's own team ids.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamInfo {
    pub id: i32,
    #[serde(default, deserialize_with = "string_or_number")]
    pub source_id: Option<String>,
    pub school: Option<String>,
    /// Set for a Division I program; CBBD lists the other ~1,200 teams it has
    /// met (non-D1 opponents) with none.
    #[serde(default)]
    pub conference: Option<String>,
}

/// An id CBBD may send as a string or a number, read as a string.
fn string_or_number<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    Ok(match Option::<Value>::deserialize(d)? {
        Some(Value::String(s)) if !s.trim().is_empty() => Some(s.trim().to_string()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    })
}

/// `{made, attempted}` as CBBD nests every shooting split.
#[derive(Debug, Default, Deserialize)]
pub struct MadeAttempted {
    pub made: Option<i32>,
    pub attempted: Option<i32>,
}

#[derive(Debug, Default, Deserialize)]
pub struct Rebounds {
    pub offensive: Option<i32>,
    pub defensive: Option<i32>,
    pub total: Option<i32>,
}

/// A team-level count CBBD nests as `{total, ...}` (points, turnovers, fouls).
#[derive(Debug, Default, Deserialize)]
pub struct Totaled {
    pub total: Option<i32>,
}

/// One row of `/games/teams`: one team's side of one game.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamGame {
    pub game_id: i32,
    pub team_id: i32,
    pub team: String,
    pub is_home: bool,
    pub team_stats: TeamStats,
}

#[derive(Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamStats {
    #[serde(default)]
    pub points: Totaled,
    #[serde(default)]
    pub field_goals: MadeAttempted,
    #[serde(default)]
    pub three_point_field_goals: MadeAttempted,
    #[serde(default)]
    pub free_throws: MadeAttempted,
    #[serde(default)]
    pub rebounds: Rebounds,
    pub assists: Option<i32>,
    pub steals: Option<i32>,
    pub blocks: Option<i32>,
    #[serde(default)]
    pub turnovers: Totaled,
    #[serde(default)]
    pub fouls: Totaled,
    pub possessions: Option<i32>,
}

/// One row of `/games/players`: one team's players in one game.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayersGame {
    pub game_id: i32,
    pub team_id: i32,
    pub is_home: bool,
    #[serde(default)]
    pub players: Vec<PlayerLine>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayerLine {
    pub athlete_id: i32,
    pub athlete_source_id: Option<String>,
    pub name: String,
    pub starter: Option<bool>,
    pub minutes: Option<i32>,
    pub points: Option<i32>,
    #[serde(default)]
    pub field_goals: MadeAttempted,
    #[serde(default)]
    pub three_point_field_goals: MadeAttempted,
    #[serde(default)]
    pub free_throws: MadeAttempted,
    #[serde(default)]
    pub rebounds: Rebounds,
    pub assists: Option<i32>,
    pub steals: Option<i32>,
    pub blocks: Option<i32>,
    pub turnovers: Option<i32>,
    pub fouls: Option<i32>,
}

/// One play from `/plays/date`, reduced to what the coverage summary counts.
/// The rest of each play (text, participants, scores) is skipped while
/// parsing rather than built and dropped.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlayCoverage {
    pub game_id: i32,
    #[serde(default)]
    pub shooting_play: bool,
    pub shot_info: Option<ShotInfo>,
    #[serde(default)]
    pub on_floor: Option<Vec<IgnoredAny>>,
}

#[derive(Debug, Deserialize)]
pub struct ShotInfo {
    pub location: Option<ShotLocation>,
}

#[derive(Debug, Deserialize)]
pub struct ShotLocation {
    pub x: Option<f64>,
    pub y: Option<f64>,
}

/// One play from `/plays/date`, with what `cbbd_plays` writes into
/// `play_by_play`. Fields it does not read (win probability, wallclock, the
/// on-floor players' names, shot coordinates) are skipped while parsing.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Play {
    pub game_id: i32,
    /// ESPN's event id for the game.
    pub game_source_id: Option<String>,
    pub id: i64,
    /// ESPN's play id: the event id followed by ESPN's own play sequence.
    pub source_id: Option<String>,
    #[serde(default)]
    pub play_type: String,
    pub is_home_team: Option<bool>,
    pub team: Option<String>,
    pub home_score: Option<i32>,
    pub away_score: Option<i32>,
    #[serde(default)]
    pub period: i32,
    pub seconds_remaining: Option<i32>,
    #[serde(default)]
    pub scoring_play: bool,
    #[serde(default)]
    pub shooting_play: bool,
    pub score_value: Option<i32>,
    pub play_text: Option<String>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub participants: Vec<Participant>,
    pub shot_info: Option<PlayShot>,
    #[serde(default, deserialize_with = "null_as_empty")]
    pub on_floor: Vec<OnFloor>,
}

/// A player named on a play. The first is the actor (shooter, rebounder,
/// fouler, the player subbing); a made shot's second is the assister.
#[derive(Debug, Deserialize)]
pub struct Participant {
    pub id: Option<i32>,
    pub name: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PlayShot {
    pub made: Option<bool>,
    /// `three_pointer`, `jumper`, `rim` or `free_throw`.
    pub range: Option<String>,
    pub location: Option<ShotLocation>,
}

impl PlayShot {
    pub fn has_location(&self) -> bool {
        self.location
            .as_ref()
            .is_some_and(|l| l.x.is_some() && l.y.is_some())
    }
}

/// One of the (normally ten) players on the floor during a play. `team` is
/// the school name, the same string the play's own `team` carries.
#[derive(Debug, Deserialize)]
pub struct OnFloor {
    pub id: Option<i32>,
    pub team: Option<String>,
}

/// A list CBBD may send as `null`, read as empty.
fn null_as_empty<'de, D, T>(d: D) -> Result<Vec<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Ok(Option::<Vec<T>>::deserialize(d)?.unwrap_or_default())
}

impl PlayCoverage {
    pub fn has_shot_location(&self) -> bool {
        self.shot_info
            .as_ref()
            .and_then(|s| s.location.as_ref())
            .is_some_and(|l| l.x.is_some() && l.y.is_some())
    }

    pub fn full_on_floor(&self) -> bool {
        self.on_floor.as_ref().is_some_and(|f| f.len() == 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quota_and_bad_keys_refuse_but_server_errors_do_not() {
        assert!(refusal(StatusCode::TOO_MANY_REQUESTS).is_some());
        assert!(refusal(StatusCode::UNAUTHORIZED).is_some());
        assert!(refusal(StatusCode::FORBIDDEN).is_some());
        assert!(refusal(StatusCode::BAD_GATEWAY).is_none());
        assert!(refusal(StatusCode::NOT_FOUND).is_none());
    }

    #[test]
    fn parses_the_shapes_cbbd_returns() {
        // Trimmed from real 2026-11-14 responses (Gonzaga vs LSU).
        let game: Game = serde_json::from_value(serde_json::json!({
            "id": 374968, "sourceId": "401921689", "season": 2027,
            "startDate": "2026-11-14T20:30:00.000Z", "status": "final",
            "neutralSite": false, "conferenceGame": false,
            "homeTeamId": 102, "homeTeam": "Gonzaga", "homePoints": 80,
            "homeTeamEloStart": 1688, "homeTeamEloEnd": 1693,
            "awayTeamId": 136, "awayTeam": "LSU", "awayPoints": 81,
            "awayTeamEloStart": 1963, "awayTeamEloEnd": 1958,
            "excitement": 6.7
        }))
        .unwrap();
        assert_eq!(game.source_id.as_deref(), Some("401921689"));
        assert_eq!(game.away_points, Some(81));

        let team: TeamGame = serde_json::from_value(serde_json::json!({
            "gameId": 374968, "teamId": 102, "team": "Gonzaga", "isHome": true,
            "teamStats": {
                "possessions": 73, "assists": 14, "steals": 8, "blocks": 8,
                "points": {"total": 79, "byPeriod": [34, 45]},
                "fieldGoals": {"made": 26, "attempted": 63, "pct": 41},
                "threePointFieldGoals": {"made": 6, "attempted": 26, "pct": 23},
                "freeThrows": {"made": 21, "attempted": 28, "pct": 75},
                "turnovers": {"total": 12, "teamTotal": 1},
                "rebounds": {"offensive": 14, "defensive": 28, "total": 42},
                "fouls": {"total": 16, "technical": 0, "flagrant": 0}
            }
        }))
        .unwrap();
        assert_eq!(team.team_stats.points.total, Some(79));
        assert_eq!(team.team_stats.rebounds.defensive, Some(28));
        assert_eq!(team.team_stats.turnovers.total, Some(12));

        let play: PlayCoverage = serde_json::from_value(serde_json::json!({
            "gameId": 374968, "shootingPlay": true, "playText": "x",
            "shotInfo": {"made": false, "location": {"x": 100, "y": 290}},
            "onFloor": [{}, {}, {}, {}, {}, {}, {}, {}, {}, {}]
        }))
        .unwrap();
        assert!(play.has_shot_location());
        assert!(play.full_on_floor());

        let teams: Vec<TeamInfo> = serde_json::from_value(serde_json::json!([
            {"id": 102, "sourceId": "2250", "school": "Gonzaga"},
            {"id": 136, "sourceId": 99, "school": "LSU"},
            {"id": 1010, "sourceId": null, "school": "Lees-McRae"},
            {"id": 1011, "school": "Somewhere"}
        ]))
        .unwrap();
        assert_eq!(teams[0].source_id.as_deref(), Some("2250"));
        assert_eq!(teams[1].source_id.as_deref(), Some("99"));
        assert_eq!(teams[2].source_id, None);
        assert_eq!(teams[3].source_id, None);

        let bare: PlayCoverage =
            serde_json::from_value(serde_json::json!({"gameId": 1, "onFloor": null})).unwrap();
        assert!(!bare.has_shot_location());
        assert!(!bare.full_on_floor());
    }
}
