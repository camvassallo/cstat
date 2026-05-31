"""Compute Coach-Above-Expectation (CAE) ratings → `coach_season_cae` +
`coach_ratings`.

CAE is a DESCRIPTIVE coach grade: how much a team out/under-performs the
talent on its roster, attributed to the coach, aggregated across the
coach's career with empirical-Bayes shrinkage. Methodology + feasibility
verdict: `docs/coach_above_expectation_design.md`. It is NOT a predictor
(refuted twice — design §3); ship descriptive-only.

Pipeline
--------
1. Per team-season residual against the roster-only projection:
       cae_raw = actual_AdjEM − phase_b      (phase_b = the backtest's
       roster-talent-only projection; the design's non-negotiable denominator —
       `served` leaks prior coaching via the baseline term and kills the signal).
2. Headline rating = the RAW residual (cae_raw). The design frames CAE as
   "coach×program over-expectation" (§2): at 5 seasons coach and program cannot
   be cleanly separated, and the program-level component is real coach context,
   not noise. RAW carries the only statistically-significant persistence
   (split-half +0.114, z≈2.1; ICC 0.135) and its top list is already mid-major
   overachievers (no blue-blood dominance), so it ships as the leaderboard value.
3. We ALSO compute a projection-quartile-de-biased residual (cae_debiased) and
   store it per-season as the prestige-adjusted view. De-bias subtracts the
   PROJECTION-quartile mean residual (the projection is miscalibrated ~−1.7 only
   at its low end; cutting quartiles on phase_b, NOT on the actual outcome, keeps
   this artifact-free — de-biasing by ACTUAL quartile would bake in outcome-
   conditioned regression, see project_projection_q1_bias_refuted). Empirically
   the de-bias strips the same-team persistence (+0.047→−0.009) while preserving
   the moved-teams transferable signal (+0.112→+0.083) — i.e. it removes the
   program component. It is the conservative lower bound, not the headline,
   because it pushes overall persistence below significance (split-half +0.049,
   z≈0.9). Surfaced for transparency / a future prestige-adjusted leaderboard.
4. Empirical-Bayes shrink per coach off the RAW residual: one-way random-effects
   variance components (σ²_within season noise, σ²_between coach skill) →
   k = σ²_w/σ²_b season-equivalents; CAE_shrunk = (n/(n+k))·mean(cae_raw), with a
   posterior credibility interval. k ≈ 6.4, so a 1–2 season coach is mostly prior.

Guards (cae_feasibility.py thresholds — abort the write if they regress, on the
HEADLINE/raw residual): ICC > 0, positive split-half reliability, top-list face
validity (mid-major overachievers + elite developers, NOT blue-blood-dominated).
The CAE-vs-projection correlation is REPORTED, not gated to zero (coach×program
confound is acknowledged, not scrubbed); the de-biased stats are reported beside
it as the prestige-adjusted comparison.

Run
---
  python3 compute_cae.py            # offline: compute + print guards + write JSON summary
  python3 compute_cae.py --write    # also upsert coach_season_cae + coach_ratings

Source of phase_b/actual is the latest per-team backtest dump in eval_history
(produced by `cstat-ingest projections-backtest --output`); coach identity is
the `coaches`/`coach_seasons` mapping from migration 024, joined to the backtest
via teams.natstat_id (the dump records the base-season team UUID).
"""

import argparse
import datetime as dt
import json
import sys
from collections import defaultdict
from pathlib import Path

from sqlalchemy import text

from cae_feasibility import corr, mean, variance_components
from db import get_engine

EVAL_DIR = Path(__file__).resolve().parent / "eval_history"
BT_GLOB = "projections_backtest_per_team_*season_*.json"

# Guard thresholds (from the feasibility study; a regression aborts the write).
MIN_ICC = 0.05          # between-coach signal must survive
MIN_SPLIT_HALF = 0.05   # odd-vs-even-year coach means must positively persist
MIN_SEASONS_FACE = 3    # coaches considered for the face-validity top list
N_QUARTILES = 4


def load_backtest() -> list[dict]:
    dumps = sorted(EVAL_DIR.glob(BT_GLOB))
    if not dumps:
        raise SystemExit(f"no backtest dump matching {BT_GLOB} in {EVAL_DIR}")
    path = dumps[-1]
    print(f"backtest dump: {path.name}")
    return json.loads(path.read_text())


