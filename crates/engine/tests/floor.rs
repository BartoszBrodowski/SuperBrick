use engine::{Cmd, Engine, PartSpec};
use glam::{DMat3, DVec3};
use std::sync::Arc;
#[test]
fn floor_follows_lowest_geometry_and_new_islands_seat_on_it() {
    let lib = Arc::new(ldraw::Library::open_default().unwrap());
    let mut e = Engine::new(
        lib,
        ldraw::Document::parse(
            b"0 floor\n1 4 0 76 0 1 0 0 0 1 0 0 0 1 3005.dat\n",
            "floor.ldr",
        ),
        None,
    );
    assert!((e.floor_height(0) - 100.).abs() < 0.01);
    let p = PartSpec {
        name: "3005.dat".into(),
        color: 1,
        pos: DVec3::ZERO,
        rot: DMat3::IDENTITY,
        id: None,
    };
    let p = e
        .place_preview(0, p, DVec3::new(120., 100., 0.), DVec3::NEG_Y, true, &[])
        .unwrap();
    assert!((p.pos.y - 76.).abs() < 0.01);
    e.execute(Cmd::Move {
        ids: vec![1],
        delta: DVec3::Y * 40.,
        free: true,
    })
    .unwrap();
    assert!((e.floor_height(0) - 140.).abs() < 0.01);
}
