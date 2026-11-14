"""Read-only connector/pose service using the existing full connector oracle.

Temporary bridge until the native shadow/physics port reaches parity. No model files
are edited. All writes remain native engine transactions.
"""
import json, os, sys, traceback
from pathlib import Path
sys.path.insert(0, sys.argv[1])
if not os.environ.get('BENCH_SHADOW') and Path(sys.argv[2]).is_dir():
    os.environ['BENCH_SHADOW'] = sys.argv[2]
import numpy as np
import physics, snaps
_policy_dev=Path(sys.argv[1]).resolve().parent/'cad/app/src'
if _policy_dev.is_dir():sys.path.insert(0,str(_policy_dev))
from cad_contact_policy import contact_review, blocking_relation, review_reason, refine_relation, curved_clip_matches
from functools import lru_cache

def _on_surface(points,colors,tris,triangle_colors,tol=.035):
    """Conservative two-way surface check, not a bounding-box mirror guess."""
    e1=tris[:,1]-tris[:,0];e2=tris[:,2]-tris[:,0];normal=np.cross(e1,e2)
    length=np.linalg.norm(normal,axis=1);normal=normal/np.maximum(length[:,None],1e-12)
    aa=np.sum(e1*e1,axis=1);bb=np.sum(e2*e2,axis=1);ab=np.sum(e1*e2,axis=1);den=aa*bb-ab*ab
    for start in range(0,len(points),32):
        delta=points[start:start+32,None,:]-tris[None,:,0,:]
        x=np.sum(delta*e1,axis=2);y=np.sum(delta*e2,axis=2)
        u=(x*bb-y*ab)/np.maximum(den,1e-20);v=(y*aa-x*ab)/np.maximum(den,1e-20)
        hit=(np.abs(np.sum(delta*normal,axis=2))<=tol)&(u>=-1e-5)&(v>=-1e-5)&(u+v<=1+1e-5)&(length>1e-8)&(colors[start:start+32,None]==triangle_colors[None,:])
        if not np.all(hit.any(axis=1)):return False
    return True

# Approximate mirror match (see _mirror_part): largest allowed deviation (LDU) and deviating share of area.
_MIRROR_APPROX_MAX=1.0
_MIRROR_APPROX_AREA=.05

def _surface_distance(points,colors,tris,triangle_colors,limit=np.inf):
    """Exact distance from each point to the nearest same-colour triangle; stops early (returning what it
    has, with the offending chunk) once some point is farther than `limit`."""
    a=tris[:,0];b=tris[:,1];c=tris[:,2];ab=b-a;ac=c-a
    def seg(p,x,y):
        xy=y-x;t=np.clip(np.sum((p-x)*xy,axis=2)/np.maximum(np.sum(xy*xy,axis=1),1e-12),0,1)
        return np.linalg.norm(p-(x+t[...,None]*xy),axis=2)
    out=np.full(len(points),np.inf)
    for start in range(0,len(points),32):
        p=points[start:start+32,None,:];ap=p-a
        d1=np.sum(ab*ap,axis=2);d2=np.sum(ac*ap,axis=2);bp=p-b;d3=np.sum(ab*bp,axis=2);d4=np.sum(ac*bp,axis=2)
        cp=p-c;d5=np.sum(ab*cp,axis=2);d6=np.sum(ac*cp,axis=2)
        va=d3*d6-d5*d4;vb=d5*d2-d1*d6;vc=d1*d4-d3*d2;den=va+vb+vc;den=np.where(np.abs(den)<1e-12,1e-12,den)
        v=vb/den;w=vc/den;inside=(v>=0)&(w>=0)&(v+w<=1)
        face=np.linalg.norm(p-(a+v[...,None]*ab+w[...,None]*ac),axis=2)
        d=np.minimum(np.minimum(seg(p,a,b),seg(p,b,c)),seg(p,a,c));d=np.where(inside,np.minimum(face,d),d)
        d=np.where(colors[start:start+32,None]==triangle_colors[None,:],d,np.inf)
        out[start:start+32]=d.min(1)
        if np.any(out[start:start+32]>limit):break
    return out

