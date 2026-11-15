# Removing Python from SuperBrick: assessment

Written 2026-11-14. Nothing has been ported yet. This is the estimate and the plan.

## Headline

About **6,000 lines of Python and 90 lines of C++** still run inside SuperBrick. Replacing them takes
about **7,000–9,000 lines of Rust**, roughly **12–16 person-weeks**. With the parity harness that already
exists, that is about **15–20 focused agent sessions**.

- Two parts carry most of the risk: the connector engine (`snaps.py` and `physics.py`) and the
  instructions planner.
- Each part ports on its own. Each switches over once it agrees with the Python on the corpus.
- Python is dropped last, by deleting `sb setup`'s download.

## What still runs Python

SuperBrick starts `ldraw::runtime::python_command()` in two places.

**Connector and checker service** (`crates/engine/src/connectors.rs` and `connectors.py`). This is a
persistent process, with three instances: `SERVICE`, `PICK_SERVICE` and `CHECK_SERVICE`. It loads the
bench-next modules that `tools/install.py` stages into `share/superbrick/connectors/bench`. It serves:

- `validate`: the checker behind `verify`, edit refusal and warnings, the GUI Checker panel, `select`,
  and the agent's `check` call;
- `check_candidates`: filters `where` candidates;
- `drag`, `connect`, `destinations`, `endpoints`, `compatible`: connector placement for `where`,
  `connect`, `hinge` and GUI drags;
- `analyze`: joints and degrees of freedom, used for `hinge` and strength;
- `rotate` and `mirror_specs`.

**Publishing worker** (`app/src/publication.rs` and `publication.py`). This is a child process per
`publish` job. It does two things:

- plans and renders building instructions (`kind:"instructions"`);
- path-traces studio renders (`kind:"studio"`), through `studio_rays.dylib`, a small C++ BVH loaded
  with ctypes.

`sb setup` downloads a relocatable CPython (python-build-standalone) and installs NumPy, Pillow and SciPy
into it (`crates/ldraw/src/runtime.rs`, 320 lines).

## Sizes, effort and risk per part

| # | Part | Python today (lines) | Rust already there | New Rust (est.) | Effort | Risk |
|---|------|---------------------|--------------------|-----------------|--------|------|
| 1 | Connector data: LDCad shadow parsing, connector instancing, matching | `snaps.py` 1,307 | none | 1,800–2,200 | 3–4 wk | **high** |
| 2 | Pair physics: keep-outs, links, conflicts, tilted narrow phase, compliance, gears | `physics.py` 907, `cad_contact_policy.py` 95, `cad_snap_rules.py` 24 | `crates/physics` 793 (shapes, stud grid, anti-studs, stud-only relation) | 1,200–1,600 | 2–3 wk | high |
| 3 | Checker: statuses, groups, LOOSE, asserts, verdict, report | `validate.py` 396, parts of `scene.py`/`orient.py` (~150) | `checker.rs` types (the report shape stays), native `validation.rs` 284 | 500–700 | 1 wk | low, given 1–2 |
| 4 | Placement service: drag, connect, hinge, analyze, mirror, rotate | `connectors.py` 495 | `placement.rs` (stud seats), `connectors.rs` client | 900–1,200 | 1.5–2 wk | medium |
| 5 | Studio renderer | `publication.py` studio, Rays, environment (~220), `studio_rays.cpp` 91 | wgpu `crates/render` (raster only) | 600–900 | 1–1.5 wk | low–medium |
| 6 | Instructions planner and pages | `publication.py` rest (~1,100: `assembly_plan` 518, subbuild sharing, congruence, framing, pages), `render.py` 363, `raster.py` 126, part of `ldraw.py` | `RenderService` (`crates/render/src/service.rs`), `publication.rs` job plumbing | 2,000–2,600 | 3–4 wk | **high** |
| 7 | Drop Python: `sb setup` download, staging, release checks | `runtime.rs` Python half (~200 Rust), `install.py` `BENCH_FILES`, `release.py` | n/a | (deletions) | 1–2 d | low |

`ldraw.py` (687 lines) does not need porting. Its job (library, colours, geometry, flattening) is
already done by `crates/ldraw`. The ports only need the Python and Rust to agree on canonical part names
and on the geometry they sample.

## Why parts 1 and 2 are the hard part

