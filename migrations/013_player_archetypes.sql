-- Player archetype clustering: assigns each qualified player-season a primary
-- "class" (Wizard, Sorcerer, etc.) and an affinity vector across all classes.
-- Populated by `training/archetypes.py` (k-means over rate stats + shot diet
-- + impact + minutes share). One row per (player_id, season).

CREATE TABLE IF NOT EXISTS player_archetypes (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    player_id UUID NOT NULL REFERENCES players(id) ON DELETE CASCADE,
    season INTEGER NOT NULL,

    cluster_id INTEGER NOT NULL,
    primary_class TEXT NOT NULL,
    secondary_class TEXT,
    primary_score DOUBLE PRECISION NOT NULL,
    secondary_score DOUBLE PRECISION,

    -- Affinity to every class (softmax over negative distance to each centroid).
    -- Shape: { "Wizard": 0.42, "Sorcerer": 0.18, ... }, sums to ~1.
    affinity_scores JSONB NOT NULL,

    -- Standardized (z-scored) feature vector used for clustering & similarity.
    -- Order matches `archetype_features.feature_names` JSONB metadata.
    feature_vector REAL[] NOT NULL,

    created_at TIMESTAMP NOT NULL DEFAULT now(),
    updated_at TIMESTAMP NOT NULL DEFAULT now(),

    UNIQUE (player_id, season)
);

CREATE INDEX IF NOT EXISTS idx_player_archetypes_player ON player_archetypes (player_id, season);
CREATE INDEX IF NOT EXISTS idx_player_archetypes_season ON player_archetypes (season);
CREATE INDEX IF NOT EXISTS idx_player_archetypes_class ON player_archetypes (season, primary_class);

-- Per-season metadata: feature names + cluster centroids + scaler params.
-- Lets the API compute distances on the fly without re-running clustering.
CREATE TABLE IF NOT EXISTS archetype_models (
    season INTEGER PRIMARY KEY,
    feature_names JSONB NOT NULL,
    -- Mapping from cluster_id (string key) to class name.
    cluster_to_class JSONB NOT NULL,
    -- Centroids in standardized space, indexed by cluster_id.
    centroids JSONB NOT NULL,
    -- Per-feature mean & std used for standardization.
    feature_means JSONB NOT NULL,
    feature_stds JSONB NOT NULL,
    n_qualified INTEGER NOT NULL,
    created_at TIMESTAMP NOT NULL DEFAULT now()
);
