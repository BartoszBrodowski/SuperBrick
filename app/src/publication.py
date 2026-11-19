"""Offline construction planning and deterministic CPU ray-cast studio previews.
Never edits the input model. Requires the installed CAD bench resources and PIL.
"""
import sys, json, math, html, os, ctypes
from pathlib import Path
import numpy as np
from PIL import Image, ImageDraw


def native_library(path):
    """Load the native ray library; on Windows make its folder searchable for dependent DLLs."""
    if sys.platform=='win32':
        folder=str(Path(path).resolve().parent)
        if hasattr(os,'add_dll_directory'):
            try:os.add_dll_directory(folder)
            except OSError:pass
        return ctypes.CDLL(str(path),winmode=0)
    return ctypes.CDLL(path)


def library_name():
    return 'studio_rays.dll' if sys.platform=='win32' else 'studio_rays.dylib' if sys.platform=='darwin' else 'studio_rays.so'


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
            dll=native_library(path);ptr=ctypes.POINTER(ctypes.c_double)
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


def cull_stud_tests(physics):
    """Exact stud-cylinder occupancy, testing only studs near the queried points.

    The bench loops over every stud of a part for each query; a 32x32
    baseplate has 1024, which dominated planning time."""
    Shape=physics.Shape
    if getattr(Shape.inside,'_culled',False):return
    original=Shape.inside
    def inside(self,P,studs=True,shrink=0.0):
        if not studs or len(self.studs)<=16:return original(self,P,studs,shrink)
        out=original(self,P,False,shrink);P=physics._f(P)
        if len(P)==0:return out
        S=np.asarray(self.studs,dtype=float);D=np.asarray(self.sdir,dtype=float)
        reach=physics.STUD_H+physics.STUD_R+1
        near=np.all((S>=P.min(0)-reach)&(S<=P.max(0)+reach),axis=1)
        for s,d in zip(S[near],D[near]):
            q=P-s;t=q@d;r2=(q*q).sum(1)-t*t
            out|=(t>physics.EPS)&(t<physics.STUD_H-physics.EPS)&(r2<(physics.STUD_R-physics.EPS)**2)
        return out
    inside._culled=True;Shape.inside=inside


REPEAT_LIMIT=240


class Congruence:
    """Proper rigid matches between sets of placed parts.

    Two sets are the same build when one rotation and translation carries
    every part onto a part of the same name and colour with identical
    geometry. Only proper rotations are tried, so a mirror-image copy matches
    only when the parts' own symmetry makes it the same build."""
    def __init__(self,refs,lib,tolerance=.35):
        import itertools
        self.refs=refs;self.lib=lib;self.tolerance=tolerance;self.shapes={};self.cache={}
        rotations=[]
        for perm in itertools.permutations(range(3)):
            for signs in itertools.product((1.,-1.),repeat=3):
                m=np.zeros((3,3))
                for row,(col,sign) in enumerate(zip(perm,signs)):m[row,col]=sign
                if np.linalg.det(m)>0:rotations.append(m)
        self.CUBE=rotations
    def kind(self,ids):
        return tuple(sorted((self.refs[i].name,int(self.refs[i].color)) for i in ids))
    def shape(self,name):
        if name not in self.shapes:
            try:g=self.lib.geom(name,'low');v=g.tris.reshape(-1,3)
            except KeyError:v=np.zeros((0,3))
            v=np.unique(np.round(v,3),axis=0) if len(v) else np.zeros((1,3))
            try:
                from scipy.spatial import cKDTree
                tree=cKDTree(v)
            except ImportError:
                class tree:
                    def query(points,v=v):
                        return np.array([np.min(np.linalg.norm(v-q,axis=1)) for q in points]),None
            self.shapes[name]=(v,v.mean(0),tree)
        return self.shapes[name]
    def center(self,i):
        p=self.refs[i];return p.rot@self.shape(p.name)[1]+p.pos
    def same_shape(self,i,j,R,t):
        """Part i carried by (R,t) occupies exactly part j's geometry."""
        a,b=self.refs[i],self.refs[j];v,_,_=self.shape(a.name);_,_,tree=self.shape(b.name)
        local=((v@a.rot.T+a.pos)@R.T+t-b.pos)@b.rot
        return bool(np.max(tree.query(local)[0])<=self.tolerance)
    def match(self,A,B):
        """Return ({a: b}, R, t) carrying the parts A onto B, or None."""
        A,B=list(A),list(B)
        if len(A)!=len(B) or self.kind(A)!=self.kind(B):return None
        key=(tuple(sorted(A)),tuple(sorted(B)))
        if key in self.cache:return self.cache[key]
        from collections import Counter
        name=lambda i:(self.refs[i].name,int(self.refs[i].color))
        counts=Counter(name(i) for i in A)
        a=min(A,key=lambda i:(counts[name(i)],-len(self.shape(self.refs[i].name)[0]),i))
        centers={j:self.center(j) for j in B};result=None
        for b in [j for j in B if name(j)==name(a)]:
            tried=[]
            for G in self.CUBE:
                R=self.refs[b].rot@G@self.refs[a].rot.T
                if any(np.allclose(R,x,atol=1e-4) for x in tried):continue
                tried.append(R);t=centers[b]-R@self.center(a)
                if not self.same_shape(a,b,R,t):continue
                mapping={};used=set();ok=True
                for i in sorted(A,key=lambda i:i!=a):
                    target=R@self.center(i)+t
                    found=[j for j in B if j not in used and name(j)==name(i) and np.linalg.norm(centers[j]-target)<=self.tolerance*2]
                    found=[j for j in found if self.same_shape(i,j,R,t)]
                    if not found:ok=False;break
                    mapping[i]=found[0];used.add(found[0])
                if ok:result=(mapping,R,t);break
            if result:break
        self.cache[key]=result
        return result


