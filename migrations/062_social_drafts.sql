-- The social-drafts bot (`cstat-ingest social`, docs/social_drafts.md): tweet
-- drafts it has posted to Slack for a human to proofread, and the pregame
-- numbers it saw before each game tipped.
--
-- Both are runtime-written on prod and excluded from `sync_to_prod.sh`, like
-- `ingest_runs`: a laptop push would erase the record of what was already
-- sent and the bot would post it again.

-- One row per thing the bot considered. `(kind, source_key)` is the dedupe
-- key (a date for the daily posts, a NatStat id for a headline), so a rerun or
-- an overlapping cron run never posts the same idea twice. A headline the
-- triage passed on is recorded too (`status = 'skipped'`) so it is not
-- reconsidered every hour. Status is `posted`, `skipped`, or `slack_failed`
-- (drafted but Slack refused it; retried from `slack_blocks`).
CREATE TABLE IF NOT EXISTS social_drafts (
    id            UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    kind          TEXT NOT NULL,
    source_key    TEXT NOT NULL,
    status        TEXT NOT NULL,
    -- What the drafter was shown and what it wrote, kept so a bad draft can
    -- be traced to its inputs.
    facts         JSONB,
    drafts        JSONB,
    image_urls    TEXT[] NOT NULL DEFAULT '{}',
    model         TEXT,
    -- The finished Slack message, kept when the post fails (`slack_failed`)
    -- so the next run resends it as is instead of paying to draft it again.
    slack_blocks  JSONB,
    post_attempts INTEGER NOT NULL DEFAULT 0,
    input_tokens  INTEGER,
    output_tokens INTEGER,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT now(),
    UNIQUE (kind, source_key)
);

-- The pregame projection for each game, captured the morning of the game.
--
-- `game_projections` cannot answer "what did we expect before tipoff" after
-- the fact: the nightly rewrites it for the whole season, and only its CamPom
-- input is point-in-time (migration 053), so a past game's row drifts toward
-- the result. An upset call made from it would partly be made from the score.
-- `upcoming_game_projections` holds the honest number only until the next
-- nightly moves the game on. This keeps it.
CREATE TABLE IF NOT EXISTS pregame_snapshots (
    game_id              UUID PRIMARY KEY REFERENCES games(id) ON DELETE CASCADE,
    home_team_id         UUID NOT NULL,
    away_team_id         UUID NOT NULL,
    home_win_prob        DOUBLE PRECISION NOT NULL,
    projected_margin     DOUBLE PRECISION NOT NULL,
    projected_home_score INTEGER,
    projected_away_score INTEGER,
    -- `model` or `preseason`, as the Schedule tab labels it.
    basis                TEXT NOT NULL,
    home_rank            INTEGER,
    away_rank            INTEGER,
    watchability         DOUBLE PRECISION,
    captured_at          TIMESTAMPTZ NOT NULL DEFAULT now()
);