def _mirror_deviation(desired,dcol,transformed,tcol):
    """(largest deviation, deviating share of area) of two meshes both ways, or None past the limits."""
    worst=(0.,0.)
    for x,xcol,y,ycol in ((desired,dcol,transformed,tcol),(transformed,tcol,desired,dcol)):
        d=np.maximum(_surface_distance(x.reshape(-1,3),np.repeat(xcol,3),y,ycol,_MIRROR_APPROX_MAX).reshape(-1,3).max(1),
                     _surface_distance(x.mean(1),xcol,y,ycol,_MIRROR_APPROX_MAX))
        if d.max()>_MIRROR_APPROX_MAX:return None
        area=.5*np.linalg.norm(np.cross(x[:,1]-x[:,0],x[:,2]-x[:,0]),axis=1)
        share=area[d>.035].sum()/max(area.sum(),1e-9)
        if share>_MIRROR_APPROX_AREA:return None
        worst=(max(worst[0],float(d.max())),max(worst[1],float(share)))
    return worst

@lru_cache(maxsize=256)
def _mirror_part(name):
    import ldraw,itertools,re
    lib=ldraw.get_library();description=lib.description(name);canonical=name
    moved=re.search(r'Moved to\s+(\S+)',description,re.I)
    if moved:
        canonical=moved[1].removesuffix('.dat')+'.dat';description=lib.description(canonical)
    if re.search(r'pattern|sticker|printed|decoration',description,re.I):
        raise ValueError(f'{name}: decorated part needs an explicit verified mirrored decoration; not substituted')
    source=lib.geom(name,'std');original=source.tris.astype(float)
    if not len(original):raise ValueError(f'{name}: missing mirror geometry')
    F=np.diag([-1.,1.,1.]);desired=original@F;center=(desired.reshape(-1,3).min(0)+desired.reshape(-1,3).max(0))/2
    def signature(points):return sorted(set(map(tuple,np.rint(points.reshape(-1,3)/.035).astype(np.int64))))
    wanted=signature(desired-center)
    swap=re.sub(r'\b(left|right)\b',lambda m:'Right' if m[0].lower()=='left' else 'Left',description,flags=re.I)
    candidates=list(dict.fromkeys([name,canonical]))
    if swap!=description:
        candidates+=sorted(k for k,v in lib.parts_index().items() if v.strip().lower()==swap.strip().lower() and k!=name)
    rotations=[]
    for perm in itertools.permutations(range(3)):
        for signs in itertools.product([-1.,1.],repeat=3):
            R=np.eye(3)[:,perm]*signs
            if np.linalg.det(R)>.9:rotations.append(R)
    rotations.sort(key=lambda R:(-float(np.trace(R)),tuple(R.flatten())))
    near=[]
    for candidate in candidates:
        mesh=lib.geom(candidate,'std');tri=mesh.tris.astype(float)
        if not len(tri):continue
        c=(tri.reshape(-1,3).min(0)+tri.reshape(-1,3).max(0))/2
        for R in rotations:
            transformed=(tri-c)@R.T+center
            if not np.allclose(transformed.min((0,1)),desired.min((0,1)),atol=.035) or not np.allclose(transformed.max((0,1)),desired.max((0,1)),atol=.035):continue
            near.append((candidate,R,c,transformed,mesh.tcol))
            # Equivalent planar surfaces may use different diagonal vertices.
            if signature(transformed-center)!=wanted:
                if not _on_surface(desired.reshape(-1,3),np.repeat(source.tcol,3),transformed,mesh.tcol):continue
                if not _on_surface(transformed.reshape(-1,3),np.repeat(mesh.tcol,3),desired,source.tcol):continue
            if not _on_surface(desired.mean(1),source.tcol,transformed,mesh.tcol):continue
            if not _on_surface(transformed.mean(1),mesh.tcol,desired,source.tcol):continue
            return candidate,R,center-R@c
    # No exact match. LDraw draws curves as polygons whose phase can be chiral: the axl3hol* axle-hole
    # primitives are four-fold pinwheels, so a symmetric brick whose only mirror plane contains the axle
    # axis (92013) matches its own mirror image only to ~0.7 LDU, and only around the hole. Accept the
    # closest pose whose every deviation is small and whose deviating surface is a sliver of the part;
    # real left/right differences (wedges, fairings, handed gears) are many LDU over large areas.
    best=None
    for candidate,R,c,transformed,tcol in near:
        dev=_mirror_deviation(desired,source.tcol,transformed,tcol)
        if dev is not None and (best is None or dev<best[0]):best=(dev,(candidate,R,center-R@c))
    if best:return best[1]
    raise ValueError(f'{name}: no verified symmetric geometry or left/right counterpart found; mirror refused')

