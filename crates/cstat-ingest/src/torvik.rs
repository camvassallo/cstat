//! Barttorvik data client and ingestion.
//!
//! Fetches player season stats (CSV) and per-game box scores (gzip JSON)
//! from barttorvik.com's public endpoints. No authentication required.

use flate2::read::GzDecoder;
use reqwest::Client;
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::io::Read;
use std::time::Duration;
use tracing::{info, warn};

/// How many times to attempt a Torvik fetch before giving up. Kept at 2 (one
/// retry) so a *full* barttorvik outage — where each attempt burns the whole
/// 120s request timeout — can't add more than ~one extra timeout per fetch to
/// the nightly. The failure we actually retry for (a mid-regeneration truncated
/// body) fails fast, not on timeout, so one retry after the backoff is enough.
const FETCH_MAX_ATTEMPTS: usize = 2;
/// Delay between Torvik fetch attempts. barttorvik regenerates its nightly data
/// files in a window of seconds; this backoff lets a run started mid-regeneration
/// retry onto the finished file instead of degrading the day.
const FETCH_BACKOFF: Duration = Duration::from_secs(30);

/// Is this error a 4xx — i.e. terminal, do not retry?
///
/// A 4xx (403 from the CloudFront edge, 401, 404) is not a transient hiccup:
/// retrying only hammers a host that has already refused us and wastes the 30s
/// backoff. Retry 5xx / connect / timeout only.
///
/// Two error shapes must both be recognised: a plain `reqwest::Error` (from
/// `error_for_status()` on paths that don't read the body) and our own
/// [`TorvikHttpError`] (from `edge_block_error`, which has to consume the
/// response to log the body). Missing the second would silently turn every 403
/// back into a 30s backoff — which is why this is a named function with its own
/// test rather than an inline expression.
///
/// Anything not carrying an HTTP status (connect, timeout, parse) is treated as
/// retryable, which is the pre-existing behaviour: the regeneration race this
/// retry exists for shows up as a malformed body, not a status. A non-2xx that is
/// neither 4xx nor 5xx (an unfollowed 3xx) also retries — it cannot occur in
/// practice, since reqwest follows redirects, and one wasted request is the
/// cheaper side to err on than mis-classifying a real 5xx as terminal.
fn is_client_error(e: &anyhow::Error) -> bool {
    e.downcast_ref::<reqwest::Error>()
        .and_then(reqwest::Error::status)
        .or_else(|| e.downcast_ref::<TorvikHttpError>().map(|t| t.status))
        .is_some_and(|s| s.is_client_error())
}

/// Retry an async Torvik fetch that may transiently fail. Retries on **any**
/// error — network *or* parse — because the regeneration race shows up as a
/// malformed body (truncated CSV, non-gzip bytes, HTML error page), not a
/// transport error, so retrying only on `reqwest` errors would miss the exact
/// failure mode we see. The final error is returned unchanged so the caller's
/// ledger/degraded-summary message is identical to the no-retry behaviour.
async fn with_retry<T, F, Fut>(what: &str, mut op: F) -> anyhow::Result<T>
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = anyhow::Result<T>>,
{
    let mut last_err: Option<anyhow::Error> = None;
    for attempt in 1..=FETCH_MAX_ATTEMPTS {
        match op().await {
            Ok(v) => return Ok(v),
            Err(e) => {
                let client_error = is_client_error(&e);
                warn!(
                    what,
                    attempt,
                    max = FETCH_MAX_ATTEMPTS,
                    error = %e,
                    client_error,
                    "Torvik fetch attempt failed"
                );
                last_err = Some(e);
                if client_error || attempt >= FETCH_MAX_ATTEMPTS {
                    break;
                }
                tokio::time::sleep(FETCH_BACKOFF).await;
            }
        }
    }
    Err(last_err.expect("loop runs at least once"))
}

/// Raw player season stats from the Torvik CSV endpoint.
///
/// `Default` exists so tests can build a row by naming only the handful of
/// fields under test (`ingest::torvik`'s linkage cases care about the name,
/// team, pid and minutes); it is never used on the parse path, which fills
/// every field positionally from the CSV.
#[derive(Debug, Clone, Default)]
pub struct TorkvikPlayerSeason {
    pub player_name: String,
    pub team: String,
    pub conf: String,
    pub gp: Option<i32>,
    pub min_per: Option<f64>,
    pub o_rtg: Option<f64>,
    pub usage: Option<f64>,
    pub effective_fg_pct: Option<f64>,
    pub true_shooting_pct: Option<f64>,
    pub orb_pct: Option<f64>,
    pub drb_pct: Option<f64>,
    pub ast_pct: Option<f64>,
    pub tov_pct: Option<f64>,
    pub ftm: Option<i32>,
    pub fta: Option<i32>,
    pub ft_pct: Option<f64>,
    pub two_pm: Option<i32>,
    pub two_pa: Option<i32>,
    pub two_p_pct: Option<f64>,
    pub tpm: Option<i32>,
    pub tpa: Option<i32>,
    pub tp_pct: Option<f64>,
    pub blk_pct: Option<f64>,
    pub stl_pct: Option<f64>,
    pub ft_rate: Option<f64>,
    pub class_year: Option<String>,
    pub height: Option<String>,
    pub jersey_number: Option<String>,
    pub porpag: Option<f64>,
    pub adj_oe: Option<f64>,
    pub personal_foul_rate: Option<f64>,
    pub year: Option<i32>,
    pub pid: Option<i32>,
    pub player_type: Option<String>,
    pub recruiting_rank: Option<f64>,
    pub ast_to_tov: Option<f64>,
    pub rim_made: Option<f64>,
    pub rim_attempted: Option<f64>,
    pub mid_made: Option<f64>,
    pub mid_attempted: Option<f64>,
    pub rim_pct: Option<f64>,
    pub mid_pct: Option<f64>,
    pub dunks_made: Option<f64>,
    pub dunks_attempted: Option<f64>,
    pub dunk_pct: Option<f64>,
    pub nba_pick: Option<f64>,
    pub d_rtg: Option<f64>,
    pub adj_de: Option<f64>,
    pub dporpag: Option<f64>,
    pub stops: Option<f64>,
    pub bpm: Option<f64>,
    pub obpm: Option<f64>,
    pub dbpm: Option<f64>,
    pub gbpm: Option<f64>,
    pub total_minutes: Option<f64>,
    pub ogbpm: Option<f64>,
    pub dgbpm: Option<f64>,
    pub oreb_pg: Option<f64>,
    pub dreb_pg: Option<f64>,
    pub treb_pg: Option<f64>,
    pub ast_pg: Option<f64>,
    pub stl_pg: Option<f64>,
    pub blk_pg: Option<f64>,
    pub ppg: Option<f64>,
}

