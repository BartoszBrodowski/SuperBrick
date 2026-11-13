# M0 — spike results

All six spikes are done, plus the start of M1 (the `ldraw` crate). Measured on an Apple M2 (8 cores,
Metal). Each spike's details are in its own `FINDINGS.md`.

## Summary

| Spike | Question | Answer |
|---|---|---|
| M1 `crates/ldraw` | Can Rust read the library and models exactly like the bench? | **Yes.** 0 differences on 504 part meshes and 195 models (5,110 flattened parts). |
| S2 render (`crates/render`) | Can wgpu draw 10k+ parts smoothly? | **10k: yes** (13 ms/frame). **100k: not yet** (72–100 ms; 29 ms without edges at low LOD). |
| S3 relation (`crates/physics`) | Is a Rust `relation` fast enough? | **As a validator, yes**: full tortoise check in 0.35 s vs Python's 70.5 s. **Too slow for live drag-snapping**, which needs the planned `fast_legal`. |
| S4 golden (`tools/golden`) | Can Python dump reference answers for Rust parity? | **Yes.** 131 cases in 1.4 s, plus the design models with `--models`. |
| S1 ACP (`spikes/acp`) | Can opencode's ACP drive the CAD through our MCP, with images? | **Yes, 4/4 probes with gpt-6-luna**, once opencode is isolated from the user's global config. |
| S5 UI (`spikes/ui`) | Can egui_dock host two wgpu viewports plus streaming chat at 60 fps? | **Yes.** Vsync-locked 60 fps, ui() 2.4 ms CPU. |
| S6 search (`spikes/search`) | Can local search find the part you mean in < 0.5 s? | **Yes.** 86.3% top-1, MRR 0.90, 6 ms, with **potion-base-8m** (model2vec). |

## M1 `crates/ldraw` (a port of `bench-next/ldraw.py`)

- Opens LeoCAD's `library.bin` (37,099 files) in **32 ms** (target < 0.3 s), with 24,735 parts and 322 colours.
- **Parity** (`cargo test -p ldraw --test parity`, against `tests/fixtures/py_ref.json` from
  `make_geom_ref.py`): 504 meshes (std + low LOD) and 195 model flattenings, **0 differences**.
- **Lossless I/O:** 195 real `.ldr`/`.mpd` files re-save byte-identical. Edited part lines parse back
  to the same parts. 60,000 synthetic part lines also round-trip.
- **10k-part model:** parse 5.8 ms, flatten 2.3 ms, 20 MB RSS. 50k: 29 ms + 12 ms, 43 MB.
- `cad` CLI: `stats`, `roundtrip`, `flatten`, `mesh`, `synth`, `render`, `relbench`.

## S2 render — wgpu 30, one shared vertex atlas, instanced draws

| Model | Parts | Baked studs | Culled studs | Culled, low LOD, no edges | First frame | RSS |
|---|---|---|---|---|---|---|
| tortoise | 1,580 | 6.1 ms | **4.0 ms** | — | 125 ms | — |
| synth10000 | 10,000 | 26.9 ms | **12.9 ms** | — | 997 ms¹ | — |
| tortoise ×7 | 11,060 | 31.7 ms | **13.2 ms** | 7.3 ms (no edges) | 223 ms | — |
| synth100000 | 100,000 | 149 ms | 71.7 ms | — | 861 ms | 309 MB |
| tortoise ×64 | 101,120 | 170 ms | 100 ms | **29.4 ms** | 946 ms | 312 MB |

¹ includes 527 ms of one-time GPU device setup.

- **Covered-stud culling** is the default. It removes 99.97% of studs in a bonded wall and halves
  frame time. It changes 0.01–0.3% of pixels (slivers at round rims) because the box cover test
  isn't exact. The real fix is culling from the connection graph: a stud with a link is covered.
- **It is vertex-bound, not fill-bound:** 800×500 costs the same as 1600×1000. Edges are ~45% of the frame.
- **To make 100k smooth (M2/M3):**
  - LOD by screen size: low-LOD primitives plus a box impostor under a few pixels.
  - Hide parts fully enclosed by neighbours, using the connection graph.
  - Draw edges only near the camera, or decimated.
  - GPU frustum culling per instance.

  Each of these is independent, and together they should bring 100k inside 16 ms.
- Fixed a conditional-line shader bug that drew stray lines toward the screen centre.

## S3 relation — `physics::relation` (port of `physics.relation`)

`cad relbench` (scenarios from `crates/physics/tests/scenarios.json` vs `fixtures/py_rel.json`;
models from `tools/golden/out/design/*.json`):

| | Rust | Python | |
|---|---|---|---|
| brick on brick | 1.03 ms | 12.3 ms | 12× |
| brick on 32×32 baseplate | 1.10 ms | 418 ms | 382× |
| typical pairs (16 scenarios) | 0.05–2.5 ms | 1–29 ms | 8–20× |
| **tortoise, same 10,796 pairs** | **1.69 s** (1 thread) / **0.35 s** (8) | 70.5 s | **42× / 201×** |
| skilltest_helmet, 1,512 pairs | 0.68 s / 0.27 s | 13.7 s | 20× / 52× |
| tortoise broad phase (sweep on x) | 3.1 ms → the same 10,796 pairs | — | |

- **Agreement with Python on tortoise:** collides 99.99% (1 pair), stud links 99.52%, contact 100%.
  Helmet: collides 100%, studs 98.5%.
