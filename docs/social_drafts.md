# Social drafts bot

`cstat-ingest social` turns the site's own data into tweet drafts and posts them to a Slack channel (`#social-drafts`) for a person to proofread. **It never posts to Twitter.** A human reads every draft, picks one or rewrites it, and tweets it.

## What it drafts

| Job (`--jobs`) | When | Kind / dedupe key | Image |
|---|---|---|---|
| `games-of-day` | From 10 AM ET, after the morning nightly | `games_of_day` and `game_of_day`, by date | matchup card for the best game |
| `lines` | From 9 AM ET, after the nightly | `lines` and `line_of_night`, by game date | radar card for the night's best line |
| `recaps` | From 9 AM ET, after the nightly | `recap`, by game id (up to 3 a night) | matchup card with the final |
| `news` | Every run, 8 AM ET to midnight | `news`, by NatStat story id (up to 3 a run) | radar card for the main player named, else the team card |

Each Slack message has the drafts in copyable code blocks with a character count, the card, links to the pages a reader would check, and the source article for a news item.

## How a draft is made

1. **Facts, deterministically.** The job gathers facts from the public API (`/api/schedule`, `/api/projections/{year}`, `/api/projected-players/{year}`), so a draft quotes the numbers a reader will find on the site, and from the database for what no endpoint serves (last night's top lines, `queries::get_top_performances`).
2. **Drafts from Claude.** The facts JSON and a short brief go to the Messages API (`social/claude.rs`), which returns three drafts with different angles as structured JSON. The standing instructions forbid any number not in the facts, competitor and data-provider names, and the internal `campom` vocabulary.
3. **Fact-check.** Every number in a draft is checked against the facts (`social/factcheck.rs`). A miss is flagged next to the draft in Slack, not dropped: it is a prompt to check one figure.
4. **Post and record.** Each card URL is fetched first and left off with a note if it does not return an image (Slack rejects the whole message otherwise, which is what happens before the API carrying the card routes is deployed). The message goes to `#social-drafts` and a row is written to `social_drafts`. A Slack failure leaves no row, so the next hourly run tries again. A row is written only after a successful post, so the cron can run as often as it likes without double-posting.

With no `ANTHROPIC_API_KEY`, or with `--no-llm`, each item gets a plain template draft instead and everything else works the same.

## Upsets need the pregame number, captured before tipoff

`game_projections` cannot say what we expected before a finished game: the nightly rebuilds it for the whole season from aggregates that already include the result (migration 053). So the games-of-the-day job writes every game on the slate to **`pregame_snapshots`** the morning of the game, and recaps compare the result against that. A game with no snapshot (any night before this bot first ran) can still get a recap for being a big game, but never an upset call. The matchup card reads the same snapshot.

That table cannot be rebuilt after the fact, which is why it and `social_drafts` are in `sync_to_prod.sh`'s `EXCLUDED` list: a full sync would erase them.

## Cards

Served by the API at public URLs, because Slack fetches images itself (an incoming webhook cannot upload files). 1200x675, Twitter's inline size. Source: `crates/cstat-api/src/routes/social_cards.rs`.

- `/og/radar/players/{id}.png?season=` is the player page's 8-axis radar, ported from `web/src/components/radarAxes.ts`; keep the two in step. It shows CAM with its tier, CAMO/CAMD and the archetype. Early in a season, when no one has percentiles yet, it falls back to last season and says so.
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

Each item is one Messages API call (`claude-opus-5-5` at medium effort by default; `SOCIAL_MODEL` overrides it), plus one news triage call per run with new headlines. A typical day is roughly 15 to 30 calls. The first live run (2026-11-02, games of the day) averaged about 1,800 input and 1,800 output tokens per item, the output including Claude's thinking: about $0.045 an item, or $20-45 a month. `social_drafts` logs input and output tokens per item, so the real spend is one query away:

```sql
SELECT date_trunc('day', created_at) AS day, model,
       sum(input_tokens) AS input, sum(output_tokens) AS output
FROM social_drafts GROUP BY 1, 2 ORDER BY 1 DESC;
```

## Known limits

- **News team tags are not used.** NatStat's `team-code` named the wrong team for 16 of 40 stories checked on 2026-11-01. The bot reads the team from the headline by our team names instead, and drops stories that name another sport. A headline that names no team (a league-wide piece) is skipped.
- **Ranks in the games-of-the-day facts are the Schedule tab's**, which blend the preseason projection early in the season, the same numbers the reader sees there.
- **Lines of the night use the game's calendar date**, not the Schedule tab's 6 AM-to-6 AM night, so a West Coast tip after midnight Eastern can land in the next day's list.