/// Raw per-game player stats from the Torvik gzip JSON endpoint.
#[derive(Debug, Clone)]
pub struct TorkvikGameRow {
    pub date_str: String,
    pub opponent: String,
    pub game_uid: String,
    pub team: String,
    pub player_name: String,
    pub pid: Option<i32>,
    pub year: Option<i32>,
    pub location: Option<String>,
    pub class_year: Option<String>,
    pub height_inches: Option<i32>,
    // Box score
    pub minutes_pct: Option<f64>,
    pub o_rtg: Option<f64>,
    pub usage: Option<f64>,
    pub pts: Option<f64>,
    pub oreb: Option<f64>,
    pub dreb: Option<f64>,
    pub ast: Option<f64>,
    pub tov: Option<f64>,
    pub stl: Option<f64>,
    pub blk: Option<f64>,
    pub pf: Option<f64>,
    // Shooting
    pub two_pm: Option<i32>,
    pub two_pa: Option<i32>,
    pub tpm: Option<i32>,
    pub tpa: Option<i32>,
    pub ftm: Option<i32>,
    pub fta: Option<i32>,
    pub rim_made: Option<i32>,
    pub rim_attempted: Option<i32>,
    pub mid_made: Option<i32>,
    pub mid_attempted: Option<i32>,
    pub dunks_made: Option<i32>,
    pub dunks_attempted: Option<i32>,
    // Advanced
    pub bpm: Option<f64>,
    pub obpm: Option<f64>,
    pub dbpm: Option<f64>,
    pub possessions: Option<f64>,
}

/// Client for fetching data from barttorvik.com.
pub struct TorkvikClient {
    http: Client,
}

impl Default for TorkvikClient {
    fn default() -> Self {
        Self::new()
    }
}

/// A non-2xx Torvik response, carrying the status so [`with_retry`] can still
/// classify a 4xx as terminal *after* the body has been consumed for
/// diagnostics.
///
/// This exists only because reading the error body is destructive. The obvious
/// shape — log headers off a `&Response`, then `resp.error_for_status()?` — is
/// what the code did before, and it cannot see the body at all. Consuming the
/// response to read it means we no longer hand back a `reqwest::Error`, so the
/// 4xx short-circuit in `with_retry` (the guard that keeps us from hammering a
/// host that already refused us) has to recognise this type too.
///
/// `Display` deliberately reproduces reqwest's `error_for_status()` wording, so
/// the `ingest_runs` error column and the Slack degraded line read exactly as
/// they did before this type existed.
#[derive(Debug)]
struct TorvikHttpError {
    status: reqwest::StatusCode,
    url: String,
}

impl std::fmt::Display for TorvikHttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let kind = if self.status.is_client_error() {
            "client"
        } else if self.status.is_server_error() {
            "server"
        } else {
            // Only reachable for a non-2xx that is neither 4xx nor 5xx — in
            // practice an unfollowed 3xx, which means Torvik changed where a file
            // lives. reqwest never produces this (`error_for_status` fires only on
            // 4xx/5xx) so there is no wording to match, and calling it "server"
            // would send the next reader hunting for an outage that isn't there.
            "unexpected"
        };
        write!(
            f,
            "HTTP status {} error ({}) for url ({})",
            kind, self.status, self.url
        )
    }
}

impl std::error::Error for TorvikHttpError {}

/// How much of a non-2xx body to log. CloudFront's deny page is ~1KB of HTML;
/// the distinguishing sentence is at the top.
const BODY_SNIPPET_CHARS: usize = 240;

/// Collapse a response body to one loggable line: whitespace runs (HTML is
/// newline-heavy) become single spaces, then truncate.
fn body_snippet(body: &str) -> String {
    let flat = body.split_whitespace().collect::<Vec<_>>().join(" ");
    match flat.char_indices().nth(BODY_SNIPPET_CHARS) {
        Some((idx, _)) => format!("{}…", &flat[..idx]),
        None => flat,
    }
}

/// Consume a non-2xx Torvik response, log the edge diagnostics, and return a
/// status-carrying error.
///
/// barttorvik is fronted by **AWS CloudFront** (not Cloudflare — the previous
/// version of this function logged `cf-ray`/`cf-mitigated`, which are
/// structurally always absent here and told us nothing for months). On a deny
/// the edge answers with `server: CloudFront` and never reaches Bart's origin
/// Apache; a served response carries `server: Apache` plus `via: … CloudFront`.
/// So `server` alone distinguishes "edge refused us" from "origin erred", and
/// `x-amz-cf-id` is the request identifier Bart or AWS can look up — the
/// `cf-ray` analogue. The body is what separates a WAF/IP rule from a geo
/// restriction, which headers alone cannot.
async fn edge_block_error(resp: reqwest::Response, what: &str) -> anyhow::Error {
    let status = resp.status();
    let url = resp.url().to_string();
    // Own the header values before `text()` consumes the response.
    let hdr = |name: &str| {
        resp.headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("-")
            .to_string()
    };
    let (amz_cf_id, amz_cf_pop, x_cache, via, server, retry_after) = (
        hdr("x-amz-cf-id"),
        hdr("x-amz-cf-pop"),
        hdr("x-cache"),
        hdr("via"),
        hdr("server"),
        hdr("retry-after"),
    );
    let body = resp.text().await.unwrap_or_default();
    warn!(
        what,
        status = %status,
        server = %server,
        x_amz_cf_id = %amz_cf_id,
        x_amz_cf_pop = %amz_cf_pop,
        x_cache = %x_cache,
        via = %via,
        retry_after = %retry_after,
        body = %body_snippet(&body),
        "Torvik non-2xx — CDN edge block diagnostics"
    );
    TorvikHttpError { status, url }.into()
}

