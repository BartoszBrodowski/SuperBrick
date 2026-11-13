# Agent / MCP integration review (2026-11-12)

This review rests on two things. First, a real session in which an agent (Claude Code) extended the
wolf's shirt in `np.ldr` (126 parts, 4 drafts) through this MCP. Second, a read of `crates/agent`,
`crates/engine/src/{drafts,presentation,validation,lib}.rs`, `app/src/{app,panels,view}.rs`,
the tests and README/IMPLEMENTATION.

## 1. How it works

```
Claude Code (MCP client)
   │ stdio, newline-delimited JSON-RPC
   ▼
cad mcp-stdio  (shim: cargo run -p cad-cli -- mcp-stdio, from ../.mcp.json)
   │ connects ONCE to $TMPDIR/cad-native.sock (or CAD_SOCKET / --socket) and byte-pipes both ways
   ▼
Desktop app process ─────────────────────────────────────────────────────────────────────────────
  "cad-mcp" accept thread (agent::serve)            one thread per connection, session = "mcp-N"
        │ agent::dispatch → agent::call
        │   look / search_parts: run on the connection thread (snapshot + headless GPU / search index)
        │   everything else: Handle::call(job) ──────────────┐
        ▼                                                    ▼
  "cad-engine" thread: sole owner of Engine {model, undo/redo, drafts, validator}
        │ after EVERY job: publish Arc<Snapshot> (model clone + drafts clone)
        ▼
  UI thread reads the snapshot → view worker threads rebuild the scene, including ghosts,
  whenever presentation_revision (revision + draft ids/states) changes → "Agent drafts" tab
```

**Transport** (`crates/agent/src/lib.rs`)
- `serve` binds a 0600 Unix socket. Each accepted connection becomes a session named `mcp-{counter}`
  (`lib.rs:44`).
- `serve_window` handles the window that starts first. It reclaims a stale socket only if this user
  owns it and connections are refused. Every later window binds `cad-native-<pid>-<n>.sock`
  (`lib.rs:56`).
- Frames are newline-delimited JSON, capped at 4 MB (`lib.rs:93`).
- The shim (`lib.rs:270`) is a dumb pipe. It exits if no window is open ("open CAD first") and
  exits when the window closes.

**Protocol** (`dispatch`, `lib.rs:120`)
- MCP `2024-11-05`, tools only: no resources, prompts or notifications.
- The tool list (`lib.rs:150`) has two parts:
  - The 16 commands from `engine::registry()` (`engine/src/lib.rs:174`), the same list the command
    palette uses ("there is no second write API"), each with an optional `turn` added.
  - 10 agent-only reads: summary, list_parts, get_selection, check, draft_status, reject,
    search_parts, colours, changes_since, look.
- Every tool's description is just its name, or the boilerplate "Shared CAD command X".
- Results are compact JSON text. `look` returns an MCP image.

**Command routing** (`lib.rs:262`)
- Arguments are deserialized into `engine::Cmd` (serde tag `command`).
- `select`, `hide` and `lock` run directly on the user model via `Engine::execute`, which pushes an
  undo step and bumps the revision.
- Everything else goes to `Engine::propose`.

**Drafts** (`engine/src/drafts.rs`)
- *Observation gate.* `summary`/`list_parts` call `observe(session)`, which records every part's
  revision. Proposing an edit to an existing part fails unless the session saw its current revision.
- *propose* (`drafts.rs:79`):
  1. Allocates real part ids for `add`.
  2. Stamps `base` (the revisions of the referenced parts).
  3. Makes the draft depend on **every** earlier pending draft from this session (`drafts.rs:103`).
  4. Clones the model, replays all of the session's pending drafts, applies the new command, then
     restores the clone (`drafts.rs:110`). Placement and collision errors surface here.
  5. Stores the *command*, not its result.
- *Effective view* (`presentation.rs:36`): a fork of the snapshot plus the session's pending
  drafts, replayed. `list_parts`, `check` and `look` use it, so an agent builds on its own
  unaccepted work, and other sessions never see it.
- *User edits*: `execute` → `refresh_drafts` (`lib.rs:1008`). A draft goes **stale** when a
  referenced part's revision or the file list changes. It goes **blocked** when a dependency is
  stale, rejected or blocked, or when replay fails.
- *accept* (`drafts.rs:155`, UI only):
  1. Takes the dependency closure.
  2. Replays it on the real model.
  3. Pushes one undo step and marks the drafts accepted.
  4. Rebases the session's remaining drafts.
  5. Grants the session observations of the parts it touched.
- *reject* (`drafts.rs:257`) is open to the owning session (MCP `reject`) and to the UI.
- *Auto-accept*: `drafts.auto` is checked at `drafts.rs:148` but never populated. It is dead code.

**Validation** (`engine/src/validation.rs`, `lib.rs:958`)
- `add` and `move` with `free:false` call `validate_changes`. It flattens every file that contains
  an affected instance and runs the **full-model** voxel validator. It then fails on the first
  collision row that touches the change, with the message "placement collides at part {flat index}".
