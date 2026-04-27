# CamPom: Player Valuation Methodology

CamPom is cstat's composite player-valuation metric. It refines Barttorvik's GBPM into a usage-, minutes-, sample-, and schedule-adjusted score that's contextualized by a player's role on their team. It's the canonical player ranking surfaced across the site (see ROADMAP §4f) and feeds the game-prediction model as a roster aggregate.

This document describes what the metric is, how it's calculated, and why each adjustment exists. Implementation lives (or will live, per Phase 4f) in `crates/cstat-core/src/compute.rs`.

## Data Source

All inputs come from the `torvik_player_stats` table (one row per player per season; populated by `cstat-ingest torvik --year YYYY`). The columns CamPom reads:

| CamPom term | `torvik_player_stats` column | Notes |
|-------------|-----------------------------|-------|
| GP | `games_played` | Games played |
| MP | `minutes_per_game` | Minutes per game |
| Min% | derived | Player share of team minutes (0–100). Derive as `total_minutes / (team_games × 40 × 5) × 100`, joining `team_season_stats` for team games. |
| USG | `usage_rate` | Usage rate (% of team possessions) |
| OGBPM | `ogbpm` | Offensive game-based plus/minus |
| DGBPM | `dgbpm` | Defensive game-based plus/minus |
| GBPM | `gbpm` | Total GBPM (= ogbpm + dgbpm) |
| OBPM / DBPM | `obpm` / `dbpm` | Box plus/minus components (used in the supplementary `bpm_adj`) |
| Conference | `conf` | Conference abbreviation |

A reference dataset (computed externally for 2026, all CamPom intermediates and finals included) is at `docs/campom_2026_baseline.csv` — used as the parity gate before iterating on the formulas.

### Why GBPM, not BPM

Barttorvik provides two separate plus/minus systems:

- **BPM** (`obpm`, `dbpm`, `bpm`) — derived entirely from the box score, same conceptual framework as Basketball Reference BPM.
- **GBPM** (`ogbpm`, `dgbpm`, `gbpm`) — incorporates on/off court data and lineup-level information beyond the box score.

GBPM is generally more reliable, especially for defense, because it captures contributions that don't show up in the box. CamPom uses GBPM as the primary input. BPM-based equivalents are computed for reference only (see `adj_bpm2` below).

## Season Constants

Three constants are computed per season from the full `torvik_player_stats` cohort, then held fixed for that season's calculations:

```
mean_mp      = AVG(minutes_per_game) across all players
mean_min_per = AVG(Min%)            across all players
usg_ref      = 17.87357708          # fixed across seasons; ≈ population mean usage
```

2026 values:

| Constant | Value |
|----------|-------|
| `mean_mp` | 17.2242 |
| `mean_min_per` | 36.6563 |
| `usg_ref` | 17.8736 |

## Step 1: Usage-Adjusted GBPM (`adj_gbpm`)

```
usg_ratio = USG / 17.8736

adj_gbpm = OGBPM × (usg_ratio ^ 0.7)
         + DGBPM × (1 − 0.1 × usg_ratio)
```

The core adjustment. Scales each GBPM component by how much of the offense the player is responsible for.

### Offensive side — `OGBPM × (USG / 17.8736)^0.7`

A player who produces 5.0 OGBPM at 30% usage is doing something harder than a player who produces 5.0 OGBPM at 12% usage — the first is creating offense on nearly a third of possessions; the second on a fraction. The 0.7 exponent applies diminishing returns so the adjustment isn't linear: 20→30 usage is worth more than 10→20, but the gap narrows.

| Usage | Multiplier on OGBPM |
|-------|---------------------|
| 10% | 0.665× |
| 15% | 0.893× |
| 17.9% (reference) | 1.000× |
| 20% | 1.077× |
| 25% | 1.266× |
| 30% | 1.441× |
| 35% | 1.604× |

### Defensive side — `DGBPM × (1 − 0.1 × USG / 17.8736)`

Defensive plus/minus metrics can be slightly inflated for high-usage players, primarily through rebounding volume. The correction is deliberately mild — even at 35% usage, the multiplier only drops to 0.80×. The belief baked in: defensive contributions are mostly real, but deserve a small haircut for high-usage players.

| Usage | Multiplier on DGBPM |
|-------|---------------------|
| 10% | 0.944× |
| 17.9% (reference) | 0.900× |
| 25% | 0.860× |
| 30% | 0.832× |
| 35% | 0.804× |

