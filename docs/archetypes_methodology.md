# Player Archetypes: Methodology & Maintenance

Player archetypes cluster every qualified D-I player-season into one of 12 D&D-class profiles. The classification is descriptive, not predictive — it answers "what kind of player is this?" so the rest of the site (Identity/Gaps on team detail, Most Similar Players, archetype-filtered Players list, Compare-page chips) can lean on a stable taxonomy.

This doc describes the system, the stability story behind the current design, and the **playbook for what to check when retraining** as we add seasons. Pair with `training/archetypes.py` for the running implementation; signatures and class names live there.

## Pipeline at a glance

```
torvik_player_stats + player_season_stats (one row per player per season)
        │  (qualification: ≥10 GP, ≥10 MPG, complete shot-zone + GBPM data)
        ▼
fetch_player_features  →  6,289 player-seasons (across 2025+2026 at the time of writing)
        │
        ▼
StandardScaler (z-score features)
        │
        ▼
KMeans(k=12, random_state=42, n_init=20)  →  12 cluster centroids in z-space
        │
        ▼
Hungarian match centroids ↔ class signatures  →  cluster_id → class name
        │
        ▼
Per-row affinity = softmax(−distance / 1.5) over all 12 clusters
        │
        ▼
player_archetypes (one row per player-season: primary, secondary, affinities, feature_vector)
archetype_models    (one row per season, all sharing centroids from this fit)
```

The 14 features fed to k-means: `rim_share`, `mid_share`, `three_share`, `ast_pct`, `tov_pct`, `usage_rate`, `orb_pct`, `drb_pct`, `stl_pct`, `blk_pct`, `ft_rate`, `ogbpm`, `dgbpm`, `min_share` (= `minutes_per_game / 40`).

**Run it:** `python -m training.archetypes --seasons 2025,2026 [--diagnostics]`

## Why combined-cohort training

The script clusters the **union of all configured seasons** in a single k-means fit and assigns every player-season against those shared centroids. This is the load-bearing design choice and the one most likely to be tempted into a "fix" by future-you.

The previous design clustered each season independently (`--season 2026`). Returning-player primary-class stability — measured by joining `player_archetypes` to itself via `torvik_player_stats.torvik_pid`, our stable cross-season ID — was **28%**. K-means redrew cluster boundaries each season, and Hungarian re-matched class names to whichever cluster scored best against each signature; small shifts in centroid position caused class labels to flip even when the underlying skill profile hadn't changed.

Combined-cohort training lifted returning-player primary stability to **45.7%** (and "primary OR secondary class match" to **75.2%**). Same player → same cluster → same class assignment, regardless of which season we look at.

**The trade-off:** combined-cohort doesn't capture genuine year-to-year evolution (rising 3PT volume, small-ball, rule changes). At a 2-3 season horizon that effect is tiny. At 5+ seasons it stops being tiny — see "Era horizon" below.

## Health metrics & retraining playbook

Run this checklist every time `--seasons` changes. The whole pass takes ~10 minutes.

### Step 1 — Run the training

```bash
python -m training.archetypes --seasons 2024,2025,2026 --diagnostics 2>&1 | tee /tmp/archetypes-train.log
```

The `--diagnostics` flag prints per-cluster size, per-season size, and mean features per class in original (un-z-scored) units. Save the log; you'll diff against it next time.

### Step 2 — Returning-player stability

This is the canonical health metric. Tripwire: **< 40% means something destabilized.** (45-50% is the realistic ceiling for now; getting to 70%+ would require post-hoc rules we explicitly chose not to add — see "Anti-patterns" below.)

```sql
-- Run for each adjacent pair of seasons in the new training set.
WITH archetype_with_pid AS (
  SELECT pa.season, pa.player_id, pa.primary_class, pa.secondary_class,
         t.torvik_pid
  FROM player_archetypes pa
  JOIN torvik_player_stats t
    ON t.player_id = pa.player_id AND t.season = pa.season
),
paired AS (
  SELECT a.torvik_pid,
         a.primary_class AS prev, b.primary_class AS curr,
         a.secondary_class AS prev_sec, b.secondary_class AS curr_sec
  FROM archetype_with_pid a
  JOIN archetype_with_pid b ON a.torvik_pid = b.torvik_pid
  WHERE a.season = 2025 AND b.season = 2026   -- adjust per pair
)
SELECT COUNT(*) AS n_returning,
  ROUND(100.0 * COUNT(*) FILTER (WHERE prev = curr) / COUNT(*), 1) AS pct_primary_stable,
  ROUND(100.0 * COUNT(*) FILTER (WHERE prev = curr OR prev = curr_sec OR prev_sec = curr) / COUNT(*), 1) AS pct_in_either
FROM paired;
```

If primary stability drops below 40%, **don't ship**. Investigate before retrying.

### Step 3 — Per-class population

```sql
SELECT primary_class,
  COUNT(*) FILTER (WHERE season = 2025) AS y2025,
  COUNT(*) FILTER (WHERE season = 2026) AS y2026
  -- add a column per new season as the archive grows
FROM player_archetypes
GROUP BY primary_class
ORDER BY primary_class;
```