impl TorkvikClient {
    pub fn new() -> Self {
        let mut builder = Client::builder()
            // Deliberately unattributed UA for now: the block we actually hit is
            // scoped to Google IP space (see below), which a UA change cannot
            // affect either way.
            //
            // TODO: switch to a contactable UA — this string plus a `(+URL)`
            // contact — if we ever need Bart to tell us apart from the abuse his
            // rule was aimed at. `tfs.rs` is likewise uncontactable, so there is
            // no attributed UA left in the tree to copy. Triggers: a
            // Railway-owned static egress IP starts getting refused, or we ask him
            // to allowlist ours. He reads his own access logs and invites contact
            // when a rule catches someone legitimate, so being identifiable is an
            // asset in that conversation, not a risk. Our nightly is 3 requests
            // total against his stated "once an hour is certainly fine".
            .user_agent("cstat/0.1")
            // Explicit timeouts so a stalled Torvik socket self-aborts instead
            // of hanging the nightly. The per-game gzip file is several MB
            // (~30s to fetch), so the request ceiling is looser than NatStat's;
            // a hard stall still aborts within 120s.
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(120));

        // Optional egress proxy — the escape hatch, not the primary fix.
        //
        // The 403s we saw were NOT a generic datacenter-IP block: AWS EC2 and
        // Railway's own ranges both served fine, and only a Google-owned egress
        // was refused. Bart blocked Google IP space to stop an abusive Apps
        // Script, and Google's published range list includes GCP *customer*
        // ranges, so a container Railway happened to place on GCP was collateral
        // damage. The fix is Railway's static outbound IPs (Railway-owned space,
        // so no provider lottery) — see `docs/torvik_egress_block.md`. This proxy
        // hook remains for the residual case: the static IPs are *shared* with
        // other Railway customers, so a co-tenant's abuse could still taint one.
        // Fail-soft: an unset var is a direct connection, an unparseable one logs
        // and falls back to direct rather than killing the client.
        if let Ok(url) = std::env::var("TORVIK_PROXY_URL")
            && !url.trim().is_empty()
        {
            match reqwest::Proxy::all(url.trim()) {
                Ok(proxy) => {
                    info!("Torvik client routing through TORVIK_PROXY_URL");
                    builder = builder.proxy(proxy);
                }
                Err(e) => {
                    warn!(error = %e, "invalid TORVIK_PROXY_URL; using a direct connection")
                }
            }
        }

