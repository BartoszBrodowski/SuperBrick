# Implementation ledger

## Assembly recovery and opposite-arm placement (2026-11-11)

- Recovered the 20:01 autosave (530 pieces) without overwriting the older `model.ldr`;
  on explicit request renamed the recovery copy to `~/np.ldr`.
- Identified the arm as the 122-piece connected side of shoulder joint 403–409,
  excluding that ball joint. Added an opposite-side copy at the free shoulder ball 402.
  All original 530 source lines/bytes were preserved; resulting file has 652 pieces.
- Mirror checks verified standard surfaces/colours and actual left/right substitutions.
  Equivalent tessellations now permit two-way vertex/surface checks rather than requiring
  identical triangulation vertices. The socket 92013 did not pass exact mesh symmetry:
  this particular edit instead used the same real socket in a proper rotation, independently
  verifying every reflected connector position/type/gender/section. This is not a general
  exemption in the Inspector mirror tool.
- Reparsed the serialized candidate and checked all nearby pairs with the full backend:
  zero collision/seating flags; confirmed Ball connection 402–531. No new hinge posing.
  Atomic save only after unchanged-source check and exclusive backup
  `~/np.before-mirrored-arm-20261111-203711.ldr`.
- Assembly clipboard/mirror tests cover whole-group placement/undo, 128-part copies,
  nested MPD instance colours, actual chiral counterpart 41770a/41769a, and refusal of
  unsupported parts. Installed-binary MCP mirror proposal also passed without source writes.
- Separately, the formerly failing 530-piece instruction snapshot now plans successfully
  through the installed CLI: 507 steps / 40 groups in 132.46 seconds, every part added once.
  Fixed no-axis spherical-joint withdrawal crash and added connected obstacle-closure search;
  final legality and sampled insertion checks remain mandatory. Independent components are
  still reported, not joined fictitiously.

## Instruction readability and construction locality

Perspective follow-up: the earlier angle-only change was still orthographic.
Instruction cover, steps and trays now use a true pinhole camera (40-degree
nominal field of view), geometry-level foreshortening and reciprocal z-depth,
not a 2D image warp. Visibility scoring and connector arrow projection use the
same perspective transform. Arrowheads scale to 2.4% of image width with a
substantial triangular tip; outlines expand by four additional pixels at
1600px. A depth/foreshortening regression distinguishes this from rotated
orthographic rendering. All 26 publication tests passed; release reinstalled.
The full 385-page perspective version is rendering in a separate fresh folder.

Follow-up: automatic instruction views are restricted to oblique angles (no
straight-on front/side/top views like the old step 15), while preserving
visibility-based selection and stable views. Insertion arrows are red.
The 25-test publication suite passed, including rejecting a previously selected
straight-on camera. The app was rebuilt/reinstalled and all 385 checked pages
are being reillustrated at 1600px without changing the build sequence or model.

Stopped the older supersampled render on request. The native fast renderer
completed all 412 original-plan images at 1600px in 213.88 seconds, retaining
the checked plan and source bytes. This timing predates adaptive views.

Instruction illustrations now retain original colours and outline additions
in red. Low-resolution visibility scoring chooses a different view when the
placement is obscured; the previous view is retained when sufficiently clear.
The actual old step 41 example changed to a view with 90.5% added-part visibility.
Repeated old steps 94–97 became one 4× page. Pages use quantities/visual trays
instead of part IDs and repetitive captions. Detached subbuilds are displayed
in a local flat frame; joins retain the checked world pose and a base-plane
tilt callout where useful (not a calibrated signed hinge-angle measurement).

Checked reverse removal prefers nearby/touching operations. A general
postprocessor moves later child additions before attachment only after
rechecking the enlarged child join and affected insertion paths. Articulated
branches are also preferred as completed subbuilds during planning. These are
bounded heuristics, not a guarantee that every human-preferred ordering is found.
Independent repeated additions may merge while retaining their validated order;
connected stacks do not merge. One-piece setup/join pairs collapse to normal
attachment steps. Unconfirmed loose parts are labelled explicitly.