def mirror_specs(req):
    axis=int(req['axis'])
    if axis not in (0,1,2):raise ValueError('Mirror axis must be X, Y or Z')
    F=np.eye(3);F[axis,axis]=-1;local=np.diag([-1.,1.,1.]);pivot=np.array(req['pivot']);result=[];mapping={}
    for part in req['specs']:
        name,A,offset=_mirror_part(part['name']);R=np.array(part['rot']).reshape(3,3).T;pos=np.array(part['pos'])
        result.append(dict(name=name,color=part['color'],pos=(pivot+F@(pos-pivot)+F@R@local@offset).tolist(),rot=(F@R@local@A).T.flatten().tolist(),id=None))
        mapping[part['name']]=name
    return dict(parts=result,mapping=mapping)
@lru_cache(maxsize=4096)
def _body(name,rot,pos,uid):
    return physics.Body(physics.get_shape(name),np.array(rot).reshape(3,3).T,np.array(pos),uid=uid)

@lru_cache(maxsize=8)
def _drag_index(key):
    """Free endpoints of the fixed scene, rebuilt only when its geometry changes."""
    rows=[(j,hidden,_body(name,rot,pos,j+1)) for j,name,rot,pos,hidden in key]
    occupied=set();order=sorted(rows,key=lambda row:row[2].flo[0])
    for k,(i,_,a) in enumerate(order):
        for j,_,b in order[k+1:]:
            if b.flo[0]>a.fhi[0]+1:break
            if np.any(a.flo>b.fhi+1) or np.any(b.flo>a.fhi+1):continue
            for m in physics.snap_links(a,b)[0]:
                for index,c,conn in ((i,m.ia,a.conns[m.ia]),(j,m.ib,b.conns[m.ib])):
                    # Partial engagement on a long bar/axle leaves its exposed span usable.
                    # A stud fills an anti-stud, but an open stud's hole goes on past the recess
                    # (a pin can still enter from the other end), so only a full engagement uses it up.
                    full=conn.length>0 and m.engage>=conn.length-.5
                    bar_hole=conn.gender=='F' and any(s[1]<5 for s in getattr(conn,'secs',()))
                    if m.type in ('ball','hinge') or full or (m.type in ('stud','side-stud') and not bar_hole):occupied.add((index,c))
    grid={}
    for j,hidden,b in rows:
        if hidden:continue
        for c,world in enumerate(b.conns):
            if (j,c) not in occupied:
                grid.setdefault(tuple(np.floor(world.pos/40).astype(int)),[]).append((j,c,world))
    return grid

# LDCad FGR positions are centers by default; CYL positions are starts.
# The legacy oracle applies CYL's default to both, shifting a single/dual
# locking hinge sideways by 12 LDU. Correct the import here, without changing
# the original bench toolkit or its historical fixtures. Explicit center flags
# and bench-authored declarations retain their stated/legacy interpretation.
# Confirmed by LDCad's author: https://forums.ldraw.org/thread-27637.html
_make_conns = snaps.make_conns
def _centered_fingers(kind, properties, src, rule='ldcad'):
    if kind == 'SNAP_FGR' and rule == 'ldcad' and 'center' not in properties:
        properties = dict(properties, center='true')
    return _make_conns(kind, properties, src, rule)
snaps.make_conns = _centered_fingers

