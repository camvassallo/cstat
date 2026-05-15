# Player Archetypes: Methodology & Maintenance

Player archetypes cluster every qualified D-I player-season into one of 12 D&D-class profiles. The classification is descriptive, not predictive — it answers "what kind of player is this?" so the rest of the site (Identity/Gaps on team detail, Most Similar Players, archetype-filtered Players list, Compare-page chips) can lean on a stable taxonomy.

This doc describes the system, the stability story behind the current design, and the **playbook for what to check when retraining** as we add seasons. Pair with `training/archetypes.py` for the running implementation; signatures and class names live there.

## Pipeline at a glance

```
torvik_player_stats + player_season_stats (one row per player per season)
        │  (qualification: ≥10 GP, ≥10 MPG, complete shot-zone + GBPM data)
        ▼
fetch_player_features  →  12,617 player-seasons (across 2023–2026 at the time of writing)
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
verify_signature_alignment (hard fail if Hungarian put labels on wrong clusters)
        │
        ▼
Per-row affinity = softmax(−distance / 1.5) over all 12 clusters
        │
        ▼
player_archetypes (one row per player-season: primary, secondary, affinities, feature_vector)
archetype_models    (one row per season, all sharing centroids from this fit)
```

The 14 features fed to k-means: `rim_share`, `mid_share`, `three_share`, `ast_pct`, `tov_pct`, `usage_rate`, `orb_pct`, `drb_pct`, `stl_pct`, `blk_pct`, `ft_rate`, `ogbpm`, `dgbpm`, `min_share` (= `minutes_per_game / 40`).

**Run it:** `cd training && python -m archetypes --seasons 2023,2024,2025,2026 [--diagnostics]` — `training/` has no `__init__.py`, so the `training.archetypes` form fails; run from inside the dir. The signature-alignment guardrail blocks the DB write on any sign or ordering mismatch between cluster centroids and signatures; bypass with `--no-verify` only when intentionally rebalancing.

## Why combined-cohort training

The script clusters the **union of all configured seasons** in a single k-means fit and assigns every player-season against those shared centroids. This is the load-bearing design choice and the one most likely to be tempted into a "fix" by future-you.

The previous design clustered each season independently (`--season 2026`). Returning-player primary-class stability — measured by joining `player_archetypes` to itself via `torvik_player_stats.torvik_pid`, our stable cross-season ID — was **28%**. K-means redrew cluster boundaries each season, and Hungarian re-matched class names to whichever cluster scored best against each signature; small shifts in centroid position caused class labels to flip even when the underlying skill profile hadn't changed.

Combined-cohort training lifts returning-player primary stability to ~46–48% per adjacent pair (and "primary OR secondary class match" to ~75–80%) on the current 4-season cohort. Same player → same cluster → same class assignment, regardless of which season we look at.

**The trade-off:** combined-cohort doesn't capture genuine year-to-year evolution (rising 3PT volume, small-ball, rule changes). At a 2-3 season horizon that effect is tiny. At 4 seasons we've already seen one real example: when we expanded from 2025–2026 to 2023–2026, the Bard cluster shifted from "low-USG pass-first distributor" to "mid-major primary creator" and Monk shifted from "disciplined wing star" to "stretch-four / versatile forward." Both prose rewrites followed the data. At 5+ seasons era effects stop being tiny — see "Era horizon" below.

## Health metrics & retraining playbook

Run this checklist every time `--seasons` changes. The whole pass takes ~10 minutes.

### Step 1 — Run the training

```bash
cd training && python -m archetypes --seasons 2023,2024,2025,2026 --diagnostics 2>&1 | tee /tmp/archetypes-train.log
```

The `--diagnostics` flag prints per-cluster size, per-season size, and mean features per class in original (un-z-scored) units. Save the log; you'll diff against it next time.

The script's `verify_signature_alignment` guardrail fires automatically before any DB write. It checks each non-zero signature weight against the assigned cluster's centroid and hard-fails if:

- **SIGN:** a positive-weight feature lands on a cluster with a notably negative z (or vice versa) — "this cluster doesn't fit this description at all."
- **ORDER:** two classes both weight the same feature, but the cluster with the higher weight has the lower mean — Hungarian put similar clusters in swapped slots.

When the guardrail fires, the violation messages tell you which `(class, feature)` pairs disagree. Treat them as Case A symptoms (see decision tree below) unless you've intentionally rebalanced signatures. Bypass with `--no-verify` only when you've reviewed the diagnostics by hand and accept the assignment.

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
  COUNT(*) FILTER (WHERE season = 2023) AS y2023,
  COUNT(*) FILTER (WHERE season = 2024) AS y2024,
  COUNT(*) FILTER (WHERE season = 2025) AS y2025,
  COUNT(*) FILTER (WHERE season = 2026) AS y2026
  -- add a column per new season as the archive grows
FROM player_archetypes
GROUP BY primary_class
ORDER BY primary_class;
```

Tripwire: **any class outside [130, 500] members per season** usually means the signature is misaligned — Hungarian gave that class label to a cluster that's too small or too large to fit the description. The bound is empirical: at ~3,150 qualified player-seasons per season, perfectly even k=12 clusters would average ~260, and ranges around 0.5×–2× of that read sensibly. Paladin (which has historically clustered ~140 in early seasons) sits just below the floor and is fine — but any class drifting toward 600+ per season usually means a cluster identity it shouldn't.

Also watch for:
- **Two seasons split very differently** for the same class (e.g., 150 in one, 600 in another). Means the cluster-to-class mapping is fighting the data.
- **A class that disappears below ~100.** It's getting starved.

### Step 4 — Cluster identity vs description

Read the `--diagnostics` output's "Mean features per class (original units)" table. For each class, compare to its description in `web/src/pages/Archetypes.tsx::CLASS_DEFS` and `archetypeColors.ts::CLASS_TAGLINES`. The descriptions are the contract; if the data shifted, the descriptions need to follow.

A few examples of what "drift" looks like in practice (taken from the 2023→2026 expansion):

- **Bard's cluster identity flipped to "mid-major primary creator"** (high USG, heavy minutes, top players are the Antoine-Davis / Jacksen-Moni / Nick-Martinelli tier). The old "pass-first distributor / backup PG" prose moved to the Fighter slot. The prose followed the data and the signature dropped its `usage_rate: -0.5` weight.
- **Monk's cluster shifted to "versatile rotation forward" / stretch-4** (heights 79–82", balanced rim/three split, rotation minutes). Old "disciplined wing star" prose was rewritten.
- **Ranger lost its `stl_pct` weight** — adding 2023/2024 showed the cluster genuinely doesn't generate elite steals, just high 3PA share at low USG. The D&D "bow = ranged" framing held on `three_share` alone.

### Step 5 — Spot-check known stars

The clusters should classify obvious cases obviously. If a known elite big doesn't land in Druid, or a primary scorer isn't in Sorcerer/Wizard, the signature for that class probably needs a tweak. We use a small canonical list (extend as new seasons land):

- Cooper Flagg (2025), Cameron Boozer (2026), Zach Edey (2023/2024), Trayce Jackson-Davis (2023), Johni Broome (all seasons) → expect **Druid**
- Walter Clayton (2023/2025), Braden Smith (all seasons), Kam Jones (2025), AJ Dybantsa (2026), V.J. Edgecombe (2025), Mark Sears (2024) → expect **Wizard**
- Khaman Maluach (2025), Donovan Clingan (2023/2024), Liam Robbins (2023) → expect **Paladin**
- John Tonje (2025), Eric Dixon (2025), Terrence Shannon (2024), Darryn Peterson (2026), Richie Saunders (2025) → expect **Sorcerer**
- Koby Brea (2024), Jack Daugherty (2025) → expect **Warlock**
- Jaylen Clark (2023), Coleman Hawkins (2024), D'Moi Hodge (2023) → expect **Rogue**
- Antoine Davis (2023), Jacksen Moni (2025), Nick Martinelli (2026), Jordan Dingle (2023) → expect **Bard** (mid-major primary creators — NOT the old "backup PG" reading)

This isn't a regression test; it's a sanity check. Use it to surface drift, not gate deploys. The signature-alignment guardrail in Step 1 is the real gate.

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

Snapshot from the most recent retrain (2023–2026 combined cohort, 12,617 player-seasons). Update when retraining; this section drifts fastest.

### Class populations

| Class | n (2023) | n (2024) | n (2025) | n (2026) | Mean OGBPM | Mean DGBPM | Mean three_share |
|---|---:|---:|---:|---:|---:|---:|---:|
| Druid | 253 | 271 | 276 | 275 | +2.73 | +1.00 | 0.18 |
| Sorcerer | 378 | 380 | 395 | 416 | +2.01 | −0.21 | 0.53 |
| Wizard | 231 | 198 | 225 | 235 | +1.84 | +1.27 | 0.36 |
| Bard | 280 | 289 | 221 | 238 | +0.03 | −1.32 | 0.34 |
| Paladin | 139 | 160 | 183 | 189 | −0.23 | +2.32 | 0.06 |
| Warlock | 264 | 282 | 358 | 382 | −0.29 | −0.57 | 0.73 |
| Rogue | 206 | 217 | 255 | 238 | −0.57 | +2.04 | 0.42 |
| Monk | 283 | 312 | 319 | 357 | −0.60 | −0.24 | 0.43 |
| Barbarian | 208 | 226 | 240 | 267 | −2.00 | +0.07 | 0.07 |
| Cleric | 279 | 244 | 167 | 162 | −2.37 | −0.60 | 0.12 |
| Ranger | 333 | 328 | 303 | 295 | −3.05 | −1.19 | 0.52 |
| Fighter | 244 | 198 | 223 | 195 | −4.03 | −0.23 | 0.34 |

Paladin (139) drops a touch below the rule-of-thumb 150 floor in 2023 — not a problem; the cluster is well-formed and Paladin populations grow steadily as ingest improves.

### Stability

Per adjacent-season pair, returning players matched by `torvik_pid`:

| Pair | n returning | Primary stable | In primary OR secondary |
|---|---:|---:|---:|
| 2023 → 2024 | 1,829 | 46.9% | 79.8% |
| 2024 → 2025 | 1,778 | 48.5% | 78.3% |
| 2025 → 2026 | 1,626 | 44.3% | 75.2% |
| **Total** | **5,233** | **46.6%** | **77.9%** |

Compare to the original per-season-clustering baseline (v1): 28.1% primary stability. Combined-cohort training is doing what it's supposed to.

### Where to look for drift first

When updating this section after a retrain, the classes that have historically been most fragile (in order of how often we've had to touch them):

1. **Bard / Fighter** — the two "low-impact guard" clusters. Cluster identities shifted dramatically when we expanded from 2 seasons to 4 (Bard moved from "pass-first distributor" to "mid-major primary creator"; the original Bard prose moved to Fighter's slot). They sit close in feature space; small signature changes flip which gets which label.
2. **Monk** — drifted from "disciplined wing star" to "stretch-four / versatile forward" between 2-season and 4-season cohorts. Watch the height distribution and the rim/three split.
3. **Cleric / Ranger** — low-impact role clusters that Hungarian sometimes shuffles. Less volatile than the guard-tier pair above, but always check their signature alignments.

Druid, Wizard, Sorcerer, Paladin, Warlock, Rogue, Barbarian have been stable across every retrain — their signatures hit distinctive enough cluster shapes that Hungarian doesn't get confused.