class Contact:
    """A placement against touching parts where no connector was recognised."""
    type='contact';axis=None;male=None;ia=None;ib=None


def assembly_plan(refs, lib, warnings=None):
    import physics, snaps
    cull_stud_tests(physics)
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
    def connection_components():
        adjacency={i:set() for i in range(len(refs))}
        for a,b in links:adjacency[a].add(b);adjacency[b].add(a)
        unseen=set(adjacency);found=[]
        while unseen:
            reached={min(unseen)};pending=list(reached)
            while pending:
                for j in adjacency[pending.pop()]:
                    if j not in reached:reached.add(j);pending.append(j)
            unseen-=reached;found.append(sorted(reached))
        return sorted(found,key=lambda c:(-len(c),min(c)))
    components=connection_components()
    if len(components)>1:
        # A small piece with no recognised connector that is wedged against
        # the model (a tile on offset SNOT edges, an arch resting on bricks)
        # is placed by contact; it is not a separate accessory.
        main=set(components[0]);placed=[]
        for component in components[1:]:
            if len(component)>4:continue
            touching=[(i,j) for i in component for j in main if not (np.any(bodies[i].flo>bodies[j].fhi+.5) or np.any(bodies[j].flo>bodies[i].fhi+.5))]
            touching=[(i,j) for i,j in touching if (lambda rel:rel is not None and rel.contact)(physics.relation(bodies[i],bodies[j]))]
            for i,j in touching:links[tuple(sorted((i,j)))]=[Contact()]
            if touching:placed+=component
        if placed:
            warnings.append('No connector was recognised for '+', '.join(f'piece {i+1} ({refs[i].name})' for i in sorted(placed))+('; it is placed against the parts it touches. Check that it is held' if len(placed)==1 else '; they are placed against the parts they touch. Check that they are held')+' in the finished model.')
            components=connection_components()
    adjacency={i:set() for i in range(len(refs))}
    for a,b in links:adjacency[a].add(b);adjacency[b].add(a)
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
    LO=np.stack([b.flo for b in bodies]);HI=np.stack([b.fhi for b in bodies])
    partners={i:[] for i in range(len(refs))}
    for (a,b),ms in links.items():
        for m in ms:partners[a].append((b,a,b,m));partners[b].append((a,a,b,m))
    joints=any(m.type in ('hinge','ball','turntable') for ms in links.values() for m in ms)
    def unit_vector(v):
        v=np.asarray(v,dtype=float);return v/np.linalg.norm(v)
    def dkey(direction):return tuple(round(float(x),6)+0. for x in direction)
    def staging(parts,withdraw):return clearances.get((tuple(sorted(parts)),dkey(withdraw)))
    def up_of(ids):
        # A subbuild is laid flat on its own stud axis: the direction its
        # internal studs point, not necessarily world up.
        ids=set(ids);votes=np.zeros(3)
        for i in ids:
            for j,a,b,m in partners[i]:
                if j not in ids or i!=a or m.type not in ('stud','tube') or m.axis is None:continue
                male,female=(a,b) if m.male=='a' else (b,a)
                axis=unit_vector(m.axis);sign=np.sign(float(axis@(refs[female].pos-refs[male].pos))) or 1.
                votes+=sign*axis
        return unit_vector(votes) if np.linalg.norm(votes)>1e-6 else np.array([0.,-1.,0.])
    def base_of(ids):
        """The piece laid down first: the largest piece at the bottom."""
        ids=list(ids);up=up_of(ids)
        corners=lambda i:np.array([[x,y,z] for x in (LO[i,0],HI[i,0]) for y in (LO[i,1],HI[i,1]) for z in (LO[i,2],HI[i,2])])
        height={i:float(np.min(corners(i)@up)) for i in ids};bottom=min(height.values())
        def footprint(i):
            size=HI[i]-LO[i];return float(np.prod(size)/max(float(abs(size@up)),1.))
        return max((i for i in ids if height[i]<=bottom+8),key=lambda i:(round(footprint(i),3),-i))
    # A press-fit cylinder (stud, pin, axle, bar) only lets go along its axis.
    AXIAL=('stud','side-stud','tube','pin-hole','axle-hole','axle-axlehole','bar')
    def withdrawal_directions(moving,fixed):
        boundary=[(a,b,m) for i in moving for j,a,b,m in partners[i] if fixed[j]]
        if not boundary:return []
        axial=[unit_vector(m.axis) for a,b,m in boundary if m.type in AXIAL and m.axis is not None and np.linalg.norm(m.axis)>1e-8]
        if axial:
            if any(abs(float(axial[0]@v))<.999 for v in axial[1:]):return []
            candidates=[axial[0],-axial[0]]
        else:
            candidates=[]
            for a,b,m in boundary:
                if m.type=='contact':
                    candidates+=[np.array(v,dtype=float) for v in ((0,-1,0),(0,1,0),(1,0,0),(-1,0,0),(0,0,1),(0,0,-1))];continue
                # Clip openings are radial to their bar axis. Explore the
                # actual connector frames as well as the joint axis.
                axes=([m.axis] if m.axis is not None else [])+[bodies[a].conns[m.ia].frame[:,k] for k in range(3)]+[bodies[b].conns[m.ib].frame[:,k] for k in range(3)]
                candidates+=[s*np.asarray(x,dtype=float) for x in axes for s in (1,-1)]
        directions=[]
        for d in candidates:
            if np.linalg.norm(d)<1e-8:continue
            d=unit_vector(d)
            if not any(float(d@v)>.999 for v in directions):directions.append(d)
        directions.sort(key=lambda d:round(float(d[1]),6))  # prefer lifting off upward
        return directions
    def sweep(moving,direction,fixed,distance):
        """Bounded 4-LDU sampled withdrawal; not a continuous collision proof."""
        key_d=dkey(direction);shift=4.
        for shift in np.arange(4,distance+4,4):
            touching=False
            for i in moving:
                lo=LO[i]+direction*shift;hi=HI[i]+direction*shift
                nearby=np.flatnonzero(fixed&np.all(lo<=HI+1,axis=1)&np.all(LO<=hi+1,axis=1));moved=None
                for j in nearby:
                    key=(i,int(j),key_d,float(shift))
                    if key not in sweep_cache:
                        if moved is None:moved=physics.Body(bodies[i].shape,refs[i].rot,refs[i].pos+direction*shift)
                        rel=physics.relation(moved,bodies[j])
                        if rel and rel.overlap>=physics.COLLIDE_MIN:
                            rel=refine_relation(moved,bodies[j],rel,physics.COLLIDE_MIN)
                        # Intermediate withdrawal need not remain seated.
                        obstructs=bool(rel and (rel.overlap>=physics.COLLIDE_MIN or any('not pressed home' not in c.reason for c in rel.conflicts)))
                        why=(f'overlap {rel.overlap:.3f} LDU³' if rel.overlap>=physics.COLLIDE_MIN else '; '.join(c.reason for c in rel.conflicts)) if obstructs else ''
                        if len(sweep_cache)>400000:sweep_cache.clear()
                        sweep_cache[key]=(obstructs,why,bool(rel and rel.contact))
                    obstructs,why,touch=sweep_cache[key];touching|=touch
                    if obstructs:return False,float(shift),int(j),f'piece {i+1} ({refs[i].name}) against {j+1} ({refs[j].name}) at {shift:g} LDU withdrawal: {why}'
            # Insertion starts from a nearby free staging pose, not at
            # infinity through unrelated parts of the model.
            if not touching and shift>=8:break
        return True,float(shift),None,''
    def articulation(nodes,neighbors):
        cuts=set();entered={};low={};clock=0
        for start in sorted(nodes):
            if start in entered:continue
            entered[start]=low[start]=clock;clock+=1;children=0
            stack=[(start,None,iter(sorted(neighbors[start]&nodes)))]
            while stack:
                u,parent,pending=stack[-1];advanced=False
                for v in pending:
                    if v==parent:continue
                    if v in entered:low[u]=min(low[u],entered[v]);continue
                    entered[v]=low[v]=clock;clock+=1
                    if u==start:children+=1
                    stack.append((v,u,iter(sorted(neighbors[v]&nodes))));advanced=True;break
                if advanced:continue
                stack.pop()
                if parent is not None:
                    low[parent]=min(low[parent],low[u])
                    if parent!=start and low[u]>=entered[parent]:cuts.add(parent)
            if children>1:cuts.add(start)
        return cuts
    # Reverse disassembly verifies sampled straight-line insertion clearance.
    # When single pieces lock each other, a connected cluster is lifted out
    # together and becomes its own checked subbuild.
    def peel(units,scope,root=None,build=None):
        units=[tuple(sorted(u)) for u in units]
        owner={i:u for u,unit in enumerate(units) for i in unit}
        neighbors={u:{owner[j] for i in unit for j in adjacency[i] if j in owner and owner[j]!=u} for u,unit in enumerate(units)}
        centers={u:np.mean([refs[i].pos for i in unit],axis=0) for u,unit in enumerate(units)}
        tops={u:min(float(LO[i,1]) for i in unit) for u,unit in enumerate(units)}
        present=np.zeros(len(refs),dtype=bool);present[list(owner)]=True
        everything=list(owner);distance=float(np.max(HI[everything].max(0)-LO[everything].min(0)))+40
        remaining=set(range(len(units)));reverse=[];blocked=[];legal={};last=None
        # Which units sit on the studs of which: courses are finished before
        # the next one starts, so in reverse the uncovered units go first.
        up=up_of(everything);covers={u:set() for u in range(len(units))}
        for u,unit in enumerate(units):
            for i in unit:
                for j,a,b,m in partners[i]:
                    if j not in owner or owner[j]==u or m.type not in ('stud','tube') or m.axis is None:continue
                    if (a if m.male=='a' else b)==i and abs(float(unit_vector(m.axis)@up))>.9:covers[u].add(owner[j])
        # Units in one course: same top height and touching side by side.
        ulo=np.array([LO[list(unit)].min(0) for unit in units]);uhi=np.array([HI[list(unit)].max(0) for unit in units])
        top=np.maximum(ulo*up,uhi*up).sum(1)
        touching=np.all((ulo[:,None]<=uhi[None]+1.)&(ulo[None]<=uhi[:,None]+1.),axis=2)&(np.abs(top[:,None]-top[None])<=1.)
        np.fill_diagonal(touching,False);course={u:set(np.flatnonzero(touching[u]).tolist()) for u in range(len(units))}
        def clear(moving,direction,fixed,obstructions):
            path=(moving,dkey(direction))
            if path in legal:return True,None
            cached=blocked_by.get(path)
            if cached is not None and fixed[cached[0]]:
                if len(obstructions)<3:obstructions.append(cached[1])
                return False,cached[0]
            ok,shift,j,detail=sweep(moving,direction,fixed,distance)
            if ok:legal[path]=shift;clearances[path]=shift;return True,None
            blocked_by[path]=(j,detail)
            if len(obstructions)<3:obstructions.append(detail)
            return False,j
        def attempt(moving,obstructions):
            fixed=present.copy();fixed[list(moving)]=False
            for direction in withdrawal_directions(moving,fixed):
                if clear(moving,direction,fixed,obstructions)[0]:return direction
            return None
        def priority(u):
            if last is None:return (False,bool(covers[u]&remaining),False,0,0.,len(units[u]),tops[u],units[u][0])
            members,center=last;distance=float(np.linalg.norm(centers[u]-center))
            region=0 if neighbors[u]&members else 1 if distance<=80 else 2
            along=not any(u in course[m] for m in members)
            return (region>1 and along,bool(covers[u]&remaining),along,region,round(distance,4),len(units[u]),tops[u],units[u][0])
        def pieces(nodes):
            found=[];todo=set(nodes)
            while todo:
                seed=min(todo);reached={seed};stack=[seed]
                while stack:
                    for v in neighbors[stack.pop()]&todo:
                        if v not in reached:reached.add(v);stack.append(v)
                todo-=reached;found.append(reached)
            return found
        def route(cluster,target):
            previous={u:None for u in cluster};queue=sorted(cluster)
            for u in queue:
                if u==target:break
                for v in sorted(neighbors[u]&remaining):
                    if v not in previous:previous[v]=u;queue.append(v)
            path=set();node=target if target in previous else None
            while node is not None and node not in cluster:path.add(node);node=previous[node]
            return path
        def extract(order,obstructions):
            cap=min(len(remaining)-1,64);tried=set();scratch=[]
            for u in order[:40]:
                if u==root:continue
                fixed=present.copy();fixed[list(units[u])]=False
                axes=[unit_vector(m.axis) for i in units[u] for j,a,b,m in partners[i] if fixed[j] and m.axis is not None and np.linalg.norm(m.axis)>1e-8]
                directions=[]
                for d in withdrawal_directions(units[u],fixed)+[s*x for x in axes for s in (1,-1)]+[np.array([0.,-1.,0.])]:
                    if not any(float(d@v)>.999 for v in directions):directions.append(d)
                for direction in directions[:4]:
                    cluster={u}
                    for _ in range(32):
                        rest=remaining-cluster
                        if not rest:break
                        parts=pieces(rest)
                        if len(parts)>1:
                            # Fragments hanging only from the cluster travel with it.
                            keep=next((p for p in parts if root in p),None) if root is not None else None
                            keep=keep or max(parts,key=lambda p:(sum(len(units[v]) for v in p),-min(p)))
                            cluster=remaining-keep
                        if (root is not None and root in cluster) or len(cluster)>cap:break
                        key=(frozenset(cluster),dkey(direction))
                        if key in tried:break
                        tried.add(key)
                        moving=tuple(sorted(i for v in cluster for i in units[v]))
                        fixed=present.copy();fixed[list(moving)]=False
                        ok,j=clear(moving,direction,fixed,scratch)
                        if ok:
                            if build(moving):return tuple(sorted(cluster)),direction
                            break
                        if j is None or owner[j]==root:break
                        grown=route(cluster,owner[j])
                        if not grown:break
                        cluster|=grown
            return None
        while len(remaining)>1:
            cuts=articulation(remaining,neighbors)
            order=sorted(remaining,key=priority);obstructions=[];chosen=None
            for u in order:
                if u in cuts or u==root:continue
                direction=attempt(units[u],obstructions)
                if direction is not None:chosen=((u,),direction);break
            if chosen is None and root in remaining and root not in cuts:
                direction=attempt(units[root],obstructions)
                if direction is not None:chosen=((root,),direction)
            if chosen is None and build is not None:chosen=extract(order,obstructions)
            if chosen is None:
                details=(' Observed obstructions: '+'; '.join(obstructions)) if obstructions else ' Removing a candidate would split the remaining assembly, or no usable boundary connector was found.'
                blocked.append(f'{scope}: no checked straight insertion sequence; hinge/clip flexibility, loops or manual repositioning may be needed.'+details);break
            members,direction=chosen;remaining-=set(members)
            for u in members:present[list(units[u])]=False
            reverse.append((members[0] if len(members)==1 else members,direction))
            last=(set(members),np.mean([centers[u] for u in members],axis=0))
            progress(f'Planning {scope}: {len(remaining)} units remain')
        if blocked:
            partials[tuple(units)]=(sorted(remaining),reverse.copy())
            return [],blocked
        first=next(iter(remaining)); return [(first,None)]+list(reversed(reverse)),[]
    base=base_of(range(len(refs)))
    if not joints and len(groups)>1:
        grouporder,issues=peel(groups,'assembly',root=next(k for k,g in enumerate(groups) if base in g))
    else:grouporder,issues=[(0,None)],[]
    # Failed predefined chunks are handled by the connected partition search
    # below, rather than replaying almost the same ordering for every chunk.
    planned_groups=[];failed_cores={};solved_cores={};solved_shapes={};congruence=Congruence(refs,lib)
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
        kind=congruence.kind(group) if 1<len(group)<=REPEAT_LIMIT else None
        for other in solved_shapes.get(kind,[]):
            # A rigid copy of an already planned subbuild reuses its plan:
            # its internal sweeps are the same motions carried by (R,t).
            found=congruence.match(other,group)
            if not found:continue
            mapping,R,_=found;pages,cached_groups,gid=solved_cores[other]
            carry=lambda ids:[mapping[i] for i in ids]
            planned_groups.extend([carry(g) for g in cached_groups])
            copied=[]
            for page in pages:
                page=dict(page,group=page['group']+offset,visible=carry(page['visible']),added=carry(page['added']))
                if page.get('direction') is not None:page['direction']=(R@np.asarray(page['direction'],dtype=float)).tolist()
                copied.append(page)
            solved_cores[signature]=([dict(s,group=s['group']-offset) for s in copied],planned_groups[offset:].copy(),gid)
            return copied,[],gid+offset
        pages,issues,gid=construct_uncached(group,depth)
        if not issues:
            solved_cores[signature]=([dict(s,group=s['group']-offset) for s in pages],planned_groups[offset:].copy(),gid-offset)
            if kind is not None:solved_shapes.setdefault(kind,[]).append(signature)
        return pages,issues,gid
    def buildable(parts,depth):
        if depth>=9:return False
        checkpoint=len(planned_groups)
        _,failed,_=construct(sorted(parts),depth+1)
        del planned_groups[checkpoint:]
        return not failed
    def assemble(group,order,depth,core=None):
        """Pages for a checked order; clusters become nested subbuilds."""
        checkpoint=len(planned_groups);children={}
        for item,withdraw in order:
            if isinstance(item,tuple):
                parts=sorted(group[u] for u in item)
                childpages,failed,cgid=construct(parts,depth+1)
                if failed:
                    del planned_groups[checkpoint:];return None,failed
                children[item]=(childpages,cgid,parts)
        planned_groups.append(group);gid=len(planned_groups)
        pages=list(core[0]) if core else [];built=list(core[1]) if core else []
        for item,withdraw in order:
            if isinstance(item,tuple):
                childpages,cgid,parts=children[item];pages+=childpages;built+=parts
                pages.append(dict(kind='join',group=cgid,visible=built.copy(),added=parts,
                    direction=(-withdraw).tolist(),staging_distance=staging(parts,withdraw)))
            else:
                added=[group[item]];built+=added
                pages.append(dict(kind='part',group=gid,visible=built.copy(),added=added,
                    direction=None if withdraw is None else (-withdraw).tolist(),
                    staging_distance=None if withdraw is None else staging(added,withdraw)))
        return pages,[]
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
                a,b=sorted(a),sorted(b);order,blocked=peel([a,b],f'completed articulated branch {len(a)} + {len(b)}',root=1)
                if blocked:continue
                checkpoint=len(planned_groups);pages=[];previous=[];ok=True
                for number,(unit,withdraw) in enumerate(order):
                    child=[a,b][unit];childpages,blocked,gid=construct(child,depth+1)
                    if blocked:ok=False;break
                    pages+=childpages
                    if number:pages.append(dict(kind='join',group=gid,visible=previous+child,added=child,direction=(-withdraw).tolist(),staging_distance=staging(child,withdraw),completed_branch=True))
                    previous+=child
                if ok:
                    planned_groups.append(group);return pages,[],len(planned_groups)
                del planned_groups[checkpoint:]
        root=group.index(base_of(group)) if len(group)>1 else None
        internal,issues=peel([[i] for i in group],f'subbuild ({len(group)} pieces)',root=root,build=lambda parts:buildable(parts,depth)) if len(group)>1 else ([(0,None)],[])
        if not issues:
            pages,issues=assemble(group,internal,depth)
            if not issues:return pages,[],len(planned_groups)
        if depth>=10:return [],issues,None
        remaining,removed=partials.get(tuple((i,) for i in group),([],[]))
        if removed:
            # Keep already proven removals. Search only the trapped core rather
            # than repeatedly replanning hundreds of unobstructed outer parts.
            core=[group[u] for u in remaining]
            pages,failed,_=construct(core,depth+1)
            if not failed:
                pages,failed=assemble(group,list(reversed(removed)),depth,core=(pages,core))
            if failed:
                failed_cores[signature]=failed
                return [],failed,None
            return pages,[],len(planned_groups)
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
                    direction=(-withdraw).tolist(),staging_distance=staging(child,withdraw)))
                previous+=child
            if ok:
                planned_groups.append(group)
                return pages,[],len(planned_groups)
            del planned_groups[checkpoint:]
        failed_cores[signature]=issues
        return [],issues,None
    if issues or joints:
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
                direction=(-direction).tolist(),staging_distance=staging(group,direction)))
    return steps,[],planned_groups


