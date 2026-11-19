"""The checker: is every piece of a model legally placed and attached?

SuperBrick's own replacement for the bench's `validate` module, built on the bench's
`physics.relation` (solid overlap, connector matches, connector conflicts). It answers in the shape
`crates/engine/src/checker.rs` reads (`Report`, `Row`, `Hit`, `RowLink`).

Per piece statuses (several can apply):
  COLLISION   solid overlap with an earlier piece (>= physics.COLLIDE_MIN LDU³ outside engaged
              connectors)                                                       -- error
  CONFLICT    connectors that touch but cannot mate (a stud not pressed home, a gear jam...) -- error
  FLOATING    not connected to the main model: touching nothing, only resting, or a separate
              group                                                             -- error
  LOOSE       connected in the finished model, but not to the model built so far at its own step
              (held only by later parts)                                        -- warning
  UNVERIFIED  attached only in a way the checker cannot prove (a part without connector data
              touching the model, an unmodelled gear pair)                      -- warning
  UNCHECKED   no collision geometry (unknown part), or excluded by the caller   -- warning
  OK          none of the above
Verdict: ILLEGAL if any error, VERIFIED if every piece is OK, else UNVERIFIED.

Numbers: `uid` is the flat index + 1 (file order). The earlier piece of a pair, in build order
(step, then uid), is the one a COLLISION or CONFLICT is blamed on the later piece against.
"""
from __future__ import annotations

from collections import Counter, defaultdict

import numpy as np

import physics

STATUSES = ("COLLISION", "CONFLICT", "FLOATING", "LOOSE", "UNVERIFIED", "UNCHECKED", "OK")
ERRORS = ("COLLISION", "CONFLICT", "FLOATING")
MAX_CONFLICTS_PER_PAIR = 3


def _label(part: str) -> str:
    name = part.replace("\\", "/").rsplit("/", 1)[-1]
    return name[:-4] if name.lower().endswith(".dat") else name


def _vec(v):
    return None if v is None else [round(float(x), 2) for x in np.asarray(v, float).reshape(3)]


class _Groups:
    """Union-find over uids."""

    def __init__(self, uids):
        self.up = {u: u for u in uids}

    def find(self, u):
        while self.up[u] != u:
            self.up[u] = self.up[self.up[u]]
            u = self.up[u]
        return u

    def join(self, a, b):
        ra, rb = self.find(a), self.find(b)
        if ra != rb:
            self.up[max(ra, rb)] = min(ra, rb)

    def groups(self, uids):
        out = defaultdict(list)
        for u in uids:
            out[self.find(u)].append(u)
        return list(out.values())


def _pairs(items):
    """Pairs of (piece, body) whose full boxes (studs included) touch, by sweep on x."""
    items = sorted(items, key=lambda pb: pb[1].flo[0])
    for k, (p, a) in enumerate(items):
        for q, b in items[k + 1:]:
            if b.flo[0] > a.fhi[0] + 1:
                break
            if np.any(a.flo > b.fhi + 1) or np.any(b.flo > a.fhi + 1):
                continue
            yield (p, a, q, b) if p.uid < q.uid else (q, b, p, a)


