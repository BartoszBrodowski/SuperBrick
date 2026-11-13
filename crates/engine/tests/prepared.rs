use engine::{Cmd, Engine, PartSpec};
use glam::{DMat3, DVec3};
use std::sync::Arc;
#[test]
fn hidden_pieces_remain_collision_obstacles_and_are_reported_for_reveal() {
    let lib = Arc::new(ldraw::Library::open_default().unwrap());
    let doc = ldraw::Document::parse(b"1 4 0 -24 0 1 0 0 0 1 0 0 0 1 3003.dat\n", "hidden.ldr");
    let mut e = Engine::new(lib.clone(), doc, None);
    e.execute(Cmd::Hide {
        ids: vec![1],
        value: true,
    })
    .unwrap();
    let part = PartSpec {
        name: "3005.dat".into(),
        color: 1,
        pos: DVec3::ZERO,
        rot: DMat3::IDENTITY,
        id: None,
    };
    let snapshot = e.snapshot();
    assert!(
        snapshot
            .prepare_placement(
                lib.clone(),
                0,
                vec![],
                part.clone(),
                DVec3::ZERO,
                DVec3::NEG_Y,
                true,
                false,
                DMat3::IDENTITY
            )
            .is_err()
    );
    assert_eq!(
        snapshot.hidden_placement_blockers(
            lib.clone(),
            0,
            &[],
            &part,
            DVec3::ZERO,
            DMat3::IDENTITY,
            true
        ),
        vec![1]
    );
    assert!(
        snapshot
            .prepare_placement(
                lib.clone(),
                0,
                vec![],
                part.clone(),
                DVec3::new(120., 0., 0.),
                DVec3::NEG_Y,
                true,
                false,
                DMat3::IDENTITY
            )
            .is_ok()
    );
    assert!(
        snapshot
            .hidden_placement_blockers(
                lib,
                0,
                &[],
                &part,
                DVec3::new(120., 0., 0.),
                DMat3::IDENTITY,
                true
            )
            .is_empty()
    );
}
#[test]
fn isolated_previews_commit_once_preserve_rotation_and_refuse_stale_edits() {
    let lib = Arc::new(ldraw::Library::open_default().unwrap());
    let mut e = Engine::new(lib.clone(), ldraw::Document::new("test.ldr", "test"), None);
    let original = e.model.doc.to_bytes();
    let part = PartSpec {
        name: "3004.dat".into(),
        color: 1,
        pos: DVec3::ZERO,
        rot: DMat3::IDENTITY,
        id: None,
    };
    let rotation = DMat3::from_rotation_y(std::f64::consts::FRAC_PI_2);
    let solution = e
        .snapshot()
        .prepare_placement(
            lib.clone(),
            0,
            vec![],
            part.clone(),
            DVec3::new(100., 0., 0.),
            DVec3::NEG_Y,
            true,
            false,
            rotation,
        )
        .unwrap();
    assert_eq!(e.model.doc.to_bytes(), original);
    e.commit_placement(solution.prepared).unwrap();
    assert_eq!(e.model.parts.len(), 1);
    assert!(
        (e.model.parts[&1].rot - rotation)
            .to_cols_array()
            .iter()
            .all(|v| v.abs() < 1e-6)
    );
    assert!(e.model.selection.contains(&1));
    let stale = e
        .snapshot()
        .prepare_placement(
            lib.clone(),
            0,
            vec![],
            part,
            DVec3::new(-100., 0., 0.),
            DVec3::NEG_Y,
            true,
            false,
            DMat3::IDENTITY,
        )
        .unwrap();
    e.execute(Cmd::Paint {
        ids: vec![1],
        color: 4,
    })
    .unwrap();
    let painted = e.model.doc.to_bytes();
    assert!(e.commit_placement(stale.prepared).is_err());
    assert_eq!(e.model.doc.to_bytes(), painted);
    e.undo(false).unwrap();
    e.undo(false).unwrap();
    assert_eq!(e.model.doc.to_bytes(), original);
}