A narrowly scoped 33078/clip interpolation recognizes the curved sausage grip
only with compatible authored curve tangents, original physical contact, no
conflicts and overlap below the unchanged cutoff. It adds no collision keep-out
and does not move the model. This addresses the saved sausage's unrecognized
grip rather than fabricating an attachment to any nearby piece.

Verification: 93 Rust tests, 24 publication tests, six installed CLI model
smokes and an explicit high-quality smoke passed. Tests cover occluded views,
normal-colour outlines, locality, repeat/stack distinction, general subbuild
reordering with physical-obstruction rejection, singleton collapse and the
curved grip. The revised full-model ordering is being checked separately;
the earlier 412-step result below does not establish its final page count.

The installed revised planner subsequently completed the real 401-piece
snapshot in 62.52 seconds: 385 steps, 35 registered groups, every part added
exactly once and no single-piece join pages. Curved-grip recognition reduced
independent components from six to five; three loose singleton pieces remain
explicitly unconfirmed, not silently attached. The source file was unchanged.
Merged additions now receive separate insertion arrows instead of an arrow at
their average position. A fresh 1600px booklet is rendering from this newly
checked plan; the earlier complete fast booklet uses the older style/order.

## Complete-model headless instruction verification

The installed `cad ~/model.ldr --generate-instructions --output NEW_DIRECTORY
--image-size 128` now completes the repaired 401-piece model: 412 numbered
steps, 27 registered subbuild groups, an HTML booklet and all 412 step images.
Every source piece is added exactly once. The first successful end-to-end run
took 515 seconds; cached blocked withdrawal paths, articulation-point pruning
and native BVH triangle-parity queries reduced the repeat run to 153.5 seconds.
Native parity is tested against the oracle's same five rays and tolerances;
collision thresholds and final-pose/insertion checks were not waived.

Six installed CLI smoke cases also generate actual booklets: ordinary stacks,
SNOT, tilted SNOT, independent assemblies, a nine-piece SNOT shell and paired
hinges. Existing output directories are refused and source bytes preserved.
The 52-piece real head was separately verified (58 illustrated steps). Current
verification: 93 Rust tests and 16 Python publication tests pass. The planner
still reports the six independently connected components in this snapshot;
it does not invent missing joins or certify hand access/stability.


## Headless instructions and validated model alignment repair

`cad MODEL --generate-instructions [--output NEW_DIRECTORY] [--image-size N] [--plan-only]` now runs without the GUI. Output directories are exclusive/new; source bytes are checked unchanged. Diagnostics and numbered HTML/PNG instructions use the same worker as the panel. Six installed-binary smoke cases cover ordinary stacks, SNOT, tilted SNOT, independent assemblies, a nine-piece connected SNOT shell requiring separate halves, and paired hinges. All check complete per-piece coverage, actual image/document creation, and refusal to overwrite outputs. Python publication tests: 15 passing; Rust workspace: 93 passing.

Planner changes include local-frame stud clustering, checked nearby clear staging positions instead of travel to infinity, connector-frame withdrawal directions (including radial clip openings), recursive connected partitioning, trapped-core reuse, and cached successful/failed subplans. The real 52-piece head subassembly generated a complete 58-step illustrated document; every part is added once. Larger-body/full-model verification is tracked separately rather than inferred from this subassembly success. Legality and sampled-clearance gates remain strict; this is not a continuous physical/stability proof.

The user explicitly authorized repairing the existing `~/model.ldr` alignment. Connector diagnostics confirmed one of the two bracket attachments was only a near match (about 1.046 LDU lateral discrepancy). Exactly one type-1 line, piece 60 (`93273.dat`), was changed: 1.412737° rotation and 0.739674 LDU origin displacement. Both bracket connectors and the two links to the neighbouring handle remain engaged; full nearby-pair review reports zero errors for all 401 pieces. The original is preserved byte-for-byte at `~/model.before-connector-repair-20261111-174339.ldr`; repaired SHA256 `9650b6a3c149af0ae4814b10a9d90105d28719cc2993af74577dc3e503d4a2f3`. No other source lines were changed. Reopen the model instead of saving a stale open copy over the repair.

