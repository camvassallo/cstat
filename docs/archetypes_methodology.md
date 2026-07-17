# Player Archetypes: Methodology & Maintenance

Player archetypes cluster every qualified D-I player-season into one of 12 D&D-class profiles. The classification is descriptive, not predictive — it answers "what kind of player is this?" so the rest of the site (Identity/Gaps on team detail, Most Similar Players, archetype-filtered Players list, Compare-page chips) can lean on a stable taxonomy.

This doc describes the system, the stability story behind the current design, and the **playbook for what to check when retraining** as we add seasons. Pair with `training/archetypes.py` for the running implementation; signatures and class names live there.

## Naming: "class" = "archetype"

**In the data layer, an archetype is called a `class`** — a D&D-metaphor holdover, since the 12 archetypes are named after D&D classes (Wizard, Rogue, Paladin, …). The DB columns are `player_archetypes.primary_class` / `secondary_class` (with `idx_player_archetypes_class`), and that name propagates through the Rust structs, the `/api/*` JSON keys, and the `web/src/api/client.ts` types as `primary_class` / `secondary_class`. **The user-facing UI says "Archetype"**, not "Class" (renamed 2026-07-15). The one genuine "Class" still shown to users is the player's class *year* (Fr/So/Jr/Sr — a different field entirely), plus the deliberately-D&D-themed "Class Quiz" mini-game. Renaming the data fields to `archetype` is tracked as a deferred cleanup in ROADMAP "Refactor Backlog" (it's a breaking API-contract change touching a migration, the ML pipeline, and the frontend types).

## Pipeline at a glance