- The differences are all connectors Python gets from **LDCad shadow data** (`snaps.py`), which the
  voxel-only port doesn't read yet: hollow-stud rounds (3062b), side studs, plate 2653. Porting
  `snaps` is M2 scope.
- **Not ported:** Python's narrow phase for non-lattice pairs (tilted30: Rust 424 LDU³ vs Python
  342), compliance margins and gears.
- **Drag-snapping implication:** ~45 ns per grid point and ~3,400 points per pair gives
  **~6 relation calls per 6 ms frame per thread (~45 on 8 threads)**. That's fine for validating on
  drop, but too slow to test every candidate pose during a drag. That confirms the plan's split:
  - `fast_legal` while dragging: precomputed per-shape voxel bitsets on the stud lattice, so a pair
    test is a shifted bitset AND (µs);
  - full `relation` on drop, and incrementally in the background linter.

## S4 golden dumper — `tools/golden/dump.py`

- Wraps `physics.relation` and `snaps.match` while the bench's own validate runs. For each case it
  writes the parts (bodies + world connectors), every pair (relation + raw match), the validate
  result and the expectations.
- `connections` (129 cases) + `fixes` (2) in 1.4 s, and all 129 verdicts match the bench.
- `--models` adds the design models and 40866: 138 cases in 91 s, 20 MB. This is what `relbench`
  compares against.

## S1 ACP + MCP — `spikes/acp`

- **openai/gpt-6-luna, 4/4 PASS:**
  - our MCP tools are reachable, both via `session/new` `mcpServers` and via config;
  - images arrive in the prompt ("Blue circle") and in tool results ("Green triangle");
  - the sandbox refuses writes.
- **Security finding:** `OPENCODE_CONFIG` *merges* with `~/.config/opencode/opencode.jsonc`, so the
  agent could reach the user's composio remote-bash MCP. The fix runs opencode with:
  - `XDG_CONFIG_HOME` → a private dir;
  - the config passed via `OPENCODE_CONFIG_CONTENT`;
  - project config and Claude Code import disabled;
  - only `provider` blocks copied from the user's config.

  The embedded agent must always run this way.
- **claude-agent/haiku:** not tested. The Claude CLI isn't logged in (`claude auth login` is the
  user's step).
- M7 todo: check whether opencode's code-mode `execute` → `fetch` can reach http(s).

## S5 UI shell — `spikes/ui`

- egui 0.36 + egui_dock 0.21 + eframe/egui-wgpu on **the same wgpu 30 as `crates/render`**, so
  there's no second GPU stack.
- Layout: toolbar (tools s/d/p/g/c, + Subbuild), Parts | Build + Subbuild | AI, every panel closable.
- **60 fps (vsync), p95 18 ms**, with both viewports orbiting 11,060 + 286 parts and the chat
  streaming markdown. ui() takes 2.4 ms CPU. Viewports only redraw when dirty.
- Gotcha fixed: egui expects a linear view of an sRGB target, otherwise the models render too dark.

## S6 search — `spikes/search`

- **Rebrickable ↔ LDraw:** 93.5% of real-set part usage maps (exact 84.4%, moved 4.0%,
  print→base 3.9%, mould/relationship 1.1%). 204 of 275 colours map. That's enough for "only parts
  that exist in this colour" and popularity ranking.
- **Encoders** (168 golden queries, tuned weights):

  | | top-1 | MRR | p50 | RAM |
  |---|---|---|---|---|
  | BM25F + priors | 84.5% | 0.886 | 0.22 ms | 98 MB |
  | + potion-base-8m | 86.3% | 0.903 | 6.1 ms | 180 MB |
  | + MiniLM-L6 | 86.3% | 0.897 | 15.7 ms | 264 MB |

- **Decision: potion-base-8m.** It ties MiniLM's quality, is 2.5× faster with 85 MB less RAM, and
  indexes in 0.5 s vs 226 s.
- General encoders add little. Popularity priors and the LEGO synonym table do most of the work.
- The path to 90% top-1 is targeted ranking fixes: family-canonical prior, technic length, printed
  assembly popularity. After that, optionally fine-tune potion on LEGO name pairs.

## Changes to the plan

1. **Rendering:** covered-stud culling is the default. Add an LOD/occlusion milestone item before
   claiming 100k (M3). 10k already meets the M1 target.
2. **Physics:** confirm `fast_legal` (voxel bitsets) as a separate drag-time path. `relation` stays
   the authoritative validator. Port `snaps.py` (shadow connectors) in M2 to close the last 0.5–1.5%.
3. **AI:** the embedded opencode always runs isolated (see S1). The ACP + Unix-socket MCP shim design holds.
4. **Search:** model2vec (potion-base-8m) via a plain lookup table, so candle isn't needed for
   search. Candle/MiniLM is dropped from the app.

## Reproduce

```bash
cd cad
cargo test -p ldraw --release                      # unit + Python parity
cargo build --release -p cad-cli
./target/release/cad stats
./target/release/cad roundtrip ../runs ../bench-next/tests ../reference ../instructions
./target/release/cad synth --parts 10000 --out /tmp/cad-synth/synth10000.ldr
./target/release/cad render /tmp/cad-synth/synth10000.ldr --studs all
./target/release/cad relbench                      # needs tools/golden/out/design (dump.py --models)
python3 tools/golden/dump.py --models              # regenerate golden dumps
(cd spikes/search && ./target/release/search-spike eval --enc potion-base-8m --fails)
(cd spikes/ui && cargo run --release -- --bench 600)
(cd spikes/acp && cargo run --release -- run --model openai/gpt-6-luna)
```
