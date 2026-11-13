"""Strict shared checker/planner decisions with dense mesh review of small flags.

Never waive an overlap because it is small or the connectors match. A flagged
pair can only be cleared by a finer geometric calculation below the unchanged
collision threshold. Unresolved calculations remain blocking.
"""
import copy
from functools import lru_cache


@lru_cache(maxsize=256)
def _dense_volumes(name_a, rot_a, pos_a, name_b, rot_b, pos_b):
    import numpy as np
    import physics
    a=physics.Body(physics.get_shape(name_a),np.array(rot_a).reshape(3,3),np.array(pos_a))
    b=physics.Body(physics.get_shape(name_b),np.array(rot_b).reshape(3,3),np.array(pos_b))
    original=physics.relation(a,b)
    if original is None or original.conflicts or original.gears:return None
    if not len(physics.mesh_tris(name_a)) or not len(physics.mesh_tris(name_b)):return None
    frame=physics.grid_frame(a,b)
    r,o=(np.eye(3),np.zeros(3)) if frame is None else frame
    la,ha=physics._frame_box(a,r,o);lb,hb=physics._frame_box(b,r,o)
    lo=np.maximum(la,lb)-1;hi=np.minimum(ha,hb)+1
    estimates=[]
    for step in (.25,.125):
        axes=physics.grid_points(lo,hi,step);shape=tuple(len(v) for v in axes)
        count=int(np.prod(shape))
        if count>12_000_000:return None  # Budget exhaustion never clears a flag.
        occupied=0
        for start in range(0,count,65536):
            ix=np.unravel_index(np.arange(start,min(start+65536,count)),shape)
            p=np.column_stack([axis[i] for axis,i in zip(axes,ix)])@r.T+o
            keep=a.occ(p,shrink=a.margin)&b.occ(p,shrink=b.margin)
            q=p[keep]
            for match in original.links:
                for ko in match.keepout:q=q[~physics.in_keepout(q,ko)]
            if len(q):q=q[physics.narrow_inside(a,q)]
            if len(q):occupied+=int(physics.narrow_inside(b,q).sum())
        estimates.append(occupied*step**3)
    return tuple(estimates)


def refine_relation(a,b,rel,threshold):
    if rel is None or rel.conflicts or not 0<rel.overlap<=32:return rel
    values=_dense_volumes(a.shape.name,tuple(a.R.flatten()),tuple(a.t),
                         b.shape.name,tuple(b.R.flatten()),tuple(b.t))
    if values is None:return rel
    result=copy.copy(rel)
    # Conservative convergence guard, not just the most favourable sample.
    result.overlap=max(values)+2*abs(values[1]-values[0])
    result.precision_review=dict(coarse=rel.overlap,estimates=list(values),
                                 guarded=result.overlap,steps=[.25,.125])
    return result


def contact_review(rel,threshold):
    return False  # A review warning must never bypass the legality gate.


def blocking_relation(rel,threshold):
    return bool(rel is not None and (rel.conflicts or rel.overlap>=threshold))


def review_reason(rel):
    return 'Unresolved geometric contact remains blocking.'
def curved_clip_matches(a,b,relation=None):
    """Recognize a curved sausage grip missed between its short shadow cylinders.

    This adds connectivity only: no keep-out is added and no solid collision or
    seating conflict is excused. Straight bars retain the normal matcher.
    """
    import numpy as np
    import physics,snaps
    name=lambda body:body.shape.name.lower().replace('\\','/').rsplit('/',1)[-1]
    if name(a)=='33078.dat':bar,clip,male=a,b,'a'
    elif name(b)=='33078.dat':bar,clip,male=b,a,'b'
    else:return []
    if not any(c.kind=='clp' for c in clip.conns):return []
    matched,conflicts,near=snaps.match(a.conns,b.conns)
    if conflicts or any(m.type=='clip-bar' for m in matched):return []
    result=[]
    for n in near:
        c,k=(a.conns[n.ia],b.conns[n.ib]) if male=='a' else (b.conns[n.ib],a.conns[n.ia])
        if c.kind!='cyl' or k.kind!='clp' or len(c.secs)!=1 or abs(c.secs[0][1]-4)>.01 or n.lateral>snaps.TOL['lateral'] or n.gap>snaps.TOL['engage']:continue
        # The clip spans several authored four-LDU curve segments. Its axis
        # must agree with a tangent in that local span, not an arbitrary tilt.
        local=[q for q in bar.conns if q.kind=='cyl' and len(q.secs)==1 and abs(q.secs[0][1]-4)<.01 and np.linalg.norm(q.pos-k.pos)<=k.length*.75+c.length]
        angle=lambda u,v:float(np.degrees(np.arccos(np.clip(abs(u@v),0,1))))
        if not local or min(angle(q.axis,k.axis) for q in local)>snaps.TOL['axis_deg']:continue
        if n.angle>max(angle(q.axis,c.axis) for q in local)+snaps.TOL['axis_deg']:continue
        rel=relation if relation is not None else physics.relation(a,b)
        if rel is None or not rel.contact or rel.overlap>=physics.COLLIDE_MIN or rel.conflicts:continue
        result.append(snaps.Match(n.ia,n.ib,'curved-clip','fixed',k.axis,k.pos,male,min(c.length,k.length),[],rule='cad-curved-contact'))
        break
    return result