def configure_render(high_quality=False):
    import render
    render._cad_high_quality=high_quality
    path=os.environ.get('CAD_RAY_LIBRARY','')
    if not Path(path).is_file():return False
    dll=native_library(path)
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


def share_repeated_subbuilds(steps,refs,lib):
    """Show a subbuild that is built two or more times once, with ×N.

    A block is the run of steps that builds one subbuild before its join (or
    one independent component). Rigidly congruent blocks become one block
    with copies=N; their joins stay in order and merge into one operation
    when the copies attach one after another at matching or mirrored places."""
    steps=[dict(s) for s in steps];congruence=Congruence(refs,lib);blocks=[]
    for k,s in enumerate(steps):
        if s['kind']!='join':continue
        child=set(s['added']);start=k;made=set()
        while start>0 and made!=child and set(steps[start-1]['added'])<=child and set(steps[start-1]['visible'])<=child:
            start-=1;made|=set(steps[start]['added'])
        if made==child and 1<len(child)<=REPEAT_LIMIT:blocks.append((start,k,sorted(child),k))
    components={}
    for k,s in enumerate(steps):
        if s.get('component') is not None:components.setdefault(s['component'],[]).append(k)
    if len(components)>1:
        for ks in components.values():
            parts=sorted({i for k in ks for i in steps[k]['added']})
            if ks==list(range(ks[0],ks[-1]+1)) and 1<len(parts)<=REPEAT_LIMIT:blocks.append((ks[0],ks[-1]+1,parts,None))
    removed=set();classes=[]
    for start,end,parts,join in sorted(blocks,key=lambda b:(-(b[1]-b[0]),b[0])):
        if any(k in removed for k in range(start,end)):continue
        for cls in classes:
            if not any(k in range(cls['start'],cls['end']) for k in range(start,end)) and congruence.match(cls['parts'],parts):
                removed.update(range(start,end));cls['copies'].append((parts,join));break
        else:classes.append(dict(start=start,end=end,parts=parts,copies=[(parts,join)]))
    copies=[1]*len(steps);joins={}
    for number,cls in enumerate(classes):
        n=len(cls['copies'])
        if n<2:continue
        for k in range(cls['start'],cls['end']):copies[k]*=n
        for copy,(parts,join) in enumerate(cls['copies'],1):
            if join is not None:joins[join]=(number,copy,n)
    result=[]
    for k,s in enumerate(steps):
        if k in removed:continue
        if copies[k]>1:s['copies']=copies[k]
        if k in joins:
            number,copy,n=joins[k];s.update(repeat_class=number,copy=copy,of=n)
            s['attachments']=[dict(added=list(s['added']),direction=s.get('direction'),staging_distance=s.get('staging_distance'))]
            previous=result[-1] if result else None
            if previous is not None and previous.get('repeat_class')==number and len(previous['attachments'])<8 and similar_places(previous,s,refs):
                previous['attachments']+=s['attachments'];previous['added']=previous['added']+s['added']
                previous['visible']=list(s['visible']);previous['copy_range']=[previous.get('copy',1),copy]
                continue
        result.append(s)
    return result


