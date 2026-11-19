"""The model the checker works on: placed pieces in build order.

SuperBrick's own replacement for the bench's `scene` module. `connectors.py` builds a `Scene`
from the flattened model and `cad_validate.validate` checks it.

Coordinates are LDraw's: `pos` in LDU, `rot` the 9 numbers of the rotation in LDraw row order
(as written on a type-1 line). A piece's `uid` is its flat index + 1 (file order).
"""
from __future__ import annotations

import numpy as np

import physics


class Placed:
    """One piece: part file name, LDraw colour, position, rotation (row order) and build step."""

    __slots__ = ("uid", "part", "color", "pos", "rot", "step")

    def __init__(self, uid, part, color, pos, rot, step=1):
        self.uid = int(uid)
        self.part = part
        self.color = int(color)
        self.pos = [float(x) for x in pos]
        self.rot = [float(x) for x in rot]
        self.step = int(step or 1)

    def __repr__(self):
        return f"Placed(#{self.uid} {self.part} c{self.color} step {self.step})"


def make_body(placed: Placed):
    """The piece as a physics body, or None when its geometry cannot be built (unknown part,
    unreadable file): such a piece is reported UNCHECKED, never silently passed."""
    try:
        shape = physics.get_shape(placed.part)
    except Exception:
        return None
    R = np.asarray(placed.rot, dtype=float).reshape(3, 3)
    return physics.Body(shape, R, np.asarray(placed.pos, dtype=float), uid=placed.uid, color=placed.color)


class Scene:
    """Pieces in file order, plus user-asserted links (`0 !BENCH ASSERT a b note`)."""

    def __init__(self, name="model"):
        self.name = name
        self.parts: list[Placed] = []
        # [{"a": uid, "b": uid, "note": str}]
        self.asserts: list[dict] = []
        self._bodies: dict[int, object] = {}

    def add(self, part, color, pos, rot, step=1) -> Placed:
        p = Placed(len(self.parts) + 1, part, color, pos, rot, step)
        self.parts.append(p)
        return p

    def body(self, p: Placed):
        """Body of piece `p` (cached per scene; subclasses may cache across scenes)."""
        if p.uid not in self._bodies:
            self._bodies[p.uid] = make_body(p)
        return self._bodies[p.uid]
