# CamPom: College Basketball Player Valuation System

## Purpose

CamPom is a player-level ranking system for college basketball, built on top of Barttorvik's publicly available advanced metrics. It is designed primarily for **transfer portal valuation** — answering the question: *how good would this player be if he transferred to a power conference program?*

The system takes raw per-game and rate statistics, adjusts for usage, minutes, sample size, and strength of schedule, and produces a single composite score that balances offensive production, defensive contribution, role size, and reliability of the underlying data.

---

## Input Data

The raw dataset is a player-level export from Barttorvik (typically ~5,000 players per season). Each row represents one player's full-season stats. The key input columns used in derived calculations are:

| Column | Description |
|--------|-------------|
| `GP` | Games played |
| `Min_per` | Percentage of team minutes played (0–100 scale) |
| `usg` | Usage rate — percentage of team possessions used while on floor |
| `mp` | Minutes per game |
| `obpm` | Offensive box plus/minus (Barttorvik) |
| `dbpm` | Defensive box plus/minus (Barttorvik) |
| `gbpm` | Game-based plus/minus (Barttorvik), equal to `ogbpm + dgbpm` |
| `ogbpm` | Offensive component of GBPM |
| `dgbpm` | Defensive component of GBPM |
| `conf` | Conference affiliation |

GBPM (game-based plus/minus) differs from traditional BPM in that it incorporates on/off court data and lineup-level information rather than relying solely on box score statistics. The offensive and defensive components (`ogbpm`, `dgbpm`) sum to `gbpm`.

---

## Derived Metrics

### Step 1: Usage-Adjusted GBPM (`adj_gbpm`)

**Formula:**

```
adj_gbpm = ogbpm × (usg / 17.87)^0.7 + dgbpm × (1 - 0.1 × (usg / 17.87))
```

**What it does:** Adjusts the offensive and defensive components of GBPM for a player's usage rate, using 17.87 as the reference point (approximately the population mean usage).

**Offensive adjustment — `(usg / 17.87)^0.7`:** This gives proportionally more credit to players who produce at high usage rates. The 0.7 exponent applies diminishing returns — going from 20% to 30% usage is harder than 10% to 20%, but the adjustment doesn't scale linearly either. A player at 30% usage gets a ~1.44x multiplier on their offensive GBPM; a player at 10% gets ~0.67x.

| Usage | Offensive Multiplier |
|-------|---------------------|
| 10 | 0.67x |
| 15 | 0.89x |
| 20 | 1.08x |
| 25 | 1.27x |
| 30 | 1.44x |
| 35 | 1.60x |

**Defensive adjustment — `(1 - 0.1 × (usg / 17.87))`:** This applies a mild downward pressure on defensive GBPM for high-usage players. The rationale is that defensive BPM metrics tend to be slightly inflated for high-usage players (partially through rebounding volume), so a small correction is applied. The range is narrow (0.97x at 5% usage to 0.78x at 40% usage), reflecting the fact that defensive metrics are less contaminated by usage than offensive metrics.

| Usage | Defensive Multiplier |
|-------|---------------------|
| 10 | 0.94x |
| 20 | 0.89x |
| 30 | 0.83x |

**Why GBPM over BPM?** GBPM uses lineup data and on/off information, making it less dependent on box score artifacts. The offensive/defensive split (`ogbpm`/`dgbpm`) is also more stable than the BPM split (`obpm`/`dbpm`), particularly for defensive evaluation. The system also computes `adj_bpm2` using the same formula structure applied to `obpm`/`dbpm` for comparison purposes.

---

### Step 2: Minutes Factors

Two separate minutes-based multipliers are computed. Both use a square root (^0.5) dampening to reward playing time without letting it dominate.

**`min_factor` — based on minutes per game:**

```
min_factor = (mp / mean_mp)^0.5
```

