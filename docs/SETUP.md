# Setting up SuperBrick locally

Building from source, running the app and its tools, where files live, and the agent interface.
For what the app does, see [FEATURES.md](FEATURES.md); to just use it,
[download the latest release](https://github.com/levtechs/SuperBrick/releases/latest).

## Build from source

**macOS**

```sh
python3 tools/install.py --app     # builds release, installs SuperBrick.app and the `sb` command
python3 tools/release.py           # builds the downloadable dist/SuperBrick-VERSION-macOS-arm64.dmg
```

Then open **SuperBrick** from Applications, Spotlight or Launchpad. The start page lists your
recent projects; **New project…** makes a `.sb` project and **Open…** takes `.sb`, `.ldr` or
`.mpd`. From a terminal:

```sh
sb                          # start page
sb model.ldr                # open plain LDraw
sb --new my-build           # create my-build.sb
sb my-build.sb              # open a project
sb my-build.sb export -o my-build.ldr
```

**Windows** (PowerShell)

```powershell
py tools\install.py               # builds release, installs sb.exe, SuperBrick.exe and the share files
py tools\release.py               # builds dist\SuperBrick-VERSION-Windows-x64.zip (with install.ps1)
```

The installer puts `sb.exe` on your user PATH, adds a Start Menu shortcut and registers the file
types (see [Where files live](#where-files-live)). Open a new terminal afterwards, then:

```powershell
sb                          # start page
sb model.ldr                # open plain LDraw
sb --new my-build           # create my-build.sb
sb my-build.sb              # open a project
sb my-build.sb export -o my-build.ldr
```

`sb.exe` is a console program: use it for the command line and the MCP server. It also opens the
GUI, from a terminal. `SuperBrick.exe` is a small launcher with no console window that starts
`sb.exe` next to it with the same arguments; shortcuts and file associations use it.

**Requirements to build (Windows):** Rust stable with the MSVC toolchain
(`x86_64-pc-windows-msvc`); Visual Studio Build Tools with the **Desktop development with C++**
workload, which provides `cl` (needed for `studio_rays.dll`, see
[BUILD-STUDIO-RAYS.md](BUILD-STUDIO-RAYS.md), and for native crates); Python 3 with `numpy` and
`Pillow`; plus the same search data and sibling folders as below. Run the installer from a
Developer PowerShell so `cl` is on PATH.

**Requirements to build:** Rust (stable), an LDraw parts library (LeoCAD's, `SB_LDRAW_ZIP`, or
`sb setup --library`), Python 3 with `numpy` and `Pillow` (`SB_PYTHON` picks the interpreter, or
`sb setup --python`), and the data the installer copies:
the part-search catalogue and models in `spikes/search/{out,models}` (not in git; see
[spikes/search/FINDINGS.md](../spikes/search/FINDINGS.md)) plus the sibling `../bench-next` and
`../data-next` folders (connector shadow library).

### Connector data for a source build (without bench-next)

Connect, Hinge and the checker need the connector helpers and the LDCad shadow library. A source
build finds them, in this order, in `SB_CONNECTOR_DATA`, `<bin>/../share/superbrick/connectors`,
`<support folder>/connectors`, then `../superbrick-data/connectors` next to the checkout, and
last `../bench-next` with `../data-next`. A folder is used only if it contains `bench/snaps.py`.

Without `bench-next`, fill `../superbrick-data/connectors` from a release bundle:
`bench/` takes `physics.py`, `snaps.py`, `ldraw.py`, `raster.py`, `render.py` and `shadow_extra/`
from the bundle's `share/superbrick/connectors/bench`, and `shadow/` takes its `shadow` folder.
SuperBrick's own modules (`cad_validate.py`, `cad_scene.py`, the contact policy and snap rules)
are not copied: a source build loads them from `app/src`. Keep this folder outside `target/`
(`cargo clean` deletes it) and, on Windows, set it up from your own terminal: files a sandboxed
(MSIX-packaged) app writes under `AppData` are hidden from other programs.

## Where files live

**macOS**

| Path | What |
|---|---|
| `/Applications/SuperBrick.app` | the app (installed from the DMG or by `tools/install.py --app`) |
| `~/.local/bin/sb`, `~/.local/bin/sb-dev` | the command line (a launcher for the app's binary with `--app`) |
| `~/.local/share/superbrick/` | search catalogue and helper assets for a command-line-only install |
| `~/Library/Application Support/SuperBrick/` | downloaded LDraw library (`ldraw/complete.zip`) and the app's own Python |
| `~/Library/Application Support/cad/` | preferences (`prefs.json`), crash recovery and thumbnails |
| `~/Library/Caches/cad/publications/` | generated instructions and studio renders |
| your `.sb` files | everything about a project: model, history, drafts, selections, assets. Keep them out of iCloud Drive and other syncing folders while open; syncing a live SQLite file can corrupt it. |

**Windows**

| Path | What |
|---|---|
| `%LOCALAPPDATA%\Programs\SuperBrick\bin\` | `sb.exe`, `sb-dev.exe` and `SuperBrick.exe`; this folder is on your user PATH |
| `%LOCALAPPDATA%\Programs\SuperBrick\share\superbrick\` | search catalogue, connector helpers and `studio_rays.dll` |
| `%APPDATA%\SuperBrick\` | downloaded LDraw library (`ldraw\complete.zip`), the app's own Python (`python\`), preferences (`prefs.json`) and crash recovery (`recovery\`) |
| `%LOCALAPPDATA%\SuperBrick\Cache\` | generated instructions and studio renders (`publications\`), helper scripts (`helpers\`) and the bench cache |
| Start Menu → SuperBrick | shortcut to `SuperBrick.exe` |
| `.sb` files | SuperBrick owns the `.sb` type, so double-clicking a project opens it; it is also offered under **Open with** for `.ldr` and `.mpd`. |
| your `.sb` files | everything about a project, as on macOS. Keep them out of OneDrive-synced folders and other syncing folders while open; syncing a live SQLite file can corrupt it. |

Double-clicking a `.sb` file while SuperBrick is already open sends it to the running window
instead of starting a second one.

Environment overrides: `SB_LDRAW_ZIP` (or `BENCH_LDRAW_ZIP`) for the parts library,
`SB_PYTHON` for the Python interpreter, `CAD_SEARCH_DATA` for search assets and
`CAD_PUBLICATION_CACHE` for the publication cache. They work the same on Windows; in PowerShell,
for example, `$env:SB_PYTHON = "C:\Python312\python.exe"` (this session only).

### Python on Windows

SuperBrick looks for a Python that can import `numpy` and `Pillow`, in this order:

1. `SB_PYTHON`
2. the copy SuperBrick installed itself (`%APPDATA%\SuperBrick\python\python.exe`)
3. whatever `py -3` launches
4. python.org installs under `%LOCALAPPDATA%\Programs\Python\Python3*` (newest first)
5. conda and miniforge in your home folder (`miniforge3`, `miniconda3`, `anaconda3`)
6. scoop (`%USERPROFILE%\scoop\shims\python.exe`)
7. `python.exe` on PATH

The Microsoft Store `python.exe` alias (anything under `WindowsApps`) is a stub and is ignored.
If none is usable, `sb setup --python` or the app's setup screen downloads and installs its own copy
under `%APPDATA%\SuperBrick\python`.

### Parts library on Windows

SuperBrick uses `SB_LDRAW_ZIP` if set, else the copy it downloaded
(`%APPDATA%\SuperBrick\ldraw\complete.zip`; `sb setup --library` or the setup screen fetches
it), else LeoCAD's `C:\Program Files\LeoCAD\library.bin` (also checked under
`Program Files (x86)`).

### Windows support is new

The Windows port is new. Known limitations:

- No MCP hot reload on Windows. After reinstalling, restart the `sb mcp` server (for Claude
  Code, restart the session or reconnect the server).
- The installer is a zip plus a PowerShell script (`install.ps1`); there is no MSI yet.
- The build is not code-signed, so SmartScreen warns on first run (**More info → Run anyway**).
- x64 only. Windows on Arm is not built.

## Working with agents

Every agent call names a user, and anything other than `human` becomes a draft that shows up in
the app's **Agents** tab, grouped by user:

```sh
sb tools                                              # the 8 tools and their schemas
sb my-build.sb --user claude look '{}'                # pictures of the whole model
sb my-build.sb --user claude search '{"query":"curved slope 2x1"}'
sb my-build.sb --user claude where '{"part":"3001","near":[0,-24,0]}'
sb my-build.sb --user claude edit '{"ops":[{"op":"place","from":"c1","pick":1}]}'
sb mcp --user claude                                  # the same tools as an MCP server (stdio)
```

`.mcp.json` registers the MCP server for Claude Code. On Windows the same commands work from
PowerShell; to register the server for Claude Code, run `sb mcp --user claude` through your
client's MCP setup (for example `claude mcp add superbrick -- sb mcp --user claude`); `sb.exe` is
on PATH after install. There is no hot reload on Windows: restart the MCP server after
reinstalling. Details are in
[Projects (.sb) and agents](#projects-sb-and-agents) below.

## Layout

| Path | What |
|---|---|
| `app/` | the desktop app (`sb` binary): egui UI, start page, project sync, publication |
| `app/assets/` | logo, icons and wordmark (`logo.svg` is the master) |
| `cli/` | developer subcommands (`sb-dev`, also reachable as `sb <subcommand>`) |
| `crates/ldraw` | LDraw parsing, library, meshes |
| `crates/engine` | model, commands, drafts, placement, validation, joints, strength |
| `crates/store` | the `.sb` project file (SQLite): trees, history, branches, drafts, prefs, assets |
| `crates/ops` | agent tools, view planner, render cost model, CLI front end and MCP server |
| `crates/render` | wgpu renderer and the warm `RenderService` |
| `crates/physics`, `crates/search` | contact physics; offline part search |
| `spikes/` | experiments kept for reference (ACP, UI, search) |
| `tools/` | installer, smoke tests, golden renders |

Design notes: [IMPLEMENTATION.md](../IMPLEMENTATION.md) (milestones and gates),
[MCP-REVIEW.md](../MCP-REVIEW.md) (the agent-interface review behind the current tools),
[HINGE.md](../HINGE.md), [M0-FINDINGS.md](../M0-FINDINGS.md).

## Development

```sh
cargo build --workspace
cargo test --workspace
cargo run -p cad-app --bin sb -- model.ldr
```

---

## Reference

### Colour warnings

Paint applies the selected colour but reports a warning if the part-colour pair
is not listed in the installed catalogue. Missing part-colour data is reported
as **unknown**, not unavailable. Check shows these warnings separately from
geometric errors, with affected piece numbers and a selection button; they do
not block editing or instruction generation. Catalogue observations are not
live supplier stock or proof that an unlisted colour was never manufactured.

### Strict instruction preflight

Instructions do not bypass unresolved contacts. Checker and planner refine small
overlap estimates using standard triangle meshes at 0.25 and 0.125 LDU spacing,
with connector keep-outs, existing compliance margins, and a conservative
convergence guard. Only a geometric result below the unchanged 6 LDU³ cutoff
clears a collision; matching connectors alone never clears it. Budget exhaustion
keeps a flag. These remain sampled tests, not an analytic collision proof.

Final poses must be seated. Intermediate withdrawal/insertion poses need not be
fully seated, but physical overlaps and other conflicts still block the path.
If no straight path is found, the planner reports observed piece-specific
obstructions; it does not invent a flexible or hinged trajectory. Existing
builds are never moved automatically.

## Instructions and studio previews

Generate instructions without opening the desktop window:

```sh
sb model.ldr --generate-instructions
sb model.ldr --generate-instructions --output ./model-instructions --image-size 512
sb model.ldr --generate-instructions --output ./plan-check --plan-only
```

The output directory must be new. Progress goes to stderr and the final result is JSON on stdout; failures return a nonzero exit code and keep diagnostic `plan.json`/`job.json`. The source model is never saved by this command. `--plan-only` performs the same geometry and assembly checks without illustrating pages.

Planning uses part-local frames for tilted stud assemblies, recursively splits trapped connected sections into independently buildable subbuilds, and checks their joining operation. Withdrawal starts from a nearby checked clear staging pose rather than unnecessarily travelling through the entire model to infinity. Clip/socket frame directions are considered in addition to joint axes. Independent connection components are documented separately, not falsely joined.

Open **Panels → Instructions** or **Panels → Studio preview** (also `sb model.ldr --instructions` / `--studio`). **Generate…** now displays results directly in the app, without asking for a folder. Instructions have Previous/Next, a page slider, new-parts tray, colour/quantity labels and the action description. Studio images appear as each angle finishes. **Export / save a copy…** is optional and copies HTML/PNGs/plan into a fresh folder, excluding the private input snapshot/log. Internal results live in `~/Library/Caches/cad/publications` (`%LOCALAPPDATA%\SuperBrick\Cache\publications` on Windows) (`CAD_PUBLICATION_CACHE` override). Jobs never change the model and run in an independently cancellable process. Outputs are stamped to the snapshot and marked outdated after edits. Python needs NumPy and Pillow; installation includes the offline helpers and compiles a multithreaded native ray-query accelerator using clang++ (MSVC `cl` on Windows; see [BUILD-STUDIO-RAYS.md](BUILD-STUDIO-RAYS.md)).

**Instructions:** connector-aware partitioning groups stud stacks in their own frame and treats side studs / articulated joints as candidate subbuild boundaries. Reverse disassembly checks sampled straight-line insertion paths against actual neighbouring geometry; obstructing groups are repartitioned where possible. Nearby/touching additions are preferred when valid, and later additions are moved into detached subbuild construction only after rechecking the larger join and affected paths. Independent repeated additions can share one quantity step while preserving their checked order; stacked additions do not merge. Single-piece subbuild setup pages collapse into normal attachment steps.

The printable `instructions.html` has a cover (page 0), large numbered steps, visual parts trays and quantities, normal colours throughout, a red outline around additions, and insertion arrows. Views change when additions would be hidden, with a turn cue; detached subbuilds are illustrated flat in a local frame, then shown at their final orientation on joining (tilt callouts measure base-plane inclination, not a calibrated joint detent). Repetitive prose and part IDs are omitted from the illustrated pages. Unconfirmed loose parts are explicitly labelled rather than given silent attachment steps. Open it in a browser; print or save to PDF there. Keep its PNGs alongside it. `plan.json` records the sequence and checks; `job.json` is the local input snapshot.

**Important planner limits:** this is a draft generator, not a general physical assembly proof. Paths use 4-LDU samples, not continuous swept volumes. Gravity, finger/tool access, flexible deformation, hinge-angle preparation and arbitrary closed-loop assembly paths are not solved. Independent connected components are built separately with an explicit assembly note; no unconfirmed joining step is invented. Blocked/colliding builds produce a diagnostic `plan.json` rather than a fabricated valid instruction document. The paired-hinge fixture now has a checked connector-frame assembly path; that does not establish support for every closed loop. Review all exported instructions physically before sharing. Connected partition search is bounded and may still fail to find an existing sequence.

**Rendering quality:** fast CAD-style rendering is the default, at the full requested image size. Instructions retain edges, flat shading, original colours and arrows, using a multithreaded native z-buffer without supersampling. Add `--high-quality` to the headless instruction command (or enable **High quality** in the panel) for 2× supersampling at the same final dimensions. Example: `sb model.ldr --generate-instructions --image-size 1600`. Add `--high-quality` for smoother edges at greater cost. Quality never changes legality or assembly-path checks.

**Studio:** four white-background CAD-style views by default. Enable **High quality** for native CPU BVH ray queries, interpolated curved-surface normals, GGX dielectric highlights, rough reflections and sampled soft shadows. Default output1024px; size320–1600 and high-quality samples1–8 are adjustable. Geometry/BVH are shared across high-quality angles. Transparent/chrome materials remain approximate opaque plastic. Without the native accelerator, the NumPy fallback is slower. Cancel terminates the worker. Both panels include hidden base pieces/repeated subbuilds but exclude unaccepted drafts.

Python regression suite: `python3 -m unittest discover -s tools -p test_publication.py -v`.

## Run

Install a release build (from this directory):

```sh
python3 tools/install.py          # command line only
python3 tools/install.py --app    # also /Applications/SuperBrick.app
```

With `--app`, `SuperBrick.app` carries its own copy of the assets and `~/.local/bin/sb` becomes a
small launcher for the app's binary (so terminal launches show SuperBrick's name and icon in the
Dock). The app owns the `.sb` type, so double-clicking a project in Finder opens it, and it is
offered under Open With for `.ldr`/`.mpd`. Without it, this installs the real native executable as `~/.local/bin/sb` (plus `sb-dev`) and offline search assets into
`~/.local/share/superbrick/search`. With `~/.local/bin` on PATH, run from **any directory**:

```sh
sb                       # new model
sb ./model.ldr            # relative to your current directory
sb /absolute/path/set.mpd
sb --help
```

Quote paths containing spaces. The installed command is the GUI, not the developer CLI.
The LeoCAD LDraw library is still required (or set `BENCH_LDRAW_ZIP` to your library archive).
To rebuild/update your installed command, rerun `python3 tools/install.py --force`.
`--prefix PATH` installs under a different prefix with the same `bin`/`share` layout.

**Windows:** run `py tools\install.py` (it also updates an existing install). To remove
SuperBrick, run `py tools\install.py --uninstall`, or `install.ps1 -Uninstall` from a release
zip. It removes the program files, the PATH entry, the Start
Menu shortcut and the file associations; your projects and the data in `%APPDATA%\SuperBrick`
are kept. Reinstalling while SuperBrick runs is fine: the installer moves the running `sb.exe`
aside first.

### Development

From this directory, with Rust 1.95+ and the existing LeoCAD library installed:

```sh
cargo run -p cad-app --bin sb -- ../runs/design/tortoise/studio/model.ldr
```

Open/save `.ldr`/`.mpd`, drag a file into the window, drag empty space to orbit, middle/right drag to pan, scroll to
zoom toward the pointer; trackpad pinch also zooms. Click selects; Shift/Cmd-click toggles selection. `S` selects, `D` deletes, `P` paints; Delete
removes selected parts, `R` rotates, **arrows rotate**, **Shift+arrows move**, Cmd-D duplicates immediately, Cmd-Z undoes. Inspector provides numeric
transforms, colour, visibility and locking. Cmd-K opens the shared JSON command palette. Panels dock,
close and reopen from the toolbar.

Select one piece or a whole assembly and use **Copy** in Inspector, **Ctrl+C**, or **Cmd+C** to activate repeat placement
with its exact pieces, colours, relative positions and orientations. Repeated MPD instances are copied as independent flattened pieces, not references back to the source. Copy itself doesn't change/remove the original;
each placement is separately undoable. Escape cancels. Text fields retain normal text copying;
Ctrl+X is not the copy-to-place shortcut.

**Ctrl/Cmd+V** recalls the last copied assembly. Inspector's **Mirror pending copy: X/Y/Z**
reflects the clipboard preview before placement, leaving the original arm alone. After placement,
press Esc/S to leave repeat-placement mode; **Mirror selected assembly** then reflects the actual
selection around the first selected piece's origin. Each placement/mirror is one undo transaction.
All pieces in a copied assembly remain selected after placing it, so use **C** to mate its ball/socket
to the opposite shoulder. Existing connector/collision checks still apply.

Mirroring tests standard LDraw geometry and fixed face colours, allowing only proper rotations of
the same symmetric part or a geometry-checked named left/right counterpart (for example 41770a/41769a).
It refuses the entire operation for missing, unsupported or decorated parts instead of inventing a
negative-scale LEGO piece. This conservative check can reject parts for which a counterpart exists
but has not been identified; installed geometry does not prove that a supplier stocks that colour.

The colour menu includes **∅ No colour**: browse all colours without a manufactured-colour restriction.
Each thumbnail/dragged piece uses a known available colour (neutral grey if availability is unknown);
file-defined printed/fixed colours are preserved. LDraw shapes generally do not specify a single
factory/default colour. Choose a regular swatch to return to colour-specific browsing.

The toolbar's **Theme** menu offers Light, Dark and System appearance. The colour swatch lives beside
the Parts search box; other filters collapse below. **Show** toggles Recent/Used sections. Piece labels
use descriptions, with IDs on hover. Preview tiles resize to fill the panel width; list mode stays
full-width while scrolling.

A view-only ghost baseplate follows the underside of the lowest piece, with stud spacing.
The camera can orbit underneath it and look upward. It resizes with the build,
including preview ghosts, with at least a four-stud margin on each side. It is never picked, counted
as a part, validated or saved in the model.

Drag a library piece into the build for a one-shot placement; this does **not** activate Place mode.
Alternatively, click a library piece to enable repeat placement, then click multiple build/ground
positions. Escape leaves that mode. Drag a placed piece (or selected group) to move it; empty-space
dragging still orbits. Blue/red transient ghosts indicate valid/blocked poses; drops are one undo
transaction and cancelled/outside drops do not edit the model. Arrow keys rotate the transient piece/group
while dragging; they don't modify the original until drop. Ground placement seats the entire
moving group and works on separate empty cells, not only for the first piece. The guide follows the
existing grounded build's stud-grid phase.

Ordinary arrow/R rotations can reseat the selection by up to one stud if the in-place rotation
collides. The search tests a 2-LDU translation grid plus exact bounding-face offsets, ordered by
shift length with fixed tie-breaking. A result must still touch/overlap the original geometry.
The nearest tested collision-free result wins; this is not a continuous global optimization.
An immediately opposite rotation on the unchanged selection restores the exact previous pose,
including its shift. Any intervening document edit invalidates that inverse shortcut. Hinge-tool
rotations retain their joint constraints and do not use this reseating search.

Ordinary dragging in the main build also tries full connector mates, including **pins, axles and
arbitrarily oriented stud/SNOT frames**; it can rotate the preview to match the receiving connector.
Native geometric-stud/ground snaps are checked immediately in the pointer frame and shown at their
confirmed pose. Full connector solving remains on a background worker, with a cached spatial index
that excludes hidden and occupied endpoints (partially exposed bars/axles remain usable).
Blue means confirmed; an unconfirmed/red preview cannot be dropped. Releasing before a confirmed
snap is ready places nothing: there are no delayed automatic drops. Drops use exactly the displayed
approved pose, without solving again. Difficult connector fits can still take longer. For explicit control, press **C**:
click the moving piece/connector, choose a visible compatible destination, click that destination,
then press **Enter / Connect**. Occluded connector markers are hidden; after choosing the moving
connector, destination markers/dropdowns are restricted to full-validated fits. The whole moving
assembly is aligned and checked using the full connector backend. Pose selects alternate fits.
Press **H** to select a rigid chunk, choose its boundary joint/axis, and drag a rotation ring or
use arrows for 15° turns. Ball joints show three axes. Loops require confirmation of the named
opposite joints that will detach. See [HINGE.md](../HINGE.md) for controls and current limits.
Alt permits explicitly unverified free placement.

**Hide · V** toggles a clicked piece's visibility. Its **Hide options → Show hidden pieces** checkbox
shows hidden parts as grey hatched ghosts; click one in Hide mode to unhide it. In other tools these
ghosts are click-through, and hidden parts' connectors aren't snap/Connect destinations. Hidden geometry
still blocks illegal placements: hovering a blocked pose temporarily reveals the hidden obstacle in red,
then hides it again when the cursor moves away. Hiding never deletes the piece or turns off collision checks.

These mechanical tools currently use a persistent **Python + NumPy** helper, with the existing
connector/physics code and shadow files installed under `~/.local/share/superbrick/connectors`.
No model files are written by Python; native commands own transactions and undo. Set `CAD_PYTHON`
to a different Python interpreter or `CAD_CONNECTOR_DATA` to another installed data directory.
This is transitional, not a claim that the native connector port or M2/M3 is finished.

**L** activates smart lasso. Its toolbar menu adjusts edge tolerance (pixels) and whether selection
stops at hinges/balls/sliding joints (default on). Once the connector graph is ready, selection
expands through fixed attachments; Shift adds to the selection. Multiple selected parts expose
**Make subbuild…** in the toolbar as well as the Inspector. Newly placed and moved pieces stay selected.

For subbuilds, select parts **within one construction step**, give a name and use the Inspector's
selection-to-subbuild button. Double-click an instance or Open subbuild for a second live viewport;
edits affect every instance. Saving writes MPD sections. Saves are atomic and refuse to overwrite an
externally changed source; use Save As/reopen to resolve that situation.

Dirty builds get **recovery autosaves every 15 seconds**, under
`~/Library/Application Support/SuperBrick/recovery` (`%APPDATA%\SuperBrick\recovery` on Windows; override `SB_RECOVERY_DIR`; `CAD_*` variables still work). These never overwrite your
original or mark it saved. Use **Recover…** to open a recovery copy, then Save As. The last interval
can still be lost on a crash; save normally before closing.

Run **Check**: the **Checker** panel opens and automatically focuses the first collision or reviewed warning. Click **Locate** to revisit a collision: the camera focuses the offending
piece, with red X-ray edges and amber edges for its conflicting neighbours, even behind other pieces.
Check now examines all nearby pairs with full connector keep-outs and seating-conflict checks, not only native collision candidates. Each issue describes the actual overlap estimate or unseated connector, lists the two part IDs, and marks the reported conflict/overlap point with a red cross. Gap suggestions and approximate-geometry warnings are explicit; nothing is moved automatically. Cleared native false positives remain blue and inspectable. Instruction planning uses the same deterministic pair order and includes these diagnoses instead of a generic collision message. This still does not certify stability, hand access or an assembly sequence. `cargo run -p cad-cli --bin sb-dev -- validate MODEL --review` runs the pairwise check read-only.

Connect loads connector endpoints without computing the entire assembly graph, on a separate lightweight service. Destination highlighting filters connector families and visibility, not every possible collision-tested pose; the selected connection is fully checked on Connect/Enter. This restores fast selection without claiming every highlighted endpoint is necessarily reachable.

In H mode, multiple joints require an explicit choice before rotation: select a numbered joint marker or the labelled joint menu. Real remote joints in the selected component remain available across incidental single-stud pivots. A nearby but unseated ball/socket is not a rotation pivot: the panel identifies it and offers an explicit collision-checked seating operation. No pose is silently changed.
Hints describe a possible move-away direction, not a verified automatic repair. Recheck after edits;
the native checker remains incomplete and its limitations are shown.

## Parts list & suppliers

Open **Parts list / suppliers** in the toolbar (or Panels → Suppliers / parts list).
`sb model.ldr --suppliers` opens it directly. The dockable panel counts the **whole
main build**, by exact part and LDraw colour, including hidden parts and every
repeated/nested subbuild instance. Pending agent proposals are not purchasing
inventory until accepted. Counts update in the background after edits; filtering
the display does not filter exports.

Supplier buttons save a list, then open the official supplier workflow:

| Supplier | Export / price workflow |
| --- | --- |
| BrickLink | BrickStore `.bsx` wanted list with BrickLink item numbers (from the LDraw `Bricklink` keywords and `~Moved to` redirects) and BrickLink colour IDs (`crates/ldraw/data/bricklink_colours.tsv`) → Wanted List upload → Buy All. Lots in a colour with no BrickLink ID are skipped and listed. |
| BuyMeBricks | Flattened `.ldr` → bulk upload and current pricing / availability. |
| Wobrick | CSV with `Part,Color,Quantity,Is Spare` → toolkit's documented Rebrickable-format upload, **using LDraw IDs / colours**. |
| JoyGoBricks | CSV → send with a quote request following its FAQ/contact instructions. Its FAQ describes a price and unavailable-parts report within roughly 24 hours; no instant API is claimed. |

**These are export-and-quote handoffs, not automatic in-app live totals.** CAD
does not upload files, log into supplier accounts or place orders. Supplier IDs,
colour mappings and stock must be reviewed on the supplier site; LDraw colour
codes are not LEGO or ordinary Rebrickable colour IDs; only the BrickLink export
translates them. In particular, a price-guide average is not a
guaranteed purchasable full-basket quote. Compare shipping, taxes, currency,
unavailable quantities and original LEGO vs compatible parts separately.

Unresolved colours, missing local parts, recursive subbuilds and unresolved model
references are flagged rather than silently omitted/replaced. Supplier exports
require explicit acknowledgement of unresolved issues. General CSV export remains
available for diagnosis. Inventory `.ldr` files contain spaced-out instances for
upload, **not the assembly**. Exports refuse to replace the source build.

Verified workflows: [BrickLink](https://www.bricklink.com/v2/wanted/upload.page),
[BuyMeBricks](https://buymebricks.com/upload),
[Wobrick](https://wobrick.com/toolkit/),
[JoyGoBricks FAQ](https://joygobricks.com/pages/faqs). BrickLink was showing a
maintenance notice when checked on 2026-11-11; supplier pages/access may change.
Live API totals require a separate supported integration and any necessary credentials.

## Local search

Installed builds find search assets alongside their install prefix, independent of the working directory.
Development builds use existing `spikes/search/out/catalog.json`, its Potion corpus and
`spikes/search/models/potion-base-8m/`; falls back to BM25 when embeddings cannot load. Without those,
a development build uses `../superbrick-data/search` next to the checkout (same layout, e.g. the
`share/superbrick/search` folder copied out of a release bundle). Assets are not
downloaded at runtime. Override their root with `SB_SEARCH_DATA` (or `CAD_SEARCH_DATA`). If none is
found, the Parts panel says so and shows the folder it looked in. The original spike's `catalog` and
`index --enc potion-base-8m` commands build them. Includes manufactured-colour filtering, feature chips,
thumbnail/list modes and recent/used parts.

## Finding pieces

- Exact IDs such as **15068** or **15068.dat** rank first even if colour/feature filters
  would normally exclude them. Such results use an available preview colour when necessary.
- Each result has a **▾ → Show similar** menu. It expands eight semantic neighbours inline under the
  thumbnail grid, independent of the current colour filter; click or drag a neighbour as usual.
- Specific descriptions give lexical/semantic evidence more weight than part popularity.
  Colours inside printed designs are no longer mistaken for manufactured-colour restrictions.
- A seeded 100-part Luna-generated benchmark uses five description styles and a part-level
  development/held-out split. Held-out top-10 retrieval improved from **66% to 84%** under
  that synthetic protocol. See [`tools/search-bench/README.md`](../tools/search-bench/README.md)
  for limitations, full results and reproduction commands.

## Projects (.sb) and agents

A `.sb` file is a SQLite project: the LDraw model (all submodels), version history and
branches, per-agent drafts, saved selections, preferences (current colour, recent parts) and
rendered assets such as instructions. `sb model.ldr` still opens plain LDraw; **File → Save as
project** or `sb --new NAME [--import model.ldr]` makes a project, and **Export LDraw…** /
`sb P.sb export -o out.ldr` writes LDraw back out.

The desktop window and any number of agents share the project file directly (WAL, short
transactions, compare-and-swap on the working tree); there is no server to start. One window
holds the editing lease; a second window follows read-only until **Take over**. Versions are
written on Save, on every accept, and as automatic checkpoints (bursts of edits are squashed;
see **History**).

### Agent tools

Every call names a user. `--user human` edits the model directly; any other name (`claude`,
`agent`, …) proposes **drafts** that appear in the window's **Agents** tab, grouped by user, for
the person to accept or reject (optionally with a note the agent sees). Drafts, selections and
candidate sets persist in the project, so an agent can resume where it left off.

```sh
sb tools                                   # list the 8 tools; `sb tools edit` for one schema
sb P.sb --user claude look '{}'            # whole model, auto-chosen views
sb P.sb --user claude search '{"query":"curved slope 2x1"}'
sb P.sb --user claude where '{"part":"3001","near":[0,-24,0]}'
sb P.sb --user claude edit '{"ops":[{"op":"place","from":"c1","pick":2}]}'
sb P.sb --user claude --ops @batch.json    # several calls in order
sb mcp --user claude [P.sb]                # MCP server (stdio) with the same tools
```

Tools: `edit` (batched, atomic: add, place, move, rotate, mirror, copy, remove, paint, connect,
hinge, group, subbuild, hide, lock; `dry_run`), `where` (legal seats for a part or assembly,
one numbered panel each, saved as `cN`), `select` (ids, box, near, colour, part, connected
by studs/rigid/touching; saved as `sN`), `look`, `search` (thumbnail grid), `verify` (errors
highlighted; `strength` min-cut between regions or weakest point), `project` (info, parts,
inspect, drafts, accept/reject, log, branches, restore, export, prefs, assets) and `publish`
(instructions or studio render as a background job; results stored as assets).

Every reply is short text plus pictures chosen for the change: a view planner picks the angles
that actually show the new or failing parts, cuts away what hides them and dims the rest, and an
adaptive cost model keeps each render well under a second (falling back to lighter detail on
very large models). A failed edit changes nothing and returns the clash: attempted parts in solid
red, blockers hatched, overlap boxes lettered with how far to move, and the nearest legal seats
as a candidate set. Units default to LDU; `"units":"grid"` uses studs/plates. Rotations are
LDraw row order (as written in `.ldr` lines) or shorthands like `"yaw90"`.

Project-root `.mcp.json` registers `sb mcp --user claude`. The older window-socket bridge
(`--mcp-stdio`, `cad-native.sock`) has been removed; open the project file instead.

## Verify

```sh
cargo build --workspace
cargo test --workspace
cargo run -p cad-cli --bin sb-dev -- roundtrip ../runs
cargo run -p cad-cli --bin sb-dev -- search-eval
python3 tools/smoke.py ../runs/design/tortoise/studio/model.ldr
```

`search-eval` checks the unchanged 168-query ranking/speed corpus, not every M5 criterion.
`sb validate MODEL` gives explicitly partial native results. `sb parity MODEL` compares with the
unchanged Python oracle and fails on differences; pin-hole parity is known to fail. Set `CAD_PYTHON`
when that oracle needs a particular interpreter/environment.

```sh
cargo run -p cad-app --bin sb -- MODEL --bench 180 --window-shot /absolute/new/window.png
cargo run -p cad-app --bin sb -- MODEL --open-submodel child.ldr --bench 180 --shot /absolute/new/viewport.png
```

`--bench` measures UI-loop intervals, not isolated GPU frame time. `--window-shot` captures the actual
window; `--shot` renders the main viewport offscreen. Verification tools never rewrite source models.