```
torvik_player_stats + player_season_stats (one row per player per season)
        │  (qualification: ≥10 GP, ≥10 MPG, complete shot-zone + GBPM data)
        ▼
fetch_player_features  →  15,658 player-seasons (across 2022–2026 at the time of writing)
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

**Run it:** `cd training && python -m archetypes --seasons 2022,2023,2024,2025,2026 [--diagnostics]` — `training/` has no `__init__.py`, so the `training.archetypes` form fails; run from inside the dir. The signature-alignment guardrail blocks the DB write on any sign or ordering mismatch between cluster centroids and signatures; bypass with `--no-verify` only when intentionally rebalancing.

### Fit is Python and annual; *assign* is Rust and nightly

The diagram above is the **fit** — k-means, Hungarian matching, the signature guardrail. It stays in Python and runs **once a year** (a deliberate, diagnostics-reviewed operation; refitting in-season would churn every season's labels — see *Why combined-cohort training*). The fit is authoritative for `archetype_models` (the frozen centroids / scaler / cluster→class map).

The **assign** half — standardize a player's 14-feature vector against the frozen model, take the nearest centroid, map to its class, softmax the affinities — was ported to Rust (`cstat_core::compute::compute_archetypes`) and runs **every nightly** as `compute_all`'s last step (2026-07-17). It reads `player_season_stats` + `torvik_player_stats` season-to-date, so in-season labels refresh as each player's sample grows, instead of freezing at the last manual `python -m archetypes` push. This is what lets prod produce archetypes with no laptop (ROADMAP *Prod self-sufficiency*, S3/P1). The Rust assign is byte-exact with this Python writer — guarded by `crates/cstat-core/tests/archetype_assign_parity.rs`, which reproduces every stored row across all fitted seasons — so recomputing a season in Rust yields the same labels the annual Python fit-and-assign did. When no `archetype_models` row exists yet (a new season before its retrain) or nobody has cleared the ≥10 GP gate, the assign step no-ops and leaves `player_archetypes` untouched.

**Consequence for prod ownership:** with the nightly assigning archetypes, **prod now owns the daily `player_archetypes` write.** The only surviving laptop→prod archetype write is the **annual** `archetype_models` refit (pushed via `sync_to_prod.sh --tables archetype_models`). A `--tables player_archetypes` laptop push would be overwritten by the next nightly — which is correct.

## Why combined-cohort training

The script clusters the **union of all configured seasons** in a single k-means fit and assigns every player-season against those shared centroids. This is the load-bearing design choice and the one most likely to be tempted into a "fix" by future-you.

The previous design clustered each season independently (`--season 2026`). Returning-player primary-class stability — measured by joining `player_archetypes` to itself via `torvik_player_stats.torvik_pid`, our stable cross-season ID — was **28%**. K-means redrew cluster boundaries each season, and Hungarian re-matched class names to whichever cluster scored best against each signature; small shifts in centroid position caused class labels to flip even when the underlying skill profile hadn't changed.

Combined-cohort training lifts returning-player primary stability to ~46–48% per adjacent pair (and "primary OR secondary class match" to ~75–80%) on the 4-season cohort, and stays in that range on the current 5-season cohort (2022-2026, 15,658 player-seasons; measured pooled 47.1% primary / 78.2% primary-or-secondary across 7,054 returning pairs — see the stability table below). Same player → same cluster → same class assignment, regardless of which season we look at.

**The trade-off:** combined-cohort doesn't capture genuine year-to-year evolution (rising 3PT volume, small-ball, rule changes). At a 2-3 season horizon that effect is tiny. At 4 seasons we already saw it bite: expanding from 2025–2026 to 2023–2026 shifted the Bard cluster from "low-USG pass-first distributor" to "mid-major primary creator" (the old Bard prose migrated to Fighter's slot), and Monk from "disciplined wing star" to "stretch-four / versatile forward." Both prose rewrites followed the data. At 5 seasons (adding 2022) the Bard / Fighter pair drifted again *on the ast_pct axis* — Bard's cluster moved further from elite passing and Fighter's cluster picked up more of the high-AST mass, so the signature-alignment guardrail fired. The relaxation dropped `ast_pct` from both classes (no longer a clean separator) and Bard's prose was tweaked to drop the "high AST%" framing; Fighter's prose was already pass-first-distributor from the prior retrain, so it stayed. At 5+ seasons era effects stop being subtle — see "Era horizon" below.

## Health metrics & retraining playbook

Run this checklist every time `--seasons` changes. The whole pass takes ~10 minutes.

### Step 1 — Run the training

```bash
cd training && python -m archetypes --seasons 2022,2023,2024,2025,2026 --diagnostics 2>&1 | tee /tmp/archetypes-train.log
```

The `--diagnostics` flag prints per-cluster size, per-season size, and mean features per class in original (un-z-scored) units. Save the log; you'll diff against it next time.

The script's `verify_signature_alignment` guardrail fires automatically before any DB write. It checks each non-zero signature weight against the assigned cluster's centroid and hard-fails if:

- **SIGN:** a positive-weight feature lands on a cluster with a notably negative z (or vice versa) — "this cluster doesn't fit this description at all."
- **ORDER:** two classes both weight the same feature, but the cluster with the higher weight has the lower mean — Hungarian put similar clusters in swapped slots.

When the guardrail fires, the violation messages tell you which `(class, feature)` pairs disagree. Treat them as Case A symptoms (see decision tree below) unless you've intentionally rebalanced signatures. Bypass with `--no-verify` only when you've reviewed the diagnostics by hand and you accept the assignment.

### Step 2 — Returning-player stability

This is the canonical health metric. Tripwire: **< 40% means something destabilized.** (45-50% is the realistic ceiling for now; getting to 70%+ would require post-hoc rules we explicitly chose not to add — see "Anti-patterns" below.)

```sql
-- All adjacent pairs in one shot; one row per (prev_season → prev_season+1).
WITH archetype_with_pid AS (
  SELECT pa.season, pa.player_id, pa.primary_class, pa.secondary_class, t.torvik_pid
  FROM player_archetypes pa
  JOIN torvik_player_stats t ON t.player_id = pa.player_id AND t.season = pa.season
  WHERE t.torvik_pid IS NOT NULL
)
SELECT
  a.season AS prev_season,
  COUNT(*) AS n_returning,
  ROUND(100.0 * COUNT(*) FILTER (WHERE a.primary_class = b.primary_class) / COUNT(*), 1) AS pct_primary_stable,
  ROUND(100.0 * COUNT(*) FILTER (WHERE a.primary_class = b.primary_class
                                  OR a.primary_class = b.secondary_class
                                  OR a.secondary_class = b.primary_class) / COUNT(*), 1) AS pct_in_either
FROM archetype_with_pid a
JOIN archetype_with_pid b ON a.torvik_pid = b.torvik_pid AND b.season = a.season + 1
GROUP BY a.season
ORDER BY a.season;
```

If primary stability drops below 40%, **don't ship**. Investigate before retrying.

### Step 3 — Per-class population

```sql
SELECT primary_class,
  COUNT(*) FILTER (WHERE season = 2022) AS y2022,
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

