use engine::{Cmd, Engine, PartSpec};
use glam::{DMat3, DVec3};
use std::sync::Arc;

#[test]
fn symmetric_surfaces_do_not_require_identical_triangle_diagonals() {
    let e = Engine::new(
        Arc::new(ldraw::Library::open_default().unwrap()),
        ldraw::Document::new("mirror.ldr", "mirror"),
        None,
    );
    for name in ["24201.dat", "32803.dat"] {
        let part = PartSpec {
            name: name.into(),
            color: 4,
            pos: DVec3::ZERO,
            rot: DMat3::IDENTITY,
            id: None,
        };
        let result = e
            .snapshot()
            .mirror_specs(0, vec![part], 2, DVec3::ZERO)
            .unwrap();
        assert_eq!(result[0].name, name);
        assert!((result[0].rot.determinant() - 1.).abs() < 1e-8);
    }
}
fn engine() -> Engine {
    Engine::new(Arc::new(ldraw::Library::open_default().unwrap()),ldraw::Document::parse(b"0 Assembly\n1 4 0 -24 0 1 0 0 0 1 0 0 0 1 3005.dat\n1 1 0 -48 0 1 0 0 0 1 0 0 0 1 3005.dat\n","assembly.ldr"),None)
}
#[test]
fn assembly_clipboard_places_as_one_transaction_without_mutating_original() {
    let mut e = engine();
    let before = e.model.doc.to_bytes();
    e.execute(Cmd::Select { ids: vec![1, 2] }).unwrap();
    let snapshot = e.snapshot();
    let parts = snapshot.copied_assembly().unwrap();
    assert_eq!(parts.len(), 2);
    assert_eq!(parts[1].pos, DVec3::new(0., -24., 0.));
    let mut cache = Default::default();
    let solution = snapshot
        .prepare_assembly(
            e.lib.clone(),
            Some(&mut cache),
            0,
            parts,
            DVec3::new(120., 0., 0.),
            DVec3::NEG_Y,
            true,
            false,
            DMat3::IDENTITY,
        )
        .unwrap();
    assert_eq!(e.model.doc.to_bytes(), before);
    e.commit_placement(solution.prepared).unwrap();
    assert_eq!(e.model.parts.len(), 4);
    assert_eq!(e.model.selection.len(), 2);
    assert_eq!(e.model.parts[&1].pos, DVec3::new(0., -24., 0.));
    assert_eq!(
        e.model.parts[&4].pos - e.model.parts[&3].pos,
        DVec3::new(0., -24., 0.)
    );
    e.undo(false).unwrap();
    assert_eq!(e.model.doc.to_bytes(), before);
}
#[test]
fn pasted_assembly_ids_come_from_the_reserved_block() {
    // Like the app: ids come from a shared project counter, reserved lazily in blocks.
    let mut e = engine();
    let counter = Arc::new(std::sync::Mutex::new(3u64));
    let shared = counter.clone();
    e.set_id_reserver(Box::new(move || {
        let mut c = shared.lock().unwrap();
        let start = *c;
        *c += 100;
        Some((start, start + 100))
    }));
    e.execute(Cmd::Select { ids: vec![1, 2] }).unwrap();
    let snapshot = e.snapshot();
    let parts = snapshot.copied_assembly().unwrap();
    let solution = snapshot
        .prepare_assembly(
            e.lib.clone(),
            None,
            0,
            parts,
            DVec3::new(120., 0., 0.),
            DVec3::NEG_Y,
            true,
            false,
            DMat3::IDENTITY,
        )
        .unwrap();
    let pasted: Vec<u64> =
        serde_json::from_value(e.commit_placement(solution.prepared).unwrap()["ids"].clone())
            .unwrap();
    // The pasted parts' ids were reserved, so no later block can hand them out again.
    assert!(pasted.iter().all(|id| *id < *counter.lock().unwrap()));
    // Mirroring the paste (remove + add) used to fail with "part id already exists".
    e.execute(Cmd::Mirror {
        ids: pasted,
        axis: 0,
        pivot: DVec3::new(120., 0., 0.),
    })
    .unwrap();
    assert_eq!(e.model.parts.len(), 4);
}
#[test]
fn mirrors_are_proper_rotations_and_chiral_counterparts_are_verified() {
    let e = engine();
    let original = PartSpec {
        name: "41770a.dat".into(),
        color: 4,
        pos: DVec3::new(40., 0., 10.),
        rot: DMat3::from_rotation_y(0.37),
        id: None,
    };
    let parts = e
        .snapshot()
        .mirror_specs(0, vec![original.clone()], 0, DVec3::ZERO)
        .unwrap();
    assert_eq!(parts[0].name, "41769a.dat");
    assert!((parts[0].rot.determinant() - 1.).abs() < 1e-6);
    let back = e.snapshot().mirror_specs(0, parts, 0, DVec3::ZERO).unwrap();
    assert_eq!(back[0].name, original.name);
    assert!((back[0].pos - original.pos).length() < 1e-8);
    assert!(
        !(back[0].rot - original.rot)
            .to_cols_array()
            .iter()
            .any(|x| x.abs() > 1e-8)
    );
}
#[test]
fn unsupported_mirror_refuses_atomically_and_plain_mirror_undoes() {
    let mut e = engine();
    let before = e.model.doc.to_bytes();
    let unsupported = PartSpec {
        name: "missing-mirror-part.dat".into(),
        color: 4,
        pos: DVec3::ZERO,
        rot: DMat3::IDENTITY,
        id: None,
    };
    assert!(
        e.snapshot()
            .mirror_specs(0, vec![unsupported], 0, DVec3::ZERO)
            .is_err()
    );
    assert_eq!(e.model.doc.to_bytes(), before);
    e.execute(Cmd::Mirror {
        ids: vec![1, 2],
        axis: 0,
        pivot: DVec3::new(100., 0., 0.),
    })
    .unwrap();
    assert_eq!(e.model.selection.len(), 2);
    e.undo(false).unwrap();
    assert_eq!(e.model.doc.to_bytes(), before);
}
#[test]
fn copying_subbuild_instances_preserves_each_leaf_and_inherited_colour() {
    let doc=ldraw::Document::parse(b"0 FILE main.ldr\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 arm.ldr\n1 1 100 0 0 1 0 0 0 1 0 0 0 1 arm.ldr\n0 FILE arm.ldr\n1 16 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n1 14 0 -24 0 1 0 0 0 1 0 0 0 1 3005.dat\n","main.ldr");
    let mut e = Engine::new(Arc::new(ldraw::Library::open_default().unwrap()), doc, None);
    e.execute(Cmd::Select { ids: vec![1, 2] }).unwrap();
    let before = e.model.doc.to_bytes();
    let parts = e.snapshot().copied_assembly().unwrap();
    assert_eq!(parts.len(), 4);
    assert_eq!(
        parts.iter().map(|p| p.color).collect::<Vec<_>>(),
        vec![4, 14, 1, 14]
    );
    assert_eq!(parts[2].pos.x, 100.);
    assert_eq!(e.model.doc.to_bytes(), before);
}
#[test]
fn large_assembly_copy_has_no_piece_count_truncation() {
    let mut e = Engine::new(
        Arc::new(ldraw::Library::open_default().unwrap()),
        ldraw::Document::new("large.ldr", "Large clipboard"),
        None,
    );
    e.execute(Cmd::Add {
        file: 0,
        free: true,
        parts: (0..128)
            .map(|i| PartSpec {
                name: "3005.dat".into(),
                color: 4,
                pos: DVec3::new((i % 16) as f64 * 40., -24., (i / 16) as f64 * 40.),
                rot: DMat3::IDENTITY,
                id: None,
            })
            .collect(),
    })
    .unwrap();
    e.execute(Cmd::Select {
        ids: (1..=128).collect(),
    })
    .unwrap();
    let before = e.model.doc.to_bytes();
    let s = e.snapshot();
    let mut cache = Default::default();
    let placed = s
        .prepare_assembly(
            e.lib.clone(),
            Some(&mut cache),
            0,
            s.copied_assembly().unwrap(),
            DVec3::new(1000., 0., 1000.),
            DVec3::NEG_Y,
            true,
            false,
            DMat3::IDENTITY,
        )
        .unwrap();
    assert_eq!(e.model.doc.to_bytes(), before);
    e.commit_placement(placed.prepared).unwrap();
    assert_eq!(e.model.parts.len(), 256);
    assert_eq!(e.model.selection.len(), 128);
}
