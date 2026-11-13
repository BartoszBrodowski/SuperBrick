use engine::{Cmd, Engine};
use glam::{DMat3, DVec3};
use std::sync::Arc;

fn scene() -> Engine {
    Engine::new(Arc::new(ldraw::Library::open_default().unwrap()),ldraw::Document::parse(
        b"0 Rotation reseating\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3004.dat\n1 1 0 0 20 1 0 0 0 1 0 0 0 1 3005.dat\n", "rotation.ldr"),None)
}
fn rotate(e: &mut Engine, angle: f64) {
    e.execute(Cmd::Rotate {
        ids: vec![1],
        axis: DVec3::NEG_Y,
        angle,
        pivot: e.model.parts[&1].pos,
        free: false,
    })
    .unwrap();
}

#[test]
fn nearest_reseat_is_deterministic_touches_original_and_reverses_exactly() {
    let mut a = scene();
    let mut b = scene();
    let bytes = a.model.doc.to_bytes();
    let original = a.model.parts[&1].clone();
    rotate(&mut a, 90.);
    rotate(&mut b, 90.);
    let placed = &a.model.parts[&1];
    assert!(
        placed.pos.length() > 0. && placed.pos.length() <= 10.001,
        "{:?}",
        placed.pos
    );
    assert_eq!(placed.pos, b.model.parts[&1].pos);
    assert_eq!(placed.rot, b.model.parts[&1].rot);
    let shape = Arc::new(physics::Shape::build(&a.lib, "3004.dat").unwrap());
    let old = physics::Body::new(shape.clone(), original.rot, original.pos);
    let new = physics::Body::new(shape, placed.rot, placed.pos);
    assert!(physics::relation(&old, &new, true).unwrap().contact);
    for _ in 0..4 {
        rotate(&mut a, -90.);
        assert_eq!(a.model.parts[&1].pos, original.pos);
        assert_eq!(a.model.parts[&1].rot, DMat3::IDENTITY);
        rotate(&mut a, 90.);
    }
    a.undo(false).unwrap();
    a.undo(false).unwrap();
    // Undo is exact, including the source line representation.
    while a.model.doc.to_bytes() != bytes {
        a.undo(false).unwrap();
    }
}