- The validator never emits FLOATING (`validation.rs:231`). Every row is UNVERIFIED or UNCHECKED,
  and the verdict is ILLEGAL (collision) or UNVERIFIED.
- `free:true` skips the check entirely.

**Rendering**
- `look` (`lib.rs:184`):
  - builds a scene from the session's effective model;
  - creates a new headless GPU device on every call;
  - uses `Camera::framing` (fixed yaw 35, pitch 28, auto-fit to the whole model);
  - defaults to 1024×768;
  - draws drafts solid, indistinguishable from user parts.
- App (`view.rs:143`, `presentation.rs:47`):
  - ghosts come from diffing name+colour+pose keys of the base model against each session's
    effective model;
  - blue hatching = added, red = removed, yellow = stale/blocked;
  - all sessions' drafts are drawn at once.

**UI** (`app/src/panels.rs:882`)
- The "Agent drafts" tab groups drafts by session and offers: Accept all pending, Reject pending,
  Accept turn N, Accept checked, and per-draft Accept/Reject.
- Each row reads `#id · turn N · state`. Hovering shows the `{:?}` Debug dump of the `Cmd` (a
  60-part `add` is a wall of text).
- Accept/reject call `handle.call` synchronously on the UI thread.

## 2. What one edit actually cost

This session took about 45 tool calls for 126 parts:
- summary, then list_parts (**172 KB**)
- bench validation and renders done out of band, because the MCP can't validate without proposing
  or render anything except the whole model
- a failed `add` ("placement collides at part 648"). The cause was transposed rotations, but the
  message pointed at a flat index
- probes, then free:true drafts that looked broken in `look`, all rejected
- 3× `add` plus `paint` with the rotations transposed
- `check` (**~50 KB**), `look`, and `draft_status` (**~20 KB**, which echoes every command back)

Then the user opened a fresh window, and the drafts were gone.

## 3. Problems, ranked

### Correctness and robustness
1. **Drafts are in-memory only and are lost silently.** Neither autosave nor the close guard knows
   about them: the guard checks only `revision != saved_revision` (`app.rs:1000`). Closing the
   window or restarting the app throws pending agent work away without a prompt.
2. **Session identity is a connection counter** (`lib.rs:44`). When the shim reconnects (window
   reopened, client restarted), the agent becomes `mcp-2`. It loses its observations and can no
   longer see (`draft_status`) or reject its own earlier drafts, which stay in the UI under an
   orphaned session.
3. **Which window the agent edits is implicit and invisible.** The first window owns the default
   socket, and later windows go to addresses no client is configured for. `summary` returns
   `files:["np.ldr"]` with no path, so the agent cannot tell `~/np.ldr` from
   `~/projects/np-claude/np.ldr`. The shim connects once and dies with the window. `.mcp.json` runs
   `cargo run`, so a broken or slow build means no MCP at all.
4. **Rotation arrays are column-major.** `PartSpec.rot` and `Part.rot` are `glam::DMat3`, whose
   serde form is column-major. That is the **transpose** of the LDraw line order `a b c d e f g h i`.
   `list_parts` and `add` agree with each other, so round-trips work. But any agent that writes
   rotations from LDraw text gets silently wrong orientations. With `free:true` nothing catches it;
   with `free:false` the only signal is a misleading collision. The schema doesn't say.
5. **`hide`/`lock` bypass the proposal model** (`lib.rs:262`). An agent can lock user parts, which
   blocks the user from editing them. It can also dirty the document and push undo steps, all
   without approval. This contradicts the "agents only propose" contract in README and
   IMPLEMENTATION.
6. **Every draft depends on every earlier pending draft** (`drafts.rs:103`). Rejecting draft 1
   blocks an unrelated draft 4 (here, a one-part paint), and accepting 4 drags in 1–3.
7. **Undo restamps every part** (`lib.rs:1003`). One user undo stales every pending draft that
   references existing parts and invalidates every agent's observations.
8. **Collision errors aren't actionable** (`lib.rs:979`). They give a 1-based flat index (neither
   the new part's spec index nor its id), never name the other part, and stop at the first
   collision.
9. **Floating parts are never reported.** A mid-air plate passes `add` (draft 1 in this session was
   exactly that). `free` is an all-or-nothing escape hatch.
10. **Auto-accept is unreachable code** that suggests a feature that doesn't exist.

### Performance and scaling
- `propose` clones the model and replays all of the session's pending drafts every time, which is
  O(n²) over a session. Each replayed `add` re-runs `validate_changes`, which is a full-model
  validator pass.
- `refresh_drafts` does the same replay for **every session on every user edit**, on the engine
  thread. User edit latency grows with the amount of pending agent work.
- UI accept/reject are synchronous `handle.call`s, so a big accept freezes the UI. So does an agent's
  `check`, which runs on the engine thread ahead of the user's click.
- Every `Handle::call`, including pure reads like `summary`, publishes a full snapshot clone.
- `look` creates a new wgpu device per call.