## Connect startup, assembly components, and neck pivots

Connect now loads endpoints without constructing the full connection graph.
Endpoint/compatibility requests use an independent helper, so they cannot queue
behind full mating or drag requests. Destination markers filter visibility and
connector compatibility; only the selected mating pose is collision checked.
This intentionally replaces the slow all-destination validation pass. On the
reported cached 401-piece input, cold endpoint loading took 0.61s and warm family
filtering 0.03s. Explicit stud mating, pin/axle/finger mating and installed MCP
mechanical smoke remain tested.

The reported instruction snapshot has components of sizes 345, 52, 1, 1, 1, 1.
The old global connected-remainder test could never start. Components now plan
separately and emit an explicit note: no missing final join is invented. Peeling
uses vectorized neighbour filtering, cached sampled collision results, early
deterministic removals, individual-part fallback before bounded repartitioning.
This fixes the disconnected-scene refusal, not general flexible/hinged assembly
planning; a complete sequence for the reported complex snapshot is not yet
established. Final-pose and sampled insertion checks remain mandatory.

H exposes genuine remote joints across incidental single-stud pivots. The cached
neck's 92013/57909 spherical connector centres are 2 LDU apart, beyond the existing
1.2-LDU engagement tolerance: no Ball edge is invented. H reports nearby unseated
ball/socket pairs and offers an explicit collision-checked seating command.
The simple isolated fixture seats and then exposes the Ball pivot. In the full
cached model, the attempted seating is blocked by other geometry; failed Connect
now reports piece IDs/names and overlap rather than blaming connector selection.
No private model or cached job has been altered.

Verification: 93 Rust tests and 11 publication tests pass; release rebuilt and
installed; installed mechanical smoke passes without modifying its source.

## Strict geometry review (supersedes the contact-warning exemption)

The previous nonblocking threshold-contact exception has been withdrawn. A
confirmed connection no longer waives any threshold overlap. Shared checker
and instruction preflight now densely resample small contacts against standard
triangles at 0.25/0.125 LDU, retaining connector keep-outs/compliance and using
`max(estimates) + 2*abs(delta)` as the guarded volume. Capped/chunked work keeps
memory bounded; exhausted budget retains the original flag. This is improved
sampled geometry, not mathematical clearance certification.

An actual coarse-volume false positive on two simple bricks is cleared in a
regression. The user's 18/60 contact is **not** cleared: dense values 6.922 and
6.869 LDU³ remain above 6; guarded value 7.027. Original model unchanged. The
withdrawal planner no longer rejects intermediate poses merely for not being
pressed home, and failed-path diagnostics identify observed obstructing pieces.
The plan manifest separately records `final_pose_check_passed` and
`assembly_path_found`; a missing straight insertion path is not reported as a
final-pose collision. Illegal preflight writes diagnostics only, never an
instruction document. The strict release was rebuilt/installed; launching its
checker against the saved model from an independent directory preserved the
source SHA-256 (matching the historical hash below).
All 90 Rust tests and 10 publication tests passed, including strict refusal to
write an instruction document after illegal final-pose preflight.

## Historical near-threshold contact review (withdrawn)

The checker and instruction planner now share `cad_contact_policy.py`. Only
triangle-checked, confirmed-connector contact at the existing 6 LDU³ boundary,
without a seating conflict, becomes a nonblocking yellow review warning. The
underlying geometry oracle and its threshold remain unchanged. Draft output
keeps these warnings in the in-app view, printable cover, and plan manifest;
larger/unmatched overlaps and gap conflicts remain blocking. No model edits.
Publication tests cover the policy exclusions and generation of numbered steps
with a retained contact warning (9 Python tests total).
The latest saved user model was checked read-only: 0 blocking errors, one
review contact at pieces 18/60. SHA-256 before/after matched
`ec8fb65bd8417cd4a7be412a4284c05b6a37736c11bf1e3caaacd38e2790a4be`.
All 90 Rust tests and 9 publication tests passed. This removes the contact
preflight blocker, not the planner's independent insertion/loop limitations.