        Self {
            http: builder.build().expect("failed to build HTTP client"),
        }
    }

    /// Lightweight reachability probe for the `preflight` health check. Issues a
    /// GET against a small known endpoint (`coachdict.json`) and checks the
    /// status without downloading/parsing the whole body — enough to confirm the
    /// host is up and serving 2xx before the nightly commits to the big fetches.
    pub async fn probe(&self) -> anyhow::Result<()> {
        let url = "https://barttorvik.com/coachdict.json";
        let resp = self.http.get(url).send().await?;
        let status = resp.status();
        if status.is_success() {
            Ok(())
        } else {
            // Surface the edge diagnostics — this is the path that fires first
            // when the egress IP is refused, so it is usually the only place the
            // block is described. The returned error is dropped on purpose:
            // preflight reports its own shorter wording, which `FeedHealth::Down`
            // carries into the ledger and the Slack alert.
            let _ = edge_block_error(resp, "probe").await;
            anyhow::bail!("Torvik returned HTTP {}", status.as_u16())
        }
    }

    /// Fetch player season stats CSV for a given year. Retries a transient
    /// barttorvik hiccup (see [`with_retry`]).
    pub async fn fetch_player_stats(&self, year: i32) -> anyhow::Result<Vec<TorkvikPlayerSeason>> {
        let players = with_retry("player_stats", || async move {
            let url = format!("https://barttorvik.com/getadvstats.php?year={year}&csv=1");
            info!(year, "fetching Torvik player stats");
            // Guard the HTTP status BEFORE reading the body as CSV: reqwest's
            // send() only errors on transport failures, not HTTP 4xx/5xx, so a
            // barttorvik error page (HTML) would otherwise flow into
            // parse_player_csv, get skipped row-by-row (< 64 cols), and return
            // Ok(vec![]) — a silent empty "success". `edge_block_error` turns it
            // into a status-carrying Err and logs the CloudFront diagnostics.
            let resp = self.http.get(&url).send().await?;
            if !resp.status().is_success() {
                return Err(edge_block_error(resp, "player_stats").await);
            }
            let body = resp.text().await?;
            let players = parse_player_csv(&body)?;
            info!(year, count = players.len(), "parsed Torvik player stats");
            Ok(players)
        })
        .await?;

        // Year guard runs *after* the retry loop: a wrong-season payload is
        // deterministic (barttorvik keeps serving the same fallback), so retrying
        // it would only waste attempts. A transient network/parse hiccup still
        // retries inside `with_retry` above.
        validate_requested_year(&players, year)?;
        Ok(players)
    }

    /// Fetch per-game player stats (gzip JSON) for a given year. Retries a
    /// transient barttorvik hiccup (see [`with_retry`]).
    pub async fn fetch_game_stats(&self, year: i32) -> anyhow::Result<Vec<TorkvikGameRow>> {
        with_retry("game_stats", || async move {
            let url = format!("https://barttorvik.com/{year}_all_advgames.json.gz");
            info!(year, "fetching Torvik game stats (gzip)");
            // See fetch_player_stats: guard the HTTP status before reading the
            // body so a 4xx/5xx page becomes a classified Err rather than a
            // confusing gzip/JSON parse error, and capture the edge diagnostics.
            let resp = self.http.get(&url).send().await?;
            if !resp.status().is_success() {
                return Err(edge_block_error(resp, "game_stats").await);
            }
            let bytes = resp.bytes().await?;

            // The server may send Content-Encoding: gzip (auto-decompressed by reqwest)
            // or raw gzip bytes. Try parsing as JSON first, fall back to gzip decompress.
            let json_str = match serde_json::from_slice::<Vec<Vec<Value>>>(&bytes) {
                Ok(rows) => {
                    let games: Vec<TorkvikGameRow> =
                        rows.iter().filter_map(|r| parse_game_row(r)).collect();
                    info!(year, count = games.len(), "parsed Torvik game stats");
                    return Ok(games);
                }
                Err(_) => {
                    let mut decoder = GzDecoder::new(&bytes[..]);
                    let mut s = String::new();
                    decoder.read_to_string(&mut s)?;
                    s
                }
            };

            let rows: Vec<Vec<Value>> = serde_json::from_str(&json_str)?;
            let games: Vec<TorkvikGameRow> =
                rows.iter().filter_map(|r| parse_game_row(r)).collect();
            info!(year, count = games.len(), "parsed Torvik game stats");
            Ok(games)
        })
        .await
    }

    /// Fetch the head-coach dictionary: every season in one file, mapping
    /// team name → head coach. Shape: `{"2026": {"Duke": "Jon Scheyer", ...}}`.
    /// No `{year}` param — the endpoint returns all seasons (1893→present).
    /// Returns year → (team name → coach), with non-numeric year keys skipped.
    pub async fn fetch_coachdict(&self) -> anyhow::Result<BTreeMap<i32, HashMap<String, String>>> {
        let url = "https://barttorvik.com/coachdict.json";
        info!("fetching Torvik coach dictionary");
        // Status-guard before decoding, same as the two season fetches: this is
        // the *same file* `probe()` uses, so it is refused first when the egress
        // IP is blocked. Without the guard a 403 surfaces as a JSON decode error
        // ("expected value at line 1 column 1") with none of the edge
        // diagnostics — which is exactly the misdiagnosis this change removes
        // everywhere else.
        let resp = self.http.get(url).send().await?;
        if !resp.status().is_success() {
            return Err(edge_block_error(resp, "coachdict").await);
        }
        // Coach values are tolerated as `Option<String>` so a stray null
        // anywhere in the 130+ years of history can't fail the whole
        // (current-season) ingest — null/missing coaches are simply dropped.
        let raw: HashMap<String, HashMap<String, Option<String>>> = resp.json().await?;
        let mut out: BTreeMap<i32, HashMap<String, String>> = BTreeMap::new();
        for (year, teams) in raw {
            if let Ok(y) = year.parse::<i32>() {
                let cleaned = teams
                    .into_iter()
                    .filter_map(|(team, coach)| coach.map(|c| (team, c)))
                    .collect();
                out.insert(y, cleaned);
            }
        }
        info!(seasons = out.len(), "parsed Torvik coach dictionary");
        Ok(out)
    }
}

// ---------------------------------------------------------------------------
// CSV parsing (headerless, positional columns)
// ---------------------------------------------------------------------------

