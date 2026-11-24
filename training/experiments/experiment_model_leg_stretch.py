"""Should the early-season blend calibrate the model leg directly (#515)?

A served prediction is `w·(slope·pre_diff + HCA) + (1 − w)·model`, with `w`
from `projection::prediction_prior_weight` and `slope` = 1.0
(`PRESEASON_SLOPE`). The model leg is compressed toward zero early in the
season, and the unit-scale preseason leg over-states by about as much, so the
two errors cancel and the blend is unbiased POOLED. #390 measured exactly that,
and a per-week regression with free coefficients on both legs only tied it on
pooled MAE.

Pooled is the wrong place to look for this failure. The cancellation needs the
two legs to agree. When a team has played well above or below its projection,
the preseason leg says little about this game and nothing un-compresses the
model leg, so the served margin should under-react to the in-season evidence
in exactly the games where that evidence matters. This script measures that
slice, then asks whether calibrating each leg on its own fixes it without
costing pooled MAE:

  - served:     the blend as shipped (the baseline every arm is paired with);
  - stretch:    the model leg multiplied by a per-bucket stretch `s_b`, the
                preseason leg at its leg-alone slope (0.59, `PRESEASON_ONLY_SLOPE`),
                so each leg is right by itself;
  - free:       per-bucket least squares on both legs (`a_b`, `c_b`), the #390
                regression, kept as the ceiling for a two-coefficient mix.

Buckets are the mean games played of the two teams (0-1, 2-3, 4-5, 6-8, 9-12,
13+), the axis the served weight already uses. Walk-forward (#361): coefficients
fit on seasons < S, scored on S, from the third season on.

**Disagreement** is `d = pre_margin − model`, the served preseason leg minus
the model leg. Each arm is scored by quartile of `|d|` within its season on:

  - MAE;
  - `toward_model`: the mean of `(actual − pred)·sign(model − pre_margin)`.
    Positive means the result landed further toward the model leg's side than
    the prediction did, i.e. the blend under-weighted the in-season evidence.
    This is the bias #515 is about; the served blend should show it growing
    with `|d|`.

Acceptance (from #515): pooled MAE no worse than served, and `toward_model` in
the top quartile materially smaller. A lower pooled MAE alone is not the bar.

Input is the dump, which needs no model here:

    cargo run --release --bin cstat-ingest -- measure-blend-accuracy --dump legs.csv
    cd training && ./.venv/bin/python experiments/experiment_model_leg_stretch.py \\
        --legs legs.csv --out eval_history/model_leg_stretch_YYYYMMDD_summary.json

Caveat inherited from the dump (`measure_blend_accuracy.rs`): the model leg's
team stats, roster aggregates and form are season aggregates (#274), so a
historical November model leg has seen more than a live one. If anything that
makes the replayed model leg LESS compressed than the served one, so a stretch
fitted here is a lower bound on what live wants.

Comparison script only; writes nothing to the database or the models.
"""

import argparse
import json

import numpy as np
import pandas as pd

HCA = 3.5
SERVED_SLOPE = 1.0
LEG_ALONE_SLOPE = 0.59
RATING_K = 12.0
FULL_SEASON_GAMES = 32.0
PREDICTION_PRIOR_SHARE = 0.5
BUCKET_EDGES = [0, 2, 4, 6, 9, 13, np.inf]
BUCKET_LABELS = ["0-1", "2-3", "4-5", "6-8", "9-12", "13+"]
ARMS = ["served", "stretch", "free"]


def taper(k, games):
    end = k / (k + FULL_SEASON_GAMES)
    return np.maximum((k / (k + games) - end) / (1.0 - end), 0.0)


def served_weight(df):
    """`projection::prediction_prior_weight`: half the rating curve, mean of the two teams."""
    return PREDICTION_PRIOR_SHARE * (taper(RATING_K, df.home_games) + taper(RATING_K, df.away_games)) / 2.0


def prepare(df):
    df = df.copy()
    df["hca"] = np.where(df.is_neutral.astype(str).str.lower() == "true", 0.0, HCA)
    df["w"] = served_weight(df)
    df["pre_margin"] = SERVED_SLOPE * df.pre_diff + df.hca
    df["served"] = df.w * df.pre_margin + (1 - df.w) * df.pit_margin
    df["bucket"] = pd.cut((df.home_games + df.away_games) / 2.0, BUCKET_EDGES, right=False, labels=BUCKET_LABELS)
    df["d"] = df.pre_margin - df.pit_margin
    df["dq"] = df.groupby("season").d.transform(lambda s: pd.qcut(s.abs().rank(method="first"), 4, labels=False) + 1)
    return df