### Agent ergonomics
- **No dry run.** The only way to test a placement is to create a draft and reject it.
- **No region query.** `list_parts` dumps the whole model (172 KB for 647 parts) and `check` dumps
  every row (~50 KB). There is no filter by ids, box, draft or "changed only", and no pagination.
- **Inline JSON only.** An agent that already builds in LDraw (as the lego-design skill does) has to
  convert, transpose and re-chunk its lines into tool arguments.
- **Tool descriptions are empty.** No units (LDU), axes (y-down), rotation order, meaning of `turn`
  or `free`, or id semantics (1-based part-line ordinals). `connect.source/target` is an opaque
  `{"type":"object"}`.
- **`look` can't aim.** Fixed angle, whole model, no focus/crop/section, no highlighting of the
  agent's own drafts, and a large default size that is costly in context.
- **No feedback channel.** The agent can't label a draft, and the user can't tell the agent why
  they rejected one. The agent learns about accept/reject only by polling.

### UI
- New drafts arrive with no badge or notification. The user has to open the tab.
- Rows have no human summary (part counts, colours, region) and no thumbnail.
- There is no way to preview, isolate, focus or toggle one draft's ghosts.
- Accepted and rejected history piles up forever.

## 4. Recommendations

### Tier 1: small, high value
1. **Persist drafts.** Write a sidecar `np.ldr.drafts.json` (or put them in the autosave), keyed by
   the file's hash and revision, and restore it on open. Make the close guard warn when pending
   drafts exist.
2. **Stable agent identity.** Take the session name from `initialize.clientInfo`, or from a
   `--session` / `CAD_SESSION` value the shim sends. On reconnect, reattach drafts and
   observations.
3. **Make the target explicit.** `summary` should return the absolute path, the window/socket and
   the pending draft count. The shim should wait for a window and retry or reconnect instead of
   exiting. `.mcp.json` should call the installed `cad mcp-stdio`, not `cargo run`.
4. **Fix the rotation convention.** Serialize `rot` row-major (LDraw order) with an explicit serde
   adapter, and document it in the schema.
5. **Actionable errors.** Report every collision as
   `{spec_index, new_id, other_id, other_name, overlap}`.
6. **Keep `hide`/`lock` away from the user model.** Make them agent-local view state, or drafts.
7. **Real tool descriptions:** units, axes, rotation order, `turn`, `free`, id semantics, and an
   example per tool.
8. Delete auto-accept, or implement it behind an explicit per-session toggle.

### Tier 2: streamline the loop (one call per iteration)
- **`propose`**: one tool that does everything for an edit.
  - Takes `ldraw: "1 <c> x y z a b c d e f g h i part.dat\n..."` or JSON parts, or `ops` for
    move/paint/remove.
  - Options: `dry_run`, `label` (shown in the UI), `depends_on`, `render`, `check`.
  - Returns:
    - the draft id, or `"dry_run"`;
    - per-part status: ok / collides with #id / unsupported / unverified;
    - new connections and the bounding box;
    - a focused render.
  - It replaces add + check + look + draft_status, and the dry-run/reject cycle.
- **`propose_file(path)`**: the agent writes an `.ldr` fragment with its own tools, and the CAD
  imports it as one draft.
- **`query`** by `ids`, `box`, `near+radius` or `draft`. Returns compact rows or LDraw lines,
  paginated.
- **`look`** with `az/el/persp`, `focus: ids|box|"drafts"`, ghost highlighting, a clip box (section
  view), multiple views in one image, and a small default size.
- **Filtered `check`** by ids, draft or changed parts, plus a summary mode. A compact
  `draft_status`.
- **Resources and notifications**: `cad://drafts`, `cad://model`, and
  `notifications/resources/updated` on accept, reject or user edit, so the agent hears outcomes and
  the user's rejection notes without polling.

### Tier 3: architecture
- **Real dependencies.** A draft depends on another only if it references that draft's ids, or the
  agent declares `depends_on`.
- **Store draft results** (the added/changed part sets) next to the command. Cache each session's
  effective model by (revision, draft ids). Validate incrementally around the changed parts using
  a spatial index instead of a full-model pass per replay.
- **Fine-grained undo stamping**: only bump the parts whose state actually changed.
- **Keep the UI responsive**: async accept/reject with progress. Cache the headless GPU. Use a typed
  draft-state enum instead of strings.

### UI
- Each draft row shows:
  - the agent's label;
  - an automatic summary, e.g. "+28 parts · 3666×4, 24201×8 … · blue/white · y126–146";
  - a thumbnail.
- Clicking a row highlights only that draft and frames it; an eye icon toggles it.
- The tab shows a badge when new drafts arrive.
- "Reject with note" sends the note back to the agent.
- Accepted and rejected history collapses.

## 5. Target loop

Today (one iteration):

```
summary → list_parts (172 KB) → [external validate/render] → add → check (50 KB) → look → draft_status
```

With Tier 1 and 2:

```
query(box) → propose(ldraw, dry_run, render:{focus:"new"}) … iterate … → propose(ldraw, label) → user accepts
```

That is about 3–6 calls instead of about 45, with less than 10 KB of text per call, and nothing
lost if the window closes.
