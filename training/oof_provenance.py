"""OOF snapshot fingerprint — the provenance stamp shared by every model
trained on the roster-impact feature frame.

**No trainer imports this any more.** `provenance.py` (#223) generalized the
mechanism to every edge of the dependency tree, and the Layer 2 trainers now
project their `oof_provenance` block out of that larger stamp. What survives
here is the frozen #218 construction, kept deliberately as an *independent*
reference implementation: `test_provenance.py` asserts the generalized helper
still reproduces this file's digests bit for bit. Fold this into `provenance.py`
and that test compares the code to itself, at which point nothing catches the
one change that would make the API refuse to boot. Do not "clean this up".

Why this exists (issue #218): `train_roster_adjo_model.py` reuses
`build_dataset` from `train_roster_impact_model.py`, so the two models share
a training frame whose player cam values come from `trajectory_oof_predictions`
/ `freshman_oof_predictions`. That shared-code design created a false intuition
that AdjO auto-updates when the OOF is regenerated — it does not, it needs its
own trainer invocation. It silently fell three OOF generations behind (#130,
#152, #211) while still loading fine, because the feature contract never
changed.

The fix is a stamp the boot validator can compare: each model records the
fingerprint of the OOF tables *as they were when it trained*. Two models with
different stamps were trained on different snapshots, regardless of how or when
they were produced — `cstat_core::inference` refuses to boot on a mismatch.

Fingerprint contents: row count plus an order-stable md5 over
`(key, target_season, mean)` per table.

- `mean` only, not `lower`/`upper`. The roster frame consumes only the mean
  (`COALESCE(traj.mean, fresh.mean, actual)`), so a band-only regen must not
  trip a mismatch it cannot have caused.
- `created_at` is excluded on purpose. A regen that reproduces identical
  predictions IS the same snapshot for training purposes; stamping the
  timestamp would flag deterministic re-runs as drift.
- The value is rounded to 6dp so REAL-to-text formatting can't make an
  identical snapshot hash differently.
"""
from __future__ import annotations

from sqlalchemy import text

from db import get_engine

# (table, key column) — the two held-out prediction sources build_dataset
# COALESCEs into the projected cam_v3 channel.
_OOF_TABLES = (
    ("trajectory_oof_predictions", "torvik_pid"),
    ("freshman_oof_predictions", "cstat_player_id"),
)


def oof_provenance() -> dict:
    """Fingerprint the OOF tables backing the roster-impact training frame.

    Returns a dict safe to embed in a model meta JSON and compare verbatim
    against another model's stamp.
    """
    eng = get_engine()
    stamp: dict = {}
    with eng.connect() as conn:
        for table, key in _OOF_TABLES:
            row = conn.execute(
                text(
                    f"SELECT count(*) AS n, "
                    f"md5(coalesce(string_agg("
                    f"  {key}::text || ':' || target_season::text || ':' "
                    f"    || round(mean::numeric, 6)::text, ',' "
                    f"  ORDER BY {key}, target_season), '')) AS digest "
                    f"FROM {table}"
                )
            ).one()
            stamp[table] = {"n_rows": int(row.n), "digest": row.digest}

    print(
        "  OOF provenance: "
        + " | ".join(
            f"{t} n={stamp[t]['n_rows']:,} md5={stamp[t]['digest'][:12]}…"
            for t, _ in _OOF_TABLES
        )
    )
    return stamp


if __name__ == "__main__":
    import json

    print(json.dumps(oof_provenance(), indent=2))