## Full pair diagnoses and explicit joint choice follow-up

The earlier candidate-only recheck missed seated-connector conflicts and small overlaps that the instruction planner inspected. Check now evaluates every nearby pair through the full oracle, uses canonical pair argument order shared with planning, preserves cleared-native diagnostics, and reports reasons, overlap estimates, connector types and conflict/overlap centroids. The UI displays diagnosis text and red point markers, not only whole-piece X-ray outlines. A regression catches a 1-LDU stud seating gap missed by native collision tests. It remains an approximate geometric/connector check, not full build certification.

A read-only check of the user's newer 397-piece saved model found the same five flagged pairs as planning: three unseated studs and two threshold-level overlap estimates. Its SHA256 stayed `92be92e3b607cd8366b5d307b3a3f008eea305b47c4580af93908fb0a2f2fa4c`. This is a different user-saved revision from the earlier 383-piece model discussed below; neither was edited by these investigations.

H now leaves an ambiguous pivot unselected, shows numbered world-space choices plus part/family labels, and refuses rotation until the user chooses. The paired-hinge pointer regression checks this refusal before explicitly choosing and still verifies release/undo with both bearings preserved. Sole pivots remain automatic.

Verification: 90 Rust tests and seven Python publication tests pass; the gap regression verifies both native-missed conflict detection and explicit planning refusal text. Release rebuilt/reinstalled. The current saved model was inspected only, with no source edits or automatic repairs.

## In-app publication, glossy studio, and checker review update (2026-11-11)

Generate now uses an internal cache without a folder prompt. Instructions show pages, parts trays, quantities/colour names and actions in-app; Studio displays finished angles progressively. Optional Export copies public HTML/PNG/plan files into a fresh directory, excluding private job/log files. Export/source-preservation tests pass.

The studio renderer now shares standard geometry/BVH across angles, smooths curved normals while retaining creases, uses dielectric GGX highlights, rough secondary reflection rays and sampled softbox shadows in linear light. A bundled C++17 multithreaded ray-query library replaces slow NumPy traversal; NumPy remains the reference fallback. Defaults are 1024 pixels/four samples, not a calibrated transparent-material or full global-illumination renderer. A read-only 383-piece / 147,197-triangle run took 1.38 seconds preparation plus 6.71 seconds for one 1024px view on this machine; not a universal performance guarantee.

Check runs outside the document actor, reviews native collision candidates with full connector keep-outs, opens Checker automatically and focuses red/amber X-rays. Cleared warnings remain inspectable in blue. In the user's unchanged `~/model.ldr`, the sole native flag was #42 (4589) against #40 (4599a): full review found a bar connection, zero solid overlap, and no conflict. The resulting UNVERIFIED/zero-error report is not complete-build certification. The installed `--check` launch was captured showing that location; original SHA256 remained unchanged.

Verification: 89 Rust tests and seven Python tests with the installed native accelerator pass, including randomized native/reference ray parity, genuine collision retention, connector false-positive clearance and export privacy. Release rebuilt/reinstalled; previous publication notes below describe the initial implementation.

## Instruction and studio panels (2026-11-11)

Added dockable **Instructions** and **Studio preview** panels, discoverable through Panels and launch flags `--instructions` / `--studio`. Jobs flatten the complete base main model (including repeated/hidden pieces, excluding unaccepted drafts), run in isolated cancellable Python processes, export into unique new directories, and mark earlier snapshots stale after edits. The source model is not rewritten. Outputs include a reproducible local input manifest and worker log.