Where `mean_mp` is the dataset-wide average minutes per game (~17.2 for 2026). This measures raw playing time volume. A player averaging 35 MPG gets ~1.43x; a player averaging 5 MPG gets ~0.54x.

**`mp_factor` — based on percentage of team minutes:**

```
mp_factor = (Min_per / mean_Min_per)^0.5
```

Where `mean_Min_per` is the dataset-wide average of Min_per (~36.7 for 2026). This is the **pace-neutral** version — it measures what share of team minutes the player commanded, regardless of whether the team plays fast or slow. `mp_factor` is used as the primary minutes multiplier in the headline ranking (`cam_gbpm`) because it avoids tempo bias.

| Minutes Per Game | `min_factor` | Min% | `mp_factor` |
|-----------------|-------------|------|-------------|
| 10 | 0.76x | 20% | 0.74x |
| 20 | 1.08x | 50% | 1.17x |
| 30 | 1.32x | 75% | 1.43x |
| 35 | 1.43x | 90% | 1.57x |

The two factors are ~96.5% correlated but diverge meaningfully for tempo outliers. Players on fast-tempo teams (e.g., Arkansas, Louisville) will have higher `min_factor` relative to `mp_factor`; players on slow-tempo teams show the reverse.

---

### Step 3: GP Shrinkage (`gp_weight`)

**Formula:**

```
gp_weight = GP / (GP + 8)
```

**What it does:** Applies Bayesian-style shrinkage based on games played. This prevents small-sample players from dominating the rankings — raw GBPM variance is ~87 standard deviation units for 1-game players vs ~4 for 20+ game players, so the underlying signal is drastically noisier for low-GP players.

The constant `k = 8` means:
- At 1 GP: 11% weight (heavily regressed toward zero)
- At 8 GP: 50% weight
- At 20 GP: 71% weight
- At 30 GP: 79% weight
- At 35 GP: 81% weight

This eliminates the need for arbitrary GP cutoffs. A 3-game player with a monster line doesn't get filtered out entirely — they just get appropriately discounted.

---

### Step 4: Conference Strength of Schedule (`sos_adj`)

**Formula:**

```
conf_sos = mean adj_gbpm of conference (GP ≥ 20 players) - overall mean adj_gbpm
sos_adj = conf_sos × 0.5
adj_gbpm_sos = adj_gbpm + sos_adj
```

**What it does:** Adjusts for the quality of competition a player faced. A player putting up numbers in the SEC faced significantly tougher opponents than a player with identical raw stats in the SWAC. For transfer portal valuation specifically, this matters because we're projecting how a player would perform in a new environment.

**How it works:** Each conference's average `adj_gbpm` (among players with 20+ games) is computed, centered at zero (the overall mean). This conference quality score is then applied to every player in that conference at a 50% transfer rate — meaning half the conference quality gap is attributed to opponent quality effects on the player's stats.

The 50% rate is deliberately conservative. Not all of a conference quality gap translates directly to individual production when a player changes environments — scheme fit, role changes, and adjustment periods all reduce the transfer effect. For more aggressive portal valuation, this rate could be increased to 0.6; for more conservative projections that trust the raw numbers, 0.4.

**Sample conference adjustments (2026):**

| Conference | Raw SOS Factor | Applied Adjustment (×0.5) |
|------------|---------------|--------------------------|
| SEC | +3.92 | +1.96 |
| Big Ten | +3.76 | +1.88 |
| Big 12 | +3.56 | +1.78 |
| ACC | +3.21 | +1.60 |
| Big East | +3.08 | +1.54 |
| MWC | +1.57 | +0.78 |
| WCC | +1.20 | +0.60 |
| A-10 | +1.16 | +0.58 |
| WAC | −0.71 | −0.35 |
| SWAC | −2.68 | −1.34 |
| MEAC | −4.38 | −2.19 |

---

## Final Composite Metrics

Three tiers of ranking columns are produced, each building on the previous:

### Original (no GP or SOS adjustment)