Tripwire: **any class outside [150, 800] members per season** usually means the signature is misaligned — Hungarian gave that class label to a cluster that's too small or too large to fit the description. The bound is empirical: at our current 6,289 player-season cohort, perfectly even k=12 clusters would average ~525, and ranges around ±40% of that have all read sensibly.

Also watch for:
- **Two seasons split very differently** for the same class (e.g., 150 in one, 600 in another). Means the cluster-to-class mapping is fighting the data.
- **A class that disappears below ~100.** It's getting starved.

### Step 4 — Cluster identity vs description

Read the `--diagnostics` output's "Mean features per class (original units)" table. For each class, compare to its description in `web/src/pages/Archetypes.tsx::CLASS_DEFS` and `archetypeColors.ts::CLASS_TAGLINES`. The descriptions are the contract; if the data shifted, the descriptions need to follow.

A few examples of what "drift" looks like in practice:

- **Bard's mean OGBPM = −3.77.** Description that says "elevates teammates" oversells. Update to "modest impact" / "low-impact distributor."
- **Ranger's mean DGBPM = −1.53.** Description that says "3-and-D wing" oversells defense. Update to "perimeter spacer."
- **Cleric's mean DGBPM = −0.53.** Description that says "glue defender / defensive intangibles" doesn't match. Update to "low-volume connector."

### Step 5 — Spot-check known stars

The clusters should classify obvious cases obviously. If a known elite big doesn't land in Druid, or a high-USG primary scorer isn't in Sorcerer, the signature for that class probably needs a tweak. We use a small canonical list (extend as new seasons land):

- Cooper Flagg (2025), Cameron Boozer (2026), Yaxel Lendeborg (both), Johni Broome (2025) → expect **Druid**
- AJ Dybantsa (2026), PJ Haggerty (both), Walter Clayton (2025) → expect **Wizard**
- Khaman Maluach, Aday Mara → expect **Paladin**
- John Tonje (2025), Eric Dixon (2025) → expect **Sorcerer**
- VJ Edgecombe (2025) → expect **Rogue**

This isn't a regression test; it's a sanity check. Use it to surface drift, not gate deploys.

## Decision tree when something drifts

After a retrain, the diagnostic + spot-check pass will surface one of three problems. Match the symptom to the fix.

### A. Hungarian put a class on the wrong cluster

**Symptoms:** Population for one class swings dramatically; spot-checks land in unexpected classes; the class's mean centroid features look nothing like the description.

