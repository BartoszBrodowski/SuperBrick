"""Offline construction planning and deterministic CPU ray-cast studio previews.
Never edits the input model. Requires the installed CAD bench resources and PIL.
"""
import sys, json, math, html, os, ctypes
from pathlib import Path
import numpy as np
from PIL import Image, ImageDraw


def progress(text):
    print(json.dumps({"progress": text}), flush=True)


class Rays:
    def __init__(self, triangles, colors):
        self.tri = np.asarray(triangles, dtype=np.float64)
        self.colors = np.asarray(colors, dtype=np.float64)
        self.a = self.tri[:, 0]
        self.e1 = self.tri[:, 1] - self.a
        self.e2 = self.tri[:, 2] - self.a
        n = np.cross(self.e1, self.e2)
        self.norm = n / np.maximum(np.linalg.norm(n, axis=1)[:, None], 1e-12)
        self.native=None;self.native_scene=None
        path=os.environ.get('CAD_RAY_LIBRARY')
        if path and Path(path).is_file():
            dll=ctypes.CDLL(path);ptr=ctypes.POINTER(ctypes.c_double)
            dll.cad_rays_build.argtypes=[ptr,ctypes.c_int];dll.cad_rays_build.restype=ctypes.c_void_p
            dll.cad_rays_cast.argtypes=[ctypes.c_void_p,ptr,ptr,ctypes.c_int,ptr,ctypes.POINTER(ctypes.c_longlong)]
            dll.cad_rays_free.argtypes=[ctypes.c_void_p]
            self.tri=np.ascontiguousarray(self.tri)
            self.native_scene=dll.cad_rays_build(self.tri.ctypes.data_as(ptr),len(self.tri))
            if self.native_scene:self.native=dll;return
        centers = self.tri.mean(axis=1)
        def build(ids):
            lo = self.tri[ids].min(axis=(0, 1)); hi = self.tri[ids].max(axis=(0, 1))
            if len(ids) <= 8:
                return lo, hi, ids, None
            axis = np.ptp(centers[ids], axis=0).argmax()
            ids = ids[np.argsort(centers[ids, axis], kind="stable")]
            mid = len(ids) // 2
            return lo, hi, build(ids[:mid]), build(ids[mid:])
        self.root = build(np.arange(len(self.tri)))

    def inside(self,points,directions):
        if self.native is None or not hasattr(self.native,'cad_rays_inside'):return None
        ptr=ctypes.POINTER(ctypes.c_double);byte=ctypes.POINTER(ctypes.c_ubyte)
        self.native.cad_rays_inside.argtypes=[ctypes.c_void_p,ptr,ctypes.c_int,ptr,ctypes.c_int,byte]
        points=np.ascontiguousarray(points,dtype=np.float64);directions=np.ascontiguousarray(directions,dtype=np.float64);out=np.zeros(len(points),dtype=np.uint8)
        self.native.cad_rays_inside(self.native_scene,points.ctypes.data_as(ptr),len(points),directions.ctypes.data_as(ptr),len(directions),out.ctypes.data_as(byte))
        return out.astype(bool)

    def cast(self, origins, directions, limit=None):
        count = len(origins)
        best = np.full(count, np.inf) if limit is None else np.array(limit, dtype=float)
        hit = np.full(count, -1, dtype=int)
        if self.native is not None:
            ptr=ctypes.POINTER(ctypes.c_double)
            origins=np.ascontiguousarray(origins,dtype=np.float64);directions=np.ascontiguousarray(directions,dtype=np.float64)
            self.native.cad_rays_cast(self.native_scene,origins.ctypes.data_as(ptr),directions.ctypes.data_as(ptr),count,best.ctypes.data_as(ptr),hit.ctypes.data_as(ctypes.POINTER(ctypes.c_longlong)))
            return best,hit
        inv = np.divide(1., directions, out=np.full_like(directions, 1e30), where=np.abs(directions)>1e-14)
        def visit(node, rays):
            if not len(rays): return
            lo, hi, left, right = node
            t0 = (lo-origins[rays])*inv[rays]; t1 = (hi-origins[rays])*inv[rays]
            near = np.maximum(np.minimum(t0,t1).max(axis=1), 0.)
            far = np.minimum(np.maximum(t0,t1).min(axis=1), best[rays])
            rays = rays[far >= near]
            if not len(rays): return
            if right is not None:
                visit(left, rays); visit(right, rays); return
            # Bounded ray packets prevent large temporary ray x triangle arrays.
            for start in range(0, len(rays), 4096):
                rr = rays[start:start+4096]
                p = np.cross(directions[rr,None,:], self.e2[left][None,:,:])
                det = np.einsum('ijk,jk->ij',p,self.e1[left])
                reciprocal = np.divide(1.,det,out=np.zeros_like(det),where=np.abs(det)>1e-10)
                tv = origins[rr,None,:]-self.a[left][None,:,:]
                u = np.einsum('ijk,ijk->ij',tv,p)*reciprocal
                q = np.cross(tv,self.e1[left][None,:,:])
                v = np.einsum('ik,ijk->ij',directions[rr],q)*reciprocal
                t = np.einsum('jk,ijk->ij',self.e2[left],q)*reciprocal
                valid = (np.abs(det)>1e-10)&(u>=-1e-8)&(v>=-1e-8)&(u+v<=1+1e-8)&(t>1e-4)&(t<best[rr,None])
                t = np.where(valid,t,np.inf)
                col = t.argmin(axis=1); distance = t[np.arange(len(rr)),col]
                take = np.isfinite(distance)
                best[rr[take]]=distance[take]; hit[rr[take]]=left[col[take]]
        visit(self.root,np.arange(count))
        return best,hit

    def __del__(self):
        if getattr(self,'native',None) is not None and self.native_scene:self.native.cad_rays_free(self.native_scene)


