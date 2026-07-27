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
       cae_raw = actual_AdjEM − roster_proj      (roster_proj = the backtest's
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
   at its low end; cutting quartiles on roster_proj, NOT on the actual outcome, keeps
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

Source of roster_proj/actual is the latest per-team backtest dump in eval_history
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


def load_backtest(explicit: Path | None = None) -> list[dict]:
    """Load the per-team backtest dump CAE scores coaches against.

    Pass `explicit` (the `--dump` flag) to name the file directly. That is
    what `retrain_downstream.sh` does, because the fallback below picks by
    *filename*, not recency, and the two disagree more often than you would
    expect — the dumps carry descriptive tags (`…_honest`, `…_traj60_DATE`,
    `…_traj60honest211_DATE`), so a fresh `…_full_11season_20260726.json`
    sorts BEFORE a months-old `…_traj60honest211_20260725.json`. Silently
    grading coaches against a superseded projection is exactly the kind of
    drift #218 was about, so when the two orderings disagree we say so.
    """
    rows, _, _ = load_backtest_with_provenance(explicit)
    return rows


def _resolve_dump(explicit: Path | None) -> Path:
    """Pick the dump file, warning when name-order and mtime-order disagree."""
    if explicit is not None:
        path = Path(explicit)
        if not path.exists():
            raise SystemExit(f"--dump {path} does not exist")
        print(f"backtest dump: {path.name}  (explicit --dump)")
        return path

    dumps = sorted(EVAL_DIR.glob(BT_GLOB))
    if not dumps:
        raise SystemExit(f"no backtest dump matching {BT_GLOB} in {EVAL_DIR}")
    path = dumps[-1]
    newest = max(dumps, key=lambda p: p.stat().st_mtime)
    if newest != path:
        print(
            f"  WARNING: picking {path.name} by name, but {newest.name} is newer "
            f"on disk. Pass --dump to be explicit about which projection "
            f"generation these grades are scored against."
        )
    print(f"backtest dump: {path.name}")
    return path


def _unwrap_dump(obj) -> tuple[list[dict], dict | None]:
    """Read either dump shape and return `(team_rows, provenance)`.

    Since #238 the backtest writes `{"provenance": {...}, "teams": [...]}` so a
    consumer can say which model generation the residuals came from. Every dump
    written before that is a bare array. Both are accepted for the same reason
    the `phase_b` -> `roster_proj` shim exists: `eval_history/` holds months of
    historical dumps that are still cited in writeups, and regenerating them to
    satisfy a format change would destroy the record they exist to preserve.

    A bare array yields `None` provenance, which downstream records honestly
    rather than papering over — an old dump genuinely cannot say what produced
    it, and claiming otherwise is the failure mode this whole chain is about.
    """
    if isinstance(obj, dict):
        return obj.get("teams", []), obj.get("provenance")
    return obj, None


def read_dump_records(path: Path) -> list[dict]:
    """Read a backtest dump into its per-team records, whichever shape it is.

    The entry point for consumers that build a DataFrame rather than going
    through `load_backtest` — `audit_preseason_projections.py`,
    `decompose_projection_error.py` and `diagnose_trajectory_attrition.py` all
    glob for the newest dump and previously called `pd.read_json` on it
    directly. That breaks on the #238 envelope: `pd.read_json` on
    `{"provenance": ..., "teams": [...]}` yields a frame of the envelope, not
    of the teams.

    Centralized here rather than shimmed in each caller because three inline
    copies of the same unwrap is precisely what drifts — the `phase_b` ->
    `roster_proj` rename is already duplicated inline in all three, which is
    how this file ended up being the only place that knew about the rename.
    """
    return _normalize_proj_keys(_unwrap_dump(json.loads(Path(path).read_text()))[0])


def load_backtest_with_provenance(
    explicit: Path | None = None,
) -> tuple[list[dict], dict | None, Path]:
    """`load_backtest`, plus the dump's provenance and the path it came from.

    Separate entry point so `load_backtest`'s signature stays exactly as the
    three other consumers (`transition_blend_diagnostic.py`,
    `pit_cae_backtest.py`, `pit_program_calibration.py`) already call it.
    """
    path = _resolve_dump(explicit)
    rows, provenance = _unwrap_dump(json.loads(path.read_text()))
    return _normalize_proj_keys(rows), provenance, path


def _normalize_proj_keys(rows: list[dict]) -> list[dict]:
    """Back-compat shim for the `phase_b → roster_proj` / `phase_a → boxscore_proj`
    rename (ROADMAP Refactor Backlog). Dumps written by the old backtest carry
    `phase_b`/`phase_a`; new dumps carry `roster_proj`/`boxscore_proj`. Alias both
    directions in place so consumers read either name on any dump — this is why
    the existing dumps did not need regenerating."""
    for row in rows:
        rp = row.get("roster_proj", row.get("phase_b"))
        bp = row.get("boxscore_proj", row.get("phase_a"))
        if rp is not None:
            row["roster_proj"] = row["phase_b"] = rp
        if bp is not None:
            row["boxscore_proj"] = row["phase_a"] = bp
    return rows


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
                "roster_proj": float(r["roster_proj"]),
                "actual": float(r["actual"]),
                "cae_raw": float(r["actual"]) - float(r["roster_proj"]),
            }
        )
    print(f"joined {len(rows)}/{len(bt)} team-seasons to a coach "
          f"({unmatched} unmatched: pre-2015 / non-D-I / unmatched team)")
    return rows