### Why not raw GBPM?

Raw GBPM treats a 12%-usage role player and a 30%-usage primary creator equivalently if their on/off numbers match. But the high-usage player is doing more — carrying a bigger load, creating more of the team's offense. Without the adjustment, role players and low-usage bigs who benefit from playing alongside good creators rank alongside genuine primary options.

### BPM equivalent (reference only)

```
adj_bpm2 = OBPM × (usg_ratio ^ 0.7) + DBPM × (1 − 0.1 × usg_ratio)
```

Same formula structure applied to box-score BPM. Useful for comparing how much the BPM and GBPM systems diverge for a given player.

## Step 2: Minutes Factors

Two volume factors. Both use a square-root exponent so that minutes matter but don't dominate.

### `min_factor` — raw minutes per game

```
min_factor = (MP / mean_mp) ^ 0.5
```

Directly reflects how much court time the player got per game.

| MPG | `min_factor` |
|-----|-------------|
| 5 | 0.539 |
| 10 | 0.762 |
| 15 | 0.933 |
| 20 | 1.078 |
| 25 | 1.205 |
| 30 | 1.320 |
| 35 | 1.426 |

### `mp_factor` — share of team minutes

```
mp_factor = (Min% / mean_min_per) ^ 0.5
```

The **pace-neutral** version. A player who plays 70% of available minutes gets the same `mp_factor` whether their team plays 65 or 75 possessions per game.

| Min% | `mp_factor` |
|------|------------|
| 20% | 0.739 |
| 40% | 1.045 |
| 60% | 1.279 |
| 80% | 1.477 |
| 90% | 1.567 |

### Why two factors?

They correlate at ~96.5% but diverge for tempo outliers. A player on a fast-tempo team might average 32 MPG but only 68% of team minutes (more possessions, more rotation), so `min_factor > mp_factor`. Slow-tempo teams show the reverse.

`mp_factor` is used in the primary ranking (`cam_gbpm`) because it isolates the player's role on their team from the team's pace of play. `min_factor` powers the secondary ranking (`min_adj_gbpm`) for a raw-minutes view.

### Why `^0.5`?

Square root creates a moderate volume reward. A 35-MPG player gets 1.43×, not 2.03× (which linear scaling would give). Prevents 40-minute ironmen from dominating purely on playing time, while still giving meaningful credit for being on the floor more.

## Step 3: GP Shrinkage (`gp_weight`)

```
gp_weight = GP / (GP + 8)
```

Bayesian shrinkage on games played. Without it, a player who plays 2 games and goes off in both could rank ahead of a 35-game starter. The standard deviation of raw GBPM is ~87 for 1-game players vs ~4 for 20+ game players — the underlying signal is catastrophically noisier for small samples.

The constant `k = 8` controls how aggressively small samples are penalized:

| GP | `gp_weight` | Interpretation |
|----|------------|----------------|
| 1 | 0.111 | ~11% credit |
| 3 | 0.273 | ~27% |
| 5 | 0.385 | ~39% |
| 8 | 0.500 | half-life |
| 15 | 0.652 | ~65% |
| 20 | 0.714 | ~71% |
| 30 | 0.789 | ~79% |
| 35 | 0.814 | ~81% |

### Why `k = 8`?

Judgment call. At `k = 8`, a player needs ~8 games to get half credit and ~30 to get ~80%. Aggressive enough to knock out 1–3 game flukes, gentle enough that a 15-game player (e.g., partial-season injury) still gets meaningful representation. To be more conservative, raise `k` to 12; to trust smaller samples more, lower it to 5.

### Why not just filter by minimum GP?

A hard cutoff is binary (a 14-GP player gets nothing, a 15-GP player gets full credit). Shrinkage is continuous — a 10-game player with monster numbers still appears in the rankings, appropriately discounted.

## Step 4: Conference Strength of Schedule (`sos_adj`)

This adjustment accounts for the quality of competition a player faced. Its purpose is specifically **portal valuation** — projecting how a player's production would translate to a different conference.

### Step 4a: Compute conference quality scores

```
-- Restrict to stable estimates only
stable = torvik_player_stats WHERE games_played >= 20 AND season = <year>

conf_quality = AVG(adj_gbpm) per conf, over stable
overall_mean = AVG(adj_gbpm) over stable
conf_sos     = conf_quality - overall_mean
```

Each conference gets a score reflecting how good its players are on average. Positive = above average; negative = below.

### Step 4b: Apply to each player

