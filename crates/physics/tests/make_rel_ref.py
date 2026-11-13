#!/usr/bin/env python3
"""Run bench-next/physics.py's relation() on tests/scenarios.json and write tests/fixtures/py_rel.json
(results, per-call timings, shape summaries) for `cad relbench --ref`.

    python3 crates/physics/tests/make_rel_ref.py      # from cad/

Shapes and geometry are cached in a temporary BENCH_CACHE, so the bench's own cache is untouched.
"""
import json
import os
import sys
import tempfile
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
os.environ["BENCH_CACHE"] = tempfile.mkdtemp(prefix="cad-relref-")
sys.path.insert(0, str(ROOT / "bench-next"))

import numpy as np  # noqa: E402
import physics  # noqa: E402


def body(side):
    name, pos, rot = side
    return physics.Body(physics.get_shape(name), np.array(rot, float).reshape(3, 3), np.array(pos, float))


def timed(fn, budget=0.4, max_reps=2000):
    fn()
    ts, start = [], time.perf_counter()
    while len(ts) < max_reps and (time.perf_counter() - start < budget or len(ts) < 5):
        t = time.perf_counter()
        fn()
        ts.append(time.perf_counter() - t)
    return float(np.median(ts) * 1e6)


def hull(h):
    return [float(np.nansum(h[0])), float(np.nansum(h[1])), int(np.isnan(h[0]).sum())]


def main():
    sc = json.loads((HERE / "scenarios.json").read_text())["scenarios"]
    out, shapes = [], {}
    for s in sc:
        for side in (s["a"], s["b"]):
            if side[0] not in shapes:
                t = time.perf_counter()
                sh = physics.build_shape(physics.get_library().resolve(side[0]) or side[0])
                ms = (time.perf_counter() - t) * 1e3
                shapes[side[0]] = {
                    "build_ms": ms, "lo": sh.lo.tolist(), "hi": sh.hi.tolist(), "studs": int(len(sh.studs)),
                    "anti": sh.anti.tolist(), "hy": hull(sh.hy), "hx": hull(sh.hx), "hz": hull(sh.hz),
                    "full_lo": sh.full_lo.tolist(), "full_hi": sh.full_hi.tolist(),
                }
        A, B = body(s["a"]), body(s["b"])
        r = physics.relation(A, B)
        res = None if r is None else {
            "overlap": float(r.overlap), "raw": float(r.raw_overlap), "contact": bool(r.contact),
            "ab": int(r.studs_ab), "ba": int(r.studs_ba), "links": [m.type for m in r.links],
            "conflicts": [c.why for c in r.conflicts] if r.conflicts and hasattr(r.conflicts[0], "why") else [str(c) for c in r.conflicts],
            "narrow": bool(r.narrow),
        }
        us = timed(lambda: physics.relation(A, B))
        out.append({"name": s["name"], "result": res, "us": us})
        print(f"{s['name']:<24} {us:9.0f} us  {res}")
    (HERE / "fixtures").mkdir(exist_ok=True)
    (HERE / "fixtures" / "py_rel.json").write_text(json.dumps({"scenarios": out, "shapes": shapes}, indent=1))


if __name__ == "__main__":
    main()