Instruction planning uses full connector relations for adjacency, world-upright stud/tube clusters as initial subbuilds, SNOT/articulated boundaries, reverse-disassembly clearance checks sampled every 4 LDU, connected-remainder checks, and repartitioning attempts when completed chunks cannot join. Numbered printable HTML pages contain a cover/page 0, faded previous pieces, new-part trays/quantities/colours and insertion arrows; the panel can browse individual step images. This is **draft straight-insertion planning**, not the complete general assembly planner requested: no continuous swept-volume proof, hand access/gravity/stability solver, flexible-clip simulation or hinge-angle preparation. The closed paired-hinge fixture remains explicitly blocked; the tool writes diagnostics instead of publishing fabricated buildable steps. World-tilted models can be over-partitioned. Existing manual H articulation is unchanged.

Studio views use standard-detail triangle geometry, a CPU BVH ray caster, shadow rays / deterministic area-light samples, white ground/background and approximate opaque plastic/specular materials at four azimuths. This is real ray casting, not editor screenshot rendering, but not a calibrated transparent/chrome path tracer. Size and samples are adjustable; no interactive/large-model completion-time guarantee. Pillow is now an explicit installed-helper dependency and render.py is bundled.

Verification: the existing 87 Rust tests pass; six new Python tests cover ray nearest-hit/miss, stack ordering, SNOT partition/join, collision/disconnected refusal, closed-hinge diagnostics, numbered document/images/manifest, render pixels and unchanged inputs. See `tools/test_publication.py`. Installed helper resources also generated a two-subbuild / three-step SNOT document and all four studio angles from an unrelated working directory. The installed native Instructions panel was opened/captured; the fixture source hash remained unchanged. Release binary rebuilt/reinstalled. Broader complex-build planning remains unverified.

The approved scope is in `~/.claude/plans/plan-new-subproject-as-fancy-valiant.md`.
Milestones are complete only when their exit criteria have been checked, not just when code exists.

| Milestone | Status |
|---|---|
| M1 library/viewer | Implemented; desktop, file and local 10k performance checks pass |
| M2 native engine and Python parity | **Incomplete**: voxel validator, pair cache, actor/transactions exist; full Python parity fails |
| M3 editing | **Partial**: piece/group dragging, library drop/repeat placement, transforms/undo, geometric-stud snapping; C connector mating and H articulation now work in main models through the full Python backend; native port/performance scope remains |
| M4 MCP/drafts | **Replaced** by SuperBrick projects: `.sb` store (`crates/store`), image-returning tools (`crates/ops`, `sb mcp` / `sb P.sb --user`), persisted per-user drafts, Agents/History panels; socket bridge removed |
| M5 selector | Integrated; unchanged 168-query ranking/speed gate passes, colour mapping coverage remains below target |
| M6 editable submodels | Core workflow implemented/tested: split live views, all-instance edits, MPD roundtrip; selection must stay within one step |

The Python toolkit is not modified. No model files are overwritten during verification.

## Verified in this implementation run

- Workspace build succeeds; `cargo test --workspace` passes **90 tests**.
- Supplier/parts-list panel compiles the whole base assembly, grouping part+LDraw
  colour and counting repeated/nested MPD instances and hidden pieces. Inventory
  runs off-thread and ignores unaccepted drafts. Toolbar / Panels access and
  `--suppliers` launch flag; searchable virtualized lots with descriptions/colour
  swatches. CSV and flattened purchasing LDR exports, missing/reference/colour
  warnings, and source-overwrite refusal. Tests cover inherited colours, repeated
  hidden subbuilds, export roundtrip, recursion/direct colours and safe export.
  Installed release launched from an unrelated working directory with
  `--suppliers`; whole-window capture confirmed 6 pieces / 4 lots / 3 distinct
  parts for the paired-hinge fixture, all four supplier buttons and colour rows.
  Source bytes remained unchanged; application exited cleanly.