def similar_places(first,second,refs):
    """Two attachments are alike when their insertion directions agree
    within about 30° (a fan of fingers), or are mirror images across the
    plane between the two copies (left and right)."""
    a,b=first['attachments'][0]['direction'],second['attachments'][0]['direction']
    if a is None or b is None:return a is None and b is None
    a,b=np.asarray(a,dtype=float),np.asarray(b,dtype=float)
    if float(a@b)>.85:return True
    ca=np.mean([refs[i].pos for i in first['attachments'][0]['added']],axis=0);cb=np.mean([refs[i].pos for i in second['attachments'][0]['added']],axis=0)
    n=cb-ca
    if np.linalg.norm(n)<1e-6:return False
    n=n/np.linalg.norm(n);mirrored=a-2*(a@n)*n
    return float(mirrored@b)>.85


def instructions(refs,lib,out,size,plan_only=False):
    import render,ldraw,physics
    progress('Checking connectors and planning reverse assembly')
    warnings=[]
    steps,issues,groups=assembly_plan(refs,lib,warnings)
    if not issues:
        steps,groups=finish_subbuilds_before_attachment(steps,groups,refs)
        steps,groups=collapse_single_piece_subbuilds(steps,groups)
        steps=merge_repeated_steps(steps,refs)
        steps=share_repeated_subbuilds(steps,refs,lib)
    (out/'plan.json').write_text(json.dumps(dict(steps=steps,issues=issues,warnings=warnings,groups=groups,
        final_pose_check_passed=bool(groups),assembly_path_found=not issues,
        checks='Full final-pose relation; 4 LDU sampled straight withdrawal/insertion. No continuous swept-volume, hand access, gravity/stability or flexible hinge-path proof.'),indent=2),encoding='utf-8')
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

    def scorer(self,visible,added,fit):
        """(fraction, pixels) of the added pieces left uncovered in a view."""
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
        return score

    def score_without(self,view,visible,added,fit):
        return self.scorer(visible,added,fit)(view)[0]

    def choose(self,visible,added,fit):
        score=self.scorer(visible,added,fit)
        initial=score(self.previous) if self.previous in self.VIEWS else (0,0)
        if initial[0]>=.72 and initial[1]>=12:return self.previous,False,initial[0]
        # Keep visible depth on every page: no front/side/top orthographic
        # views, even when their visibility score happens to be higher.
        candidates=([self.previous] if self.previous in self.VIEWS else [])+list(self.VIEWS)
        ranked=[(score(v),-i,v) for i,v in enumerate(dict.fromkeys(candidates))]
        best=max(ranked,key=lambda x:(round(x[0][0],2),x[0][1],x[1]));view=best[2];changed=view!=self.previous;self.previous=view
        return view,changed,best[0][0]

    def occluders(self,view,visible,added,fit,minimum=3):
        """Pieces in front of the added pieces in this view, most covering first."""
        import render,ldraw
        visible=list(visible);pieces=[self.triangles(i) for i in visible]
        triangles=np.concatenate(pieces);owners=np.concatenate([np.full(len(t),i) for i,t in zip(visible,pieces)])
        flags=np.isin(owners,list(added))
        fitpoints=np.concatenate([self.triangles(i).reshape(-1,3) for i in fit])
        r,u,c=render.camera_basis(*view);projection=np.stack([r,-u,-c])@ldraw.LDRAW_TO_BUILDER
        points=fitpoints@projection.T;origin,distance=perspective_frame(points);points=perspective_points(points,origin,distance)
        lo,hi=points.min(0),points.max(0);scale=224/max(float(hi[0]-lo[0]),float(hi[1]-lo[1]),1e-6);center=(lo+hi)/2
        t=perspective_points(triangles@projection.T,origin,distance);t[:,:,:2]=(t[:,:,:2]-center[:2])*scale+128
        pixels,depth,ids=render.rasterize(t,256,256)
        z=np.full(256*256,np.inf);np.minimum.at(z,pixels,depth)
        front=depth<=z[pixels]+1e-9;winners=np.full(256*256,-1,dtype=np.int64);winners[pixels[front]]=ids[front]
        where,_,_=render.rasterize(t[flags],256,256);where=np.unique(where)
        hidden=winners[where];hidden=hidden[(hidden>=0)]
        hidden=owners[hidden][~flags[hidden]]
        names,counts=np.unique(hidden,return_counts=True)
        return [int(i) for i,n in sorted(zip(names,counts),key=lambda x:-x[1]) if n>=minimum]


