#!/usr/bin/env python3
"""Write tests/fixtures/py_ref.json: what bench-next/ldraw.py says about part meshes and model
flattening, for the Rust crate's parity test (tests/parity.rs).

    python3 crates/ldraw/tests/make_geom_ref.py      # from cad/, after changing either side

Meshes go through Library._flatten directly so nothing is written to the bench's geometry cache.
"""
import json
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]  # .../lego
sys.path.insert(0, str(ROOT / "bench-next"))

import numpy as np  # noqa: E402
import ldraw  # noqa: E402

MODEL_DIRS = ["runs", "bench-next/tests", "reference", "instructions"]

# Parts worth checking even if no model uses them: studs of every kind, SNOT, hinges, technic,
# minifig, round and printed parts, moved-to aliases.
EXTRA_PARTS = """
3001 3003 3004 3005 3009 3010 3020 3022 3023 3024 3031 3068b 3069b 3070b 3062b 4073 6141 3040b
3039 3665 3298 3048 2420 3710 3622 3666 3795 3958 4589 3794b 15573 87087 11477 15068 98138 2357
3701 3700 3702 32523 32062 6536 3673 2780 32316 32140 2429c01 3937 3938 2436a 99780 99781 44728
3626c 973 3815 4070 30414 2877 87079 3960 4740 6143 3941 61252 4085c 3747b 3176 60470 4274 30136
32000 48336 3832 3460 2445 6636 30357 92946 85984 54200 50950 11153 61409 2654 3680 3679 18654
87580 4282 41539 4286 3062a 3626bpb0001 3068bpb0001 4150p01 6014 6015 30027b 30028 2342 3829c01
""".split()


def hist(codes):
    vals, counts = np.unique(np.asarray(codes, np.int64), return_counts=True)
    return [[int(v), int(c)] for v, c in zip(vals, counts)]


def summary(g: "ldraw.Geom") -> dict:
    v = g.tris.reshape(-1, 3).astype(np.float64)
    lv = g.lines.reshape(-1, 3).astype(np.float64)
    lo, hi = g.bbox(exclude_studs=True)
    return {
        "tris": int(len(g.tris)),
        "tstud": int(g.tstud.sum()),
        "lines": int(len(g.lines)),
        "cond": int(len(g.cond)),
        "tcol": hist(g.tcol),
        "lcol": hist(g.lcol),
        "ccol": hist(g.ccol),
        "sum": v.sum(0).tolist() if len(v) else [0.0, 0.0, 0.0],
        "sq": float((v * v).sum()),
        "abs": float(np.abs(v).sum()),
        "lsum": lv.sum(0).tolist() if len(lv) else [0.0, 0.0, 0.0],
        "bbox": [list(map(float, lo)), list(map(float, hi))],
        "studs": [
            [*map(float, p), *map(float, d), bool(h)]
            for p, d, h in zip(g.stud_pos, g.stud_dir, g.stud_hollow)
        ],
    }


def model_files():
    out = []
    for d in MODEL_DIRS:
        base = ROOT / d
        if base.exists():
            out += [p for p in base.rglob("*") if p.is_file() and p.suffix.lower() in (".ldr", ".mpd")]
    return sorted(out)


def main():
    lib = ldraw.get_library()
    models, used = {}, set()
    for f in model_files():
        rel = str(f.relative_to(ROOT))
        try:
            refs = ldraw.flatten_model(ldraw.load_model_file(str(f)), lib, base_dir=f.parent)
        except Exception as e:  # noqa: BLE001 - a file the bench can't read is reported, not fatal
            print(f"skip {rel}: {e}", file=sys.stderr)
            continue
        models[rel] = [
            [r.name, int(r.color), [float(x) for x in r.pos], [float(x) for x in np.asarray(r.rot).reshape(-1)],
             int(r.step), r.meta.get("path", "")]
            for r in refs
        ]
        used.update(ldraw.Library.norm(r.name) for r in refs)

    parts = {}
    names = sorted(used | {ldraw.Library.norm(n + ".dat") for n in EXTRA_PARTS})
    for name in names:
        entry = {}
        for lod in ("std", "low"):
            key = lib.resolve(name, lod)
            entry[lod] = None if key is None else summary(lib._flatten(key, lod, {}, 0))
        parts[name] = entry

    out = HERE / "fixtures" / "py_ref.json"
    out.parent.mkdir(exist_ok=True)
    out.write_text(json.dumps({"parts": parts, "models": models}, separators=(",", ":")))
    missing = sum(1 for e in parts.values() if e["std"] is None)
    print(f"wrote {out.relative_to(ROOT)}: {len(parts)} parts ({missing} unknown), {len(models)} models, "
          f"{sum(map(len, models.values()))} flattened parts")


if __name__ == "__main__":
    main()
