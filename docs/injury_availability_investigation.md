# Injury / availability in projections — investigation and verdict

**Status: investigation, not built. Recommendation is DO NOT build the
availability feature into the served models.** This note records the question,
the method, the numbers, and the three independent reasons the feature is not
worth shipping, so it does not get re-litigated. No behavior changes ship with
this note.

## The question

Should player injury / availability feed the game predictor and/or the
projection models? Two distinct sub-questions:

1. **Training / retrospective** — who actually played is directly observable
   (a `player_game_stats` row exists only if the player logged minutes), so no
   injury feed is needed to *label* absences. The real question is whether
   availability carries signal the models don't already capture.
2. **Inference / prospective** — to use availability live you need to know who
   *will* dress before tip, which requires an external injury feed.

The gate the user set: if there is no realistic data source, and/or the served
model already prices the effect, there is no point building it.

## Method

- Population: 2015–2026, ~60k team-games. Local DB.
- Player value: `torvik_player_stats.cam_gbpm_v3_psos` (the canonical CamPom
  value column), ranked within each team-season.
- "Absent" = **no real-minutes `player_game_stats` row** for that game
  (`minutes IS NULL OR minutes = 0` also counts as absent), restricted to the
  player's **active span** (first→last appearance for that team-season) so
  mid-year arrivals / departures / transfers are not mistaken for a missed game.
- "Fresh" absence = a missed game where the player **played the team's
  immediately-preceding game** (so rolling-form features still include him and
  Elo has not yet drifted — isolates the single-game discontinuity).
- Two lineup-blind baselines, i.e. models that do NOT know who dresses:
  - **Elo** — `game_forecasts.{home,away}_elo_before`. Blind to roster and
    schedule; knows only prior W/L. Expected margin fit from the data itself:
    `margin ≈ 0.0303·elo_diff + 2.80·home` (k = points per Elo point,
    home-court = 2.80). Residual = actual − expected.
  - **Pit ML model** — the served point-in-time predictor, via
    `GET /api/predict?...&as_of_date=<game_date>` per game (basis `pit`).

## Result 1 — the effect is real against a *weak* (Elo) baseline

Elo residual when a team's top-CamPom player is out, by value rank (negative =
team did worse than a roster/schedule-blind model expected):

| CamPom rank | Any-absence effect | Fresh single-game effect | Win rate present → absent |
|---|---|---|---|
| #1 (best) | −2.42 pts | **−2.28 pts** | 51.1% → 39.2% |
| #2 | −1.45 pts | −1.52 pts | 51.2% → 41.7% |
| #3 | −0.52 pts | −0.48 pts | 50.9% → 43.5% |
| #4 | −0.53 pts | — | 50.7% → 45.5% |
| #5 and below | ~0 / noise | — | ~flat |

Clean, monotonic in CamPom rank, and the fresh-absence cut matches the
any-absence number (so it is not Elo drift). **This is a genuine on-court effect
and, as a free by-product, it independently validates CamPom as a value metric.**
The value is concentrated in the top 1–2 players; a 6th man missing does nothing.

## Result 2 — the served pit model already absorbs it

The same games run through the actual pit ML predictor (pit-basis only, ~9k
games, absent-team perspective):

| group | n | avg pred | avg actual | mean resid | win rate |
|---|---|---|---|---|---|
| rank-1 present | 2431 | +0.26 | +0.87 | +0.61 | .516 |
| rank-1 ABSENT  | 1619 | −1.54 | −0.44 | +1.10 | .455 |
| rank-2 present | 2440 | +0.19 | +0.67 | +0.48 | .513 |
| rank-2 ABSENT  | 1966 | −1.56 | −0.15 | +1.41 | .463 |

Absence effect (absent resid − present resid): **rank-1 +0.48 pts (1.1σ),
rank-2 +0.93 pts (2.3σ)** — small, statistically weak, and the *wrong sign*
(absent teams very slightly *beat* the pit prediction). The −2.3 pt Elo effect
is gone.