- Cooper Flagg (2025), Cameron Boozer (2026), Zach Edey (2023/2024), Trayce Jackson-Davis (2023), Johni Broome (2023–2025) → expect **Druid**
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

**Fix:** Tweak the affected class's signature in `ARCHETYPE_SIGNATURES` (in `training/archetypes.py`). Add or strengthen weights on the dimensions that should distinguish it from neighboring clusters; add small *negative* weights on dimensions where the class shouldn't compete with another (e.g., Fighter's `min_share: -0.3, usage_rate: -0.3, ogbpm: -0.3` anchors it to the low-USG rotation-depth cluster instead of competing with mid-tier scoring clusters). The biggest recent example: forcing the Wizard label onto the elite-guard cluster required adding `ogbpm: 1.5, dgbpm: 0.5` to Wizard's signature so Hungarian wouldn't keep handing that cluster to Bard.

**After the fix:** Rerun the training; the signature-alignment guardrail (Step 1) will surface remaining sign/order mismatches. Beware of cascade effects: tweaking one signature can cause Hungarian to swap labels on *other* clusters too. Don't be surprised; just audit all 12 again and let the guardrail tell you when you're clean.

### B. A class's cluster identity genuinely shifted

**Symptoms:** Population is reasonable; spot-checks mostly land where expected; but the cluster's mean features no longer match the prose. The data moved without the description following.

**Fix:** Rewrite the class's prose in three places, in lockstep:

1. `web/src/components/archetypeColors.ts::CLASS_TAGLINES` — the one-liner.
2. `web/src/pages/Archetypes.tsx::CLASS_DEFS` — the long description, signature badges, and "Comparable" line.
3. `training/archetypes.py::ARCHETYPE_SIGNATURES` — the inline comment next to the signature dict (developer reference).

Don't touch the signature itself unless the prose-only fix doesn't cover it. In the 2023–2026 expansion we rewrote prose for Bard, Monk, Fighter, Cleric, Ranger, Rogue, Sorcerer, and Warlock — the cluster identities had shifted enough that the descriptions needed to catch up, but the signatures only needed small relaxations to pass the guardrail. The 2022 addition was milder: only Bard needed a prose tweak (dropped "high AST%" — see "Where to look for drift first" below).

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

## In-season stability: how many games until a label is trustworthy?

**Measured 2026-07-17.** Harness `training/experiment_archetype_stability.py`; summary artifact `training/eval_history/archetype_stability_20260717_summary.json`. Re-run after any retrain (~30s) — the curve is a property of the fitted model, not a constant.

**Three different things are called "stability" in this doc. Do not conflate them:**

| Metric | Question | Where |
|---|---|---|
| Returning-player stability | does a player keep his class *across seasons*? | Step 2 (~45–50%) |
| Cluster identity | does Hungarian put a class on the *same cluster* after a **refit**? | Steps 3–4 |
| **In-season stability** | does a label computed from **N games** match the *full-season* label? | this section |

They are unrelated. **Sorcerer is listed above as stable across every retrain, yet is the single *least* early-season-stable class** — fragility under refit and fragility under small samples have nothing to do with each other.

### Method

Assignment only, never a refit. Load the frozen combined-cohort model from `archetype_models` (`centroids`, `feature_means`, `feature_stds`, `cluster_to_class` — all 12 season rows share one centroid set). Rebuild each player's 15-feature vector from their **first N games only**: the `compute_player_season_stats` SQL (`compute.rs:905-1030`) with a game-rank filter for the rate half, `torvik_player_game_stats` for the Torvik half, and `ogbpm`/`dgbpm` as the possession-weighted mean of per-game `obpm`/`dbpm` (the method `compute_campom_at.py:57` uses). Standardize with the frozen scaler, take the nearest centroid, score against the player's actual full-season label.

**The control is the point.** At N = all-games the pipeline scores **93.2%**, not 100%. Substituting the *season* `ogbpm`/`dgbpm` for the reconstruction scores **exactly 100.0%** — which proves the rate-stat SQL and the assignment path are exact, and attributes the entire 6.8% gap to the point-in-time GBPM approximation (corr 0.93 / 0.96). **In production that error does not exist**: the nightly refreshes `torvik_player_stats` with true season-to-date values, so a real in-season assign reads the season column directly. The table below therefore *understates* what is achievable in-season. Read every row against 93.2%, not against 100%.

### Result — pooled 2022–2026 (~16k player-seasons)

| N games | primary match | top-2 match | % of ceiling |
|---|---|---|---|
| 1 | 30.7% | 49.1% | 32.9% |
| 3 | 42.7% | 63.5% | 45.8% |
| 5 | 52.4% | 73.6% | 56.2% |
| 8 | 61.3% | 82.4% | 65.8% |
| **10** *(the shipped gate)* | **65.7%** | 85.6% | 70.5% |
| 15 | 74.8% | 91.8% | 80.3% |
| 20 | 81.9% | 95.6% | 87.9% |
| ALL *(control)* | 93.2% | 99.3% | 100% |

**The curve never plateaus — it is still climbing steeply at N=20.**

Per-class recall at the gate (N=10, 2024–2026), worst → best: Sorcerer 56.2%, Monk 56.7%, Bard 59.8%, Ranger 61.4%, Druid 63.7%, Wizard 67.8%, Warlock 69.5%, Cleric 71.6%, Fighter 71.9%, Barbarian 72.7%, Rogue 74.4%, Paladin 77.7%. **Even the best class is only ~78% at the gate.**

### What this means for the >=10 GP / >=10 MPG gate

- **The gate is not a stability threshold.** It was inherited, never validated; this is the validation. At the gate the label is **65.7%** right — roughly one player in three is wrong the moment we first show them.
- **There is no better N to move it to.** Lowering to 5 gives 52.4%; raising to 20 still only reaches 81.9%. The archetype is *inherently a season-long-sample statistic* — it converges only as the sample becomes the season. Early-season noise is a property of the metric, not a tuning error.
- **Top-2 is the lever, not N.** 85.6% at the gate vs 65.7% for a bare primary. Showing primary+secondary, a provisional marker, or the `affinity_scores` already stored on `player_archetypes` is far more defensible early than a single hard label.
- **A prior-season label is worth ~5 games of current data.** Step 2's returning-player stability (~45–50% primary, ~65–76% either) is a dead heat with N=5 (52.4% / 73.6%). So the crossover is ~5–6 games: seed returners from last season for the first handful of games, then hand over to current-season assignment. Note this covers only returners (~1,650 of ~3,200 qualified) — freshmen have neither a seed nor a sample.

The in-season plan built on these numbers lives in ROADMAP Phase 6, *"Archetype in-season cold start"*.

## Era horizon: when combined-cohort breaks down

**UPDATE 2026-07-17 — the shipped model is now a 12-season fit, double this section's predicted failure point, and it did not fail.** `archetype_models` holds 12 season rows (2015–2026, **36,643 qualified player-seasons**, fit 2026-07-16) sharing one centroid set. Measured returning-player stability across all 11 adjacent pairs: 51.7, 54.3, 51.1, 49.9, 51.8, 48.0, 50.3, 50.4, 47.9, 47.8, **44.1** — mean ~49.8%, still inside the 45–50% band this doc calls the realistic ceiling, and every pair above the <40% don't-ship tripwire. **So the "fails at 6+ seasons" prediction below was too conservative** and should be read as a hypothesis that the data has now partly refuted, not a rule. Two caveats keep it alive rather than deleting it: the most recent pair (2025→2026) is the **lowest of all eleven** and the trend across pairs is gently downward, which is exactly the shape gradual era-dilution would take; and the *prose* below still correctly describes the mechanism. Watch the newest pair against the 40% tripwire on each retrain — that is the early-warning signal. **The "Current snapshot" section's 2022–2026 / 15,658-player-season figures are stale by a full retrain; treat the numbers here as authoritative until it is refreshed.**

The original reasoning, retained because the mechanism is sound even though the threshold was wrong: combined-cohort training was fine at the 5-season horizon (2022-2026 held the same ~47% returning-player stability the 4-season fit had). It was expected to start failing somewhere around **6+ seasons**, when era effects make players from different eras non-comparable on the same feature scale. We've already seen real cluster-identity drift between the 2-season fit and the 4-season fit (Bard and Monk both required prose rewrites), and the 5-season fit triggered another Bard/Fighter signature relaxation; the same forces compound as we extend backwards. The candidate triggers:

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
- The ~46% stability number we have is honest. Forcing it higher would just be lying.

If you want higher stability, fix the inputs (combined-cohort, signature tweaks) — not the outputs.

## Reference: current state

Snapshot from the most recent retrain (2022–2026 combined cohort, **15,658 player-seasons**, 2026-05-15). Update when retraining; this section drifts fastest.

### Class populations

| Class | n (2022) | n (2023) | n (2024) | n (2025) | n (2026) | Mean OGBPM | Mean DGBPM | Mean three_share |
|---|---:|---:|---:|---:|---:|---:|---:|---:|
| Druid | 242 | 249 | 260 | 263 | 261 | +2.83 | +0.92 | 0.18 |
| Sorcerer | 307 | 311 | 310 | 333 | 367 | +2.12 | +0.72 | 0.55 |
| Wizard | 242 | 262 | 234 | 245 | 251 | +1.98 | +0.53 | 0.35 |
| Bard | 335 | 341 | 354 | 308 | 306 | +0.12 | −1.56 | 0.41 |
| Paladin | 155 | 140 | 162 | 184 | 186 | −0.13 | +2.38 | 0.06 |
| Warlock | 298 | 293 | 307 | 374 | 383 | −0.38 | −0.87 | 0.72 |
| Monk | 298 | 290 | 323 | 321 | 362 | −0.42 | +0.14 | 0.44 |
| Rogue | 172 | 167 | 180 | 225 | 215 | −0.79 | +2.17 | 0.34 |
| Barbarian | 182 | 224 | 240 | 257 | 282 | −2.01 | +0.08 | 0.07 |
| Cleric | 289 | 280 | 245 | 164 | 168 | −2.27 | −0.61 | 0.12 |
| Ranger | 295 | 318 | 302 | 282 | 287 | −3.46 | −0.79 | 0.53 |
| Fighter | 226 | 223 | 188 | 209 | 181 | −3.73 | −0.57 | 0.33 |

Paladin (140 in 2023) dips just under the rule-of-thumb 150 floor — not a problem; the cluster is well-formed and Paladin populations grow steadily as ingest improves. Cleric drops markedly in 2025–2026 (164 / 168) vs the older seasons (~245+), reflecting genuine cluster drift toward Monk as the "versatile-rotation forward" framing absorbed more players.

### Stability

Per adjacent-season pair, returning players matched by `torvik_pid` — measured on the current 5-season fit (2026-05-15):

| Pair | n returning | Primary stable | In primary OR secondary |
|---|---:|---:|---:|
| 2022 → 2023 | 1,821 | 48.9% | 80.1% |
| 2023 → 2024 | 1,829 | 47.5% | 78.8% |
| 2024 → 2025 | 1,778 | 48.7% | 79.0% |
| 2025 → 2026 | 1,626 | 42.8% | 74.6% |
| **Total** | **7,054** | **47.1%** | **78.2%** |

Compare to the original per-season-clustering baseline (v1): 28.1% primary stability. Combined-cohort training is doing what it's supposed to. With 2022 added the cluster geometry shifted (Bard / Fighter prose updated, signature `ast_pct` weights dropped) but stability held; the load-bearing design — one k-means fit across the union — is unchanged.

### Where to look for drift first

When updating this section after a retrain, the classes that have historically been most fragile (in order of how often we've had to touch them):

1. **Bard / Fighter** — the two non-elite guard clusters (Bard is mid-USG mid-major leads; Fighter is low-USG rotation depth). Cluster identities shifted dramatically when we expanded from 2 seasons to 4 — Bard moved from "pass-first distributor" to "mid-major primary creator," and the original Bard prose effectively migrated to Fighter's slot. They sit close in feature space; small signature changes flip which gets which label.
2. **Monk** — drifted from "disciplined wing star" to "stretch-four / versatile forward" between 2-season and 4-season cohorts. Watch the height distribution and the rim/three split.
3. **Cleric / Ranger** — low-impact role clusters that Hungarian sometimes shuffles. Less volatile than the guard-tier pair above, but always check their signature alignments.

Druid, Wizard, Sorcerer, Paladin, Warlock, Rogue, Barbarian have been stable across every retrain — their signatures hit distinctive enough cluster shapes that Hungarian doesn't get confused.
