"""Real geometry/planner/ray tests; run with unittest discovery from the CAD root."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch
import numpy as np

ROOT=Path(__file__).resolve().parents[1]
sys.path.insert(0,str(ROOT.parent/'bench-next'))
os.environ.setdefault('BENCH_SHADOW',str(ROOT.parent/'data-next/ldcad_shadow/LDCadShadowLibrary-main'))
import ldraw
import snaps
original_make_conns=snaps.make_conns
def centered(kind,properties,src,rule='ldcad'):
    if kind=='SNAP_FGR' and rule=='ldcad' and 'center' not in properties:properties=dict(properties,center='true')
    return original_make_conns(kind,properties,src,rule)
snaps.make_conns=centered
spec=importlib.util.spec_from_file_location('publication',ROOT/'app/src/publication.py')
publication=importlib.util.module_from_spec(spec);spec.loader.exec_module(publication)


class PublicationTests(unittest.TestCase):
    def test_spherical_joint_without_a_single_axis_has_a_checked_assembly_path(self):
        refs=[ldraw.Ref('92013.dat',4,np.zeros(3),np.eye(3)),ldraw.Ref('57909.dat',1,np.array([80.,0.,0.]),np.diag([-1.,1.,-1.]))]
        steps,issues,groups=publication.assembly_plan(refs,self.lib)
        self.assertFalse(issues,issues)
        self.assertEqual(sorted(i for s in steps if s['kind']=='part' for i in s['added']),[0,1])
        self.assertTrue(any(s.get('direction') is not None for s in steps))
    @classmethod
    def setUpClass(cls):cls.lib=ldraw.get_library()

    def test_repeated_independent_parts_merge_but_stacked_parts_do_not(self):
        refs=[ldraw.Ref('3005.dat',4,np.array([40.*i,0.,0.]),np.eye(3)) for i in range(3)]
        steps=[dict(kind='part',group=1,visible=list(range(i+1)),added=[i],direction=None if i==0 else [0,1,0],staging_distance=8) for i in range(3)]
        merged=publication.merge_repeated_steps(steps,refs)
        self.assertEqual(len(merged),2);self.assertEqual(merged[1]['added'],[1,2]);self.assertEqual(merged[1]['source_steps'],[2,3])
        for i,p in enumerate(refs):p.pos=np.array([0.,-24.*i,0.])
        self.assertEqual(len(publication.merge_repeated_steps(steps,refs)),3)

    def test_single_piece_subbuild_becomes_one_normal_attachment(self):
        steps=[dict(kind='part',group=1,visible=[0],added=[0],direction=None),dict(kind='part',group=2,visible=[1],added=[1],direction=None),dict(kind='join',group=2,visible=[0,1],added=[1],direction=[0,1,0],staging_distance=8)]
        planned,groups=publication.collapse_single_piece_subbuilds(steps,[[0],[1]])
        self.assertEqual(len(planned),2);self.assertEqual(planned[-1]['kind'],'part');self.assertEqual(planned[-1]['added'],[1]);self.assertEqual(groups[planned[-1]['group']-1],[0,1])

    def test_curved_sausage_clip_has_checked_operation_not_an_unattached_page(self):
        import physics
        from cad_contact_policy import curved_clip_matches
        rotation=np.array([[-.999066,0,.043215],[0,1,0],[-.043215,0,-.999066]])
        refs=[ldraw.Ref('6019.dat',4,np.zeros(3),np.eye(3)),ldraw.Ref('33078.dat',6,np.array([.272781,2.,-19.864931]),rotation)]
        a,b=[physics.Body(physics.get_shape(p.name),p.rot,p.pos) for p in refs]
        self.assertEqual(physics.relation(a,b).overlap,0)
        self.assertEqual(len(curved_clip_matches(a,b)),1)
        steps,issues,groups=publication.assembly_plan(refs,self.lib)
        self.assertFalse(issues,issues);self.assertTrue(any(s['direction'] is not None for s in steps))
        self.assertTrue(any(len(s['visible'])==2 for s in steps))
        b=physics.Body(b.shape,b.R,b.t+np.array([0,30,0]))
        self.assertEqual(curved_clip_matches(a,b),[])

    def test_general_subbuild_additions_move_before_join_only_if_paths_stay_clear(self):
        import physics
        refs=[ldraw.Ref('3005.dat',4,np.array([0.,-24.*i,0.]),np.eye(3)) for i in range(3)]
        steps=[dict(kind='part',group=1,visible=[0],added=[0],direction=None),dict(kind='part',group=2,visible=[1],added=[1],direction=None),dict(kind='join',group=2,visible=[0,1],added=[1],direction=[0,1,0],staging_distance=8),dict(kind='part',group=1,visible=[0,1,2],added=[2],direction=[0,1,0],staging_distance=8)]
        planned,groups=publication.finish_subbuilds_before_attachment(steps,[[0],[1]],refs)
        self.assertEqual(planned[-1]['kind'],'join');self.assertEqual(planned[-1]['added'],[1,2])
        self.assertEqual([s['added'] for s in planned if s['kind']=='part'],[[0],[1],[2]])
        # An overhead obstacle permits the small original join but blocks the
        # enlarged subbuild's withdrawal path. Never reorder past that obstacle.
        refs.append(ldraw.Ref('3005.dat',1,np.array([0.,-76.,0.]),np.eye(3)))
        obstructed=[dict(s,visible=s['visible']+[3]) if k>=2 else dict(s) for k,s in enumerate(steps)]
        rejected,_=publication.finish_subbuilds_before_attachment(obstructed,[[0,3],[1]],refs)
        self.assertEqual(rejected,obstructed)

    def test_camera_changes_for_occlusion(self):
        refs=[ldraw.Ref('3005.dat',4,np.array([0.,0.,0.]),np.eye(3)),ldraw.Ref('3005.dat',1,np.array([0.,0.,22.]),np.eye(3))]
        chooser=publication.InstructionViews(refs,self.lib)
        first=chooser.previous
        # Test the hidden direction, then require a visible alternative rather
        # than a fixed universal azimuth for the booklet.
        best=[]
        for target in [0,1]:
            chooser.previous=first;view,changed,visible=chooser.choose([0,1],{target},[0,1]);best.append((changed,visible))
        self.assertTrue(any(changed for changed,_ in best));self.assertTrue(all(v>.5 for _,v in best))

    def test_instruction_views_never_keep_straight_on_camera(self):
        chooser=publication.InstructionViews(self.stack(),self.lib)
        for direct in [(0,0),(90,0),(-90,0),(180,0),(0,90)]:
            chooser.previous=direct
            view,changed,_=chooser.choose([0,1],{1},[0,1])
            self.assertTrue(changed)
            self.assertIn(view,chooser.VIEWS)
            self.assertNotEqual(view[0]%90,0)
            self.assertGreaterEqual(abs(view[1]),25)
            self.assertLess(abs(view[1]),60)

    def test_perspective_has_foreshortening_and_consistent_reciprocal_depth(self):
        # Equal-width objects farther from the camera must appear smaller;
        # rotating an orthographic view cannot pass this test.
        points=np.array([[-10.,0.,-20.],[10.,0.,-20.],[-10.,0.,20.],[10.,0.,20.]])
        projected=publication.perspective_points(points,np.zeros(3),100.)
        near=projected[1,0]-projected[0,0];far=projected[3,0]-projected[2,0]
        self.assertAlmostEqual(near/far,1.5)
        self.assertLess(projected[0,2],projected[2,2])
        self.assertTrue(np.allclose(projected[:,2],100.-10000./(100.+points[:,2])))
        with self.assertRaises(ValueError):
            publication.perspective_points(np.array([[0.,0.,-101.]]),np.zeros(3),100.)

    def test_neighboring_region_finishes_before_hopping_to_other_tower(self):
        refs=[ldraw.Ref('3035.dat',7,np.zeros(3),np.eye(3))]
        for x in [-30.,30.]:
            refs.extend(ldraw.Ref('3005.dat',4,np.array([x,-24.*level,-10.]),np.eye(3)) for level in [1,2,3])
        steps,issues,_=publication.assembly_plan(refs,self.lib)
        self.assertFalse(issues,issues)
        columns=[refs[i].pos[0] for s in steps if s['kind']=='part' for i in s['added'] if i]
        self.assertEqual(sum(a!=b for a,b in zip(columns,columns[1:])),1,columns)

    def stack(self):
        return [ldraw.Ref('3005.dat',4,np.array([0.,0.,0.]),np.eye(3)),
                 ldraw.Ref('3005.dat',1,np.array([0.,-24.,0.]),np.eye(3))]

    def test_native_raster_matches_reference_coverage_depth_and_visibility(self):
        import render,raster
        previous=render.rasterize
        try:
            if not publication.configure_render(False):self.skipTest('Native raster library unavailable')
            rng=np.random.default_rng(2718)
            tris=rng.uniform(-30,150,(180,3,3));tris[:,:,2]=rng.uniform(-20,20,(180,3))
            # Degenerate, fully off-screen, and reversed-winding triangles.
            tris=np.concatenate([tris,np.zeros((1,3,3)),tris[:3,::-1]])
            expected_depth,expected_ids=raster.zbuffer(tris,128,96)
            pix,depth,ids=render.rasterize(tris,128,96)
            actual=np.full((96*128,),np.inf);actual[pix]=depth;actual=actual.reshape(96,128)
            np.testing.assert_array_equal(np.isfinite(actual),np.isfinite(expected_depth))
            np.testing.assert_allclose(actual,expected_depth,atol=1e-10)
            self.assertTrue(np.all(ids>=0))
        finally:render.rasterize=previous

    def test_fast_and_high_quality_keep_requested_output_resolution(self):
        import render
        from PIL import Image
        previous=render.rasterize
        try:
            for high in [False,True]:
                publication.configure_render(high)
                with tempfile.TemporaryDirectory(prefix='cad-quality-test-') as d:
                    out=Path(d);result=publication.instructions(self.stack(),self.lib,out,192)
                    with Image.open(out/'step-0002.png') as image:self.assertEqual(image.size,(192,192))
                    self.assertEqual(result['steps'],2)
        finally:
            render.rasterize=previous;render._cad_high_quality=False

    def test_contact_at_cutoff_still_blocks_even_with_confirmed_connectors(self):
        sys.path.insert(0,str(ROOT/'app/src'))
        from cad_contact_policy import blocking_relation,contact_review
        contact=SimpleNamespace(overlap=6.,conflicts=[],links=[object()],narrow=True)
        self.assertFalse(contact_review(contact,6.));self.assertTrue(blocking_relation(contact,6.))
        for overrides in [dict(overlap=6.01),dict(conflicts=[object()]),dict(links=[]),dict(narrow=False)]:
            case=SimpleNamespace(**{**vars(contact),**overrides})
            self.assertFalse(contact_review(case,6.));self.assertTrue(blocking_relation(case,6.))

    def test_dense_mesh_review_corrects_a_coarse_volume_false_positive(self):
        import physics
        sys.path.insert(0,str(ROOT/'app/src'))
        from cad_contact_policy import refine_relation,blocking_relation
        shape=physics.get_shape('3005.dat')
        a=physics.Body(shape,np.eye(3),np.zeros(3))
        b=physics.Body(shape,np.eye(3),np.array([19.,0.,19.]))
        coarse=physics.relation(a,b)
        self.assertGreaterEqual(coarse.overlap,physics.COLLIDE_MIN)
        refined=refine_relation(a,b,coarse,physics.COLLIDE_MIN)
        self.assertIsNot(refined,coarse);self.assertFalse(blocking_relation(refined,physics.COLLIDE_MIN))
        self.assertLess(refined.overlap,6.);self.assertGreaterEqual(coarse.overlap,6.)

    def test_intermediate_withdrawal_need_not_be_fully_seated(self):
        import physics
        original=physics.relation
        def with_unseated_intermediate(a,b,*args,**kwargs):
            if abs(np.linalg.norm(a.t-b.t)-28.)<1e-5:
                return SimpleNamespace(overlap=0.,contact=False,conflicts=[SimpleNamespace(reason='stud not pressed home (gap)')])
            return original(a,b,*args,**kwargs)
        with patch.object(physics,'relation',side_effect=with_unseated_intermediate):
            steps,issues,_=publication.assembly_plan(self.stack(),self.lib)
            self.assertFalse(issues,issues);self.assertEqual(len(steps),2)

    def test_insertion_starts_at_nearby_clear_pose_not_infinity(self):
        import physics
        original=physics.relation
        def distant_obstacle(a,b,*args,**kwargs):
            if np.linalg.norm(a.t-b.t)>=40:
                return SimpleNamespace(overlap=100.,conflicts=[],contact=True)
            return original(a,b,*args,**kwargs)
        with patch.object(physics,'relation',side_effect=distant_obstacle):
            steps,issues,_=publication.assembly_plan(self.stack(),self.lib)
        self.assertFalse(issues,issues)
        self.assertEqual(steps[1]['staging_distance'],8.)

    def test_tilted_stacks_group_in_their_own_frame(self):
        angle=np.radians(37);c,s=np.cos(angle),np.sin(angle)
        R=np.array([[c,-s,0.],[s,c,0.],[0.,0.,1.]])
        refs=[ldraw.Ref(p.name,p.color,R@p.pos,R@p.rot) for p in self.stack()]
        steps,issues,groups=publication.assembly_plan(refs,self.lib)
        self.assertFalse(issues,issues);self.assertEqual(len(groups),1)
        self.assertEqual(len(steps),2)

    def test_ray_triangle_nearest_and_miss(self):
        triangle=np.array([[[-1.,-1.,0.],[1.,-1.,0.],[0.,1.,0.]]])
        rays=publication.Rays(np.concatenate([triangle,triangle+[0,0,2]]),np.ones((2,3))*255)
        distance,hit=rays.cast(np.array([[0.,0.,-2.],[5.,0.,-2.]]),np.array([[0.,0.,1.],[0.,0.,1.]]))
        self.assertEqual(hit.tolist(),[0,-1]);self.assertAlmostEqual(distance[0],2.)

    def test_native_matches_reference_for_random_rays(self):
        path=os.environ.get('CAD_RAY_LIBRARY')
        if not path or not Path(path).is_file():self.skipTest('native accelerator not compiled for this run')
        rng=np.random.default_rng(74);tri=rng.normal(size=(50,3,3));colors=np.ones((50,3))
        origins=rng.normal(size=(2400,3))*3;directions=rng.normal(size=(2400,3));directions/=np.linalg.norm(directions,axis=1)[:,None]
        accelerated=publication.Rays(tri,colors);self.assertIsNotNone(accelerated.native)
        os.environ.pop('CAD_RAY_LIBRARY')
        try:reference=publication.Rays(tri,colors)
        finally:os.environ['CAD_RAY_LIBRARY']=path
        for limit in [None,np.full(len(origins),2.)]:
            a,ai=accelerated.cast(origins,directions,limit);b,bi=reference.cast(origins,directions,limit)
            np.testing.assert_array_equal(ai,bi);np.testing.assert_allclose(a,b,atol=1e-8)

    def test_stack_is_one_subbuild_with_foundation_first(self):
        steps,issues,groups=publication.assembly_plan(self.stack(),self.lib)
        self.assertFalse(issues);self.assertEqual(len(groups),1)
        self.assertEqual([s['added'] for s in steps],[[0],[1]])
        self.assertEqual(steps[1]['direction'],[0.,1.,0.])

    def test_side_stud_splits_into_joinable_subbuilds(self):
        refs=ldraw.flatten_model(ldraw.load_model_file(str(ROOT.parent/'bench-next/tests/connections/headlight_side_plate.ldr')),lib=self.lib)
        steps,issues,groups=publication.assembly_plan(refs,self.lib)
        self.assertFalse(issues,issues);self.assertEqual(len(groups),2)
        self.assertEqual(sum(s['kind']=='join' for s in steps),1)
        self.assertEqual(sorted(i for s in steps if s['kind']=='part' for i in s['added']),[0,1])

    def test_collision_and_disconnected_builds_are_not_published_as_valid(self):
        refs=self.stack();refs[1].pos=refs[0].pos.copy()
        steps,issues,_=publication.assembly_plan(refs,self.lib)
        self.assertFalse(steps);self.assertTrue(issues)
        with tempfile.TemporaryDirectory(prefix='cad-illegal-publication-') as d:
            out=Path(d)
            with self.assertRaisesRegex(ValueError,'Final-pose legality check failed'):
                publication.instructions(refs,self.lib,out,160)
            plan=json.loads((out/'plan.json').read_text())
            self.assertFalse(plan['final_pose_check_passed'])
            self.assertFalse(plan['assembly_path_found'])
            self.assertFalse((out/'instructions.html').exists())

        refs=self.stack();refs[1].pos=np.array([0.,-25.,0.])
        steps,issues,_=publication.assembly_plan(refs,self.lib)
        self.assertFalse(steps);self.assertTrue(any('not pressed home' in issue for issue in issues),issues)
        self.assertTrue(any('3005.dat' in issue for issue in issues))

    def test_snot_shell_requires_constructible_halves_then_join(self):
        refs=ldraw.flatten_model(ldraw.load_model_file(str(ROOT/'tests/fixtures/instruction_snot_shell.ldr')),lib=self.lib)
        steps,issues,groups=publication.assembly_plan(refs,self.lib)
        self.assertFalse(issues,issues)
        self.assertTrue(any(s['kind']=='join' for s in steps))
        self.assertEqual(sorted(i for s in steps if s['kind']=='part' for i in s['added']),list(range(len(refs))))

    def test_native_inside_matches_oracle_five_ray_parity(self):
        import physics
        if not os.environ.get('CAD_RAY_LIBRARY'):self.skipTest('native library not requested')
        rng=np.random.default_rng(731)
        for name in ['3005.dat','93273.dat','99780.dat']:
            triangles=physics.mesh_tris(name);scene=publication.Rays(triangles,np.zeros((len(triangles),3)))
            if scene.native is None or not hasattr(scene.native,'cad_rays_inside'):self.skipTest('native inside accelerator not built')
            lo=triangles.min(axis=(0,1))-1;hi=triangles.max(axis=(0,1))+1;points=rng.uniform(lo,hi,(2000,3))
            np.testing.assert_array_equal(scene.inside(points,physics._RAYS),physics.ray_inside(points,triangles))

    def test_two_point_snot_alignment_is_engaged_and_buildable(self):
        import physics
        refs=[ldraw.Ref('2436a.dat',7,np.zeros(3),np.eye(3)),
              ldraw.Ref('93273.dat',7,np.array([0.,10.,-14.]),np.array([[0.,0.,1.],[1.,0.,0.],[0.,1.,0.]]))]
        a,b=[physics.Body(physics.get_shape(r.name),r.rot,r.pos) for r in refs]
        self.assertEqual(len(physics.snap_links(a,b)[0]),2)
        self.assertEqual(physics.relation(a,b).overlap,0)
        steps,issues,_=publication.assembly_plan(refs,self.lib)
        self.assertFalse(issues,issues)
        self.assertEqual(sorted(i for s in steps if s['kind']=='part' for i in s['added']),[0,1])

    def test_parallel_hinges_find_checked_connector_frame_withdrawals(self):
        refs=ldraw.flatten_model(ldraw.load_model_file(str(ROOT/'tests/fixtures/parallel_hinges.ldr')),lib=self.lib)
        steps,issues,groups=publication.assembly_plan(refs,self.lib)
        self.assertFalse(issues,issues)
        self.assertEqual(sorted(i for s in steps if s['kind']=='part' for i in s['added']),list(range(len(refs))))
        self.assertTrue(all(s.get('staging_distance',0)>=8 for s in steps if s['direction'] is not None))
    def test_independent_assemblies_are_built_separately_without_fake_joins(self):
        refs=self.stack()+self.stack()
        for r in refs[2:]:r.pos=r.pos+np.array([200.,0.,0.])
        warnings=[];steps,issues,groups=publication.assembly_plan(refs,self.lib,warnings)
        self.assertFalse(issues,issues);self.assertEqual(len(groups),2)
        self.assertTrue(any('independent' in s for s in warnings))
        self.assertEqual(sorted(i for s in steps for i in s['added']),list(range(4)))
        self.assertFalse(any(s['kind']=='join' for s in steps))
        self.assertEqual({s['component'] for s in steps},{1,2})

    def test_document_images_numbering_manifest_and_studio_pixels(self):
        from PIL import Image
        refs=self.stack();before=[r.line() for r in refs]
        with tempfile.TemporaryDirectory(prefix='cad-publication-test-') as d:
            out=Path(d);result=publication.instructions(refs,self.lib,out,160)
            self.assertEqual(result['steps'],2)
            self.assertEqual(len(result['pages']),2)
            self.assertEqual(result['pages'][0]['parts'],'1×')
            text=(out/'instructions.html').read_text()
            self.assertIn('Page 0',text);self.assertIn('<h1>1</h1>',text);self.assertIn('<h1>2</h1>',text)
            self.assertNotIn('previous pieces are faded',text);self.assertNotIn('3005.dat',text)
            self.assertTrue(result['pages'][0]['local_build_frame'])
            plan=json.loads((out/'plan.json').read_text())
            self.assertEqual(len(plan['steps']),2)
            self.assertTrue(plan['final_pose_check_passed'])
            self.assertTrue(plan['assembly_path_found'])
            for name in ['cover.png','step-0001.png','step-0002.png','parts-0001.png']:
                self.assertTrue((out/name).is_file())
            publication.studio(refs,self.lib,out/'studio.png',size=96,samples=1)
            pixels=np.asarray(Image.open(out/'studio.png'))
            self.assertTrue((pixels[0,0]==255).all());self.assertGreater(np.count_nonzero(pixels.min(axis=2)<180),100)
            self.assertGreater(len(np.unique(pixels.reshape(-1,3),axis=0)),20)
        self.assertEqual([r.line() for r in refs],before)


if __name__=='__main__':unittest.main()