def lstsq_through_origin(X, y):
    coef, *_ = np.linalg.lstsq(X, y, rcond=None)
    return coef


def fit(train):
    """Per-bucket coefficients for the two calibrated arms."""
    out = {}
    for b in BUCKET_LABELS:
        t = train[train.bucket == b]
        if len(t) < 50:
            out[b] = {"stretch": 1.0, "free": (SERVED_SLOPE, 1.0)}
            continue
        pre_leg = t.w * (LEG_ALONE_SLOPE * t.pre_diff + t.hca)
        model_part = (1 - t.w) * t.pit_margin
        s = lstsq_through_origin(model_part.to_numpy()[:, None], (t.actual - pre_leg).to_numpy())[0]
        y = (t.actual - t.w * t.hca).to_numpy()
        X = np.column_stack([(t.w * t.pre_diff).to_numpy(), model_part.to_numpy()])
        a, c = lstsq_through_origin(X, y)
        out[b] = {"stretch": float(s), "free": (float(a), float(c))}
    return out


def predict(test, coefs):
    stretch = np.empty(len(test))
    free = np.empty(len(test))
    for i, (b, w, pre_diff, hca, pit) in enumerate(
        zip(test.bucket, test.w, test.pre_diff, test.hca, test.pit_margin)
    ):
        cb = coefs[b]
        stretch[i] = w * (LEG_ALONE_SLOPE * pre_diff + hca) + (1 - w) * cb["stretch"] * pit
        a, c = cb["free"]
        free[i] = w * (a * pre_diff + hca) + c * (1 - w) * pit
    test = test.copy()
    test["stretch"] = stretch
    test["free"] = free
    return test


def toward_model(df, arm):
    return (df.actual - df[arm]) * np.sign(df.pit_margin - df.pre_margin)


def z_score(d):
    return float(d.mean() / (d.std() / np.sqrt(len(d)))) if len(d) > 1 else float("nan")


def report(scored):
    out = {"games": len(scored), "arms": {}}
    base = (scored.served - scored.actual).abs()
    for arm in ARMS:
        err = (scored[arm] - scored.actual).abs()
        by_season = scored.assign(e=err, b=base).groupby("season")[["e", "b"]].mean()
        quartiles = {}
        for q in range(1, 5):
            sl = scored[scored.dq == q]
            quartiles[f"q{q}"] = {
                "n": len(sl),
                "mae": float((sl[arm] - sl.actual).abs().mean()),
                "toward_model": float(toward_model(sl, arm).mean()),
            }
        out["arms"][arm] = {
            "mae": float(err.mean()),
            "delta_vs_served": float((err - base).mean()),
            "z_vs_served": z_score(err - base) if arm != "served" else 0.0,
            "seasons_better": int((by_season.e < by_season.b).sum()) if arm != "served" else 0,
            "seasons": len(by_season),
            "bias": float((scored[arm] - scored.actual).mean()),
            "by_disagreement_quartile": quartiles,
        }
    return out


def main():
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    ap.add_argument("--legs", required=True, help="measure-blend-accuracy --dump CSV")
    ap.add_argument("--last-season", type=int, default=2026, help="last finished season")
    ap.add_argument("--out", help="write the summary JSON here")
    args = ap.parse_args()

    df = pd.read_csv(args.legs)
    df = prepare(df[df.season <= args.last_season].reset_index(drop=True))

    seasons = sorted(df.season.unique())[2:]
    scored, picks = [], []
    for s in seasons:
        coefs = fit(df[df.season < s])
        picks.append({"season": int(s), "coefficients": coefs})
        scored.append(predict(df[df.season == s], coefs))
    scored = pd.concat(scored, ignore_index=True)

    summary = {
        "walk_forward": report(scored),
        "final_fit_all_seasons": fit(df),
        "fold_coefficients": picks,
    }
    print(json.dumps(summary, indent=2))
    if args.out:
        with open(args.out, "w") as f:
            json.dump(summary, f, indent=2)


if __name__ == "__main__":
    main()
