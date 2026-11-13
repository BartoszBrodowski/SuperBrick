# Mechanical tools: Connect (C) and Hinge (H)

## Connect pins, axles, hinge fingers and balls

1. Press **C**, then click the **moving** piece and the destination piece.
2. Leave connector selection on **Auto**, or click connector dots / choose the named connectors
   in the two dropdowns. Hole centres can be picked through their connector dots.
3. Press **Enter** or **Connect**. Change **Pose** before connecting to request an alternate fit.

The source's connected assembly moves together. An already connected source/destination pair
must be articulated with H rather than reconnected. Mating tries compatible connector orientations,
prefers engagement, and checks collisions, incompatible connector conflicts and preserved links.

## Articulate

- Press **H** and click a piece. Its rigid chunk and attached moving-side branches are selected.
- Choose the boundary joint in the joint dropdown. Fixed connections are not rotation pivots.
- Drag a ring; a yellow wire preview shows the proposed pose. Release applies one validated,
  undoable transaction. The final solid/connector check happens on release, not every preview frame.
- Click a ring / Axis button to choose an axis. Hinges show one; unconstrained balls show three.
- **Plain arrows** rotate the chosen axis in 15° steps. **Shift+arrows** request translation instead.
- Ring dragging snaps to 15°; holding Shift during the drag gives 1° increments.
- Multiple hinges on the **same axis line** act as one pivot and stay connected together.
  Opposite axis directions are equivalent; merely parallel but offset axes are not.
- When an incompatible loop exists, explicitly approve the named opposite non-rigid joint(s) to detach.
  Fixed braces are never silently cut. Invalid/unconfirmed motion leaves the model unchanged.
  Unconfirmed release now displays the refusal instead of silently dropping the preview.
- Switch to S to orbit with empty-space dragging; Alt+empty-space drag also orbits in H mode.

Outside H mode, plain arrows rotate selected parts by 90° and Shift+arrows move on the
camera-relative stud plane. R remains the 90° rotation shortcut.

## Implementation and limits

The native graph planner groups all confirmed links per part pair before computing mobility.
A persistent Python/NumPy helper reuses the full existing `bench-next` shadow connector and solid
logic; Python never writes the model. The installer copies its modules and shadow data beside the
native executable. Native command registration, revision stamps, locks, atomic rollback and undo
apply equally to C/H commands and MCP proposals.

Current mechanical editing is limited to the main document model. Submodel references are rigid
assembly nodes; editing the internal joints of shared instances needs instance-aware constraints
and is refused in child viewports. Ordinary drag/drop combines an immediate native stud/ground
preview with background full-connector mating; C provides explicit endpoint selection for difficult
fits. A drop requires a confirmed visible pose and never completes later at an unseen pose.
The broader native validator is still incomplete. Sliding manipulators,
continuous collision-checked GPU previews and large-assembly latency targets remain future work.

Regression tests cover separated pin/hole, axle/cross-hole and finger-hinge mating; actual hinge
rotation/undo; two ball/socket families; H click/arrow and C two-piece/Enter UI paths; upstream
selection, opposite-loop cuts and fixed-brace refusal. A two-hinge/bridging-plate fixture verifies
both bearings remain engaged after rotation, tilted-world operation and undo. Real egui pointer
events cover ring press/drag/release committing the paired-hinge preview.