fn parse_player_csv(body: &str) -> anyhow::Result<Vec<TorkvikPlayerSeason>> {
    let mut rdr = csv::ReaderBuilder::new()
        .has_headers(false)
        .from_reader(body.as_bytes());
    let mut players = Vec::new();
    let mut schema_checked = false;

    for result in rdr.records() {
        let rec = result?;
        if rec.len() < 64 {
            continue;
        }
        // Schema guard: the CSV is headerless, so a column Bart inserts or
        // reorders would silently misalign every downstream field. Assert the
        // *shape* of a handful of load-bearing columns on the first data row and
        // fail loudly instead of writing garbage. Runs once (all rows share a
        // layout).
        if !schema_checked {
            validate_player_csv_schema(&rec)?;
            schema_checked = true;
        }
        players.push(TorkvikPlayerSeason {
            player_name: rec.get(0).unwrap_or("").to_string(),
            team: rec.get(1).unwrap_or("").to_string(),
            conf: rec.get(2).unwrap_or("").to_string(),
            gp: parse_int(&rec, 3),
            min_per: parse_f64(&rec, 4),
            o_rtg: parse_f64(&rec, 5),
            usage: parse_f64(&rec, 6),
            effective_fg_pct: parse_f64(&rec, 7),
            true_shooting_pct: parse_f64(&rec, 8),
            orb_pct: parse_f64(&rec, 9),
            drb_pct: parse_f64(&rec, 10),
            ast_pct: parse_f64(&rec, 11),
            tov_pct: parse_f64(&rec, 12),
            ftm: parse_int(&rec, 13),
            fta: parse_int(&rec, 14),
            ft_pct: parse_f64(&rec, 15),
            two_pm: parse_int(&rec, 16),
            two_pa: parse_int(&rec, 17),
            two_p_pct: parse_f64(&rec, 18),
            tpm: parse_int(&rec, 19),
            tpa: parse_int(&rec, 20),
            tp_pct: parse_f64(&rec, 21),
            blk_pct: parse_f64(&rec, 22),
            stl_pct: parse_f64(&rec, 23),
            ft_rate: parse_f64(&rec, 24),
            class_year: non_empty(&rec, 25),
            height: non_empty(&rec, 26),
            jersey_number: non_empty(&rec, 27),
            porpag: parse_f64(&rec, 28),
            adj_oe: parse_f64(&rec, 29),
            personal_foul_rate: parse_f64(&rec, 30),
            year: parse_int(&rec, 31),
            pid: parse_int(&rec, 32),
            player_type: non_empty(&rec, 33),
            recruiting_rank: parse_f64(&rec, 34),
            ast_to_tov: parse_f64(&rec, 35),
            rim_made: parse_f64(&rec, 36),
            rim_attempted: parse_f64(&rec, 37),
            mid_made: parse_f64(&rec, 38),
            mid_attempted: parse_f64(&rec, 39),
            rim_pct: parse_f64(&rec, 40),
            mid_pct: parse_f64(&rec, 41),
            dunks_made: parse_f64(&rec, 42),
            dunks_attempted: parse_f64(&rec, 43),
            dunk_pct: parse_f64(&rec, 44),
            nba_pick: parse_f64(&rec, 45),
            d_rtg: parse_f64(&rec, 46),
            adj_de: parse_f64(&rec, 47),
            dporpag: parse_f64(&rec, 48),
            stops: parse_f64(&rec, 49),
            bpm: parse_f64(&rec, 50),
            obpm: parse_f64(&rec, 51),
            dbpm: parse_f64(&rec, 52),
            gbpm: parse_f64(&rec, 53),
            total_minutes: parse_f64(&rec, 54),
            ogbpm: parse_f64(&rec, 55),
            dgbpm: parse_f64(&rec, 56),
            oreb_pg: parse_f64(&rec, 57),
            dreb_pg: parse_f64(&rec, 58),
            treb_pg: parse_f64(&rec, 59),
            ast_pg: parse_f64(&rec, 60),
            stl_pg: parse_f64(&rec, 61),
            blk_pg: parse_f64(&rec, 62),
            ppg: parse_f64(&rec, 63),
        });
    }
    Ok(players)
}

/// Sanity-check that the positional Torvik player CSV still matches the column
/// map `parse_player_csv` relies on. Because the feed is headerless we assert the
/// *type shape* of a few load-bearing columns on the first data row.
///
/// Two signal tiers, tuned to catch a column insert/reorder without false-firing
/// on a stray value (the existing per-field parse already tolerates non-numerics
/// by yielding `None`, so the guard must be at least as forgiving):
/// - **Strong:** a text column (name/team/conf) that is a *bare number*. A real
///   player name / team / conference is never purely numeric, so even one is drift.
/// - **Weak:** a numeric column (gp/usage/pid/gbpm) holding non-numeric text. A
///   lone one could be a sentinel (`"N/A"`), so we only treat **two or more** as
///   drift — a genuine shift misaligns all of them at once.
///
/// Empty cells are tolerated throughout — a legitimate row can have blank optional
/// fields.
fn validate_player_csv_schema(rec: &csv::StringRecord) -> anyhow::Result<()> {
    let cell = |idx: usize| rec.get(idx).map(str::trim).filter(|s| !s.is_empty());
    let is_pure_number = |idx: usize| cell(idx).is_some_and(|s| s.parse::<f64>().is_ok());
    let is_nonnumeric = |idx: usize| cell(idx).is_some_and(|s| s.parse::<f64>().is_err());

    // Strong: a numeric value in a text slot — one is enough.
    let mut violations: Vec<String> = [(0usize, "player_name"), (1, "team"), (2, "conf")]
        .iter()
        .filter(|(i, _)| is_pure_number(*i))
        .map(|(i, n)| format!("col {i} ({n}) is numeric"))
        .collect();
    let text_drift = !violations.is_empty();

    // Weak: non-numeric text in a numeric slot — needs two-plus to count.
    let num_violations: Vec<String> = [(3usize, "gp"), (6, "usage"), (32, "pid"), (53, "gbpm")]
        .iter()
        .filter(|(i, _)| is_nonnumeric(*i))
        .map(|(i, n)| format!("col {i} ({n}) is non-numeric"))
        .collect();
    let num_drift = num_violations.len() >= 2;
    violations.extend(num_violations);

    if text_drift || num_drift {
        let sample: Vec<&str> = rec.iter().take(8).collect();
        anyhow::bail!(
            "Torvik player CSV schema drift — the positional column map is stale \
             (did barttorvik add/reorder a column?). Violations: {}. \
             Refusing to write misaligned rows. First 8 cells: {sample:?}",
            violations.join(", ")
        );
    }
    Ok(())
}

/// Guard against barttorvik's silent future-year fallback. `getadvstats.php?year=N`
/// returns HTTP 200 with the *latest available* season's rows when season N hasn't
/// started yet — e.g. a 2027 request today serves the byte-identical 2026 file
/// (verified 2026-07-16). The [`validate_player_csv_schema`] guard only checks
/// column *shape*, so it waves this through: the rows are structurally valid, just
/// for the wrong season. Compare the rows' embedded year (col 31, parsed into
/// [`TorkvikPlayerSeason::year`]) against what was requested and refuse a mismatch,
/// so an early-bootstrap `torvik --year 2027` can't quietly persist last season's
/// players stamped as this one. (The per-game path `{year}_all_advgames.json.gz`
/// 404s on a not-yet-started year, so it fails loudly on its own and needs no guard.)
fn validate_requested_year(players: &[TorkvikPlayerSeason], requested: i32) -> anyhow::Result<()> {
    // Modal year among the rows that carry one. A single-season CSV is
    // homogeneous, but a stray null/parse-miss shouldn't get a vote.
    let mut counts: HashMap<i32, usize> = HashMap::new();
    for p in players {
        if let Some(y) = p.year {
            *counts.entry(y).or_default() += 1;
        }
    }
    let Some((&modal, &modal_n)) = counts.iter().max_by_key(|(_, n)| **n) else {
        // No row carried a year (empty or degenerate fetch) — nothing to
        // contradict the request; that case surfaces elsewhere.
        return Ok(());
    };
    if modal != requested {
        anyhow::bail!(
            "Torvik player CSV year mismatch — requested {requested} but the feed returned \
             season {modal} data ({modal_n} of {} rows). barttorvik silently serves the latest \
             available season for a not-yet-started year; refusing to persist {modal} players \
             stamped as {requested}.",
            players.len()
        );
    }
    Ok(())
}