def run(req):
    if req['action']=='mirror_specs':return mirror_specs(req)
    entries = list(req['parts'])
    if req['action']=='drag' and not req.get('moving'):
        new=dict(req['part'],id=max([p['id'] for p in req['roots']]+[0])+1)
        entries.append(new)
        req=dict(req,roots=req['roots']+[new],moving=[new['id']],source={'part':new['id']})
    bodies = [_body(p['name'],tuple(p['rot']),tuple(p['pos']),i+1) for i,p in enumerate(entries)]
    roots = {p['id']:p for p in req['roots']}
    pairs = {}
    # Sweep broad phase, not an all-pairs geometry pass.
    # Free dragging may detach, so it needs no scene-wide connectivity graph.
    if req['action']=='check_pairs':
        requested=[tuple(p) for p in req.get('pairs',[])];pairs=list(requested)
        if req.get('all_nearby'):
            seen={tuple(sorted(p)) for p in pairs};order=sorted(range(len(bodies)),key=lambda i:bodies[i].flo[0])
            for at,a in enumerate(order):
                for b in order[at+1:]:
                    if bodies[b].flo[0]>bodies[a].fhi[0]+1:break
                    if np.any(bodies[a].flo>bodies[b].fhi+1) or np.any(bodies[b].flo>bodies[a].fhi+1):continue
                    key=tuple(sorted((a,b)))
                    if key not in seen:pairs.append(key);seen.add(key)
        result=[]
        for a,b in pairs:
            x,y=sorted((a,b));rel=physics.relation(bodies[x],bodies[y])
            rel=refine_relation(bodies[x],bodies[y],rel,physics.COLLIDE_MIN)
            warning=contact_review(rel,physics.COLLIDE_MIN)
            bad=blocking_relation(rel,physics.COLLIDE_MIN)
            if not bad and not warning and (a,b) not in requested:continue
            reasons=[c.reason for c in rel.conflicts] if rel else []
            precision=getattr(rel,'precision_review',None)
            if precision:reasons.append(f"Dense triangle review: 0.25/0.125 LDU estimates {precision['estimates']}; guarded overlap {precision['guarded']:.3f} LDU³, threshold {physics.COLLIDE_MIN:g}.")
            if warning:reasons.insert(0,review_reason(rel))
            elif rel and rel.overlap>=physics.COLLIDE_MIN:reasons.insert(0,f'Solid geometry overlaps by {rel.overlap:.2f} LDU³ after connector keep-outs (threshold {physics.COLLIDE_MIN:g} LDU³).')
            points=[np.asarray(c.point).tolist() for c in rel.conflicts] if rel else []
            if rel and rel.overlap>=physics.COLLIDE_MIN and rel.where is not None:points.append(np.asarray(rel.where).tolist())
            result.append(dict(a=a,b=b,collision=bad,warning=warning,overlap=float(rel.overlap) if rel else 0.,raw_overlap=float(rel.raw_overlap) if rel else 0.,reasons=reasons,points=points,types=sorted(set(m.type for m in physics.snap_links(bodies[x],bodies[y])[0]))))
        return dict(pairs=result)
    if req['action'] in ('endpoints','compatible'):
        connectors=[dict(part=p['id'],leaf=i,index=c,position=x.pos.tolist(),axis=x.axis.tolist(),label=x.label())
                    for i,(p,b) in enumerate(zip(entries,bodies)) if not p.get('hidden') for c,x in enumerate(b.conns)]
        if req['action']=='endpoints':return dict(graph=dict(parts=sorted(roots),joints=[]),connectors=connectors)
        source=req['source'];local=[c for i,(p,b) in enumerate(zip(entries,bodies)) if p['id']==source['part'] and (source.get('leaf') is None or source['leaf']==i) for k,c in enumerate(b.conns) if source.get('index') is None or source['index']==k]
        return dict(destinations=[dict(part=c['part'],leaf=c['leaf'],index=c['index']) for c in connectors if c['part']!=source['part'] and any(snaps.compatible(a,bodies[c['leaf']].conns[c['index']]) for a in local)])
    order = [] if req['action']=='drag' else sorted(range(len(bodies)), key=lambda i: bodies[i].flo[0])
    for k,i in enumerate(order):
        a=bodies[i]
        for j in order[k+1:]:
            b=bodies[j]
            if b.flo[0]>a.fhi[0]+1: break
            if entries[i]['id']==entries[j]['id'] or np.any(a.flo>b.fhi+1) or np.any(b.flo>a.fhi+1): continue
            ms=physics.snap_links(a,b)[0]+curved_clip_matches(a,b)
            if ms:
                key=tuple(sorted((entries[i]['id'],entries[j]['id'])))
                pairs.setdefault(key,[]).extend(ms)
    joints=[]
    for n,((a,b),ms) in enumerate(sorted(pairs.items())):
        kind,axis=snaps.combine_dof(ms)
        if kind=='ball': motion={'Ball':{'axes':np.eye(3).tolist()}}
        elif 'rotates' in kind and axis is not None:
            motion={('Cylindrical' if 'slides' in kind else 'Revolute'):{'axis':np.asarray(axis).tolist()}}
        elif 'slides' in kind and axis is not None: motion={'Slide':{'axis':np.asarray(axis).tolist()}}
        elif kind=='fixed': motion='Fixed'
        else: continue # unknown/free is not a rigid connection
        joints.append(dict(id=n+1,a=a,b=b,point=np.asarray(ms[0].point).tolist(),motion=motion,
                           label=' + '.join(sorted(set(m.type for m in ms)))))
    if req['action']=='analyze':
        connectors=[dict(part=p['id'],leaf=i,index=c,position=x.pos.tolist(),axis=x.axis.tolist(),label=x.label())
                    for i,(p,b) in enumerate(zip(entries,bodies)) for c,x in enumerate(b.conns)]
        near=[]
        spherical=[(i,k,c) for i,b in enumerate(bodies) for k,c in enumerate(b.conns) if c.kind=='gen' and c.bounding and c.bounding[0]=='sph']
        for at,(i,ci,a) in enumerate(spherical):
            for j,cj,b in spherical[at+1:]:
                if entries[i]['id']==entries[j]['id'] or not snaps.compatible(a,b):continue
                distance=float(np.linalg.norm(a.pos-b.pos))
                if snaps.TOL['point']<distance<=4:
                    near.append(dict(source=dict(part=entries[i]['id'],leaf=i,index=ci),target=dict(part=entries[j]['id'],leaf=j,index=cj),distance=distance,position=((a.pos+b.pos)/2).tolist(),label='ball/socket — not seated'))
        return dict(graph=dict(parts=sorted(roots),joints=joints),connectors=connectors,near_joints=near)
    def component(start):
        seen={start}
        while True:
            more={b if a in seen else a for a,b in pairs if (a in seen)!=(b in seen)}
            if not more: return seen
            seen.update(more)
    fixed_cache={};refusals=[]
    def checked(moving,R,t,detach=(),preserve=True):
        moved=[physics.Body(b.shape,R@b.R,R@b.t+t,uid=b.uid) if p['id'] in moving else b
               for p,b in zip(entries,bodies)]
        fixed_key=tuple(sorted(moving))
        if fixed_key not in fixed_cache:
            indices=[j for j,q in enumerate(entries) if q['id'] not in moving]
            fixed_cache[fixed_key]=(indices,np.array([bodies[j].flo for j in indices]).reshape(-1,3),np.array([bodies[j].fhi for j in indices]).reshape(-1,3))
        indices,lo,hi=fixed_cache[fixed_key]
        for i,(p,a) in enumerate(zip(entries,moved)):
            if p['id'] not in moving: continue
            for index in np.flatnonzero(np.all(a.flo<=hi+1,axis=1)&np.all(lo<=a.fhi+1,axis=1)):
                j=indices[index];b=moved[j]
                rel=physics.relation(a,b)
                if rel is not None and (rel.overlap>=physics.COLLIDE_MIN or rel.conflicts):
                    if len(refusals)<3:
                        why='; '.join(c.reason for c in rel.conflicts) or f'overlap {rel.overlap:.3f} LDU³'
                        refusals.append(f"#{p['id']} ({p['name']}) against #{entries[j]['id']} ({entries[j]['name']}): {why}")
                    return None
        cut={tuple(sorted((j['a'],j['b']))) for j in joints if j['id'] in detach}
        for a,b in pairs:
            if not preserve or (a in moving)==(b in moving) or (a,b) in cut: continue
            found=False
            for i,p in enumerate(entries):
                if p['id']!=a: continue
                for j,q in enumerate(entries):
                    if q['id']==b and physics.snap_links(moved[i],moved[j])[0]: found=True; break
                if found: break
            if not found: return None
        return [dict(id=uid,pos=(R@np.array(roots[uid]['pos'])+t).tolist(),
                     rot=(R@np.array(roots[uid]['rot']).reshape(3,3).T).T.reshape(-1).tolist()) for uid in sorted(moving)]
    if req['action']=='rotate':
        moving=set(req['moving']); axis=np.array(req['axis'],float); axis/=np.linalg.norm(axis)
        angle=np.deg2rad(req['angle']); x,y,z=axis
        K=np.array([[0,-z,y],[z,0,-x],[-y,x,0]])
        R=np.eye(3)+np.sin(angle)*K+(1-np.cos(angle))*K@K
        pivot=np.array(req['pivot']); poses=checked(moving,R,pivot-R@pivot,req['detach'])
        if poses is None: raise ValueError('Rotation blocked: collision or a preserved connection would break')
        return dict(poses=poses)
    if req['action'] in ('connect','destinations','drag'):
        drag=req['action']=='drag'; destinations=req['action']=='destinations'
        want_all=drag and bool(req.get('all')); every=[]
        source=req['source']; target=req.get('target'); moving=set(req['moving']) if drag else component(source['part'])
        if target is not None and target['part'] in moving: raise ValueError('These pieces are already connected; use H to articulate their joint')
        near=np.array(req.get('near',[0,0,0]))
        allowed={tuple((x['part'],x['leaf'],x['index'])) for x in req.get('targets',[])}
        candidates=[]
        nearby=None
        target_rows=None
        if drag:
            fixed_key=tuple((j,q['name'],tuple(q['rot']),tuple(q['pos']),q.get('hidden',False)) for j,q in enumerate(entries) if q['id'] not in moving)
            grid=_drag_index(fixed_key);cell=np.floor(near/40).astype(int)
            # 'all' enumerates every legal seat within 'radius' (agents); plain drag keeps the
            # 45 LDU cursor neighbourhood.
            radius=float(req.get('radius',45));reach=max(2,int(np.ceil(radius/40)))
            choices=[]
            for x in range(cell[0]-reach,cell[0]+reach+1):
                for y in range(cell[1]-reach,cell[1]+reach+1):
                    for z in range(cell[2]-reach,cell[2]+reach+1):
                        for j,cj,world in grid.get((x,y,z),[]):
                            distance=float(np.linalg.norm(world.pos-near))
                            if distance<=radius:choices.append((distance,j,cj))
            nearby={(j,cj) for _,j,cj in sorted(choices)}
            by_body={}
            for _,j,cj in sorted(choices):by_body.setdefault(j,[]).append((cj,bodies[j].conns[cj]))
            target_rows=[(j,entries[j],bodies[j],conns) for j,conns in by_body.items()]
        for i,(p,a) in enumerate(zip(entries,bodies)):
            if p['id']!=source['part'] or (source.get('leaf') is not None and i!=source['leaf']): continue
            source_conns=enumerate(a.conns)
            for ci,local in source_conns:
                if source.get('index') is not None and ci!=source['index']: continue
                for j,q,b,conns in (target_rows if drag else ((j,q,b,enumerate(b.conns)) for j,(q,b) in enumerate(zip(entries,bodies)))):
                    if q['id'] in moving or q.get('hidden',False): continue
                    if target is not None and (q['id']!=target['part'] or (target.get('leaf') is not None and j!=target['leaf'])): continue
                    for cj,world in conns:
                        if drag and (j,cj) not in nearby: continue
                        if target is not None and target.get('index') is not None and cj!=target['index']: continue
                        endpoint=(q['id'],j,cj)
                        if destinations and endpoint not in allowed: continue
                        if drag and np.linalg.norm(world.pos-near)>radius: continue
                        if not snaps.compatible(local,world): continue
                        relative=world.frame.T @ local.frame
                        aligned=float(np.degrees(np.arctan2(relative[0,2]-relative[2,0],relative[0,0]+relative[2,2])))
                        # Connector frames may themselves be tilted by any angle. Also retain
                        # the current free twist where the connector permits it, not just quarters.
                        angles=(req.get('twist',0),90,180,270,aligned,aligned+180) if drag else (req.get('twist',0),90,180,270)
                        ball=None
                        for angle in dict.fromkeys(round(x%360,6) for x in angles):
                            for flip in (False,True):
                                for R,t,m in snaps.mate(local,world,angle,flip):
                                    if m.dof=='ball': ball=m
                                    # Seat fully, then preserve the part's facing direction before
                                    # minimizing travel. Origin distance alone tips/flips a plate
                                    # just because its hinge is offset from that origin. Use the
                                    # part's current local Y, not world Y (vertical builds count).
                                    up=a.R[:,1]
                                    facing=1-float(np.clip(up@(R@up),-1,1))
                                    turn=float(np.arccos(np.clip((np.trace(R)-1)/2,-1,1)))
                                    travel=float(np.linalg.norm(R@a.t+t-a.t))
                                    score=(-round(float(m.engage),3),round(facing,6),round(turn,6),round(travel,6))
                                    if drag:
                                        score=(round(float(np.linalg.norm(world.pos-near)),3),round(turn,6),-round(float(m.engage),3),round(travel,6))
                                    candidates.append((score,R,t,endpoint))
                        if ball is not None:
                            # A ball joint turns freely, so the frames' quarter twists are only
                            # samples. Also offer the piece exactly as it is, just moved onto the
                            # ball: a mirrored leg's socket frame has the other handedness, and every
                            # frame-aligned pose would swing it into its neighbour.
                            t=np.asarray(world.pos,float)-np.asarray(local.pos,float)
                            travel=float(np.linalg.norm(t))
                            score=(-round(float(ball.engage),3),0.0,0.0,round(travel,6))
                            if drag:
                                score=(round(float(np.linalg.norm(world.pos-near)),3),0.0,-round(float(ball.engage),3),round(travel,6))
                            candidates.append((score,np.eye(3),t,endpoint))
        candidates.sort(key=lambda x:x[0]); seen=set(); legal=[]
        found=set()
        for _,R,t,endpoint in candidates:
            if destinations and endpoint in found: continue
            key=(endpoint if destinations else ()) + tuple(np.round(np.r_[R.reshape(-1),t],4))
            if key in seen: continue
            seen.add(key)
            if drag and len(seen)>(int(req.get('budget',2048)) if want_all else 64): break
            poses=checked(moving,R,t,preserve=not drag)
            if poses is not None:
                if destinations:
                    found.add(endpoint); continue
                if want_all:
                    every.append(dict(poses=poses,rotation=R.T.reshape(-1).tolist(),translation=t.tolist()))
                    if len(every)>=int(req.get('limit',64)): break
                    continue
                legal.append(poses)
                if len(legal)>req.get('pose',0): return dict(poses=legal[-1],candidate=len(legal),rotation=R.T.reshape(-1).tolist(),translation=t.tolist())
        if destinations: return dict(destinations=[dict(part=a,leaf=b,index=c) for a,b,c in sorted(found)])
        if want_all: return dict(candidates=every)
        if legal: return dict(poses=legal[0],candidate=0)
        detail=(' Checked poses were blocked by: '+'; '.join(dict.fromkeys(refusals))) if refusals else ' Choose matching source and destination dots.'
        raise ValueError('No legal mating pose for these connectors.'+detail)
    raise ValueError('Unknown connector action')

for line in sys.stdin:
    try: reply=dict(result=run(json.loads(line)))
    except Exception as e: reply=dict(error=f'{type(e).__name__}: {e}')
    try: print(json.dumps(reply),flush=True)
    except (BrokenPipeError,ConnectionResetError):
        # Native parent quit/terminated: don't report over the broken pipe.
        fd=os.open(os.devnull,os.O_WRONLY)
        os.dup2(fd,sys.stdout.fileno());os.close(fd)
        break