def framing(refs,lib,steps,assemblies,minimum=80.,growth=2.):
    """Pieces each step's camera is fitted to.

    Within one assembly the zoom is held steady and only steps out when the
    build has grown to about twice the size it had when that zoom was set, so
    a first small piece is not a speck in a frame sized for the finished
    subbuild and the camera does not creep on every page."""
    bounds={}
    def extent(ids):
        lo=np.full(3,np.inf);hi=np.full(3,-np.inf)
        for i in ids:
            if i not in bounds:
                p=refs[i];v=lib.geom(p.name,'low').tris.reshape(-1,3)@p.rot.T+p.pos
                bounds[i]=(v.min(0),v.max(0)) if len(v) else (p.pos,p.pos)
            lo=np.minimum(lo,bounds[i][0]);hi=np.maximum(hi,bounds[i][1])
        return max(float(np.linalg.norm(hi-lo)),minimum)
    fits=[None]*len(steps);k=0
    while k<len(steps):
        run=k
        while run+1<len(steps) and assemblies[run+1] is assemblies[k]:run+=1
        start=k
        while start<=run:
            limit=growth*extent(steps[start]['visible']);end=start
            while end<run and extent(steps[end+1]['visible'])<=limit:end+=1
            shown=sorted(set(steps[end]['visible'])&assemblies[start] or steps[end]['visible'])
            for t in range(start,end+1):fits[t]=shown
            start=end+1
        k=run+1
    return fits