def join_coaches(bt: list[dict], conn) -> list[dict]:
    """Attach the canonical coach (id + name) to each backtest team-season.

    The dump records the *base-season* team UUID, so we hop
    teams.natstat_id → coach_seasons.team_natstat_id at the dump's season —
    the same cross-season resolution the rest of the pipeline uses."""
    cs_rows = conn.execute(
        text(
            """
            SELECT cs.team_natstat_id AS tn, cs.season AS season,
                   co.id::text AS coach_id, co.canonical_name AS coach
            FROM coach_seasons cs
            JOIN coaches co ON co.id = cs.coach_id
            WHERE cs.team_natstat_id IS NOT NULL
            """
        )
    ).fetchall()
    lut = {(r.tn, r.season): (r.coach_id, r.coach) for r in cs_rows}
    tid2ns = {
        r.id: r.natstat_id
        for r in conn.execute(text("SELECT id::text AS id, natstat_id FROM teams"))
    }

    rows, unmatched = [], 0
    for r in bt:
        ns = tid2ns.get(r["team_id"])
        hit = lut.get((ns, r["season"]))
        if hit is None:
            unmatched += 1
            continue
        coach_id, coach = hit
        rows.append(
            {
                "coach_id": coach_id,
                "coach": coach,
                "team_natstat_id": ns,
                "team": r["team_name"],
                "season": r["season"],
                "phase_b": float(r["phase_b"]),
                "actual": float(r["actual"]),
                "cae_raw": float(r["actual"]) - float(r["phase_b"]),
            }
        )
    print(f"joined {len(rows)}/{len(bt)} team-seasons to a coach "
          f"({unmatched} unmatched: pre-2015 / non-D-I / unmatched team)")
    return rows


