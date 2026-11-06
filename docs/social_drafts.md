# Social drafts bot

`cstat-ingest social` turns the site's own data into tweet drafts and posts them to a Slack channel (`#social-drafts`) for a person to proofread. **It never posts to Twitter.** A human reads every draft, picks one or rewrites it, and tweets it.

## What it drafts

| Job (`--jobs`) | When | Kind / dedupe key | Image |
|---|---|---|---|
| `games-of-day` | From 10 AM ET, after the morning nightly | `games_of_day` and `game_of_day`, by date | matchup card for the best game |
| `lines` | From 9 AM ET, after the nightly | `lines` and `line_of_night`, by game date | radar card for the night's best line |
| `recaps` | From 9 AM ET, after the nightly | `recap`, by game id (up to 3 a night) | matchup card with the final |
| `news` | Every run, 8 AM ET to midnight | `news`, by NatStat story id (up to 3 a run) | radar card for the main player named, else the team card |

Each Slack message has the drafts in copyable code blocks with a character count and a source line, the research trail (which lookups Claude made), the card, links to the pages a reader would check, and the source article for a news item.

## How a draft is made

1. **Facts, deterministically.** The job gathers facts from the public API (`/api/schedule`, `/api/projections/{year}`, `/api/projected-players/{year}`), so a draft quotes the numbers a reader will find on the site, and from the database for what no endpoint serves (last night's top lines, `queries::get_top_performances`).
2. **Research, then drafts.** Claude gets the facts, a brief, and read-only research tools over the site's own API (`social/research.rs`): `player_profile` (season, percentiles, season highs, last ten games), `player_career`, `similar_players` (this year or any year), `team_profile` (ratings, every result against our pregame number, rotation, archetype mix), `team_lineups`, `team_coach`, `team_rankings`, `predict_matchup` (key factors, prior meetings, cross-era what-ifs), `conference_standings` and `search_players`. It digs the way a curious reader would (is this a career night or a habit, who does he play like, what does the model weigh here) and hands in three drafts through a strict `submit_drafts` tool, each with a `source` line naming where its numbers came from. Typical items use 3 to 10 tool calls (capped at 12). The standing instructions ask for a playful, specific voice, use the archetypes for flavor, and forbid any number Claude did not see in the facts or a tool result, competitor and data-provider names, and the internal `campom` vocabulary.
3. **Fact-check.** Every number in a draft is checked against the facts and every tool result Claude saw (`social/factcheck.rs`). A miss is flagged next to the draft in Slack, not dropped: it is a prompt to check one figure.
4. **Post and record.** Each card URL is fetched first and left off with a note if it does not return an image (Slack rejects the whole message otherwise, which is what happens before the API carrying the card routes is deployed). The message goes to `#social-drafts` and a row is written to `social_drafts`, so the cron can run as often as it likes without double-posting. If Slack refuses the post, the row is `slack_failed` with the finished message stored, and each later run resends it from storage (up to 6 attempts) instead of paying to draft it again. With no `SLACK_WEBHOOK_SOCIAL` the bot refuses to start, rather than drafting for nobody; `--dry-run` previews without one. 

With no `ANTHROPIC_API_KEY`, or with `--no-llm`, each item gets a plain template draft instead and everything else works the same.

## Upsets need the pregame number, captured before tipoff

`game_projections` cannot say what we expected before a finished game: the nightly rebuilds it for the whole season from aggregates that already include the result (migration 053). So the games-of-the-day job writes every game on the slate to **`pregame_snapshots`** the morning of the game, and recaps compare the result against that. A game with no snapshot (any night before this bot first ran) can still get a recap for being a big game, but never an upset call. The matchup card reads the same snapshot.

That table cannot be rebuilt after the fact, which is why it and `social_drafts` are in `sync_to_prod.sh`'s `EXCLUDED` list: a full sync would erase them.

## Cards

Served by the API at public URLs, because Slack fetches images itself (an incoming webhook cannot upload files). 1200x675, Twitter's inline size. Source: `crates/cstat-api/src/routes/social_cards.rs`.

- `/og/radar/players/{id}.png?season=` is the player page's 8-axis radar, ported from `web/src/components/radarAxes.ts`; keep the two in step. It shows CAM with its tier, CAMO/CAMD and the archetype. It uses the current season, or the year in `?season=`, and percentiles exist from a player's first game (`queries::percentile_min_games`), so an early-season card can rest on one game. A player with none in that season falls back to the season before and says so.
- `/og/matchup/{game_id}.png` shows records, our ranks and the win probability bar. Before tipoff, a game with no stored projection gets the preseason-only projection the Schedule tab shows. After the final it shows the score and the pregame probability from the snapshot, never a number made up afterwards.
- `/og/team/{id}.png?season=` shows our rank (or the preseason projection rank before games), and the top five players by CAM with archetypes, falling back to last season's roster.

Cards read only from the database by id, never from query text, so nobody can mint an official-looking card with numbers of their choosing. Anything unrenderable redirects to `/og-image.png`. The bot adds `?v=<date>` so the edge cache does not serve a pre-final card after the game.

## Running it

```bash
# See what it would post for a day, without posting or writing anything
cargo run --bin cstat-ingest -- social --jobs lines,recaps --date 2026-03-20 --dry-run
cargo run --bin cstat-ingest -- social --jobs games-of-day --date 2026-11-02 --dry-run --no-llm

# Post for real, ignoring the clock and what was already sent
cargo run --bin cstat-ingest -- social --jobs games-of-day --force
```

`--date` replays a day and implies the job is due. Locally the data comes from the public site unless `SOCIAL_API_BASE` points at a local API. **Local and prod mint different UUIDs**, so a local DB with prod's API (or the reverse) produces links and cards that 404 for anything the bot reads from the DB.

## Deploying (manual steps)

1. **Slack**: create `#social-drafts` and an incoming webhook for it (the same Slack app as the alert channels).
2. **Railway**: add a new service from this repo, set its config file path to **`/railway.social.json`** (hourly at :05, `cstat-ingest social`), and give it the same `DATABASE_URL` (the private, same-region URL) and `NATSTAT_API_KEY` as the nightly cron service, plus `SLACK_WEBHOOK_SOCIAL` and `ANTHROPIC_API_KEY`. Like the nightly it must exit; never schedule the API service. See `docs/deploy_nightly_cron.md` for the service-config details, which apply unchanged.
3. **API**: the card routes ship with the API image, so deploy the API before or with the cron service, or the first posts will show the fallback image.

Each job records a `social_*` step to `ingest_runs`. None is served-critical, so a failure never turns `/api/health/ingest` red.

## Cost

Each item is one research loop on `claude-opus-5-5` at medium effort (`SOCIAL_MODEL` overrides it), plus one news triage call per run with new headlines. Every round of a loop resends the conversation, but most of that is served from the prompt cache at a tenth of the input rate. Measured on the first live batch (2026-11-02, seven items): 16k to 68k input tokens per item (20 to 65% cache reads; a slate preview with 8 to 10 lookups is the expensive end) and 1.6k to 3.7k output, about **$0.14 an item** on average.

A typical in-season day is about 13 items (2 games of the day, 2 lines, up to 3 recaps, up to 6 news), so roughly $1.80 a day, or **about $55 a month**. News is the lever: `SOCIAL_NEWS_MAX_PER_DAY` (default 6) and `SOCIAL_NEWS_MAX_PER_RUN` (default 3). `claude-sonnet-5-5` costs half as much per token. `social_drafts` logs tokens per item:

```sql
SELECT date_trunc('day', created_at) AS day, model,
       sum(input_tokens) AS input, sum(output_tokens) AS output
FROM social_drafts GROUP BY 1, 2 ORDER BY 1 DESC;
```

## Known limits

- **News team tags are not used.** NatStat's `team-code` named the wrong team for 16 of 40 stories checked on 2026-11-01. The bot reads the team from the headline by our team names instead, and drops stories that name another sport. A headline that names no team (a league-wide piece) is skipped.
- **Ranks in the games-of-the-day facts are the Schedule tab's**, which blend the preseason projection early in the season, the same numbers the reader sees there.
- **Lines of the night use the game's calendar date**, not the Schedule tab's 6 AM-to-6 AM night, so a West Coast tip after midnight Eastern can land in the next day's list.