def render_instructions(refs,lib,out,size,steps,groups,warnings):
    """Illustrate an already checked plan; legality remains the caller's gate."""
    import render,ldraw,physics
    from cad_contact_policy import curved_clip_matches
    ssaa=2 if getattr(render,'_cad_high_quality',False) else 1
    progress('Rendering cover');perspective_render(refs,size=size,lib=lib,ssaa=ssaa,highlight=False).save(out/'cover.png')
    pages=['<section><h1>Build instructions</h1><img src="cover.png"><p>Page 0 · '+str(len(refs))+' pieces</p><p>Draft: sampled insertion checks; review physical access and stability before sharing.</p></section>']
    if warnings:pages[0]=pages[0].replace('</section>','<h2>Assembly notes — review before building</h2><ul>'+''.join('<li>'+html.escape(w)+'</li>' for w in warnings)+'</ul></section>')
    page_info=[];viewers={};display_frames={};render.HIGHLIGHT_RGB=(220,40,30)
    group_sets=[set(g) for g in groups]
    def first_piece(members):
        # The piece laid down first in this assembly: its frame is the build frame.
        for s in steps:
            if s['added'] and set(s['added'])<=members:return s['added'][0]
        return min(members)
    main_builds=[g for g in group_sets if not any(g<h for h in group_sets)];numbers={};anchors={}
    world_anchors={first_piece(g) for g in main_builds}
    def assembly(step):
        if step['kind']=='part':return group_sets[step['group']-1]
        # A join is shown in the frame of the assembly it completes.
        shown=set(step['visible']);holders=[g for g in group_sets if shown<=g]
        return min(holders,key=len) if holders else set(range(len(refs)))
    assemblies=[assembly(s) for s in steps]
    fits=framing(refs,lib,steps,assemblies)
    for index,step in enumerate(steps,1):
        progress(f'Illustrating step {index}/{len(steps)}')
        name=f'step-{index:04}.png'
        members=assemblies[index-1];fit_ids=fits[index-1]
        anchor_id=first_piece(members);key=('frame',anchor_id)
        if key not in display_frames:
            # Subbuilds are built flat in the frame of their first piece, not
            # held at the final articulated pose throughout their construction;
            # the finished model keeps the orientation it was designed in.
            anchor=refs[anchor_id]
            if anchor_id in world_anchors:frame=np.eye(3);origin=np.zeros(3)
            else:frame=anchor.rot.T;origin=anchor.pos
            display=[ldraw.Ref(p.name,p.color,frame@(p.pos-origin),frame@p.rot) for p in refs]
            display_frames[key]=(display,frame,origin);viewers[key]=InstructionViews(display,lib)
        display,frame,origin=display_frames[key];views=viewers[key]
        selected=[ldraw.Ref(display[i].name,display[i].color,display[i].pos,display[i].rot,step=1 if i in step['added'] else 0) for i in step['visible']]
        fit=[display[i] for i in fit_ids]
        view,turned,visibility=views.choose(step['visible'],set(step['added']),fit_ids)
        ghosts=views.occluders(view,step['visible'],set(step['added']),fit_ids) if visibility<.9 else []
        img=perspective_render(selected,size=size,lib=lib,ssaa=ssaa,step=1,highlight=True,fit_refs=fit,az=view[0],el=view[1])
        if ghosts:
            # Cut away what hides the new pieces, and keep it as a faint
            # translucent ghost so the surroundings still read.
            hidden=set(ghosts);clear=perspective_render([r for i,r in zip(step['visible'],selected) if i not in hidden],size=size,lib=lib,ssaa=ssaa,step=1,highlight=True,fit_refs=fit,az=view[0],el=view[1])
            full=np.asarray(img).astype(float);cut=np.asarray(clear).astype(float)
            differs=np.abs(full-cut).max(axis=2)>6
            differs&=~((cut[:,:,0]==220)&(cut[:,:,1]==40)&(cut[:,:,2]==30))
            blend=cut.copy();blend[differs]=cut[differs]*.72+full[differs]*.28
            info=clear.info.copy();img=Image.fromarray(blend.round().astype(np.uint8));img.info.update(info)
            visibility=views.score_without(view,[i for i in step['visible'] if i not in hidden],set(step['added']),fit_ids)
        from PIL import ImageFilter
        pixels=np.asarray(img);red=(pixels[:,:,0]==220)&(pixels[:,:,1]==40)&(pixels[:,:,2]==30)
        expanded=np.asarray(Image.fromarray(red.astype(np.uint8)*255).filter(ImageFilter.MaxFilter(2*max(2,round(size/400))+1)))>0
        pixels=pixels.copy();pixels[expanded]=(220,40,30);info=img.info.copy();img=Image.fromarray(pixels);img.info.update(info)
        # One arrow per placed piece, or per attached copy of a subbuild.
        operations=step.get('attachments') or [dict(added=step['added'],direction=step['direction'],staging_distance=step.get('staging_distance'))]
        for operation in operations:
            if operation['direction'] is None:continue
            r,u,c=render.camera_basis(*view);projection=np.stack([r,-u,-c])@ldraw.LDRAW_TO_BUILDER
            centers=[]
            endpoints=[]
            for i in operation['added']:
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
            direction=frame@np.array(operation['direction']);cam=img.info['cam']
            def pixel(p):
                p=perspective_points(np.asarray(p),np.array(cam['perspective_center']),cam['perspective_distance'])
                return (p[:2]-np.array(cam['center'])[:2])*cam['scale']+size/2
            for center in centers:
                center=frame@(center-origin)
                end=pixel(projection@center);start=pixel(projection@(center-direction*min(30,operation.get('staging_distance') or 30)));start=np.clip(start,12,size-12);delta=end-start
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
        shown=operations[0]['added'] if step.get('attachments') else step['added']
        trayparts=[display[i] for i in shown] if step['kind']=='join' else [ldraw.Ref(part,color,np.array([120.*k,0.,0.]),np.eye(3)) for k,(part,color) in enumerate(sorted(lots))]
        tray=perspective_render(trayparts,size=220,lib=lib,highlight=False,ssaa=ssaa)
        trayname=f'parts-{index:04}.png';tray.save(out/trayname)
        attached=len(step.get('attachments') or [])
        labels='Completed subbuild'+(f' ×{attached}' if attached>1 else '') if step['kind']=='join' else ' · '.join(f'{qty}×' for _,qty in sorted(lots.items()))
        if step['kind']=='join' and step.get('of',1)>1:
            first,last=step.get('copy_range') or [step['copy']]*2
            labels+=f' (copy {first} of {step["of"]})' if first==last else f' (copies {first}–{last} of {step["of"]})'
        copies=step.get('copies',1)
        badge=f'<span class="times">×{copies}</span>' if copies>1 else ''
        # Subbuilds are numbered in the order the reader meets them; the
        # finished model itself carries no subbuild title.
        build=group_sets[step['group']-1]
        if step['group'] not in anchors:anchors[step['group']]=first_piece(build)
        if anchors[step['group']] in world_anchors or len(build)<2 or step.get('single_piece_attachment'):title=''
        else:title='Subbuild '+str(numbers.setdefault(min(build),len(numbers)+1))
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
        page_info.append(dict(title=title,parts=labels,action=action,view=list(view),projection='perspective',added_visibility=visibility,local_build_frame=step['kind']=='part',attachment_tilt_degrees=tilt,copies=copies,attachments=max(attached,1),ghosted=ghosts))
        turn='<span class="turn">↻ Turn the model</span>' if turned else ''
        note=(f'<p class="angle">Subbuild tilt: {tilt:.0f}°</p>' if tilt is not None and tilt>.5 else '')+('<p>Placement needs a closer view — review this step.</p>' if visibility<.15 else '')+('<p>Set aside — connection not confirmed in the saved model.</p>' if loose else '')
        pages.append(f'<section><header><h1>{index}</h1>{badge}<small>{title}</small>{turn}</header><aside><img src="{trayname}"><b>{labels}</b></aside><img class="assembly" src="{name}">{note}</section>')
    (out/'instructions.html').write_text('<!doctype html><meta charset="utf-8"><title>CAD build instructions</title><style>body{font:18px system-ui;background:#eee;margin:0}section{box-sizing:border-box;background:white;max-width:950px;min-height:100vh;margin:20px auto;padding:32px;break-after:page}header{display:flex;align-items:center;gap:24px}.times{font-size:30px;font-weight:700;color:white;background:#d22;border-radius:10px;padding:2px 14px}h1{font-size:58px;margin:0}small{color:#555}.turn{margin-left:auto}h2{font-size:23px}section>img{width:100%;max-height:72vh;object-fit:contain}aside{display:inline-flex;align-items:center;gap:24px;padding:8px 24px 8px 8px;background:#e3f0fa;border:1px solid #a5bdd0;border-radius:12px;margin-top:16px}aside img{width:140px}p{line-height:1.5}@media print{body{background:white}section{margin:0;min-height:0;height:98vh;padding:15mm}}</style>'+''.join(pages),encoding='utf-8')
    return dict(images=['cover.png'],document='instructions.html',pages=page_info,steps=len(steps),groups=len(groups),warnings=warnings,status='Draft generated after final-pose and sampled insertion checks. Review access/stability; sampled checks are not a continuous physical proof.')


def main():
    for stream in (sys.stdin,sys.stdout):
        try:stream.reconfigure(encoding='utf-8')
        except (AttributeError,ValueError):pass
    job=json.load(sys.stdin);out=Path(job['out'])
    sys.path.insert(0,job['bench'])
    sys.path.insert(0,str(Path(job['bench']).parent/'cad/app/src'))
    import os
    os.environ.setdefault('BENCH_SHADOW',job['shadow'])
    os.environ.setdefault('CAD_RAY_LIBRARY',str(Path(job['bench']).parent/library_name()))
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
