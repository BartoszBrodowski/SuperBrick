"""SuperBrick's corrections to how the bench reads LDCad shadow data.

Applied by the connector/checker service and by `sb-dev parity`'s bench reference, so the
checker, `where`/`connect`/`hinge` and the parity reference all read connectors the same way.

LDCad FGR positions are centres by default; CYL positions are starts. The bench applies CYL's
default to both, shifting single/dual locking hinges sideways by 12 LDU. Explicit `center`
flags and bench-authored declarations keep their stated meaning.
Confirmed by LDCad's author: https://forums.ldraw.org/thread-27637.html
"""


def apply(snaps):
    if getattr(snaps.make_conns, "_cad_centered_fingers", False):
        return
    make_conns = snaps.make_conns

    def centered_fingers(kind, properties, src, rule="ldcad"):
        if kind == "SNAP_FGR" and rule == "ldcad" and "center" not in properties:
            properties = dict(properties, center="true")
        return make_conns(kind, properties, src, rule)

    centered_fingers._cad_centered_fingers = True
    snaps.make_conns = centered_fingers