- BrickLink / BuyMeBricks / Wobrick / JoyGoBricks buttons explicitly export a
  supported list or generic quote-request CSV and open official workflow pages.
  **No automatic upload, purchase, live in-app price total or authenticated vendor
  API integration is implemented.** Users review mappings, missing quantities,
  shipping/tax/currency on supplier sites. Wobrick's documented upload uses LDraw
  colour codes; no guessed BrickLink/GoBricks colour conversion is made.
  Native stud/ground previews are again prepared immediately in the pointer frame, reusing shape
  caches. Full connector matching remains in isolated background workers with latest-only jobs;
  neither UI nor document actor waits for Python while dragging. Prepared placements commit once
  after revision/bytes checks without solving again. Releases without an approved visible pose
  place nothing; deferred automatic drops were removed. A gated successful late-result regression
  verifies no placement occurs after release. Drag arrows rotate only transient geometry; Connect retains orbit.
  A gated slow-solver test proves requests coalesce and the document actor remains callable.
  Automatic drag caches transformed bodies and a fixed-scene free-connector spatial index,
  excludes occupied/hidden endpoints, preserves exposed partial bar/axle spans, vectorizes broad-phase
  collision queries and caches the exact latest validated request. Source/target enumeration is no
  longer truncated to the first 16/12 connectors; full collision tests retain a 64-candidate budget.
  Difficult solves can still take time; no large-build
  per-query latency guarantee or native-physics parity is claimed.
  Ordinary rotation reseating searches within 20 LDU on a 2-LDU grid plus exact bounding-face offsets,
  requires contact/overlap with original geometry, and ranks by shift length with fixed tie-breaks.
  Immediate inverse rotations restore stored exact poses, not a fresh optimization. The regression
  checks a genuinely blocked rotation, repeatability, contact, repeated inverse pairs and undo.
  This is a bounded discrete search, not a globally minimal continuous solution or full joint legality.
  Hide (V) adds click-toggle visibility and Show hidden grey ghosts. They are click-through except
  in Hide mode, never snap/Connect targets, and remain collision obstacles. Failed hover reveals
  hidden blockers red, restoring normal visibility on exit. Real egui/GPU tests cover toggling,
  blocked-hover reveal/restoration, click-through picking and unhide picking; engine tests preserve
  hidden collision constraints. Python exits quietly if its native parent's reply pipe closes;
  an observed BrokenPipe traceback after termination is not proof of the termination's root cause.
  Latest desktop update adds opaque-mesh connector occlusion, async full-validated destination
  filtering, L smart-lasso with joint boundaries/tolerance and toolbar subbuild creation, inline
  similar results, floor-following placement, recovery autosaves, clickable
  checker X-ray focus, and selection retention after UI placement/movement.
  New regressions cover visible/occluded markers, legal destinations, existing/new pin and axle
  drag mates, a 37-degree receiving stud frame, fixed-versus-ball lasso traversal, dynamic floor
  seating and recovery-file safety. Installed mechanical smoke still passes without source edits.
  Full connector solving still uses a serialized Python helper on background preview jobs;
  large-build solve latency/hysteresis remains unfinished. Recovery uses separate atomic snapshots at
  15-second intervals, not silent writes to the user's original model.
  Camera floor restrictions were subsequently removed: orbit pitch again spans both upward and
  downward views, and pan/zoom targets are not clamped to the floor. The ground guide still follows
  the lowest piece. A real pointer-orbit regression checks the camera can go underneath and look up.
  New checks cover literal IDs overriding filters, semantic neighbours, printed-design colour
  handling, and stale/live socket recovery. The installed release was verified to return 15068
  first even with a deliberately incompatible colour filter.
  Picker ranking now uses a Luna-generated 100-part / 500-description benchmark with a frozen
  80/20 part split: held-out top-10 **66% → 84%**, MRR@50 **0.358 → 0.672**. These are synthetic
  metadata-only queries, not human evaluation. Full protocol and reports are in
  `tools/search-bench/README.md`. Each picker result offers eight semantic neighbours.
  New coverage drives real egui pointer events through library drop, piece dragging,
  preview rendering, single-transaction undo and repeat ground placement. Ground
  phase, occupied cells and moving a stack by its upper piece have regressions.
  Four native topology tests cover rigid chunks, axes and opposite loop cuts. Full-backend
  tests connect separated pins, axles and hinge fingers, rotate/undo a hinge and exercise
  two ball/socket families. Actual egui tests drive H click/arrows and C two-piece/Enter.
  H now has joint/axis selection and draggable rings; see HINGE.md for exact limits.