- **`snaps.py` encodes the bench's reading of the LDCad shadow library:**
  - `SNAP_CYL` sections, grids, caps, `center`, mirroring, `SNAP_INCL` references and `shadow_extra`
    overrides;
  - the matching rules for each link type: stud, side-stud, tube, pin-hole, axle, clip-bar, bar,
    hinge (fingers), ball and gen;
  - `combine_dof`.

  Every small difference changes which pairs count as connected. That flips OK to FLOATING, LOOSE or
  UNVERIFIED.
- **`physics.relation` decides collisions:** solid overlap after connector keep-outs ≥ `COLLIDE_MIN`.
  It also decides conflicts: studs not pressed home, blocked or covered connectors. Results sit right at
  the tolerances (`EPS`, `CONNECT_TOL`, keep-out padding), so the Rust must sample the same points.
  Agreeing on the verdict alone is not enough.
- **The bench is still moving:**
  - this week it got the box sweep, the stud and keep-out prefilters, and the FGR centring rule;
  - the run_connections fixtures depend on the old FGR offset (see `bench-next/CHANGES.md`).

  Port from a tagged bench-next, and run the parity gate on every later bench change.

## Why part 6 is the other hard part

- **`assembly_plan` (518 lines) is heuristics tuned on real sets:** peel order, base first, courses,
  sub-builds finished before they attach, repeats shown once as ×N.
- **It calls `physics.relation` to test removal directions**, so it can only move after parts 1–2.
- **Another session is actively rewriting it** (instructions-planner). Port it only once that work
  settles.
- **The page look comes from the numpy rasteriser:** edges, outlines and perspective framing in
  `render.py`/`raster.py`, plus the C++ `cad_rasterize`. Re-create it on the wgpu `RenderService` with
  an edge/outline pass rather than port the numpy code. Expect a round of visual review against current
  pages.

## Order

1. **Connector data in Rust** (part 1), as a new crate, e.g. `crates/connect`.
   - **Gate:** for every part in the corpus and the run_connections fixtures, Rust and Python produce
     the same instanced connectors (type, position, axis, grid, flags). Add a dump action to the
     Python service for this, and compare with a `sb-dev` subcommand.
2. **Pair relation** (part 2), extending `crates/physics`.
   - **Gate:** the same `relation()` numbers for every pair the checker evaluates on the 6-model corpus
     (overlap, links with types and dof, conflicts, gears) within tolerance. Also
     `bench-next/tests/run_connections.py`'s 129 cases, re-expressed as Rust tests.
3. **Checker** (part 3). Switch `engine::checker::check` to native, and keep the Python as
   `sb-dev parity`'s reference.
   - **Gate:** `sb-dev parity` passes with 0 differences on the corpus. It already does with the Python
     checker.
   - **Win:** cold checks of the 644-part house take 6.6 s in Python and should drop well under 1 s.
     The per-pose relation cache becomes an in-process map.
4. **Placement service** (part 4). Then delete `connectors.py` and the `SERVICE`/`PICK_SERVICE`
   backends.
   - **Gate:** `crates/engine/tests/connectors.rs` and the ops tests for `where`, `connect` and `hinge`,
     plus a recorded set of drag queries compared against Python.
5. **Studio renderer** (part 5), on a CPU path tracer with a BVH in Rust, or a wgpu compute pass. Then
   drop `studio_rays.cpp`.
6. **Instructions planner and pages** (part 6), after the planner rewrite has settled.
   - **Gate:** the same `plan.json` (steps, groups, issues) for the corpus, then a visual review of
     the pages.
7. **Remove Python** (part 7):
   - delete `install_python`, `PYTHON_URL`/`PYTHON_PACKAGES` and `python_command()`;
   - delete the bench staging in `install.py` and its checks in `release.py`;
   - delete the Python step in `sb setup` and the GUI setup screen.

## Outside SuperBrick (not in scope)

These keep using a system `python3`. None of them is loaded by SuperBrick itself.

- The skills' helper scripts:
  - lego-design's `clumps.py`, `sculpt.py`, `terrain.py`, `dome.py` and `blockout.py` (they emit
    `edit` ops files);
  - bftb-transcribe's `booklet.py` and `lint_text.py`.
- The bftb-blind-test skill: its tactile bench, `compare` and grading.
- bench-next itself, which stays as the parity reference until step 3 is done. After that it stays only
  as the blind test's frozen copy.