def debias_by_projection_quartile(rows: list[dict]) -> list[dict]:
    """Subtract the projection-quartile mean residual from each row's cae_raw.

    Quartiles are cut on roster_proj (the projection), NOT on the actual outcome.
    Mutates rows in place (adds `cae_debiased`) and returns the per-bucket bias
    table for the summary."""
    srt = sorted(rows, key=lambda x: x["roster_proj"])
    n = len(srt)
    bounds = [n * i // N_QUARTILES for i in range(N_QUARTILES + 1)]
    buckets = []
    for i in range(N_QUARTILES):
        g = srt[bounds[i]:bounds[i + 1]]
        bias = mean([x["cae_raw"] for x in g])
        lo, hi = g[0]["roster_proj"], g[-1]["roster_proj"]
        for x in g:
            x["cae_debiased"] = x["cae_raw"] - bias
        buckets.append({"q": i + 1, "n": len(g), "roster_proj_lo": lo,
                        "roster_proj_hi": hi, "mean_resid": bias})
    return buckets


def center_by_season(rows: list[dict]) -> list[dict]:
    """Subtract each season's mean residual from every row's cae_raw.

    Produces `cae_centered` — a COMPARISON-ONLY view. Centering removes the
    season-level component of the residual, which mixes projection
    miscalibration (artifact) with genuine era effects (real coaching signal)
    inseparably. Era-neutral ranking is valid; "how much did this coach add"
    is NOT (it would erase the real era component). Headline stays raw.

    Mutates rows in place (adds `cae_centered`) and returns the per-season
    bias table for the summary."""
    by_season: dict[int, list[dict]] = defaultdict(list)
    for x in rows:
        by_season[x["season"]].append(x)
    table = []
    for s in sorted(by_season):
        g = by_season[s]
        bias = mean([x["cae_raw"] for x in g])
        for x in g:
            x["cae_centered"] = x["cae_raw"] - bias
        table.append({"season": s, "n": len(g), "mean_resid": bias})
    return table


def posterior_ci(mean_resid: float, n: int, s2w: float, s2b: float):
    """EB shrink + 95% credibility interval under the normal random-effects
    model: prior a~N(0,σ²_b), likelihood mean~N(a, σ²_w/n).
        shrink   = n/(n+k),  k = σ²_w/σ²_b
        post_sd  = sqrt(σ²_b · k/(n+k))
    Returns (shrunk, reliability, ci_low, ci_high)."""
    # No between-coach variance (variance_components clamps σ²_b at 0) → no
    # signal to shrink toward, so the posterior collapses to the prior mean 0.
    # Guards the season-centered path especially: centering pushes MS_between
    # toward MS_within, so the clamp can fire on some cohorts. (vc_of guards
    # k the same way; posterior_ci hadn't.)
    if s2b <= 0:
        return 0.0, 0.0, 0.0, 0.0
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
    ap.add_argument("--dump", type=Path, default=None,
                    help="explicit per-team backtest dump to score against. "
                         "Without it the newest-by-FILENAME match wins, which "
                         "is not always the newest on disk (see load_backtest).")
    args = ap.parse_args()

    bt, dump_provenance, dump_path = load_backtest_with_provenance(args.dump)
    if dump_provenance is None:
        print(
            "  NOTE: this dump predates the #238 provenance envelope, so the "
            "grades cannot record which projection generation they were scored "
            "against. Rerun `projections-backtest` to get one."
        )
    engine = get_engine()
    with engine.connect() as conn:
        rows = join_coaches(bt, conn)

    buckets = debias_by_projection_quartile(rows)
    print("\nprojection-quartile de-bias (subtracted from cae_raw):")
    for b in buckets:
        print(f"  Q{b['q']} n={b['n']:4d}  roster_proj∈[{b['roster_proj_lo']:+.0f},"
              f"{b['roster_proj_hi']:+.0f}]  mean_resid={b['mean_resid']:+.2f}")

    season_bias = center_by_season(rows)
    print("\nseason-centering (subtracted from cae_raw → cae_centered, "
          "comparison-only):")
    for b in season_bias:
        print(f"  {b['season']} n={b['n']:4d}  mean_resid={b['mean_resid']:+.2f}")

    # Variance components — headline on RAW, reported alongside the de-biased.
    def vc_of(key):
        v = variance_components([{"coach": x["coach_id"], "resid": x[key]}
                                 for x in rows])
        if v is None:
            raise SystemExit("variance components unavailable (need ≥2 multi-season coaches)")
        s2w, s2b, _n0, C, N = v
        icc = s2b / (s2b + s2w) if (s2b + s2w) else 0.0
        k = s2w / s2b if s2b > 0 else float("inf")
        return {"s2w": s2w, "s2b": s2b, "icc": icc, "k": k, "C": C, "N": N}

    vraw = vc_of("cae_raw")
    vdeb = vc_of("cae_debiased")
    vcen = vc_of("cae_centered")
    s2w, s2b, icc = vraw["s2w"], vraw["s2b"], vraw["icc"]
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
        m_cen = mean([x["cae_centered"] for x in xs])
        shrunk, rel, lo, hi = posterior_ci(m, len(xs), s2w, s2b)
        adj_shrunk, _, _, _ = posterior_ci(m_adj, len(xs), vdeb["s2w"], vdeb["s2b"])
        cen_shrunk, _, _, _ = posterior_ci(m_cen, len(xs), vcen["s2w"], vcen["s2b"])
        seasons = [x["season"] for x in xs]
        ratings.append({
            "coach_id": cid, "coach": xs[0]["coach"], "n": len(xs),
            "raw_mean": m, "adj_mean": m_adj, "cen_mean": m_cen, "shrunk": shrunk,
            "adj_shrunk": adj_shrunk, "cen_shrunk": cen_shrunk, "reliability": rel,
            "ci_low": lo, "ci_high": hi,
            "first_season": min(seasons), "last_season": max(seasons),
            "roster_proj_mean": mean([x["roster_proj"] for x in xs]),
        })
    ratings.sort(key=lambda r: -r["shrunk"])

    # --- Guards (on the headline/raw residual) ---
    sh_raw, sh_n = split_half(rows, "cae_raw")
    sh_deb, _ = split_half(rows, "cae_debiased")
    eligible = [r for r in ratings if r["n"] >= MIN_SEASONS_FACE]
    prestige_corr = corr([r["raw_mean"] for r in eligible],
                         [r["roster_proj_mean"] for r in eligible])
    print("\nguards (headline = RAW):")
    print(f"  ICC                         {icc:.3f}  (min {MIN_ICC})  "
          f"{'OK' if icc > MIN_ICC else 'FAIL'}")
    print(f"  split-half (odd/even yrs)   {sh_raw:+.3f} n={sh_n}  (min {MIN_SPLIT_HALF})  "
          f"{'OK' if sh_raw > MIN_SPLIT_HALF else 'FAIL'}")
    print(f"  CAE vs projection corr      {prestige_corr:+.3f}  "
          "(reported; coach×program confound, not gated)")
    print(f"  [de-biased split-half       {sh_deb:+.3f} — prestige-adjusted lower bound]")

    print(f"\ntop 15 by shrunk CAE (≥{MIN_SEASONS_FACE} seasons) — face-validity check:")
    for r in eligible[:15]:
        print(f"  {r['coach']:24s} CAE={r['shrunk']:+5.2f} "
              f"[{r['ci_low']:+5.2f},{r['ci_high']:+5.2f}] "
              f"raw={r['raw_mean']:+5.2f} n={r['n']} ({r['first_season']}–{r['last_season']})")
    print(f"\nbottom 8 by shrunk CAE (≥{MIN_SEASONS_FACE} seasons):")
    for r in eligible[-8:]:
        print(f"  {r['coach']:24s} CAE={r['shrunk']:+5.2f} n={r['n']}")

    guards_pass = icc > MIN_ICC and sh_raw > MIN_SPLIT_HALF

    # --- Summary artifact ---
    summary = {
        # `utcnow()` is deprecated and scheduled for removal; the Python 3.13
        # move in #222 started warning about it. `.replace` keeps the trailing
        # "Z" the previous summaries were written with, so the field format is
        # unchanged for anything reading the eval_history ledger.
        "generated_at": dt.datetime.now(dt.timezone.utc)
        .isoformat()
        .replace("+00:00", "Z"),
        "denominator": "roster_proj",
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
    date_str = dt.datetime.now(dt.timezone.utc).strftime("%Y%m%d")
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

    write_db(engine, rows, ratings, dump_provenance, dump_path)
    print(f"\nwrote {len(rows)} coach_season_cae rows + {len(ratings)} coach_ratings rows")


def write_db(
    engine,
    rows: list[dict],
    ratings: list[dict],
    dump_provenance: dict | None = None,
    dump_path: Path | None = None,
) -> None:
    with engine.begin() as conn:
        conn.execute(text("TRUNCATE coach_season_cae, coach_ratings"))
        conn.execute(
            text(
                """
                INSERT INTO coach_season_cae
                  (coach_id, season, team_natstat_id, actual_adjem, projection,
                   cae_raw, cae_debiased, cae_centered)
                VALUES
                  (:coach_id, :season, :tn, :actual, :roster_proj, :cae_raw, :cae_deb,
                   :cae_cen)
                """
            ),
            [{"coach_id": x["coach_id"], "season": x["season"],
              "tn": x["team_natstat_id"], "actual": x["actual"],
              "roster_proj": x["roster_proj"], "cae_raw": x["cae_raw"],
              "cae_deb": x["cae_debiased"], "cae_cen": x["cae_centered"]}
             for x in rows],
        )
        conn.execute(
            text(
                """
                INSERT INTO coach_ratings
                  (coach_id, n_seasons, cae_raw_mean, cae_shrunk, cae_adj_mean,
                   cae_adj_shrunk, cae_centered_mean, cae_centered_shrunk,
                   reliability, ci_low, ci_high, first_season, last_season)
                VALUES
                  (:coach_id, :n, :raw, :shrunk, :adj_m, :adj_s, :cen_m, :cen_s,
                   :rel, :lo, :hi, :fs, :ls)
                """
            ),
            [{"coach_id": r["coach_id"], "n": r["n"], "raw": r["raw_mean"],
              "shrunk": r["shrunk"], "adj_m": r["adj_mean"],
              "adj_s": r["adj_shrunk"], "cen_m": r["cen_mean"],
              "cen_s": r["cen_shrunk"], "rel": r["reliability"], "lo": r["ci_low"],
              "hi": r["ci_high"], "fs": r["first_season"], "ls": r["last_season"]}
             for r in ratings],
        )

        # Which projection generation these grades were scored against (#238),
        # in the same transaction as the grades. CAE is the roster-impact
        # residual, so a retrain moves every grade — that is expected and
        # descriptive, but it means a grade is only interpretable alongside the
        # projection it was measured from.
        #
        # `dump_provenance` is None for a pre-#238 dump. Recorded as null
        # rather than omitted: an old dump genuinely cannot say what produced
        # it, and a row that quietly claims nothing is wrong is the failure
        # this chain exists to break.
        conn.execute(
            text(
                """
                INSERT INTO artifact_provenance
                    (artifact, artifact_key, provenance, computed_at)
                VALUES ('coach_season_cae', 'all', CAST(:prov AS jsonb), now())
                ON CONFLICT (artifact, artifact_key) DO UPDATE SET
                    provenance  = EXCLUDED.provenance,
                    computed_at = now()
                """
            ),
            {
                "prov": json.dumps(
                    {
                        "produced_by": "training/compute_cae.py --write",
                        "scored_against_dump": dump_path.name if dump_path else None,
                        "dump_provenance": dump_provenance,
                    }
                )
            },
        )


if __name__ == "__main__":
    main()