**Fix:** Tweak the affected class's signature in `ARCHETYPE_SIGNATURES` (in `training/archetypes.py`). Add or strengthen weights on the dimensions that should distinguish it from neighboring clusters; add small *negative* weights on dimensions where the class shouldn't compete with another (e.g., Fighter's `min_share: 0.0, usage_rate: -0.3` keeps it out of the elite-wing cluster that should belong to Monk).

This is what we did for Fighter — its original "near zero everywhere" signature made it a residual sink that absorbed whatever the more-distinctive clusters didn't claim.

**After the fix:** Rerun the training, redo the full diagnostic pass. Beware of cascade effects: tweaking one signature can cause Hungarian to swap labels on *other* clusters too. We saw exactly this — when Fighter pulled away from the elite-wing cluster, Monk took its place and inherited that cluster's identity. Don't be surprised; just audit all 12 again.

### B. A class's cluster identity genuinely shifted

**Symptoms:** Population is reasonable; spot-checks mostly land where expected; but the cluster's mean features no longer match the prose. The data moved without the description following.

**Fix:** Rewrite the class's prose in three places, in lockstep:

1. `web/src/components/archetypeColors.ts::CLASS_TAGLINES` — the one-liner.
2. `web/src/pages/Archetypes.tsx::CLASS_DEFS` — the long description, signature badges, and "Comparable" line.
3. `training/archetypes.py::ARCHETYPE_SIGNATURES` — the inline comment next to the signature dict (developer reference).

Don't touch the signature itself unless the prose-only fix doesn't cover it. We did this for Bard, Ranger, Cleric, Barbarian, and Monk in the most recent retrain.

### C. A real new cluster emerged that no class fits

**Symptoms:** A cluster's centroid is genuinely distinct from any signature; spot-checks land in surprising classes; multiple classes are competing for the same cluster.

**Fix:** This is the only case where you should rename a class or add/remove one. Renaming is preferred over adding because:
- Color palette in `archetypeColors.ts::CLASS_COLORS` doesn't need a new entry.
- Deep-links like `/players?archetype=Wizard` keep working if you swap the class name in the same slot.
- The 12-class taxonomy is part of the product identity; growing it dilutes the D&D framing.

If you must add a class (because a true new cluster appeared and no existing class is a good rename target), update:
- `ARCHETYPE_SIGNATURES` (Python)
- `CLASS_COLORS`, `CLASS_TAGLINES` (frontend)
- `CLASS_DEFS` (frontend)
- The `K = len(CLASSES)` derivation handles the rest.
- Audit any frontend code that hardcodes a class list (grep for class names — there shouldn't be any beyond the dicts above).

## Era horizon: when combined-cohort breaks down

Combined-cohort training is fine at our current 2-3 season horizon. It will start to fail somewhere around **5+ seasons**, when era effects make players from different eras non-comparable on the same feature scale. The candidate triggers:

- **3PT volume.** D-I three-point attempt rate has risen ~50% over the last decade. A 2026 Warlock and a 2010 Warlock have completely different `three_share` distributions. Combined-cohort z-scoring would compress the modern signal.
- **Small-ball / positional fluidity.** Druid (positionless big) is a recent archetype; it didn't exist in the early 2010s in the same way. Forcing pre-small-ball seasons through the same clustering will dilute it.
- **Rule changes.** Shot clock, charge circle, freedom of movement — all reshape rate stats and scoring efficiency.

**When the archive crosses ~5 seasons, revisit** with one of these strategies (in order of complexity):

1. **Sliding window.** Cluster on the last 3 seasons only. Older seasons get classified against the most recent window's centroids (or are excluded from archetype assignments entirely). Simpler than era-aware; loses long-tail.
2. **Era-bucketed clustering.** Split seasons into eras (e.g., 2007-2014, 2015-2020, 2021+). Cluster each bucket independently with the same signatures. Returning players within an era stay stable; cross-era comparisons become explicitly era-tagged.
3. **Per-decade models.** One archetype model per decade. Most invasive; most expressive.

Whichever path, the schema already supports it: `archetype_models` is keyed by season, so different seasons can point at different fits without a migration. The signatures themselves likely don't need era-specific versions — the shapes of "elite scorer," "rim protector," etc. are stable; only the feature distributions shift.

Until that horizon is crossed, **stay on combined-cohort.** It's the simplest model that gets the stability gain we need.

## Anti-patterns we tried and rejected

Two ideas that look superficially appealing and are not worth re-litigating:

### Per-season clustering

What we shipped in v1. Each season got its own k-means fit and Hungarian assignment. Returning-player primary stability was **28%** because k-means redrew cluster boundaries each season independently. The fix (combined-cohort) costs us "year-to-year evolution sensitivity," which is the right trade at our horizon. Don't go back to this.

### Post-hoc lock-in for returning players

The idea: if a player was Druid in 2025, force them to stay Druid in 2026 unless their feature vector has moved by more than X standard deviations. Mechanically lifts returning-player stability toward 100%.

Why it's bad:
- Hides legitimate year-to-year evolution (a star going from Druid to Sorcerer because they took on more usage is real signal).
- Tunable threshold X is arbitrary and hard to debug.
- Brittle: a single feature outlier can flip a player's class either way regardless of the rest of the profile.
- The 45.7% stability number we have is honest. Forcing it higher would just be lying.

If you want higher stability, fix the inputs (combined-cohort, signature tweaks) — not the outputs.

## Reference: current state

Snapshot from the most recent retrain (2025+2026 combined). Update when retraining; this section drifts fastest.

### Class populations

| Class | n (2025) | n (2026) | Mean OGBPM | Mean DGBPM | Mean three_share |
|---|---:|---:|---:|---:|---:|
| Druid | 223 | 241 | +2.78 | +1.44 | 0.15 |
| Monk | 339 | 379 | +1.81 | −0.31 | 0.57 |
| Wizard | 234 | 288 | +1.53 | −0.03 | 0.36 |
| Sorcerer | 285 | 280 | +1.04 | −0.85 | 0.34 |
| Rogue | 211 | 207 | +0.45 | +2.26 | 0.37 |
| Fighter | 311 | 357 | −0.47 | +0.60 | 0.49 |
| Warlock | 335 | 348 | −0.52 | −0.71 | 0.74 |
| Paladin | 154 | 173 | −0.76 | +2.34 | 0.06 |
| Barbarian | 221 | 254 | −1.94 | −0.15 | 0.09 |
| Cleric | 202 | 203 | −2.72 | −0.53 | 0.15 |
| Ranger | 290 | 309 | −3.33 | −1.53 | 0.51 |
| Bard | 235 | 210 | −3.77 | −0.01 | 0.35 |

### Stability

| Metric | Per-season (v1) | Combined-cohort (current) |
|---|---:|---:|
| Returning players (torvik_pid joined) | 1,521 | 1,522 |
| Primary class stable | 28.1% | 45.7% |
| In primary OR secondary | 56.7% | 75.2% |

### Where to look for drift first

When updating this section after a retrain, the classes that have historically been most fragile (in order of how often we've had to touch them):

1. **Fighter** — defines itself by the negative space; any signature change anywhere can pull Fighter onto a different cluster.
2. **Monk** — competes with Fighter for "balanced" mid-tier identity; small shifts swap their assignments.
3. **Cleric / Bard / Ranger** — three "low-impact" clusters that Hungarian sometimes shuffles among themselves.

Druid, Wizard, Sorcerer, Paladin, Warlock, Rogue, Barbarian have been stable across every retrain — their signatures hit distinctive enough cluster shapes that Hungarian doesn't get confused.