**Mechanism.** On absence games the pit model — without knowing anyone is out —
already predicts the team **~1.8 pts lower** than on present-game controls
(`avg pred` −1.54 vs +0.26 for rank-1). It gets there because games where a
star sits skew toward tougher opponents and weaker team contexts (Elo saw the
same: average Elo gap −55 on absent games vs +7 present), and the pit features
(adjO/adjD, SOS, rolling form, opponent strength) capture that selection. The
team's *actual* decline is only ~1.3 pts (`avg actual` −0.44 vs +0.87), so the
pit model already docks these teams **more** than they actually fall off. There
is no residual margin error left for an `is_out` feature to correct.

Win rate does drop (~.52 → .46) because near-even games flip, but **margin** —
the training target and the served number — is already handled.

**Architectural corollary.** Even a perfect availability flag would barely move
the current feature vector. `features.rs::get_roster_agg_pit` builds
minutes-weighted *means* (`w_gbpm`, …) plus a "star" slot = the highest-*minutes*
player. Removing one player from an ~8-man weighted mean is a rounding error, and
the "star" slot does not change unless the highest-*minutes* player (not
necessarily the highest-CamPom player) is the one out. Capturing availability
would require a *value-weighted "minutes of CamPom lost tonight"* channel, i.e. a
feature redesign, not a flag.

## Result 3 — no viable data source, and no way to backtest

Vendor survey for a per-player NCAA MBB availability feed usable pre-tip:

| Source | Programmatic injuries? | Notes |
|---|---|---|
| ESPN hidden JSON API | No | CBB `/injuries` endpoint returns an empty array (works for NBA/NFL only) |
| CollegeBasketballData (cbbd) | No | ~22 endpoints, none for injuries/availability |
| Sportradar NCAAMB | No | NBA/WNBA packages include injuries; NCAAMB deliberately does not |
| SportsDataIO | Partial | Only real API carrying D-I injuries, but real-time is commercial/contact-sales; the ~$99 self-serve tier is **next-day** (useless pre-tip); current snapshot only |
| RotoWire | Licensed only | Best data/latency, but enterprise/contact-sales, no published price; scraping the public page **violates their ToS** |

**No vendor sells point-in-time historical injury status** (who was Out on a past
date). Consequence: the feature **cannot be backtested honestly** — it can only be
validated forward by self-snapshotting a paid feed daily and building an as-of
store. This also violates the standing "prod is self-sufficient, all inputs via
API" constraint (see `docs/` prod self-sufficiency notes): a scraped/licensed
injury feed is another laptop-or-vendor dependency.

## Verdict

Do not build. Three independent gates fail:

1. **Marginal value ≈ 0** — the served pit model already prices the effect
   through schedule/SOS/form features (Result 2).
2. **No obtainable source** — no free/cheap self-serve pre-tip feed exists;
   paid options are commercial-only (Result 3).
3. **Not backtestable** — no point-in-time history is purchasable, so the
   feature could never be validated against the past (Result 3).

More historical seasons would not change this: the sample is already saturated
(the effect vanishes because the model *absorbs* it, not for lack of data), and
going pre-2015 crosses the 2015-16 30-second-shot-clock scoring-regime break.

## What would re-open this

- A **value-weighted roster feature** (CamPom-minutes lost) that the current
  mean-based aggregate cannot express — worth exploring on its own merits
  (it also sharpens normal predictions), and a prerequisite before availability
  could add anything.
- A **cheap pre-tip programmatic feed with history** appearing on the market.
- Building a **forward-only as-of snapshot store** now (accept ~0 near-term
  value) purely to accumulate backtestable data for a future re-test.

## Reproduction

Method is self-contained in SQL + one predict loop; the working scratch files
were transient. Core shapes:

```sql
-- player value ranking per team-season
top_players AS (
  SELECT pss.player_id, pss.team_id, pss.season,
         ROW_NUMBER() OVER (PARTITION BY pss.team_id, pss.season
                            ORDER BY tps.cam_gbpm_v3_psos DESC) AS value_rank
  FROM player_season_stats pss
  JOIN torvik_player_stats tps
    ON tps.player_id = pss.player_id AND tps.season = pss.season
  WHERE pss.games_played >= 5 AND pss.minutes_per_game >= 10
    AND tps.cam_gbpm_v3_psos IS NOT NULL)

-- active span + appearances gate absence to in-span games; "fresh" = LAG(played)
-- over the player's team-games ordered by date. Elo baseline from game_forecasts;
-- pit baseline from GET /api/predict?...&as_of_date=<game_date> (basis 'pit'),
-- residual taken from the absent team's perspective.
```
