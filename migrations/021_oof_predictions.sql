-- Out-of-fold predictions from the trajectory and freshman-impact models.
-- Persisted at training time so historical-year API routes serve held-out
-- projections instead of in-sample inference (which the model effectively
-- memorized, inflating elite historical names by ~3-5 CamPom). See ROADMAP
-- §"Refactor Backlog > Serve held-out trajectory/freshman predictions for
-- historical years" for motivation.
--
-- Lifecycle: these tables are repopulated end-to-end on every retrain
-- (training/train_trajectory_model.py and train_freshman_model.py).
-- Forward-year cohorts (the year the model is meant to project INTO) have
-- no OOF rows; the API falls back to live inference for those. The boot
-- validator gates on `meta["oof_persisted"] == true` so a stale meta +
-- empty table can't silently regress to in-sample serving.
--
-- No FKs: torvik_pid isn't a PK anywhere (it's a column on
-- torvik_player_stats, duplicated across seasons), and the OOF rows are
-- essentially a regenerable cache of training-time predictions — point-in
-- -time integrity is the training pipeline's job, not the schema's.

-- Trajectory model: leave-one-pair-out predictions keyed by torvik_pid
-- (cross-season stable per memory; 96% coverage). target_season is the
-- season the model is projecting INTO (= source season + 1). One row per
-- (player, transition) the model trained on.
CREATE TABLE trajectory_oof_predictions (
    torvik_pid INTEGER NOT NULL,
    target_season INTEGER NOT NULL,
    mean REAL NOT NULL,
    lower REAL NOT NULL,
    upper REAL NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (torvik_pid, target_season)
);

-- Freshman impact model: leave-one-class-out predictions keyed by the
-- recruit's resolved cstat player_id. The freshman training query filters
-- `WHERE r.cstat_player_id IS NOT NULL`, so every persisted row has a
-- valid UUID. target_season is the recruit's first cstat season
-- (= recruit_year + 1).
CREATE TABLE freshman_oof_predictions (
    cstat_player_id UUID NOT NULL,
    target_season INTEGER NOT NULL,
    mean REAL NOT NULL,
    lower REAL NOT NULL,
    upper REAL NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    PRIMARY KEY (cstat_player_id, target_season)
);
