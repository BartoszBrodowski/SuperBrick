use glam::DVec3;
use ldraw::{Document, Library};
use render::{Scene, SceneOptions, StudMode};

#[test]
fn panel_empty_space_must_not_erase_the_six_studs_of_a_plate() {
    let lib = Library::open_default().unwrap();
    let doc = Document::parse(
        b"0 Panel and plate regression\n1 7 0 -120 0 1 0 0 0 1 0 0 0 1 59349.dat\n1 4 0 -16 -2 1 0 0 0 1 0 0 0 1 3666.dat\n",
        "studs.ldr",
    );
    let panel = lib.mesh("59349.dat", ldraw::Lod::Std).unwrap();
    let plate = lib.mesh("3666.dat", ldraw::Lod::Std).unwrap();
    let (lo, hi) = panel.bbox(true);
    let studs: Vec<_> = plate.studs.iter().filter(|s| s.outer).collect();
    assert_eq!(studs.len(), 6);
    // The old point-in-box test mistakes empty space above the panel's foot
    // for solid material, even though the narrow plate's studs remain exposed.
    assert!(
        studs.iter().any(|s| {
            let q = s.pos + s.dir * 2.0 + glam::vec3(0., 104., -2.);
            q.cmpge(lo + glam::Vec3::splat(0.5)).all() && q.cmple(hi - glam::Vec3::splat(0.5)).all()
        }),
        "panel bounds {lo:?}..{hi:?}, studs {:?}",
        studs.iter().map(|s| (s.pos, s.dir)).collect::<Vec<_>>()
    );
    for tilt in [0., std::f64::consts::FRAC_PI_2] {
        let mut flat = doc.flatten(None);
        let rotation = glam::DMat3::from_rotation_z(tilt);
        for p in &mut flat {
            p.rot = rotation * p.rot;
            p.pos = rotation * p.pos + DVec3::new(150., -20., 75.);
        }
        for mode in [StudMode::Instanced, StudMode::Culled] {
            let scene = Scene::build(
                &lib,
                &flat,
                &SceneOptions {
                    studs: mode,
                    ..Default::default()
                },
            );
            assert!(scene.stats.missing.is_empty());
            assert_eq!(
                scene.instances.iter().filter(|p| p.id == 1).count(),
                7,
                "one body and all six exposed studs must be drawn ({mode:?})"
            );
        }
    }
}