```
cam_gbpm = adj_gbpm × mp_factor
min_adj_gbpm = adj_gbpm × min_factor
```

These are the pure rate-times-volume metrics. Useful for evaluating per-minute production quality, but vulnerable to small-sample noise and conference strength differences.

### v2: GP-Adjusted

```
cam_gbpm_v2 = adj_gbpm × mp_factor × gp_weight
min_adj_gbpm_v2 = adj_gbpm × min_factor × gp_weight
```

Adds sample size reliability weighting. This is the recommended column for **conference-neutral** analysis — comparing players within the same conference, or when you want to see raw production without SOS opinions baked in.

### v3: SOS + GP-Adjusted (Portal Valuation)

```
cam_gbpm_v3 = adj_gbpm_sos × mp_factor × gp_weight
min_adj_gbpm_v3 = adj_gbpm_sos × min_factor × gp_weight
```

The full model. The SOS adjustment is applied to `adj_gbpm` before the minutes and GP factors, so it scales with playing time. This is the recommended column for **transfer portal valuation** — projecting how a player's production would translate to a new conference.

---

## Supplementary Metrics

These are computed for reference and comparison but are not used in the headline rankings:

| Metric | Formula | Purpose |
|--------|---------|---------|
| `bpm_adj` | `(2 × usg - obpm - dbpm) × min_factor` | Measures gap between usage and BPM production, scaled by minutes. High values indicate a player whose usage exceeds their efficiency. |
| `adj_bpm2` | `obpm × (usg/17.87)^0.7 + dbpm × (1 - 0.1 × (usg/17.87))` | Same usage adjustment formula as `adj_gbpm`, but applied to box-score BPM instead of GBPM. Useful for comparison. |

---

## Reference Constants (2026 Season)

| Constant | Value | Source |
|----------|-------|--------|
| `mean_mp` | 17.22 | Dataset mean of minutes per game |
| `mean_Min_per` | 36.66 | Dataset mean of Min_per |
| `usg_ref` | 17.87 | Reference usage rate (~population mean) |
| `exponent` | 0.5 | Square root dampening on minutes factors |
| `gp_shrinkage_k` | 8 | Bayesian shrinkage constant for games played |
| `sos_transfer_rate` | 0.5 | Fraction of conference quality gap applied |

These constants should be recalculated each season using that season's dataset means.

---

## Interpretation Guide

**What does a cam_gbpm_v3 of 15 mean?** Roughly: this is a high-impact starter who, accounting for usage, minutes share, sample reliability, and conference difficulty, projects as a top-50ish player nationally in terms of total contribution.

**Typical ranges (2026):**

| Tier | cam_gbpm_v3 Range | Example Players |
|------|-------------------|-----------------|
| Elite | 20+ | Cameron Boozer |
| All-Conference | 15–20 | AJ Dybantsa, Yaxel Lendeborg, Zuby Ejiofor |
| Quality Starter | 10–15 | Braden Smith, Nick Martinelli, Aday Mara |
| Rotation Player | 5–10 | Solid contributors at power conference level |
| Replacement | 0–5 | Below-average contributors |
| Negative Value | < 0 | Actively hurting the team when on court |

---

## Limitations & Future Work

- **Conference SOS is coarse.** The adjustment is conference-level, not team-level or schedule-level. A team in a weak conference that played a tough non-conference schedule gets the same penalty as one that didn't.
- **No positional adjustment.** A 6'1" point guard and a 6'10" center with the same cam_gbpm_v3 are ranked equivalently, even though their transfer value may differ based on positional scarcity.
- **Single-season snapshot.** No multi-year regression or development trajectory modeling.
- **The SOS transfer rate (0.5) is an assumption**, not an empirically calibrated parameter. Calibrating it against historical portal transfer outcomes would strengthen the model.
- **Injury/absence context is invisible.** A player with 10 GP due to injury in an otherwise full season looks the same as a walk-on who played 10 games.