- Copy in Inspector / Ctrl+C / Cmd+C activates repeat placement for one selected piece, retaining
  exact colour/orientation without modifying the source. Native Copy events are supported, Cut and
  plain C are not consumed as copy. Regression covers shortcuts and real preview/click placements,
  original preservation, repeated placement and undo; library picking resets orientation normally.
- Coaxial hinge bearings are partitioned together without detachment, rather than mistaken for an
  interfering loop. Backend validation still checks both preserved links; off-axis constraints and
  rigid braces retain their guards. Regression covers real paired locking hinges with bridging
  plates, world tilt, undo, and the actual ring-release UI path. Unconfirmed loop releases show feedback.
- Fixed FGR metadata import: LDCad finger connectors are centered by default, unlike cylinders.
  The old interpretation left the fork 12 LDU beside the ratchet. Regression checks actual
  geometric center alignment (not merely the oracle reporting a link), including vertical builds.
  Original bench sources/fixtures remain untouched; CAD uses a corrected centered fixture.
- Disabled unsafe bounding-box stud hiding: a panel's empty interior must not erase a 1×6 plate's
  six exposed studs. Both instancing modes keep geometry and let GPU depth testing occlude it.
  Old culling/performance figures above are historical; the approximate culling speedup no longer applies.
- Parts colour menu now supports “No colour”: unrestricted availability browsing, per-part known-colour
  thumbnails/drag payloads, and a neutral fallback when the catalogue has no colour data.
- `tools/smoke_mechanical.py` launches the installed binary from a scratch working directory;
  real MCP proposals connect separated pin/hole, axle/cross-hole and finger-hinge pairs, then
  rotate the hinge. All four proposals remain pending and the source file stays unchanged.
- `cargo clippy --workspace --all-targets` could not run: Clippy is not installed in the current toolchain.
- Tests cover actor publication, atomic transactions, stable IDs/undo, stale refusal, rejected
  dependencies, user occupancy blocking, partial acceptance rebasing, deterministic copy/subbuild draft
  allocations, external-save protection, vertical-surface geometric nudges, fast/full native collision
  checks, submodel MPD flattening, original LDraw Python geometry parity and search filters.
- Flattened parts retain reference-occurrence ancestry. Moving a nested submodel or its child checks
  collision involvement in every parent/world instance, not merely in the child's local viewport.
- `cad roundtrip ../runs`: 26 files / 4,746 part lines; `cad roundtrip ..`: 336 files / 6,256 lines.
  Comparisons are in memory; source files are never rewritten.
- Actual 10k desktop launch: first scene 783–784 ms, UI-loop p50 about 16.6 ms, p95 17.4–21.6 ms.
  These are UI-loop intervals, **not** isolated GPU timings or guarantees for other hardware.
  `/usr/bin/time -l`: maximum RSS 287,539,200 bytes; peak footprint 850,232,424 bytes.
- Earlier `cad search-eval` run: 168 unchanged golden queries, top-1 **93.45%**, MRR **0.95114**, p95 **8.3–8.6 ms**.
  The new adaptive picker retains **91.1% top-1 / 97.0% top-10 / 0.934 MRR@50** on that corpus,
  trading a small common-query regression for improved rare/descriptive retrieval. It does not
  improve every query; the frozen held-out benchmark above is the broader quality check.
  Whole-process maximum RSS 189,120,512 bytes; encoder-only memory not separately measured.
- `tools/smoke.py` starts the real desktop app, proposes a 50-part clump through actual MCP, verifies
  only its session sees the proposal and the base/source remain unchanged, captures the whole window,
  exits cleanly. Blue hatched ghost geometry was visually checked. This is a **scripted MCP client**,
  not the planned real-Claude-Code + concurrent native-user acceptance test.