fn parse_f64(rec: &csv::StringRecord, idx: usize) -> Option<f64> {
    rec.get(idx)?.trim().parse().ok()
}

fn parse_int(rec: &csv::StringRecord, idx: usize) -> Option<i32> {
    rec.get(idx)?.trim().parse::<f64>().ok().map(|v| v as i32)
}

fn non_empty(rec: &csv::StringRecord, idx: usize) -> Option<String> {
    let s = rec.get(idx)?.trim();
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

// ---------------------------------------------------------------------------
// Gzip JSON parsing (array of arrays, positional)
// ---------------------------------------------------------------------------

fn parse_game_row(row: &[Value]) -> Option<TorkvikGameRow> {
    if row.len() < 53 {
        return None;
    }
    Some(TorkvikGameRow {
        date_str: val_str(row, 0)?,
        opponent: val_str(row, 5)?,
        game_uid: val_str(row, 6)?,
        team: val_str(row, 47)?,
        player_name: val_str(row, 48)?,
        pid: val_i32(row, 51),
        year: val_i32(row, 52),
        location: val_str_opt(row, 46),
        class_year: val_str_opt(row, 50),
        height_inches: val_i32(row, 49),
        minutes_pct: val_f64(row, 8),
        o_rtg: val_f64(row, 9),
        usage: val_f64(row, 10),
        pts: val_f64(row, 33),
        oreb: val_f64(row, 34),
        dreb: val_f64(row, 35),
        ast: val_f64(row, 36),
        tov: val_f64(row, 37),
        stl: val_f64(row, 38),
        blk: val_f64(row, 39),
        pf: val_f64(row, 42),
        two_pm: val_i32(row, 23),
        two_pa: val_i32(row, 24),
        tpm: val_i32(row, 25),
        tpa: val_i32(row, 26),
        ftm: val_i32(row, 27),
        fta: val_i32(row, 28),
        rim_made: val_i32(row, 19),
        rim_attempted: val_i32(row, 20),
        mid_made: val_i32(row, 21),
        mid_attempted: val_i32(row, 22),
        dunks_made: val_i32(row, 17),
        dunks_attempted: val_i32(row, 18),
        bpm: val_f64(row, 44),
        obpm: val_f64(row, 30),
        dbpm: val_f64(row, 31),
        possessions: val_f64(row, 43),
    })
}

fn val_str(row: &[Value], idx: usize) -> Option<String> {
    match &row[idx] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn val_str_opt(row: &[Value], idx: usize) -> Option<String> {
    match &row[idx] {
        Value::String(s) if !s.is_empty() => Some(s.clone()),
        _ => None,
    }
}

fn val_f64(row: &[Value], idx: usize) -> Option<f64> {
    match &row[idx] {
        Value::Number(n) => n.as_f64(),
        Value::String(s) => s.trim().parse().ok(),
        _ => None,
    }
}

fn val_i32(row: &[Value], idx: usize) -> Option<i32> {
    match &row[idx] {
        Value::Number(n) => n.as_f64().map(|v| v as i32),
        Value::String(s) => s.trim().parse::<f64>().ok().map(|v| v as i32),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // -- CSV parsing --------------------------------------------------------

    #[test]
    fn parse_csv_valid_row() {
        // Build a 64-column CSV row
        let mut cols = vec![""; 64];
        cols[0] = "Cooper Flagg";
        cols[1] = "Duke";
        cols[2] = "ACC";
        cols[3] = "35"; // gp
        cols[4] = "32.5"; // min_per
        cols[5] = "118.2"; // o_rtg
        cols[6] = "28.1"; // usage
        cols[25] = "Fr"; // class_year
        cols[26] = "6-9"; // height
        cols[53] = "8.7"; // gbpm
        cols[63] = "18.4"; // ppg
        let csv_line = cols.join(",");

        let players = parse_player_csv(&csv_line).unwrap();
        assert_eq!(players.len(), 1);
        let p = &players[0];
        assert_eq!(p.player_name, "Cooper Flagg");
        assert_eq!(p.team, "Duke");
        assert_eq!(p.conf, "ACC");
        assert_eq!(p.gp, Some(35));
        assert_eq!(p.min_per, Some(32.5));
        assert_eq!(p.o_rtg, Some(118.2));
        assert_eq!(p.usage, Some(28.1));
        assert_eq!(p.class_year.as_deref(), Some("Fr"));
        assert_eq!(p.height.as_deref(), Some("6-9"));
        assert_eq!(p.gbpm, Some(8.7));
        assert_eq!(p.ppg, Some(18.4));
    }

    #[test]
    fn parse_csv_skips_short_rows() {
        let csv = "a,b,c\n"; // only 3 columns
        let players = parse_player_csv(csv).unwrap();
        assert!(players.is_empty());
    }

    // -- Requested-year guard ----------------------------------------------

    /// Build a minimal valid player row stamped with `year` in col 31.
    fn row_for_year(name: &str, year: i32) -> String {
        let mut cols = vec![String::new(); 64];
        cols[0] = name.to_string();
        cols[1] = "Duke".to_string();
        cols[2] = "ACC".to_string();
        cols[3] = "30".to_string(); // gp
        cols[31] = year.to_string(); // year
        cols[32] = "12345".to_string(); // pid
        cols.join(",")
    }

    #[test]
    fn year_guard_accepts_matching_season() {
        let csv = format!("{}\n{}", row_for_year("A", 2026), row_for_year("B", 2026));
        let players = parse_player_csv(&csv).unwrap();
        assert!(validate_requested_year(&players, 2026).is_ok());
    }

    #[test]
    fn year_guard_rejects_future_year_fallback() {
        // barttorvik serves 2026 rows for a 2027 request; the guard must catch it.
        let csv = format!("{}\n{}", row_for_year("A", 2026), row_for_year("B", 2026));
        let players = parse_player_csv(&csv).unwrap();
        let err = validate_requested_year(&players, 2027).unwrap_err();
        assert!(
            err.to_string().contains("year mismatch"),
            "expected a year-mismatch error, got: {err}"
        );
    }

    #[test]
    fn year_guard_ignores_empty_fetch() {
        // No rows → nothing to contradict the request; other checks handle empties.
        assert!(validate_requested_year(&[], 2027).is_ok());
    }

    #[test]
    fn parse_csv_rejects_shifted_schema() {
        // Simulate barttorvik inserting a leading column: every field shifts
        // right by one, so a numeric value (usage) lands in the player-name slot
        // and the real name lands in the team slot. The schema guard must reject.
        let mut cols = vec![""; 65];
        cols[0] = "28.1"; // a number where the name should be — the tell
        cols[1] = "Cooper Flagg";
        cols[2] = "Duke";
        cols[3] = "ACC";
        cols[4] = "35";
        let csv_line = cols.join(",");
        let err = parse_player_csv(&csv_line).unwrap_err();
        assert!(
            err.to_string().contains("schema drift"),
            "expected a schema-drift error, got: {err}"
        );
    }

    #[test]
    fn parse_csv_tolerates_lone_nonnumeric_sentinel() {
        // A stray non-numeric value in ONE numeric column (e.g. a "N/A" sentinel)
        // must not be mistaken for a column shift — the per-field parse already
        // tolerates it by yielding None, and the guard requires a numeric name/
        // team/conf or 2+ misaligned numeric columns before declaring drift.
        let mut cols = vec![""; 64];
        cols[0] = "Test Player";
        cols[1] = "Team";
        cols[2] = "Conf";
        cols[3] = "35"; // gp numeric
        cols[6] = "N/A"; // lone non-numeric in the usage slot — tolerated
        cols[53] = "8.7"; // gbpm numeric
        let csv_line = cols.join(",");
        let players = parse_player_csv(&csv_line).unwrap();
        assert_eq!(players.len(), 1);
        assert!(players[0].usage.is_none());
    }

    #[test]
    fn parse_csv_handles_empty_optional_fields() {
        let mut cols = vec![""; 64];
        cols[0] = "Test Player";
        cols[1] = "Team";
        cols[2] = "Conf";
        // All numeric fields left empty
        let csv_line = cols.join(",");
        let players = parse_player_csv(&csv_line).unwrap();
        assert_eq!(players.len(), 1);
        assert!(players[0].gp.is_none());
        assert!(players[0].ppg.is_none());
        assert!(players[0].class_year.is_none());
    }

    // -- JSON game row parsing ----------------------------------------------

    fn make_game_row() -> Vec<Value> {
        let mut row = vec![json!(null); 53];
        row[0] = json!("2026-01-15"); // date
        row[5] = json!("North Carolina"); // opponent
        row[6] = json!("20260115-duke-unc"); // game_uid
        row[8] = json!(78.5); // minutes_pct
        row[9] = json!(120.3); // o_rtg
        row[10] = json!(28.5); // usage
        row[17] = json!(2); // dunks_made
        row[18] = json!(3); // dunks_attempted
        row[19] = json!(4); // rim_made
        row[20] = json!(7); // rim_attempted
        row[21] = json!(1); // mid_made
        row[22] = json!(3); // mid_attempted
        row[23] = json!(6); // two_pm
        row[24] = json!(10); // two_pa
        row[25] = json!(3); // tpm
        row[26] = json!(7); // tpa
        row[27] = json!(4); // ftm
        row[28] = json!(5); // fta
        row[30] = json!(3.2); // obpm
        row[31] = json!(1.1); // dbpm
        row[33] = json!(22.0); // pts
        row[34] = json!(2.0); // oreb
        row[35] = json!(6.0); // dreb
        row[36] = json!(5.0); // ast
        row[37] = json!(3.0); // tov
        row[38] = json!(1.0); // stl
        row[39] = json!(2.0); // blk
        row[42] = json!(2.0); // pf
        row[43] = json!(65.0); // possessions
        row[44] = json!(4.5); // bpm
        row[46] = json!("H"); // location
        row[47] = json!("Duke"); // team
        row[48] = json!("Cooper Flagg"); // player_name
        row[49] = json!(81); // height_inches
        row[50] = json!("Fr"); // class_year
        row[51] = json!(12345); // pid
        row[52] = json!(2026); // year
        row
    }

    #[test]
    fn parse_game_row_valid() {
        let row = make_game_row();
        let g = parse_game_row(&row).unwrap();
        assert_eq!(g.date_str, "2026-01-15");
        assert_eq!(g.team, "Duke");
        assert_eq!(g.player_name, "Cooper Flagg");
        assert_eq!(g.opponent, "North Carolina");
        assert_eq!(g.pts, Some(22.0));
        assert_eq!(g.oreb, Some(2.0));
        assert_eq!(g.dreb, Some(6.0));
        assert_eq!(g.ast, Some(5.0));
        assert_eq!(g.tpm, Some(3));
        assert_eq!(g.tpa, Some(7));
        assert_eq!(g.ftm, Some(4));
        assert_eq!(g.fta, Some(5));
        assert_eq!(g.bpm, Some(4.5));
        assert_eq!(g.possessions, Some(65.0));
        assert_eq!(g.height_inches, Some(81));
        assert_eq!(g.class_year.as_deref(), Some("Fr"));
        assert_eq!(g.location.as_deref(), Some("H"));
        assert_eq!(g.pid, Some(12345));
        assert_eq!(g.year, Some(2026));
    }

    #[test]
    fn parse_game_row_too_short() {
        let row = vec![json!(null); 10];
        assert!(parse_game_row(&row).is_none());
    }

    #[test]
    fn parse_game_row_missing_required_string() {
        let mut row = make_game_row();
        row[0] = json!(null); // date is required
        assert!(parse_game_row(&row).is_none());
    }

    // -- Value helpers ------------------------------------------------------

    #[test]
    fn val_str_from_string() {
        let row = vec![json!("hello")];
        assert_eq!(val_str(&row, 0), Some("hello".to_string()));
    }

    #[test]
    fn val_str_from_number() {
        let row = vec![json!(42)];
        assert_eq!(val_str(&row, 0), Some("42".to_string()));
    }

    #[test]
    fn val_str_from_null() {
        let row = vec![json!(null)];
        assert_eq!(val_str(&row, 0), None);
    }

    #[test]
    fn val_str_from_empty_string() {
        let row = vec![json!("")];
        assert_eq!(val_str(&row, 0), None);
    }

    #[test]
    fn val_f64_from_number() {
        let row = vec![json!(12.5)];
        assert_eq!(val_f64(&row, 0), Some(12.5));
    }

    #[test]
    fn val_f64_from_string_number() {
        let row = vec![json!("7.25")];
        assert_eq!(val_f64(&row, 0), Some(7.25));
    }

    #[test]
    fn val_f64_from_null() {
        let row = vec![json!(null)];
        assert_eq!(val_f64(&row, 0), None);
    }

    #[test]
    fn val_i32_from_number() {
        let row = vec![json!(42)];
        assert_eq!(val_i32(&row, 0), Some(42));
    }

    #[test]
    fn val_i32_from_float() {
        let row = vec![json!(3.9)];
        assert_eq!(val_i32(&row, 0), Some(3)); // truncates
    }

    #[test]
    fn val_i32_from_string() {
        let row = vec![json!("7")];
        assert_eq!(val_i32(&row, 0), Some(7));
    }

    // -- retry policy -------------------------------------------------------

    fn http_err(code: u16) -> anyhow::Error {
        TorvikHttpError {
            status: reqwest::StatusCode::from_u16(code).unwrap(),
            url: "https://barttorvik.com/test".to_string(),
        }
        .into()
    }

    /// A 4xx is terminal — one attempt, no retry, no 30s backoff. This is the
    /// guard that keeps us from hammering a host that already refused us, so it
    /// runs offline and unconditionally: the previous version was `#[ignore]`d
    /// behind a real GET to barttorvik, which meant the guard was never actually
    /// verified in CI *and* checking it cost Bart a request.
    #[tokio::test]
    async fn with_retry_does_not_retry_client_errors() {
        use std::sync::atomic::{AtomicUsize, Ordering};
        let calls = AtomicUsize::new(0);
        let result: anyhow::Result<()> = with_retry("client_error", || {
            calls.fetch_add(1, Ordering::SeqCst);
            async { Err(http_err(403)) }
        })
        .await;
        assert!(result.is_err(), "a 403 must surface as an error");
        assert_eq!(calls.load(Ordering::SeqCst), 1, "a 4xx must not be retried");
    }

    /// The classifier, tested directly rather than through `with_retry` — the
    /// retryable paths would pay the real 30s backoff, and the point is that the
    /// 4xx short-circuit is *specific* rather than "never retries anything".
    #[test]
    fn classifier_recognises_both_error_shapes() {
        assert!(is_client_error(&http_err(403)), "403 is terminal");
        assert!(is_client_error(&http_err(404)), "404 is terminal");
        assert!(!is_client_error(&http_err(503)), "5xx stays retryable");
        assert!(
            !is_client_error(&anyhow::anyhow!("truncated CSV")),
            "a parse error carries no status and stays retryable — this is the \
             regeneration race the retry exists for"
        );
    }

    /// The ledger `error` column and the Slack degraded line are built from this
    /// text, so it must stay byte-identical to reqwest's `error_for_status()`
    /// wording that it replaced.
    #[test]
    fn torvik_http_error_matches_reqwest_wording() {
        assert_eq!(
            http_err(403).to_string(),
            "HTTP status client error (403 Forbidden) for url (https://barttorvik.com/test)"
        );
        assert_eq!(
            http_err(503).to_string(),
            "HTTP status server error (503 Service Unavailable) for url \
             (https://barttorvik.com/test)"
        );
        // Neither class: labelled honestly rather than blamed on the server.
        assert_eq!(
            http_err(301).to_string(),
            "HTTP status unexpected error (301 Moved Permanently) for url \
             (https://barttorvik.com/test)"
        );
    }

    #[test]
    fn body_snippet_flattens_and_truncates() {
        assert_eq!(
            body_snippet("<HTML>\n  <HEAD>\n<TITLE>403 Forbidden</TITLE>\n"),
            "<HTML> <HEAD> <TITLE>403 Forbidden</TITLE>"
        );
        let long = "x".repeat(BODY_SNIPPET_CHARS + 50);
        let out = body_snippet(&long);
        assert_eq!(
            out.chars().count(),
            BODY_SNIPPET_CHARS + 1,
            "truncated + ellipsis"
        );
        assert!(out.ends_with('…'));
        assert_eq!(body_snippet(""), "");
    }
}