def studio_scene(refs,lib):
    import render
    triangles=[]; colors=[];corners=[];cache={}
    for ref in refs:
        g=lib.geom(ref.name,'std')
        triangles.append(g.tris @ ref.rot.T + ref.pos)
        colors.append(render._code_rgb(lib.colors,g.tcol,ref.color))
        if ref.name not in cache:
            tri=np.asarray(g.tris,dtype=float);face=np.cross(tri[:,1]-tri[:,0],tri[:,2]-tri[:,0]);face/=np.maximum(np.linalg.norm(face,axis=1)[:,None],1e-12)
            _,index=np.unique(np.round(tri.reshape(-1,3),5),axis=0,return_inverse=True)
            adjacency=[[] for _ in range(int(index.max())+1)]
            for i,vertex in enumerate(index):adjacency[vertex].append(i//3)
            smooth=np.empty((len(tri),3,3))
            for i,vertex in enumerate(index):
                normal=face[i//3];near=face[adjacency[vertex]];d=near@normal;near=near[np.abs(d)>.8]*np.sign(d[np.abs(d)>.8])[:,None]
                n=near.sum(axis=0);smooth[i//3,i%3]=n/max(np.linalg.norm(n),1e-12)
            cache[ref.name]=smooth
        corners.append(cache[ref.name]@ref.rot.T)
    tri=np.concatenate(triangles); col=np.concatenate(colors)
    if not len(tri): raise ValueError('No renderable triangles')
    bvh=Rays(tri,col)
    bvh.corner_normals=np.concatenate(corners)
    return bvh

def surface_normals(bvh,ids,points):
    tv=points-bvh.a[ids];a=bvh.e1[ids];b=bvh.e2[ids]
    aa=np.sum(a*a,axis=1);ab=np.sum(a*b,axis=1);bb=np.sum(b*b,axis=1);ta=np.sum(tv*a,axis=1);tb=np.sum(tv*b,axis=1)
    det=aa*bb-ab*ab;u=np.divide(bb*ta-ab*tb,det,out=np.zeros_like(det),where=np.abs(det)>1e-15);v=np.divide(aa*tb-ab*ta,det,out=np.zeros_like(det),where=np.abs(det)>1e-15)
    n=bvh.corner_normals[ids];value=n[:,0]*(1-u-v)[:,None]+n[:,1]*u[:,None]+n[:,2]*v[:,None]
    return value/np.maximum(np.linalg.norm(value,axis=1)[:,None],1e-12)

def environment(d):
    # Two rectangular studio softboxes, reflected in dielectric plastic.
    den=np.maximum(-d[:,1],1e-5);x=d[:,0]/den;z=d[:,2]/den
    key=(d[:,1]<-.1)&(np.abs(x+1.0)<.65)&(np.abs(z+.7)<1.35)
    fill=(d[:,1]<-.1)&(np.abs(x-1.3)<.45)&(np.abs(z-.2)<1.5)
    return np.where(key,3.4,np.where(fill,2.1,.42))[:,None]*np.ones((1,3))

def studio(refs, lib, out, size=1024, az=-35, el=28, samples=4, scene=None):
    bvh=scene if scene is not None else studio_scene(refs,lib)
    tri=bvh.tri;col=bvh.colors
    lo=tri.min(axis=(0,1)); hi=tri.max(axis=(0,1)); center=(lo+hi)/2
    radius=max(np.linalg.norm(hi-lo)/2,20.)
    az,el=np.radians([az,el]); eye_dir=np.array([math.sin(az)*math.cos(el),-math.sin(el),-math.cos(az)*math.cos(el)])
    forward=-eye_dir; right=np.cross(forward,[0.,-1.,0.]); right/=np.linalg.norm(right); up=np.cross(right,forward)
    corners=np.array([[x,y,z] for x in [lo[0],hi[0]] for y in [lo[1],hi[1]] for z in [lo[2],hi[2]]])-center
    span=max(np.ptp(corners@right),np.ptp(corners@up))*1.32
    span=max(span,40.)
    result=np.zeros((size*size,3)); floor=hi[1]+.1
    rng=np.random.default_rng(20261111)
    for sample in range(samples):
        progress(f'Ray casting {Path(out).name}: sample {sample+1}/{samples}')
        yy,xx=np.mgrid[:size,:size]; jitter=rng.random(2) if samples>1 else [.5,.5]
        origins=center+eye_dir*radius*4+((xx.ravel()+jitter[0])/size-.5)[:,None]*span*right+(.5-(yy.ravel()+jitter[1])/size)[:,None]*span*up
        dirs=np.tile(forward,(len(origins),1)); t,ids=bvh.cast(origins,dirs)
        ft=(floor-origins[:,1])/forward[1]
        ground=(ft>0)&((ids<0)|(ft<t)); obj=(ids>=0)&~ground
        color=np.ones((len(origins),3)); points=np.zeros_like(origins); normals=np.tile([0.,-1.,0.],(len(origins),1))
        points[ground]=origins[ground]+ft[ground,None]*dirs[ground]
        points[obj]=origins[obj]+t[obj,None]*dirs[obj]
        normals[obj]=surface_normals(bvh,ids[obj],points[obj])
        back=(normals@forward)>0; normals[back]*=-1
        active=np.where(ground|obj)[0]
        visibility=np.zeros(len(active))
        for light_sample in range(4):
            light=center+radius*np.array([-2.5,-4.,-3.])+rng.uniform(-.25,.25,3)*radius
            vectors=light-points[active]; distance=np.linalg.norm(vectors,axis=1); ld=vectors/distance[:,None]
            _,shadow=bvh.cast(points[active]+normals[active]*.06,ld,distance-.1)
            visibility+=np.where(shadow<0,1.,.18)/4
        lambert=np.maximum(0.,np.sum(normals[active]*ld,axis=1))
        shade=.55+.45*lambert*visibility
        color[active]=shade[:,None]
        color[obj]*=(col[ids[obj]]/255.)**2.2
        halfvec=ld-forward;halfvec/=np.maximum(np.linalg.norm(halfvec,axis=1)[:,None],1e-9)
        ndh=np.clip(np.sum(normals[active]*halfvec,axis=1),0,1)
        ndv=np.clip(normals[active]@(-forward),.001,1);ndl=np.clip(lambert,.001,1)
        alpha2=.22**4;distribution=alpha2/(math.pi*(ndh*ndh*(alpha2-1)+1)**2)
        k=(.22+1)**2/8;geometry=(ndv/(ndv*(1-k)+k))*(ndl/(ndl*(1-k)+k))
        specular=.04*distribution*geometry/(4*ndv*ndl)*lambert*visibility*3
        color[active]+=specular[:,None]
        # Trace a rough glossy reflection; use Schlick dielectric Fresnel rather than tinting highlights.
        oi=np.where(obj)[0]
        if len(oi):
            n=normals[oi];reflection=dirs[oi]-2*np.sum(dirs[oi]*n,axis=1)[:,None]*n
            noise=rng.normal(size=(1,3))*.025;reflection+=noise;reflection/=np.linalg.norm(reflection,axis=1)[:,None]
            rt,ri=bvh.cast(points[oi]+n*.08,reflection)
            reflected=environment(reflection);rh=ri>=0
            if rh.any():reflected[rh]=(col[ri[rh]]/255.)**2.2*.7
            fresnel=.04+.96*(1-np.clip(np.sum(n*(-dirs[oi]),axis=1),0,1))**5
            color[oi]=color[oi]*(1-fresnel[:,None])+reflected*fresnel[:,None]
        # Floor shadows fade away from the model, preserving a white studio background.
        color[ground]=(.82+.18*visibility[np.isin(active,np.where(ground)[0])])[:,None]
        result+=np.maximum(color,0)
    image=result/samples
    Image.fromarray(np.uint8(np.clip(image,0,1).reshape(size,size,3)**(1/2.2)*255)).save(out)


def assembly_plan(refs, lib, warnings=None):
    import physics, snaps
    try:
        from cad_contact_policy import contact_review, blocking_relation, review_reason, refine_relation, curved_clip_matches
    except ModuleNotFoundError:
        sys.path.insert(0,str(Path(__file__).resolve().parent))
        from cad_contact_policy import contact_review, blocking_relation, review_reason, refine_relation, curved_clip_matches
    if warnings is None:warnings=[]
    bodies=[physics.Body(physics.get_shape(r.name),r.rot,r.pos) for r in refs]
    links={}; errors=[]
    order=sorted(range(len(bodies)),key=lambda i: bodies[i].flo[0])
    for at,a in enumerate(order):
        for b in order[at+1:]:
            if bodies[b].flo[0]>bodies[a].fhi[0]+1: break
            if np.any(bodies[a].flo>bodies[b].fhi+1) or np.any(bodies[b].flo>bodies[a].fhi+1): continue
            x,y=sorted((a,b));rel=physics.relation(bodies[x],bodies[y])
            rel=refine_relation(bodies[x],bodies[y],rel,physics.COLLIDE_MIN)
            if contact_review(rel,physics.COLLIDE_MIN):
                warnings.append(f'Pieces {x+1} ({refs[x].name}) and {y+1} ({refs[y].name}): '+review_reason(rel))
            if blocking_relation(rel,physics.COLLIDE_MIN):
                why=[c.reason for c in rel.conflicts]
                if rel.overlap>=physics.COLLIDE_MIN:why.insert(0,f'solid overlap {rel.overlap:.2f} LDU³ after connector keep-outs')
                errors.append(f'Pieces {x+1} ({refs[x].name}) and {y+1} ({refs[y].name}): '+ '; '.join(why))
            matches=physics.snap_links(bodies[x],bodies[y])[0]+curved_clip_matches(bodies[x],bodies[y],rel)
            if matches: links[(x,y)]=matches
    if errors: return [],errors,[]
    # A set can contain independent accessories or an unseated subassembly.
    # Plan each connected component; never invent a joining operation.
    adjacency={i:set() for i in range(len(refs))}
    for a,b in links:adjacency[a].add(b);adjacency[b].add(a)
    unseen=set(adjacency);components=[]
    while unseen:
        reached={min(unseen)};pending=list(reached)
        while pending:
            for j in adjacency[pending.pop()]:
                if j not in reached:reached.add(j);pending.append(j)
        unseen-=reached;components.append(sorted(reached))
    components.sort(key=lambda c:(-len(c),min(c)))
    if len(components)>1:
        warnings.append(f'The saved model has {len(components)} independent connection components (sizes '+', '.join(str(len(c)) for c in components)+'). These are built separately; no unconfirmed joining step is invented. Check intended joints before treating this as one connected assembly.')
        steps=[];groups=[]
        for number,component in enumerate(components,1):
            local,issues,subgroups=assembly_plan([refs[i] for i in component],lib,warnings)
            offset=len(groups)
            if issues:return [],[f'Independent assembly {number}: '+issue for issue in issues],groups+[[component[i] for i in g] for g in subgroups]
            groups.extend([[component[i] for i in g] for g in subgroups])
            for step in local:
                step=dict(step,group=step['group']+offset,component=number)
                step['visible']=[component[i] for i in step['visible']];step['added']=[component[i] for i in step['added']];steps.append(step)
        return steps,[],groups
    # Ordinary stud stacks form subbuilds; side studs and articulated interfaces are boundaries.
    parent=list(range(len(refs)))
    def root(x):
        while parent[x]!=x: x=parent[x]
        return x
    for (a,b),matches in links.items():
        # A conventional stack can itself be tilted: classify in both parts'
        # frames, not the global Y direction. Side-stud and joint interfaces
        # remain explicit boundaries.
        if all(m.type in ('stud','tube') and
               abs(float(np.dot(m.axis,refs[a].rot[:,1])))>.98 and
               abs(float(np.dot(m.axis,refs[b].rot[:,1])))>.98 for m in matches): parent[root(b)]=root(a)
    groups={}
    for i in range(len(refs)): groups.setdefault(root(i),[]).append(i)
    groups=sorted(groups.values(),key=lambda g:(-max(bodies[i].fhi[1] for i in g),min(g)))
    sweep_cache={};clearances={};partials={};blocked_by={}
    # Reverse disassembly verifies sampled straight-line insertion clearance.
    def peel(units, scope):
        remaining=set(range(len(units))); reverse=[]; blocked=[]
        owner={i:u for u,unit in enumerate(units) for i in unit}
        neighbors={u:{owner[j] for i in unit for j in adjacency[i] if j in owner and owner[j]!=u} for u,unit in enumerate(units)}
        centers={u:np.mean([refs[i].pos for i in unit],axis=0) for u,unit in enumerate(units)}
        while len(remaining)>1:
            cuts=set();seen={};low={};clock=[0]
            def visit(u,parent=None):
                seen[u]=low[u]=clock[0];clock[0]+=1;children=0
                for v in sorted(neighbors[u]&remaining):
                    if v==parent:continue
                    if v in seen:low[u]=min(low[u],seen[v]);continue
                    children+=1;visit(v,u);low[u]=min(low[u],low[v])
                    if parent is not None and low[v]>=seen[u]:cuts.add(u)
                if parent is None and children>1:cuts.add(u)
            visit(min(remaining))
            choices=[];obstructions=[]
            def priority(u):
                if not reverse:return (0,0,len(units[u]),min(bodies[i].flo[1] for i in units[u]),min(units[u]))
                previous=reverse[-1][0];distance=float(np.linalg.norm(centers[u]-centers[previous]))
                region=0 if u in neighbors[previous] else 1 if distance<=80 else 2
                return (region,round(distance,4),len(units[u]),min(bodies[i].flo[1] for i in units[u]),min(units[u]))
            for u in sorted(remaining,key=priority):
                if u in cuts:continue
                moving=set(units[u]); fixed=set().union(*(set(units[v]) for v in remaining if v!=u))
                # A reverse removal must not leave a collection of disconnected fragments.
                fixed_ids=np.array(sorted(fixed),dtype=np.int64)
                fixed_lo=np.stack([bodies[j].flo for j in fixed_ids])
                fixed_hi=np.stack([bodies[j].fhi for j in fixed_ids])
                boundary=[(a,b,m) for (a,b),mm in links.items() if (a in moving and b in fixed) or (b in moving and a in fixed) for m in mm]
                if not boundary: continue
                directions=[]
                for a,b,m in boundary:
                    # Clip openings are radial to their bar axis. Explore the
                    # actual connector frames as well as the joint axis.
                    axes=[m.axis]+[bodies[a].conns[m.ia].frame[:,k] for k in range(3)]+[bodies[b].conns[m.ib].frame[:,k] for k in range(3)]
                    for d in [sign*axis for axis in axes if axis is not None for sign in (1,-1)]:
                        if np.linalg.norm(d)<1e-8:continue
                        d=np.asarray(d)/np.linalg.norm(d)
                        if not any(np.dot(d,v)>.999 for v in directions): directions.append(np.array(d))
                for direction in directions:
                    pathkey=(tuple(sorted(moving)),tuple(direction))
                    cached=blocked_by.get(pathkey)
                    if cached is not None and cached[0] in fixed:
                        if len(obstructions)<3:obstructions.append(cached[1])
                        continue
                    allids=moving|fixed
                    distance=max(np.max(np.stack([bodies[i].fhi for i in allids]),axis=0)-np.min(np.stack([bodies[i].flo for i in allids]),axis=0))+40
                    legal=True
                    # Bounded 4-LDU sampling, not a continuous collision proof or stability test.
                    for shift in np.arange(4,distance+4,4):
                        still_touching=False
                        for i in moving:
                            moved=physics.Body(bodies[i].shape,refs[i].rot,refs[i].pos+direction*shift)
                            nearby=fixed_ids[np.all(moved.flo<=fixed_hi+1,axis=1)&np.all(fixed_lo<=moved.fhi+1,axis=1)]
                            for j in nearby:
                                key=(i,int(j),tuple(direction),float(shift))
                                if key not in sweep_cache:
                                    rel=physics.relation(moved,bodies[j])
                                    if rel and rel.overlap>=physics.COLLIDE_MIN:
                                        rel=refine_relation(moved,bodies[j],rel,physics.COLLIDE_MIN)
                                    # Intermediate withdrawal need not remain seated.
                                    obstructs=bool(rel and (rel.overlap>=physics.COLLIDE_MIN or any('not pressed home' not in c.reason for c in rel.conflicts)))
                                    why=(f'overlap {rel.overlap:.3f} LDU³' if rel.overlap>=physics.COLLIDE_MIN else '; '.join(c.reason for c in rel.conflicts)) if obstructs else ''
                                    if len(sweep_cache)>100000:sweep_cache.clear()
                                    sweep_cache[key]=(obstructs,why,bool(rel and rel.contact))
                                obstructs,why,touching=sweep_cache[key]
                                still_touching |= touching
                                if obstructs:
                                    legal=False
                                    detail=f'piece {i+1} ({refs[i].name}) against {j+1} ({refs[j].name}) at {shift:g} LDU withdrawal: {why}'
                                    blocked_by[pathkey]=(int(j),detail)
                                    if len(obstructions)<3:obstructions.append(detail)
                                    break
                            if not legal: break
                        if not legal: break
                        # Insertion starts from a nearby free staging pose, not
                        # at infinity through unrelated parts of the model.
                        # Keep sampling until the unit is detached and clear.
                        if not still_touching and shift>=8:break
                    if legal:
                        clearances[(tuple(sorted(moving)),tuple(direction))]=float(shift)
                        # Prefer easy, small removals; deterministic ties and directions.
                        choices.append((len(moving),min(bodies[i].flo[1] for i in moving),min(moving),u,direction));break
                if choices:break
            if not choices:
                details=(' Observed obstructions: '+'; '.join(obstructions)) if obstructions else ' Removing a candidate would split the remaining assembly, or no usable boundary connector was found.'
                blocked.append(f'{scope}: no checked straight insertion sequence; hinge/clip flexibility, loops or manual repositioning may be needed.'+details);break
            _,_,_,u,direction=min(choices,key=lambda c:c[:4]);remaining.remove(u);reverse.append((u,direction))
            progress(f'Planning {scope}: {len(remaining)} units remain')
        if blocked:
            partials[tuple(tuple(u) for u in units)]=(sorted(remaining),reverse.copy())
            return [],blocked
        first=next(iter(remaining)); return [(first,None)]+list(reversed(reverse)),[]
    grouporder,issues=peel(groups,'assembly') if len(groups)>1 else ([(0,None)],[])
    # Failed predefined chunks are handled by the connected partition search
    # below, rather than replaying almost the same ordering for every chunk.
    planned_groups=[];failed_cores={};solved_cores={}
    def connected(ids):
        if not ids:return False
        seen={min(ids)};pending=list(seen)
        while pending:
            for j in adjacency[pending.pop()] & ids:
                if j not in seen:seen.add(j);pending.append(j)
        return seen==ids
    def construct(group,depth=0):
        signature=tuple(sorted(group));offset=len(planned_groups)
        if signature in solved_cores:
            pages,cached_groups,gid=solved_cores[signature]
            planned_groups.extend(cached_groups)
            return [dict(s,group=s['group']+offset) for s in pages],[],gid+offset
        pages,issues,gid=construct_uncached(group,depth)
        if not issues:
            solved_cores[signature]=([dict(s,group=s['group']-offset) for s in pages],planned_groups[offset:].copy(),gid-offset)
        return pages,issues,gid
    def construct_uncached(group,depth=0):
        signature=tuple(sorted(group))
        if signature in failed_cores:return [],failed_cores[signature],None
        # Prefer completing articulated branches before adding them to the
        # parent. Cutting all hinge bearings together also handles paired axes.
        if depth<10 and len(group)>3:
            ids=set(group);joint_edges={pair for pair,ms in links.items() if any(m.type in ('hinge','ball','turntable','clip-bar','clip-edge') for m in ms)}
            unseen=ids.copy();branches=[]
            while unseen:
                found={min(unseen)};pending=list(found)
                while pending:
                    i=pending.pop()
                    for j in adjacency[i]&ids:
                        if tuple(sorted((i,j))) not in joint_edges and j not in found:found.add(j);pending.append(j)
                unseen-=found;branches.append(found)
            for a in sorted(branches,key=lambda s:(len(s),min(s))):
                b=ids-a
                if min(len(a),len(b))<2 or not connected(b):continue
                a,b=sorted(a),sorted(b);order,blocked=peel([a,b],f'completed articulated branch {len(a)} + {len(b)}')
                if blocked:continue
                checkpoint=len(planned_groups);pages=[];previous=[];ok=True
                for number,(unit,withdraw) in enumerate(order):
                    child=[a,b][unit];childpages,blocked,gid=construct(child,depth+1)
                    if blocked:ok=False;break
                    pages+=childpages
                    if number:pages.append(dict(kind='join',group=gid,visible=previous+child,added=child,direction=(-withdraw).tolist(),staging_distance=clearances.get((tuple(child),tuple(withdraw))),completed_branch=True))
                    previous+=child
                if ok:
                    planned_groups.append(group);return pages,[],len(planned_groups)
                del planned_groups[checkpoint:]
        internal,issues=peel([[i] for i in group],f'subbuild ({len(group)} pieces)') if len(group)>1 else ([(0,None)],[])
        if not issues:
            planned_groups.append(group);gid=len(planned_groups);built=[];pages=[]
            for unit,withdraw in internal:
                added=[group[unit]];built+=added
                pages.append(dict(kind='part',group=gid,visible=built.copy(),added=added,
                    direction=None if withdraw is None else (-withdraw).tolist(),
                    staging_distance=None if withdraw is None else clearances.get((tuple(added),tuple(withdraw)))))
            return pages,[],gid
        if depth>=10:return [],issues,None
        remaining,removed=partials.get(tuple((i,) for i in group),([],[]))
        if removed:
            # Keep already proven removals. Search only the trapped core rather
            # than repeatedly replanning hundreds of unobstructed outer parts.
            core=[group[u] for u in remaining]
            pages,failed,_=construct(core,depth+1)
            if failed:
                failed_cores[signature]=failed
                return [],failed,None
            planned_groups.append(group);gid=len(planned_groups);built=core.copy()
            for unit,withdraw in reversed(removed):
                added=[group[unit]];built+=added
                pages.append(dict(kind='part',group=gid,visible=built.copy(),added=added,
                    direction=(-withdraw).tolist(),staging_distance=clearances.get((tuple(added),tuple(withdraw)))))
            return pages,[],gid
        # A stud-connected shell need not be one indivisible subbuild. Search
        # connected halves and small graph neighbourhoods, then actually verify
        # the completed halves can mate before recursively constructing them.
        ids=set(group);candidates={}
        def offer(a,priority=1):
            a=set(a);b=ids-a
            if min(len(a),len(b))<2 or not connected(a) or not connected(b):return
            a,b=sorted((a,b),key=lambda s:tuple(sorted(s)))
            key=tuple(sorted(a))
            crossing=sum(1 for i in a for j in adjacency[i]&b)
            value=(priority,crossing,abs(len(a)-len(b)),sorted(a),sorted(b))
            if key not in candidates or value[:3]<candidates[key][:3]:candidates[key]=value
        # A blocked pin/curved shell may need its actual obstructing neighbour
        # built with it, rather than an arbitrary spatial cut. Grow connected
        # obstacle closures, then test the resulting subassembly's real sweep.
        paths=list(blocked_by.items())
        seeds=[p for p in paths if len(p[0][0])<=8 and set(p[0][0])<=ids]
        for (moving,direction),_ in seeds[-96:]:
            local=set(moving)
            if not local or not local<=ids:continue
            for _ in range(8):
                obstacles={v[0] for (unit,d),v in paths if set(unit)<=local and d==direction and v[0] in ids-local}
                if not obstacles:break
                for target in sorted(obstacles):
                    queue=list(sorted(local));parent_path={i:None for i in local};found=False
                    for i in queue:
                        if i==target:found=True;break
                        for j in sorted(adjacency[i]&ids):
                            if j not in parent_path:parent_path[j]=i;queue.append(j)
                    if not found:continue
                    node=target
                    while node is not None and node not in local:
                        local.add(node);node=parent_path[node]
                offer(local,0)
                if len(local)>len(ids)//2:break
        # Articulated branches need not be separable by a world-axis plane.
        # Bridge subtrees are natural candidate limbs/subassemblies.
        entered={};low={};clock=[0]
        def bridges(i,parent=None):
            entered[i]=low[i]=clock[0];clock[0]+=1;subtree={i}
            for j in sorted(adjacency[i]&ids):
                if j==parent:continue
                if j in entered:low[i]=min(low[i],entered[j]);continue
                child=bridges(j,i);subtree|=child;low[i]=min(low[i],low[j])
                if low[j]>entered[i]:offer(child)
            return subtree
        bridges(min(ids))
        axes=[np.eye(3)[k] for k in range(3)]
        for i in group:
            for axis in refs[i].rot.T:
                if not any(abs(float(axis@v))>.999 for v in axes):axes.append(axis)
            if len(axes)>=9:break
        for axis in axes:
            values={i:float(refs[i].pos@axis) for i in group};levels=sorted(set(round(v,4) for v in values.values()))
            for lo,hi in zip(levels,levels[1:]):offer({i for i,v in values.items() if v<(lo+hi)/2})
        for seed in group:
            local={seed};front={seed}
            for _ in range(8):
                front=set().union(*(adjacency[i]&ids for i in front))-local if front else set()
                local|=front;offer(local)
                if len(local)>len(group)//2:break
        for _,_,_,a,b in sorted(candidates.values())[:96]:
            order,failed=peel([a,b],f'joining {len(a)} + {len(b)} pieces')
            if failed:continue
            checkpoint=len(planned_groups);pages=[];previous=[];ok=True
            for number,(unit,withdraw) in enumerate(order):
                child=[a,b][unit];part_pages,failed,gid=construct(child,depth+1)
                if failed:ok=False;break
                pages+=part_pages
                if number:pages.append(dict(kind='join',group=gid,visible=previous+child,added=child,
                    direction=(-withdraw).tolist(),staging_distance=clearances.get((tuple(sorted(child)),tuple(withdraw)))))
                previous+=child
            if ok:
                planned_groups.append(group)
                return pages,[],len(planned_groups)
            del planned_groups[checkpoint:]
        failed_cores[signature]=issues
        return [],issues,None
    if issues or any(m.type in ('hinge','ball','turntable') for ms in links.values() for m in ms):
        pages,failed,_=construct(list(range(len(refs))))
        return pages,failed,planned_groups if not failed else groups
    steps=[]
    for sequence,(gi,direction) in enumerate(grouporder):
        group=groups[gi];pages,issues,gid=construct(group)
        if issues:return [],issues,groups
        steps+=pages
        if sequence:
            previous=[i for g,_ in grouporder[:sequence] for i in groups[g]]
            steps.append(dict(kind='join',group=gid,visible=previous+group,added=group,
                direction=(-direction).tolist(),staging_distance=clearances.get((tuple(sorted(group)),tuple(direction)))))
    return steps,[],planned_groups


def configure_render(high_quality=False):
    import render
    render._cad_high_quality=high_quality
    path=os.environ.get('CAD_RAY_LIBRARY','')
    if not Path(path).is_file():return False
    dll=ctypes.CDLL(path)
    if not hasattr(dll,'cad_rasterize'):return False
    double=ctypes.POINTER(ctypes.c_double);integer=ctypes.POINTER(ctypes.c_longlong)
    dll.cad_rasterize.argtypes=[double,ctypes.c_int,ctypes.c_int,ctypes.c_int,double,integer]
    dll.cad_rasterize.restype=None
    def rasterize(triangles,width,height):
        triangles=np.ascontiguousarray(triangles,dtype=np.float64)
        depth=np.empty(width*height,dtype=np.float64);ids=np.empty(width*height,dtype=np.int64)
        dll.cad_rasterize(triangles.ctypes.data_as(double),len(triangles),width,height,depth.ctypes.data_as(double),ids.ctypes.data_as(integer))
        pixels=np.flatnonzero(ids>=0)
        return pixels,depth[pixels],ids[pixels]
    render.rasterize=rasterize
    return True


def finish_subbuilds_before_attachment(steps,groups,refs):
    """Move dependent additions earlier only after rechecking affected paths."""
    import physics
    from cad_contact_policy import refine_relation, curved_clip_matches
    steps=[dict(s,added=list(s['added']),visible=list(s['visible'])) for s in steps];groups=[list(g) for g in groups]
    bodies=[physics.Body(physics.get_shape(p.name),p.rot,p.pos) for p in refs];neighbors={i:set() for i in range(len(refs))}
    for i,a in enumerate(bodies):
        for j in range(i):
            b=bodies[j]
            if np.any(a.flo>b.fhi+1) or np.any(b.flo>a.fhi+1):continue
            if physics.snap_links(a,b)[0] or curved_clip_matches(a,b):neighbors[i].add(j);neighbors[j].add(i)
    def clear(step):
        direction=step.get('direction')
        if direction is None:return True
        fixed=set(step['visible'])-set(step['added'])
        for distance in np.arange(4,(step.get('staging_distance') or 8)+.001,4):
            for i in step['added']:
                a=bodies[i];moved=physics.Body(a.shape,a.R,a.t-np.array(direction)*distance)
                for j in fixed:
                    b=bodies[j]
                    if np.any(moved.flo>b.fhi+1) or np.any(b.flo>moved.fhi+1):continue
                    rel=refine_relation(moved,b,physics.relation(moved,b),physics.COLLIDE_MIN)
                    if rel and (rel.overlap>=physics.COLLIDE_MIN or any('not pressed home' not in c.reason for c in rel.conflicts)):return False
        return True
    index=0
    while index<len(steps):
        join=steps[index]
        if join['kind']!='join':index+=1;continue
        child=set(join['added']);pulled=[]
        for k in range(index+1,len(steps)):
            candidate=steps[k]
            if candidate['kind']!='part' or candidate.get('component')!=join.get('component'):continue
            additions=set(candidate['added']);existing=set(candidate['visible'])-additions;anchors=set().union(*(neighbors[i]&existing for i in additions))
            if anchors and anchors<=child:
                pulled.append(k);child|=additions
        if not pulled:index+=1;continue
        checkpoint=len(groups);gid=checkpoint+1;groups.append(sorted(child));built=set(join['added']);prefix=[];valid=True
        for k in pulled:
            s=dict(steps[k]);built|=set(s['added']);s.update(group=gid,visible=sorted(built),completed_before_attachment=True)
            prefix.append(s)
            if not clear(s):valid=False;break
        newjoin=dict(join,group=gid,added=sorted(child),visible=sorted(set(join['visible'])|child),completed_before_attachment=True)
        affected=[]
        if valid and clear(newjoin):
            for k in range(index+1,pulled[-1]+1):
                if k in pulled:continue
                s=dict(steps[k]);oldchild=set(join['added'])
                if oldchild&set(s['visible']):s['visible']=sorted(set(s['visible'])|child)
                if s['kind']=='join' and oldchild<=set(s['added']):s['added']=sorted(set(s['added'])|child)
                if not clear(s):valid=False;break
                affected.append(s)
        else:valid=False
        if valid:
            steps[index:pulled[-1]+1]=prefix+[newjoin]+affected
            index+=len(prefix)+1
        else:
            del groups[checkpoint:];index+=1
    return steps,groups


def collapse_single_piece_subbuilds(steps,groups):
    steps=[dict(s) for s in steps];groups=[list(g) for g in groups];removed=set()
    for k,s in enumerate(steps):
        if s['kind']!='join' or len(s['added'])!=1:continue
        piece=s['added'][0]
        for j in range(k-1,-1,-1):
            before=steps[j]
            if before['kind']=='part' and before['added']==[piece] and before['visible']==[piece]:
                removed.add(j);groups.append(list(s['visible']));s.update(kind='part',group=len(groups),single_piece_attachment=True);break
    return [s for k,s in enumerate(steps) if k not in removed],groups


def merge_repeated_steps(steps,refs):
    """Repeat similar independent attachments in one numbered operation."""
    import physics
    result=[];bodies={}
    def body(i):
        if i not in bodies:
            p=refs[i];bodies[i]=physics.Body(physics.get_shape(p.name),p.rot,p.pos)
        return bodies[i]
    for number,original in enumerate(steps,1):
        step=dict(original);step['source_steps']=[number];previous=result[-1] if result else None
        eligible=previous is not None and step['kind']==previous['kind']=='part' and step['group']==previous['group'] and step.get('component')==previous.get('component') and len(step['added'])==1 and len(previous['added'])<8 and step.get('direction') is not None and previous.get('direction') is not None
        if eligible:
            i=step['added'][0];p=refs[i];first=refs[previous['added'][0]]
            eligible=p.name==first.name and p.color==first.color and float(np.dot(step['direction'],previous['direction']))>.9999 and abs(float(p.rot[:,1]@first.rot[:,1]))>.9999 and all(np.linalg.norm(p.pos-refs[j].pos)<=80 and not physics.snap_links(body(i),body(j))[0] for j in previous['added'])
        if eligible:
            # Preserve the proven one-at-a-time order; no simultaneous sweep is
            # claimed, and directly stacked/connected additions never merge.
            previous['added']=previous['added']+step['added'];previous['visible']=list(step['visible']);previous['source_steps']+=step['source_steps'];previous['repeat_operation']=True
            previous['staging_distance']=min(previous.get('staging_distance') or 30,step.get('staging_distance') or 30)
        else:result.append(step)
    return result


def instructions(refs,lib,out,size,plan_only=False):
    import render,ldraw,physics
    progress('Checking connectors and planning reverse assembly')
    warnings=[]
    steps,issues,groups=assembly_plan(refs,lib,warnings)
    if not issues:
        steps,groups=finish_subbuilds_before_attachment(steps,groups,refs)
        steps,groups=collapse_single_piece_subbuilds(steps,groups)
        steps=merge_repeated_steps(steps,refs)
    (out/'plan.json').write_text(json.dumps(dict(steps=steps,issues=issues,warnings=warnings,groups=groups,
        final_pose_check_passed=bool(groups),assembly_path_found=not issues,
        checks='Full final-pose relation; 4 LDU sampled straight withdrawal/insertion. No continuous swept-volume, hand access, gravity/stability or flexible hinge-path proof.'),indent=2))
    if issues:
        stage='Final-pose legality check failed. ' if not groups else 'Final-pose geometry checks passed, but the planner could not establish a buildable sequence. '
        raise ValueError(stage+'; '.join(issues[:6])+' See plan.json. Nothing has been labelled verified.')
    if plan_only:return dict(steps=len(steps),groups=len(groups),warnings=warnings,plan='plan.json')
    return render_instructions(refs,lib,out,size,steps,groups,warnings)


def perspective_points(points,center,distance):
    """Pinhole projection; reciprocal depth interpolates correctly in z-buffer."""
    p=np.asarray(points,dtype=float)-center
    depth=distance+p[...,2]
    if np.any(depth<=0):raise ValueError('Instruction geometry crosses the perspective camera')
    out=p.copy();out[...,:2]*=(distance/depth)[...,None]
    out[...,2]=distance*(1-distance/depth)
    return out

def perspective_frame(points):
    lo,hi=points.min(0),points.max(0);center=(lo+hi)/2
    distance=max(float(np.linalg.norm(hi-lo)/2),1.)/np.tan(np.radians(20))
    return center,distance

def perspective_render(refs,*,size,lib,az=-40,el=30,fit_refs=None,**kwargs):
    # Adapt geometry into the existing tested raster/edge pipeline rather than
    # resampling a finished orthographic image. All geometry, lines and arrows
    # share the same pinhole camera and reciprocal depth.
    import render,ldraw
    from types import SimpleNamespace
    r,u,c=render.camera_basis(az,el);projection=np.stack([r,-u,-c])@ldraw.LDRAW_TO_BUILDER
    fitting=fit_refs if fit_refs is not None else refs
    points=np.concatenate([(lib.geom(p.name,'low').tris.reshape(-1,3)@p.rot.T+p.pos)@projection.T for p in fitting])
    center,distance=perspective_frame(points);geometries={}
    def converted(parts,prefix):
        result=[]
        for i,p in enumerate(parts):
            g=lib.geom(p.name,'low');name=f'{prefix}{i}'
            def warp(a):
                world=a@p.rot.T+p.pos
                return perspective_points(world@projection.T,center,distance)@projection
            tri=warp(g.tris);lines=warp(g.lines);cond=warp(g.cond)
            vertices=tri.reshape(-1,3)
            geometries[name]=SimpleNamespace(tris=tri,lines=lines,cond=cond,tcol=g.tcol,lcol=g.lcol,ccol=g.ccol,
                bbox=lambda exclude_studs=False,v=vertices:(v.min(0),v.max(0)))
            result.append(ldraw.Ref(name,p.color,np.zeros(3),np.eye(3),step=p.step))
        return result
    visible=converted(refs,'visible');fit=converted(fitting,'fit')
    proxy=SimpleNamespace(colors=lib.colors,geom=lambda name,lod:geometries[name])
    image=render.render_refs(visible,size=size,lib=proxy,az=az,el=el,fit_refs=fit,**kwargs)
    image.info['cam'].update(perspective_center=center.tolist(),perspective_distance=distance,projection='perspective')
    return image

class InstructionViews:
    """Choose a stable view, changing it when the added assembly is occluded."""
    VIEWS=((-40,30),(40,30),(140,30),(-140,30),(-40,55),(40,55),(140,55),(-140,55),(-40,-30),(40,-30),(140,-30),(-140,-30))
    def __init__(self,refs,lib):
        self.refs=refs;self.lib=lib;self.world={};self.previous=(-40,30)

    def triangles(self,i):
        if i not in self.world:
            p=self.refs[i];self.world[i]=self.lib.geom(p.name,'low').tris@p.rot.T+p.pos
        return self.world[i]

    def choose(self,visible,added,fit):
        import render,ldraw
        pieces=[self.triangles(i) for i in visible]
        triangles=np.concatenate(pieces);flags=np.concatenate([np.full(len(t),i in added) for i,t in zip(visible,pieces)])
        fitpoints=np.concatenate([self.triangles(i).reshape(-1,3) for i in fit])
        def score(view):
            r,u,c=render.camera_basis(*view);projection=np.stack([r,-u,-c])@ldraw.LDRAW_TO_BUILDER
            points=fitpoints@projection.T;origin,distance=perspective_frame(points);points=perspective_points(points,origin,distance)
            lo,hi=points.min(0),points.max(0);scale=224/max(float(hi[0]-lo[0]),float(hi[1]-lo[1]),1e-6);center=(lo+hi)/2
            t=perspective_points(triangles@projection.T,origin,distance);t[:,:,:2]=(t[:,:,:2]-center[:2])*scale+128
            pixels,depth,ids=render.rasterize(t,256,256)
            z=np.full(256*256,np.inf);np.minimum.at(z,pixels,depth)
            front=depth<=z[pixels]+1e-9;winners=np.full(256*256,-1,dtype=np.int64);winners[pixels[front]]=ids[front]
            seen=int(flags[winners[winners>=0]].sum())
            alone,_,_=render.rasterize(t[flags],256,256)
            fraction=seen/max(len(np.unique(alone)),1)
            return fraction,seen
        initial=score(self.previous) if self.previous in self.VIEWS else (0,0)
        if initial[0]>=.72 and initial[1]>=12:return self.previous,False,initial[0]
        # Keep visible depth on every page: no front/side/top orthographic
        # views, even when their visibility score happens to be higher.
        candidates=([self.previous] if self.previous in self.VIEWS else [])+list(self.VIEWS)
        ranked=[(score(v),-i,v) for i,v in enumerate(dict.fromkeys(candidates))]
        best=max(ranked,key=lambda x:(round(x[0][0],2),x[0][1],x[1]));view=best[2];changed=view!=self.previous;self.previous=view
        return view,changed,best[0][0]


def render_instructions(refs,lib,out,size,steps,groups,warnings):
    """Illustrate an already checked plan; legality remains the caller's gate."""
    import render,ldraw,physics
    from cad_contact_policy import curved_clip_matches
    ssaa=2 if getattr(render,'_cad_high_quality',False) else 1
    progress('Rendering cover');perspective_render(refs,size=size,lib=lib,ssaa=ssaa,highlight=False).save(out/'cover.png')
    pages=['<section><h1>Build instructions</h1><img src="cover.png"><p>Page 0 · '+str(len(refs))+' pieces</p><p>Draft: sampled insertion checks; review physical access and stability before sharing.</p></section>']
    if warnings:pages[0]=pages[0].replace('</section>','<h2>Assembly notes — review before building</h2><ul>'+''.join('<li>'+html.escape(w)+'</li>' for w in warnings)+'</ul></section>')
    page_info=[];viewers={};display_frames={};render.HIGHLIGHT_RGB=(220,40,30)
    for index,step in enumerate(steps,1):
        progress(f'Illustrating step {index}/{len(steps)}')
        name=f'step-{index:04}.png'
        fit_ids=groups[step['group']-1] if step['kind']=='part' else list(range(len(refs)))
        key=('part',step['group']) if step['kind']=='part' else ('join',0)
        if key not in display_frames:
            # Standalone subbuilds are built flat in their own frame, not held
            # at the final articulated pose throughout their construction.
            anchor=refs[fit_ids[0]]
            frame=anchor.rot.T if step['kind']=='part' else np.eye(3)
            origin=anchor.pos if step['kind']=='part' else np.zeros(3)
            display=[ldraw.Ref(p.name,p.color,frame@(p.pos-origin),frame@p.rot) for p in refs]
            display_frames[key]=(display,frame,origin);viewers[key]=InstructionViews(display,lib)
        display,frame,origin=display_frames[key];views=viewers[key]
        selected=[ldraw.Ref(display[i].name,display[i].color,display[i].pos,display[i].rot,step=1 if i in step['added'] else 0) for i in step['visible']]
        fit=[display[i] for i in fit_ids]
        view,turned,visibility=views.choose(step['visible'],set(step['added']),fit_ids)
        img=perspective_render(selected,size=size,lib=lib,ssaa=ssaa,step=1,highlight=True,fit_refs=fit,az=view[0],el=view[1])
        from PIL import ImageFilter
        pixels=np.asarray(img);red=(pixels[:,:,0]==220)&(pixels[:,:,1]==40)&(pixels[:,:,2]==30)
        expanded=np.asarray(Image.fromarray(red.astype(np.uint8)*255).filter(ImageFilter.MaxFilter(2*max(2,round(size/400))+1)))>0
        pixels=pixels.copy();pixels[expanded]=(220,40,30);info=img.info.copy();img=Image.fromarray(pixels);img.info.update(info)
        if step['direction'] is not None:
            r,u,c=render.camera_basis(*view);projection=np.stack([r,-u,-c])@ldraw.LDRAW_TO_BUILDER
            centers=[]
            endpoints=[]
            for i in step['added']:
                contacts=[]
                a=physics.Body(physics.get_shape(refs[i].name),refs[i].rot,refs[i].pos)
                for j in step['visible']:
                    if j in step['added']:continue
                    b=physics.Body(physics.get_shape(refs[j].name),refs[j].rot,refs[j].pos)
                    contacts.extend(m.point for m in physics.snap_links(a,b)[0])
                    contacts.extend(m.point for m in curved_clip_matches(a,b))
                centers.append(np.mean(contacts,axis=0) if contacts else refs[i].pos)
                endpoints.extend(contacts)
            if step['kind']=='join':centers=[np.mean(endpoints if endpoints else centers,axis=0)]
            direction=frame@np.array(step['direction']);cam=img.info['cam']
            def pixel(p):
                p=perspective_points(np.asarray(p),np.array(cam['perspective_center']),cam['perspective_distance'])
                return (p[:2]-np.array(cam['center'])[:2])*cam['scale']+size/2
            for center in centers:
                center=frame@(center-origin)
                end=pixel(projection@center);start=pixel(projection@(center-direction*min(30,step.get('staging_distance') or 30)));start=np.clip(start,12,size-12);delta=end-start
                if np.linalg.norm(delta)>3:
                    unit=delta/np.linalg.norm(delta);side=np.array([-unit[1],unit[0]])
                    head=max(14,size*.024);half=head*.52
                    if np.linalg.norm(delta)<head*1.8:start=end-unit*head*1.8
                    base=end-unit*head
                    draw=ImageDraw.Draw(img);draw.line([tuple(start),tuple(base+unit*2)],fill=(220,40,30),width=max(4,round(size*.005)))
                    draw.polygon([tuple(end),tuple(base+side*half),tuple(base-side*half)],fill=(220,40,30))
        img.save(out/name)
        lots={}
        for i in step['added']:
            key=(refs[i].name,refs[i].color);lots[key]=lots.get(key,0)+1
        trayparts=[display[i] for i in step['added']] if step['kind']=='join' else [ldraw.Ref(part,color,np.array([120.*k,0.,0.]),np.eye(3)) for k,(part,color) in enumerate(sorted(lots))]
        tray=perspective_render(trayparts,size=220,lib=lib,highlight=False,ssaa=ssaa)
        trayname=f'parts-{index:04}.png';tray.save(out/trayname)
        labels='Completed subbuild' if step['kind']=='join' else ' · '.join(f'{qty}×' for _,qty in sorted(lots.items()))
        title='Subbuild '+str(step['group']) if len(groups[step['group']-1])>1 and not step.get('single_piece_attachment') else ''
        action='Turn the model' if turned else ''
        loose=len(refs)>1 and len(groups[step['group']-1])==1 and step['kind']=='part' and step['direction'] is None
        if loose:title='Unattached piece';action='Set aside — connection not confirmed in the saved model.'
        tilt=None
        if step['kind']=='join':
            old_ids=[i for i in step['visible'] if i not in step['added']]
            if old_ids:
                a=refs[step['added'][0]].rot[:,1];b=refs[old_ids[0]].rot[:,1]
                tilt=float(np.degrees(np.arccos(np.clip(abs(a@b)/(np.linalg.norm(a)*np.linalg.norm(b)),0,1))))
                if tilt>.5:action=(action+'. ' if action else '')+f'Subbuild tilt: {tilt:.0f}°'
        if visibility<.15:action=('Turn the model. ' if turned else '')+'Placement needs a closer view — review this step.'
        page_info.append(dict(title=title,parts=labels,action=action,view=list(view),projection='perspective',added_visibility=visibility,local_build_frame=step['kind']=='part',attachment_tilt_degrees=tilt))
        turn='<span class="turn">↻ Turn the model</span>' if turned else ''
        note=(f'<p class="angle">Subbuild tilt: {tilt:.0f}°</p>' if tilt is not None and tilt>.5 else '')+('<p>Placement needs a closer view — review this step.</p>' if visibility<.15 else '')+('<p>Set aside — connection not confirmed in the saved model.</p>' if loose else '')
        pages.append(f'<section><header><h1>{index}</h1><small>{title}</small>{turn}</header><aside><img src="{trayname}"><b>{labels}</b></aside><img class="assembly" src="{name}">{note}</section>')
    (out/'instructions.html').write_text('<!doctype html><meta charset="utf-8"><title>CAD build instructions</title><style>body{font:18px system-ui;background:#eee;margin:0}section{box-sizing:border-box;background:white;max-width:950px;min-height:100vh;margin:20px auto;padding:32px;break-after:page}header{display:flex;align-items:center;gap:24px}h1{font-size:58px;margin:0}small{color:#555}.turn{margin-left:auto}h2{font-size:23px}section>img{width:100%;max-height:72vh;object-fit:contain}aside{display:inline-flex;align-items:center;gap:24px;padding:8px 24px 8px 8px;background:#e3f0fa;border:1px solid #a5bdd0;border-radius:12px;margin-top:16px}aside img{width:140px}p{line-height:1.5}@media print{body{background:white}section{margin:0;min-height:0;height:98vh;padding:15mm}}</style>'+''.join(pages))
    return dict(images=['cover.png'],document='instructions.html',pages=page_info,steps=len(steps),groups=len(groups),warnings=warnings,status='Draft generated after final-pose and sampled insertion checks. Review access/stability; sampled checks are not a continuous physical proof.')


def main():
    job=json.load(sys.stdin);out=Path(job['out'])
    sys.path.insert(0,job['bench'])
    sys.path.insert(0,str(Path(job['bench']).parent/'cad/app/src'))
    import os
    os.environ.setdefault('BENCH_SHADOW',job['shadow'])
    os.environ.setdefault('CAD_RAY_LIBRARY',str(Path(job['bench']).parent/'studio_rays.dylib'))
    import ldraw,snaps,physics
    # Same five-ray, same-tolerance triangle parity test as the oracle, using
    # cached native BVHs instead of allocating point-by-triangle tensors.
    reference_inside=physics.narrow_inside;inside_scenes={}
    def accelerated_inside(body,points):
        name=body.shape.name
        if name not in inside_scenes:
            triangles=physics.mesh_tris(name)
            inside_scenes[name]=Rays(triangles,np.zeros((len(triangles),3))) if len(triangles) else None
        scene=inside_scenes[name]
        result=scene.inside((points-body.t)@body.R,physics._RAYS) if scene is not None else None
        return reference_inside(body,points) if result is None else result
    physics.narrow_inside=accelerated_inside
    # Match CAD's corrected LDCad default: finger connectors are centered.
    original=snaps.make_conns
    def centered(kind,p,src,rule='ldcad'):
        if kind=='SNAP_FGR' and rule=='ldcad' and 'center' not in p:p=dict(p,center='true')
        return original(kind,p,src,rule)
    snaps.make_conns=centered
    lib=ldraw.get_library()
    refs=[ldraw.Ref(p['name'],p['color'],np.array(p['pos']),np.array(p['rot']).reshape(3,3).T,step=1) for p in job['parts']]
    if not refs:raise ValueError('The build has no pieces')
    for r in refs:lib.geom(r.name,'low')  # Refuse partial output with missing geometry.
    high_quality=job.get('high_quality',False)
    native=configure_render(high_quality)
    progress(f'Rendering mode: {"high quality" if high_quality else "fast CAD-style"}; native rasterizer: {native}')
    if job['kind']=='instructions':result=instructions(refs,lib,out,job['size'],job.get('plan_only',False))
    else:
        images=[]
        if high_quality:progress('Preparing shared smooth studio geometry and ray accelerator')
        scene=studio_scene(refs,lib) if high_quality else None
        for i,az in enumerate([-35,55,145,235]):
            name=f'studio-{i+1}.png'
            if high_quality:studio(refs,lib,out/name,job['size'],az,samples=job.get('samples',4),scene=scene)
            else:
                import render
                render.render_refs(refs,size=job['size'],az=az,el=28,lib=lib,ssaa=1,highlight=False).save(out/name)
            images.append(name)
            print(json.dumps(dict(image=name)),flush=True)
        result=dict(images=images,status='Glossy ray-cast studio views with sampled soft shadows.' if high_quality else 'Fast CAD-style views: flat shading and edges, no ray tracing or shadows.')
    print(json.dumps({'result':result}),flush=True)


if __name__=='__main__':
    try:main()
    except Exception as error:
        if not isinstance(error,ValueError):
            import traceback
            traceback.print_exc(file=sys.stderr)
        print(json.dumps({'error':f'{type(error).__name__}: {error}'}),flush=True);sys.exit(1)