def validate(scene, unchecked=(), relation=None):
    """Check `scene` (a cad_scene.Scene). `unchecked`: uids or part names to leave out of the
    tests. `relation(p, q, body_p, body_q)` may supply cached `physics.relation` results."""
    parts = list(scene.parts)
    by_uid = {p.uid: p for p in parts}
    order = {p.uid: (p.step, p.uid) for p in parts}
    skip = set(unchecked or ())
    status = {p.uid: set() for p in parts}
    detail = {p.uid: [] for p in parts}
    hits = {p.uid: [] for p in parts}
    pair_links = {}            # (a, b) -> {"types": Counter, "dof": str, "axis": list|None, "asserted": str}
    unproven = set()           # (a, b): contact the checker cannot prove is a connection
    gears = []
    bodies = {}

    def say(uid, st, text):
        status[uid].add(st)
        if text not in detail[uid]:
            detail[uid].append(text)

    for p in parts:
        b = None if (p.uid in skip or p.part in skip) else scene.body(p)
        if b is None or not b.shape.known:
            say(p.uid, "UNCHECKED", "no collision geometry" if p.uid not in skip and p.part not in skip
                else "left out of the check")
            continue
        bodies[p.uid] = b

    for p, a, q, b in _pairs([(by_uid[u], b) for u, b in bodies.items()]):
        try:
            rel = relation(p, q, a, b) if relation else physics.relation(a, b)
        except Exception as e:  # a part the connector model chokes on: say so, don't crash the check
            for x in (p, q):
                say(x.uid, "UNVERIFIED", f"checker could not test #{q.uid if x is p else p.uid}: {type(e).__name__}")
            continue
        if rel is None:
            continue
        early, late = (p, q) if order[p.uid] <= order[q.uid] else (q, p)
        if rel.overlap >= physics.COLLIDE_MIN:
            hits[late.uid].append(dict(with_=early.uid, kind="collision", overlap=round(float(rel.overlap), 1),
                                       reason="", where=_vec(rel.where)))
            say(late.uid, "COLLISION", f"collides with #{early.uid} {_label(early.part)} ({rel.overlap:.0f} LDU³)")
        seen = set()
        for c in rel.conflicts:
            if c.reason in seen or len(seen) >= MAX_CONFLICTS_PER_PAIR:
                continue
            seen.add(c.reason)
            hits[late.uid].append(dict(with_=early.uid, kind="conflict", overlap=0.0, reason=c.reason,
                                       where=_vec(getattr(c, "point", None))))
            say(late.uid, "CONFLICT", f"conflicts with #{early.uid} {_label(early.part)}: {c.reason}")
        for g_status, g_detail in rel.gears:
            gears.append(dict(a=p.uid, b=q.uid, status=g_status, detail=g_detail))
            if g_status == "unverified":
                for x, y in ((p, q), (q, p)):
                    say(x.uid, "UNVERIFIED", f"gear pair with #{y.uid} not modelled: {g_detail}")
        if rel.links:
            kind, axis = physics.snaps.combine_dof(rel.links)
            pair_links[(p.uid, q.uid)] = dict(types=Counter(m.type for m in rel.links), dof=kind,
                                              axis=_vec(axis), asserted="")
        elif rel.contact and rel.overlap < physics.COLLIDE_MIN and not rel.conflicts:
            # Touching with no connector link. A part with no connector data at all may well be held
            # (glued, wedged, a sticker): attached, but not provably. Two parts that both have
            # connectors and do not mate are only resting on each other.
            if not a.snaps.conns or not b.snaps.conns:
                unproven.add((p.uid, q.uid))

    for a_ in scene.asserts or ():
        a, b = int(a_["a"]), int(a_["b"])
        if a in by_uid and b in by_uid and a != b:
            key = (min(a, b), max(a, b))
            link = pair_links.setdefault(key, dict(types=Counter(), dof="fixed", axis=None, asserted=""))
            link["asserted"] = str(a_.get("note") or "asserted")
            unproven.discard(key)

    uids = [p.uid for p in parts]
    linked = _Groups(uids)
    for a, b in list(pair_links) + list(unproven):
        linked.join(a, b)
    groups = sorted(linked.groups(uids), key=lambda g: (-len(g), min(order[u] for u in g)))
    groups = [sorted(g) for g in groups]
    main = set(groups[0]) if groups and len(groups[0]) > 1 else set()

    # FLOATING: outside the main model (pieces without geometry are only UNCHECKED).
    if len(parts) > 1:
        for g in groups:
            if set(g) == main:
                continue
            for u in g:
                if u not in bodies:
                    continue
                if len(g) == 1:
                    say(u, "FLOATING", "connected to nothing (not touching, or only resting)")
                else:
                    say(u, "FLOATING", f"in a separate group of {len(g)} pieces not connected to the main model")

    # LOOSE: in the main model, but not attached to what was built up to its own step.
    steps = sorted({p.step for p in parts})
    if len(steps) > 1 and main:
        for s in steps:
            upto = [u for u in main if by_uid[u].step <= s]
            g = _Groups(upto)
            for a, b in list(pair_links) + list(unproven):
                if a in g.up and b in g.up:
                    g.join(a, b)
            comps = g.groups(upto)
            # The model so far: the component holding most earlier pieces (then the largest, then
            # the one started first).
            comps.sort(key=lambda c: min(order[u] for u in c))
            base = set(max(comps, key=lambda c: (sum(1 for u in c if by_uid[u].step < s), len(c))))
            for u in upto:
                if by_uid[u].step == s and u not in base:
                    say(u, "LOOSE", f"not connected to the model built so far at step {s} (held only by later parts)")

    # UNVERIFIED: held only by contact the checker cannot prove.
    proven = {u for ab in pair_links for u in ab}
    for a, b in unproven:
        for u, v in ((a, b), (b, a)):
            if u not in proven and u in bodies:
                say(u, "UNVERIFIED", f"touches #{v} but has no connector the checker can match")

    rows = []
    sources = Counter()
    link_types = Counter()
    for (a, b), link in pair_links.items():
        link_types.update(link["types"])
    for p in sorted(parts, key=lambda p: order[p.uid]):
        b = bodies.get(p.uid)
        src = b.source if b is not None else "none"
        sources[src] += 1
        st = [s for s in STATUSES if s in status[p.uid]] or ["OK"]
        row_links = []
        for (a, c), link in sorted(pair_links.items()):
            if p.uid in (a, c):
                row_links.append(dict(**{"with": c if a == p.uid else a}, types=dict(link["types"]),
                                      dof=link["dof"], axis=link["axis"], asserted=link["asserted"]))
        rows.append(dict(uid=p.uid, step=p.step, label=_label(p.part), status=st, detail=list(detail[p.uid]),
                         source=src, links=row_links,
                         hits=[{("with" if k == "with_" else k): v for k, v in h.items()} for h in hits[p.uid]]))

    errors = sum(1 for r in rows if any(s in ERRORS for s in r["status"]))
    warnings = sum(1 for r in rows if "LOOSE" in r["status"])
    unverified = sum(1 for r in rows if "UNVERIFIED" in r["status"])
    verdict = "ILLEGAL" if errors else "VERIFIED" if all(r["status"] == ["OK"] for r in rows) else "UNVERIFIED"
    links = sorted((a, b, max(1, sum(l["types"].values()))) for (a, b), l in pair_links.items())
    return dict(pieces=len(parts), steps=len(steps), verdict=verdict, rows=rows, errors=errors,
                warnings=warnings, unverified=unverified, groups=groups, links=links,
                link_types=dict(sorted(link_types.items())), gears=gears, sources=dict(sources))