- A scratch MPD was made from a selection, its submodel duplicated and its child painted; an actual
  desktop screenshot shows the edited child in both root instances and in the split editable child view.
- Standalone search spike still compiles after sharing implementation with the production crate.

## Failed gate and remaining scope (do not mark complete)

`cad parity ../bench-next/tests/connections/pin_in_brick_hole.ldr` returns exit 1:
Python says VERIFIED, one group and one pin-hole link; native says UNVERIFIED, two groups, zero stud links.
This is an **observed failure**, not merely an unrun test.

1. **M2 first:** port shadow inheritance/SNAP types, connector matching and full relation semantics from
   `bench-next/snaps.py` and `physics.py`; then global statuses, conflict/loose/floating/completeness,
   gears, compliance and precise tilted narrow phase. Run 129 connection cases, all 17 fixes and full
   design/reference parity. Native currently never certifies a nonempty model VERIFIED. Establish the
   10k full/incremental validation performance gates; they have not been established.
2. **M3:** connector snap hysteresis/budgeting, connector indexes and surface GridTags,
    jumper/neighbour-hop semantics, fully native connector mating/articulation and slide handles,
    instance-aware shared-subbuild mechanical constraints, mirror/chiral mapping, clipboard/array UX,
    complete connector/link overlays and status inspector,
    100k LOD/occlusion/GPU culling and randomized-drop tests. Main-model dragging/placement now tries
     full Python connector mates with native geometric-stud/ground fallback; nudging and fallback
     remain partial, so this is **not** legal-only editing. C/H and accepted full-connector drag
     poses use the Python connector/solid backend; H's wire preview is validated on release.
3. **M4:** connector/explain/top/free-connector tools, complete camera/step state, deletion tombstones,
   turn undo, external file watching/merge policy (save-time protection exists), move arrows, real-agent
   interleaving test. Autoaccept is withheld while validation is incomplete. Proposal selection uses
   checkboxes, not direct ghost picking. Stale/blocked drafts need explicit rejection/reproposal.
4. **M5:** cached Rebrickable inventory-row mapping **93.5%**, below 95%; improve mappings and rebuild.
   Tags are description-derived, not connector-derived. Add content-hash invalidation, persistent
   thumbnail cache, encoder-only memory measurement and independent/held-out queries. `search-eval`
   passing does **not** establish all M5 exit gates.
5. **M6:** support cross-step selections without losing construction order; expand external-reference,
   nested/chiral and live-interaction tests. Full connector legality still depends on M2.

## Code map for resuming

- `crates/engine`: command/schema registry, model/actor, validation cache, placement, drafts, effective
  session views/ghosts; `tests/transactions.rs` is the main behavioral regression suite.
- `crates/store`: `.sb` SQLite project (trees, commits/branches, CAS working tree, leases, id blocks, drafts, selections, candidates, prefs, assets, checkpoints).
- `crates/ops`: agent tools (edit/where/select/look/search/verify/project/publish), view planner (`planner.rs`), render cost model (`cost.rs`), overlays, CLI/MCP front end (`front.rs`).
- `app/src/project.rs`: window ↔ project sync (publish debounce, lease, follower mode, checkpoints, global prefs).
- `crates/agent`: legacy socket bridge, no longer built (not in the workspace).
- `crates/search`: shared catalogue/analyzer/ranker and static Potion encoder, without Candle in the app.
- `app/src/app.rs`: shell/docking/state; `view.rs`: renderer/picking/camera; `panels.rs`: selector,
  inspector, shared palette and user-controlled draft acceptance.
- `cli/src/product.rs`: partial native validation, explicit Python parity, search evaluation, fresh-file command application.
- `tools/smoke.py`: drove the removed socket bridge; superseded by `crates/ops/tests/tools.rs` and `app/src/project.rs` tests.