def debias_by_projection_quartile(rows: list[dict]) -> list[dict]:
    """Subtract the projection-quartile mean residual from each row's cae_raw.

    Quartiles are cut on phase_b (the projection), NOT on the actual outcome.
    Mutates rows in place (adds `cae_debiased`, `proj_q`) and returns the
    per-bucket bias table for the summary."""
    srt = sorted(rows, key=lambda x: x["phase_b"])
    n = len(srt)
    bounds = [n * i // N_QUARTILES for i in range(N_QUARTILES + 1)]
    buckets = []
    for i in range(N_QUARTILES):
        g = srt[bounds[i]:bounds[i + 1]]
        bias = mean([x["cae_raw"] for x in g])
        lo, hi = g[0]["phase_b"], g[-1]["phase_b"]
        for x in g:
            x["cae_debiased"] = x["cae_raw"] - bias
            x["proj_q"] = i
        buckets.append({"q": i + 1, "n": len(g), "phase_b_lo": lo,
                        "phase_b_hi": hi, "mean_resid": bias})
    return buckets


def posterior_ci(mean_resid: float, n: int, s2w: float, s2b: float):
    """EB shrink + 95% credibility interval under the normal random-effects
    model: prior a~N(0,σ²_b), likelihood mean~N(a, σ²_w/n).
        shrink   = n/(n+k),  k = σ²_w/σ²_b
        post_sd  = sqrt(σ²_b · k/(n+k))
    Returns (shrunk, reliability, ci_low, ci_high)."""
    k = s2w / s2b
    rel = n / (n + k)
    shrunk = rel * mean_resid
    post_sd = (s2b * k / (n + k)) ** 0.5
    return shrunk, rel, shrunk - 1.96 * post_sd, shrunk + 1.96 * post_sd


def split_half(rows: list[dict], key: str) -> tuple[float, int]:
    """Odd-vs-even-season coach-mean correlation (reliability sanity check)."""
    odd, even = defaultdict(list), defaultdict(list)
    for x in rows:
        (odd if x["season"] % 2 else even)[x["coach_id"]].append(x[key])
    both = [c for c in odd if c in even]
    if len(both) < 5:
        return 0.0, len(both)
    xs = [mean(odd[c]) for c in both]
    ys = [mean(even[c]) for c in both]
    return corr(xs, ys), len(both)


def main() -> None:
    ap = argparse.ArgumentParser()
    ap.add_argument("--write", action="store_true",
                    help="upsert coach_season_cae + coach_ratings (default: dry-run)")
    args = ap.parse_args()

    bt = load_backtest()
    engine = get_engine()
    with engine.connect() as conn:
        rows = join_coaches(bt, conn)

    buckets = debias_by_projection_quartile(rows)
    print("\nprojection-quartile de-bias (subtracted from cae_raw):")
    for b in buckets:
        print(f"  Q{b['q']} n={b['n']:4d}  phase_b∈[{b['phase_b_lo']:+.0f},"
              f"{b['phase_b_hi']:+.0f}]  mean_resid={b['mean_resid']:+.2f}")

    # Variance components — headline on RAW, reported alongside the de-biased.
    def vc_of(key):
        v = variance_components([{"coach": x["coach_id"], "resid": x[key]}
                                 for x in rows])
        if v is None:
            raise SystemExit("variance components unavailable (need ≥2 multi-season coaches)")
        s2w, s2b, n0, C, N = v
        icc = s2b / (s2b + s2w) if (s2b + s2w) else 0.0
        k = s2w / s2b if s2b > 0 else float("inf")
        return {"s2w": s2w, "s2b": s2b, "icc": icc, "k": k, "C": C, "N": N}

    vraw = vc_of("cae_raw")
    vdeb = vc_of("cae_debiased")
    s2w, s2b, icc, k = vraw["s2w"], vraw["s2b"], vraw["icc"], vraw["k"]
    print(f"\nvariance components (coaches ≥2: C={vraw['C']}, N={vraw['N']}):")
    print(f"  RAW (headline)   σ²_w={vraw['s2w']:.2f} σ²_b={vraw['s2b']:.2f} "
          f"(σ={vraw['s2b']**0.5:.2f})  ICC={vraw['icc']:.3f}  k={vraw['k']:.1f}")
    print(f"  de-biased (adj)  σ²_w={vdeb['s2w']:.2f} σ²_b={vdeb['s2b']:.2f} "
          f"(σ={vdeb['s2b']**0.5:.2f})  ICC={vdeb['icc']:.3f}  k={vdeb['k']:.1f}")

    # Per-coach career aggregation + shrink (headline off cae_raw; de-biased
    # mean carried alongside as the prestige-adjusted view).
    by_coach = defaultdict(list)
    for x in rows:
        by_coach[x["coach_id"]].append(x)
    ratings = []
    for cid, xs in by_coach.items():
        m = mean([x["cae_raw"] for x in xs])
        m_adj = mean([x["cae_debiased"] for x in xs])
        shrunk, rel, lo, hi = posterior_ci(m, len(xs), s2w, s2b)
        adj_shrunk, _, _, _ = posterior_ci(m_adj, len(xs), vdeb["s2w"], vdeb["s2b"])
        seasons = [x["season"] for x in xs]
        ratings.append({
            "coach_id": cid, "coach": xs[0]["coach"], "n": len(xs),
            "raw_mean": m, "adj_mean": m_adj, "shrunk": shrunk,
            "adj_shrunk": adj_shrunk, "reliability": rel,
            "ci_low": lo, "ci_high": hi,
            "first_season": min(seasons), "last_season": max(seasons),
            "phase_b_mean": mean([x["phase_b"] for x in xs]),
        })
    ratings.sort(key=lambda r: -r["shrunk"])

    # --- Guards (on the headline/raw residual) ---
    sh_raw, sh_n = split_half(rows, "cae_raw")
    sh_deb, _ = split_half(rows, "cae_debiased")
    eligible = [r for r in ratings if r["n"] >= MIN_SEASONS_FACE]
    prestige_corr = corr([r["raw_mean"] for r in eligible],
                         [r["phase_b_mean"] for r in eligible])
    print(f"\nguards (headline = RAW):")
    print(f"  ICC                         {icc:.3f}  (min {MIN_ICC})  "
          f"{'OK' if icc > MIN_ICC else 'FAIL'}")
    print(f"  split-half (odd/even yrs)   {sh_raw:+.3f} n={sh_n}  (min {MIN_SPLIT_HALF})  "
          f"{'OK' if sh_raw > MIN_SPLIT_HALF else 'FAIL'}")
    print(f"  CAE vs projection corr      {prestige_corr:+.3f}  "
          f"(reported; coach×program confound, not gated)")
    print(f"  [de-biased split-half       {sh_deb:+.3f} — prestige-adjusted lower bound]")

    print(f"\ntop 15 by shrunk CAE (≥{MIN_SEASONS_FACE} seasons) — face-validity check:")
    for r in [r for r in eligible][:15]:
        print(f"  {r['coach']:24s} CAE={r['shrunk']:+5.2f} "
              f"[{r['ci_low']:+5.2f},{r['ci_high']:+5.2f}] "
              f"raw={r['raw_mean']:+5.2f} n={r['n']} ({r['first_season']}–{r['last_season']})")
    print(f"\nbottom 8 by shrunk CAE (≥{MIN_SEASONS_FACE} seasons):")
    for r in eligible[-8:]:
        print(f"  {r['coach']:24s} CAE={r['shrunk']:+5.2f} n={r['n']}")

    guards_pass = icc > MIN_ICC and sh_raw > MIN_SPLIT_HALF

    # --- Summary artifact ---
    summary = {
        "generated_at": dt.datetime.utcnow().isoformat() + "Z",
        "denominator": "phase_b",
        "headline": "raw (coach×program over-expectation)",
        "n_team_seasons": len(rows),
        "n_coaches": len(ratings),
        "variance_components_raw": vraw,
        "variance_components_debiased": vdeb,
        "guards": {"icc": icc, "split_half_raw": sh_raw,
                   "split_half_debiased": sh_deb, "split_half_n": sh_n,
                   "prestige_corr": prestige_corr, "passed": guards_pass},
        "debias_buckets": buckets,
        "top15": [{kk: r[kk] for kk in ("coach", "n", "raw_mean", "shrunk",
                                        "ci_low", "ci_high", "first_season",
                                        "last_season")}
                  for r in eligible[:15]],
    }
    date_str = dt.datetime.utcnow().strftime("%Y%m%d")
    out = EVAL_DIR / f"cae_compute_{date_str}_summary.json"
    out.write_text(json.dumps(summary, indent=2, default=float))
    print(f"\nwrote {out}")

    if not args.write:
        print("\n(dry-run — pass --write to upsert coach_season_cae + coach_ratings)")
        if not guards_pass:
            print("WARNING: guards FAILED — would refuse to write.")
        return

    if not guards_pass:
        sys.exit("guards FAILED (ICC or split-half regressed) — refusing to write")

    write_db(engine, rows, ratings)
    print(f"\nwrote {len(rows)} coach_season_cae rows + {len(ratings)} coach_ratings rows")


def write_db(engine, rows: list[dict], ratings: list[dict]) -> None:
    with engine.begin() as conn:
        conn.execute(text("TRUNCATE coach_season_cae, coach_ratings"))
        conn.execute(
            text(
                """
                INSERT INTO coach_season_cae
                  (coach_id, season, team_natstat_id, actual_adjem, projection,
                   cae_raw, cae_debiased)
                VALUES
                  (:coach_id, :season, :tn, :actual, :phase_b, :cae_raw, :cae_deb)
                """
            ),
            [{"coach_id": x["coach_id"], "season": x["season"],
              "tn": x["team_natstat_id"], "actual": x["actual"],
              "phase_b": x["phase_b"], "cae_raw": x["cae_raw"],
              "cae_deb": x["cae_debiased"]} for x in rows],
        )
        conn.execute(
            text(
                """
                INSERT INTO coach_ratings
                  (coach_id, n_seasons, cae_raw_mean, cae_shrunk, cae_adj_mean,
                   cae_adj_shrunk, reliability, ci_low, ci_high,
                   first_season, last_season)
                VALUES
                  (:coach_id, :n, :raw, :shrunk, :adj_m, :adj_s, :rel, :lo, :hi,
                   :fs, :ls)
                """
            ),
            [{"coach_id": r["coach_id"], "n": r["n"], "raw": r["raw_mean"],
              "shrunk": r["shrunk"], "adj_m": r["adj_mean"],
              "adj_s": r["adj_shrunk"], "rel": r["reliability"], "lo": r["ci_low"],
              "hi": r["ci_high"], "fs": r["first_season"], "ls": r["last_season"]}
             for r in ratings],
        )


if __name__ == "__main__":
    main()