_WORDS = {"VERIFIED": "LEGAL (verified)", "ILLEGAL": "ILLEGAL", "UNVERIFIED": "UNVERIFIED"}


def report(res, name="model", problems_only=False):
    """Plain-text report of a `validate` result (pieces as #uid)."""
    groups = res["groups"]
    lines = [f"{name}: {res['pieces']} piece{'s' if res['pieces'] != 1 else ''}, {res['steps']} step"
             f"{'s' if res['steps'] != 1 else ''}, {len(groups)} group{'s' if len(groups) != 1 else ''}"
             f" -> {_WORDS.get(res['verdict'], res['verdict'])}"]
    counts = Counter(s for r in res["rows"] for s in r["status"])
    tally = ", ".join(f"{counts[s]} {s}" for s in STATUSES if s != "OK" and counts[s])
    if tally:
        lines.append(tally)
    if res["link_types"]:
        lines.append("links: " + ", ".join(f"{n} {k}" for k, n in res["link_types"].items()))
    for r in res["rows"]:
        if problems_only and r["status"] == ["OK"]:
            continue
        lines.append(f"  #{r['uid']} {r['label']} (step {r['step']}) [{', '.join(r['status'])}]"
                     + ("".join(f"\n      {d}" for d in r["detail"])))
    return "\n".join(lines)