```
sos_adj      = conf_sos[player.conf] × 0.5
adj_gbpm_sos = adj_gbpm + sos_adj
```

The 0.5 multiplier is the "transfer rate" — the fraction of conference quality gap attributed to opponent-quality effects on the player's stats.

### 2026 conference SOS table

Generated from the `docs/campom_2026_baseline.csv` reference dataset, sorted strongest to weakest:

| Conference | Raw SOS | Applied (×0.5) |
|-----------|---------|----------------|
| SEC | +3.920 | +1.960 |
| Big Ten | +3.756 | +1.878 |
| Big 12 | +3.557 | +1.779 |
| ACC | +3.209 | +1.605 |
| Big East | +3.081 | +1.540 |
| MWC | +1.567 | +0.783 |
| WCC | +1.197 | +0.598 |
| A-10 | +1.157 | +0.579 |
| American | +0.591 | +0.296 |
| MVC | +0.457 | +0.228 |
| C-USA | −0.280 | −0.140 |
| Ivy | −0.445 | −0.223 |
| Big West | −0.450 | −0.225 |
| CAA | −0.623 | −0.311 |
| WAC | −0.706 | −0.353 |
| MAC | −0.706 | −0.353 |
| Big Sky | −0.818 | −0.409 |
| Southland | −0.883 | −0.441 |
| Horizon | −0.922 | −0.461 |
| Big South | −1.128 | −0.564 |
| Sun Belt | −1.225 | −0.613 |
| Summit | −1.524 | −0.762 |
| Southern | −1.886 | −0.943 |
| ASUN | −1.988 | −0.994 |
| MAAC | −2.090 | −1.045 |
| OVC | −2.501 | −1.250 |
| Patriot | −2.537 | −1.268 |
| SWAC | −2.677 | −1.339 |
| NEC | −3.038 | −1.519 |
| America East | −3.286 | −1.643 |
| MEAC | −4.384 | −2.192 |

These should be recomputed at the start of each season's compute pipeline run, not hardcoded — the table here is a snapshot.

### Why 0.5 and not 1.0?

Not all of a conference quality gap translates to individual performance when a player changes environments. Scheme fit, role changes, adjustment periods, and the difference between "average opponent quality" and "specific matchups" all reduce the effect. At 1.0, you're saying a MEAC player's numbers are fully explained by bad competition — overstated. At 0.5, roughly half the gap is competition-driven and half is real.

### Tuning the transfer rate

For more aggressive portal valuation (stronger belief that conference strength matters), increase to 0.6. For more conservative projections (trusting raw production more), decrease to 0.4. **Empirically calibrating this against historical transfer outcomes is the highest-priority improvement** — see ROADMAP §4f.

## Step 5: Supplementary Metric `bpm_adj`

```
bpm_adj = (2 × USG − OBPM − DBPM) × min_factor
```

Diagnostic only — not used in the composite rankings. Measures the gap between a player's usage and their BPM efficiency, scaled by minutes. High value = the player's usage exceeds their BPM production (using a lot of possessions without converting them into plus/minus value). Negative = more efficient than their usage would suggest.

## Final Composite Columns

Three tiers, each building on the previous. All three are computed and stored.

### Original (no GP or SOS adjustment)

```
cam_gbpm     = adj_gbpm × mp_factor
min_adj_gbpm = adj_gbpm × min_factor
```

Pure rate-times-volume. Useful for evaluating per-minute production quality, but vulnerable to small-sample noise and conference strength differences.

### v2: GP-Adjusted

```
cam_gbpm_v2     = adj_gbpm × mp_factor × gp_weight
min_adj_gbpm_v2 = adj_gbpm × min_factor × gp_weight
```

Adds reliability weighting. Recommended for **conference-neutral** analysis — comparing players within the same league, or when you want raw production without SOS opinions baked in.

### v3: SOS + GP-Adjusted (Portal Valuation)

```
cam_gbpm_v3     = adj_gbpm_sos × mp_factor × gp_weight
min_adj_gbpm_v3 = adj_gbpm_sos × min_factor × gp_weight
```

The full model. SOS is applied to `adj_gbpm` before the minutes and GP factors, so it scales with playing time. Recommended for **portal valuation** and used as the canonical site-wide player ranking (default sort on the Players tab and team rosters per ROADMAP §4f).

### Offensive / defensive splits

The o-side and d-side components of `adj_gbpm` are computed separately inside Step 1 already. cstat stores them as first-class outputs at every tier:

```
cam_o_gbpm     = OGBPM × usg_ratio^0.7        × mp_factor
cam_d_gbpm     = DGBPM × (1 − 0.1×usg_ratio) × mp_factor
cam_gbpm       = cam_o_gbpm + cam_d_gbpm

cam_o_gbpm_v2  = ... × gp_weight
cam_d_gbpm_v2  = ... × gp_weight

cam_o_gbpm_v3  = (OGBPM × usg_ratio^0.7        × mp_factor + sos_o_share) × gp_weight
cam_d_gbpm_v3  = (DGBPM × (1 − 0.1×usg_ratio) × mp_factor + sos_d_share) × gp_weight
```

Where `sos_o_share` and `sos_d_share` are the SOS adjustment proportionally split between offense and defense (split logic TBD as part of the 4f implementation — likely proportional to each side's contribution to `adj_gbpm`).

## Worked Example 1 — Flory Bidunga (Kansas, B12)

Inputs from `torvik_player_stats`:

| Field | Value |
|-------|-------|
| games_played | 35 |
| minutes_per_game (MP) | 31.55 |
| Min% (derived) | 78.3 |
| usage_rate | 19.6 |
| ogbpm | 4.2958 |
| dgbpm | 5.0538 |
| conf | B12 |

**Step 1 — usage adjustment:**

```
usg_ratio = 19.6 / 17.8736 = 1.0966
o_mult    = 1.0966 ^ 0.7    = 1.0667
d_mult    = 1 − 0.1 × 1.0966 = 0.8903

adj_gbpm  = 4.2958 × 1.0667 + 5.0538 × 0.8903 = 9.0818
```

Offensive side gets a small boost (slightly above-average usage); defensive side gets a small haircut. Net result: `adj_gbpm = 9.08` vs raw `gbpm = 9.35`.

**Step 2 — minutes factors:**

```
min_factor = (31.55 / 17.2242) ^ 0.5 = 1.3535
mp_factor  = (78.3  / 36.6563) ^ 0.5  = 1.4615
```

Both well above 1.0 — starter playing big minutes.

**Step 3 — GP shrinkage:**

```
gp_weight = 35 / (35 + 8) = 0.8140
```

Full-season player; ~81% credit (near asymptotic max).

**Step 4 — SOS:**

```
conf_sos[B12] = +3.557
sos_adj       = 3.557 × 0.5 = +1.779
adj_gbpm_sos  = 9.082 + 1.779 = 10.860
```

Big 12 is a strong conference. Bidunga gets +1.78 points of credit for the opposition quality.

**Final composites:**

```
cam_gbpm     =  9.082 × 1.462         = 13.272
cam_gbpm_v2  =  9.082 × 1.462 × 0.814 = 10.803
cam_gbpm_v3  = 10.860 × 1.462 × 0.814 = 12.920
```

## Worked Example 2 — Dior Johnson (Tarleton State, WAC)

Shows how SOS and usage adjustments interact for a high-usage mid-major player — the type of prospect where CamPom diverges most from recruiting-pedigree rankings.

| Field | Value |
|-------|-------|
| games_played | 19 |
| minutes_per_game (MP) | 26.31 |
| Min% (derived) | 43.6 |
| usage_rate | 36.5 |
| ogbpm | 9.3729 |
| dgbpm | 0.1415 |
| conf | WAC |

**Step 1 — usage adjustment:**

```
usg_ratio = 36.5 / 17.8736 = 2.0420
o_mult    = 2.0420 ^ 0.7    = 1.7082
d_mult    = 1 − 0.1 × 2.0420 = 0.7958

adj_gbpm  = 9.3729 × 1.7082 + 0.1415 × 0.7958 = 15.5628
```

Extremely high usage (36.5%) earns a massive offensive multiplier — `ogbpm` of 9.37 scales up to 16.01 after adjustment. Near-zero `dgbpm` barely matters either way. The system is recognizing that carrying 36.5% of your team's possessions while maintaining strong efficiency is very hard to do.

**Step 2 — minutes and GP factors:**

```
mp_factor = (43.6 / 36.6563) ^ 0.5 = 1.0907
gp_weight = 19 / (19 + 8)          = 0.7037
```

Modest minutes share; meaningful GP penalty (only 19 games).

**Step 3 — with and without SOS:**

```
cam_gbpm_v2 = 15.563 × 1.091 × 0.704 = 11.944    (no SOS)

conf_sos[WAC] = −0.706
sos_adj       = −0.706 × 0.5 = −0.353
adj_gbpm_sos  = 15.563 − 0.353 = 15.210
cam_gbpm_v3   = 15.210 × 1.091 × 0.704 = 11.673  (with SOS)
```

The WAC SOS adjustment is modest — only −0.35 off `adj_gbpm`. Despite playing in a weaker conference, Johnson's extreme usage and strong efficiency still project him as an elite portal target.

## Interpretation Guide

What does a `cam_gbpm_v3` value mean? Roughly: a high-impact starter who, accounting for usage, minutes share, sample reliability, and conference difficulty, projects as a top-N player nationally in terms of total contribution.

Typical 2026 ranges:

| Tier | `cam_gbpm_v3` | Example players |
|------|--------------|-----------------|
| Elite | 20+ | Cameron Boozer |
| All-Conference | 15–20 | AJ Dybantsa, Yaxel Lendeborg, Zuby Ejiofor |
| Quality starter | 10–15 | Braden Smith, Nick Martinelli, Aday Mara |
| Rotation player | 5–10 | Solid contributors at power-conference level |
| Replacement | 0–5 | Below-average contributors |
| Negative value | < 0 | Actively hurting the team when on court |

## Tunable Parameters

All constants are kept as named values in the compute pipeline so each experiment is a one-PR change (per ROADMAP §4f's iteration plan).

| Parameter | Current value | Controls | Raise to… | Lower to… |
|-----------|--------------|----------|-----------|-----------|
| `offense_exponent` | 0.7 | How aggressively high-usage offense is rewarded | Reward stars more | Trust low-usage players more |
| `defense_discount` | 0.1 | High-usage defensive penalty | Penalize harder | Trust DGBPM at face value |
| `usg_ref` | 17.87 | Neutral usage point (multiplier ≈ 1.0) | Shift neutral up | Shift neutral down |
| `minutes_exponent` | 0.5 (sqrt) | How much playing time matters vs rate | Let minutes dominate | More of a pure rate stat |
| `gp_k` | 8 | Games for half credit | Penalize small samples harder | Trust small samples more |
| `sos_transfer_rate` | 0.5 | How much conference strength adjusts individual stats | Weight opponent quality more (portal projection) | Trust raw production more (in-conference comparison) |

These constants are the inputs to the parameter sweep described in ROADMAP §4f, where the predict model serves as the fitness function.

## Known Limitations

These motivate the iteration ideas in ROADMAP §4f.

1. **Conference-level SOS is coarse.** A team in a weak conference with a brutal non-conference schedule gets the same adjustment as a team in the same conference that played cupcakes. cstat already computes minutes-weighted opponent adj-efficiency per player (`player_strength_of_schedule`); swapping that in for `conf_sos × 0.5` is a strict upgrade.
2. **No positional adjustment.** A 6'1" guard and a 6'10" center with identical `cam_gbpm_v3` rank the same, even though their positional scarcity and role versatility differ.
3. **Single-season snapshot.** No multi-year development trajectory, no regression to mean, no age curves.
4. **The SOS transfer rate (0.5) is an assumption.** Not empirically calibrated against historical portal outcomes. Calibrating against actual transfer-and-replay data is the highest-leverage improvement.
5. **Injury context is invisible.** A player with 10 GP because of a torn ACL looks the same as a walk-on with 10 garbage-time appearances. GP shrinkage penalizes both equally.
6. **Defensive GBPM is inherently noisier than offensive GBPM.** The system trusts it at nearly face value (only a mild usage discount). More aggressive defensive skepticism — or weighting offensive and defensive components differently — could improve predictive accuracy.
7. **Role context is only USG.** Usage is one axis of role. A 30%-usage primary scorer and a 30%-usage point guard play different roles; usage alone treats them identically. Layering in shot diet, playmaking rates, and defensive specialty would make CamPom genuinely role-aware (planned in §4f).

## Cross-References

- `crates/cstat-core/src/compute.rs` — implementation lives here (planned in Phase 4f)
- `docs/campom_2026_baseline.csv` — externally-computed 2026 reference dataset; used as the parity gate in §4f before iteration
- ROADMAP.md §4f — full rollout plan: implement → validate against the baseline → iterate (predict-model fitness function, hyperparameter grid search, role-context extensions) → ship to API + tables across the site
- `migrations/008_torvik_player_stats.sql` — schema for the input table
- `docs/torvik-api-guide.md` — how the input data is fetched and parsed
