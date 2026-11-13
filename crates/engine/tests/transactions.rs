use engine::{Cmd, Engine, PartSpec};
use glam::{DMat3, DVec3};
use ldraw::{Document, Library};
use std::sync::{Arc, OnceLock};
fn lib() -> Arc<Library> {
    static LIB: OnceLock<Arc<Library>> = OnceLock::new();
    LIB.get_or_init(|| {
        Arc::new(Library::open_default().expect("LDraw library required for integration tests"))
    })
    .clone()
}
fn engine() -> Engine {
    Engine::new(lib(),Document::parse(b"0 preserved header\r\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\r\n1 1 60 0 0 1 0 0 0 1 0 0 0 1 3003.dat\r\n","model.ldr"),None)
}
fn add(name: &str, pos: DVec3) -> Cmd {
    Cmd::Add {
        file: 0,
        parts: vec![PartSpec {
            name: name.into(),
            pos,
            color: 4,
            rot: DMat3::IDENTITY,
            id: None,
        }],
        free: true,
    }
}
#[test]
fn no_op_save_and_actor_publication() {
    let e = engine();
    let bytes = e.model.doc.to_bytes();
    let h = engine::Handle::spawn(e);
    assert_eq!(h.snapshot().model.doc.to_bytes(), bytes);
    h.command(Cmd::Paint {
        ids: vec![1],
        color: 14,
    })
    .unwrap();
    assert_eq!(h.snapshot().model.parts[&1].color, 14);
}

#[test]
fn ground_placement_keeps_stud_phase_and_allows_separate_islands() {
    let mut e = Engine::new(lib(), Document::new("main.ldr", "ground"), None);
    let spec = PartSpec {
        name: "3004.dat".into(),
        color: 4,
        pos: DVec3::ZERO,
        rot: DMat3::IDENTITY,
        id: None,
    };
    for x in [0.0, 100.0, -100.0] {
        let part = e
            .place_preview(
                0,
                spec.clone(),
                DVec3::new(x, 0., 0.),
                DVec3::NEG_Y,
                true,
                &[],
            )
            .unwrap();
        let shape = physics::Shape::build(&e.lib, &part.name).unwrap();
        let body = physics::Body::new(Arc::new(shape), part.rot, part.pos);
        assert!(body.fhi.y.abs() < 0.01);
        for anti in &body.anti {
            assert!((anti.x / 20.0 - (anti.x / 20.0).round()).abs() < 0.01);
            assert!((anti.z / 20.0 - (anti.z / 20.0).round()).abs() < 0.01);
        }
        e.execute(Cmd::Add {
            file: 0,
            parts: vec![part],
            free: false,
        })
        .unwrap();
    }
    assert!(
        e.place_preview(0, spec.clone(), DVec3::ZERO, DVec3::NEG_Y, true, &[])
            .is_err()
    );
    // Legacy models may start half a stud out of phase. Align the guide and new pieces to them.
    let mut old = Engine::new(lib(), Document::new("main.ldr", "legacy"), None);
    old.execute(add("3004.dat", DVec3::new(0., -24., 0.)))
        .unwrap();
    let phase = old.ground_phase(0);
    assert!(
        (phase.length() - 10.0).abs() < 0.01,
        "legacy lattice phase: {phase:?}"
    );
    let placed = old
        .place_preview(0, spec, DVec3::new(100., 0., 0.), DVec3::NEG_Y, true, &[])
        .unwrap();
    assert!(placed.pos.z.abs() < 0.01);
}

#[test]
fn moving_a_stack_from_its_upper_piece_keeps_the_whole_group_above_ground() {
    let mut e = Engine::new(lib(), Document::new("main.ldr", "stack"), None);
    e.execute(add("3005.dat", DVec3::new(0., -24., 0.)))
        .unwrap();
    e.execute(add("3005.dat", DVec3::new(0., -48., 0.)))
        .unwrap();
    let p = e.model.parts[&2].clone();
    let anchor = PartSpec {
        name: p.name,
        color: p.color,
        pos: p.pos,
        rot: p.rot,
        id: None,
    };
    let command = e
        .move_preview(
            0,
            vec![1, 2],
            anchor,
            DVec3::new(100., 0., 0.),
            DVec3::NEG_Y,
            true,
            false,
        )
        .unwrap();
    if let Cmd::Move { delta, .. } = &command {
        assert!(delta.y.abs() < 0.01);
    } else {
        panic!("expected move");
    }
    e.execute(command).unwrap();
    assert!((e.model.parts[&1].pos.y + 24.).abs() < 0.01);
    assert!((e.model.parts[&2].pos.y + 48.).abs() < 0.01);
}
#[test]
fn failed_batch_is_atomic_and_undo_preserves_ids() {
    let mut e = engine();
    let before = e.model.doc.to_bytes();
    assert!(
        e.execute(Cmd::Move {
            ids: vec![1, 99],
            delta: DVec3::X,
            free: true
        })
        .is_err()
    );
    assert_eq!(e.model.doc.to_bytes(), before);
    e.execute(Cmd::Paint {
        ids: vec![1, 2],
        color: 14,
    })
    .unwrap();
    e.undo(false).unwrap();
    assert_eq!(e.model.doc.to_bytes(), before);
    e.undo(true).unwrap();
    assert_eq!(e.model.parts[&1].color, 14);
    e.execute(Cmd::Remove { ids: vec![1] }).unwrap();
    e.undo(false).unwrap();
    assert!(e.model.parts.contains_key(&1));
    assert_eq!(
        e.execute(add("3003.dat", DVec3::X * 120.0)).unwrap()["ids"][0],
        3
    );
}
#[test]
fn stale_agent_edit_is_refused_and_draft_chain_is_atomic() {
    let mut e = engine();
    e.observe("agent");
    e.propose(
        "agent".into(),
        1,
        Cmd::Move {
            ids: vec![1],
            delta: DVec3::Z * 60.0,
            free: true,
        },
    )
    .unwrap();
    e.execute(Cmd::Paint {
        ids: vec![1],
        color: 14,
    })
    .unwrap();
    assert_eq!(e.drafts.ops[0].state, "stale");
    assert!(
        e.accept("agent", None).unwrap()["accepted"]
            .as_array()
            .unwrap()
            .is_empty()
    );
    assert!(
        e.propose(
            "agent".into(),
            2,
            Cmd::Paint {
                ids: vec![1],
                color: 1
            }
        )
        .is_err()
    );
    e.observe("agent");
    let d = e
        .propose("agent".into(), 3, add("3003.dat", DVec3::X * 120.0))
        .unwrap()["draft"]
        .as_u64()
        .unwrap();
    let id = match &e.drafts.ops.last().unwrap().commands[0] {
        Cmd::Add { parts, .. } => parts[0].id.unwrap(),
        _ => unreachable!(),
    };
    let d2 = e
        .propose(
            "agent".into(),
            3,
            Cmd::Paint {
                ids: vec![id],
                color: 2,
            },
        )
        .unwrap()["draft"]
        .as_u64()
        .unwrap();
    e.accept("agent", Some(vec![d2])).unwrap();
    assert_eq!(e.model.parts[&id].color, 2);
    assert_eq!(
        e.drafts.ops.iter().find(|o| o.id == d).unwrap().state,
        "accepted"
    );
    e.undo(false).unwrap();
    assert!(!e.model.parts.contains_key(&id));
}
#[test]
fn rejected_dependencies_block_descendants() {
    let mut e = engine();
    e.observe("agent");
    let out = e
        .propose("agent".into(), 1, add("3003.dat", DVec3::X * 120.0))
        .unwrap();
    let part = out["result"]["ids"][0].as_u64().unwrap();
    e.propose(
        "agent".into(),
        2,
        Cmd::Paint {
            ids: vec![part],
            color: 2,
        },
    )
    .unwrap();
    e.propose(
        "agent".into(),
        3,
        Cmd::Paint {
            ids: vec![part],
            color: 4,
        },
    )
    .unwrap();
    // An unrelated draft must survive the rejection.
    e.propose("agent".into(), 4, add("3003.dat", DVec3::X * 240.0))
        .unwrap();
    assert!(e.drafts.ops[1].dependencies.contains(&e.drafts.ops[0].id));
    assert!(e.drafts.ops[3].dependencies.is_empty());
    let id = e.drafts.ops[0].id;
    e.reject("agent", vec![id]).unwrap();
    assert_eq!(e.drafts.ops[1].state, "blocked");
    assert_eq!(e.drafts.ops[2].state, "blocked");
    assert_eq!(e.drafts.ops[3].state, "pending");
}
#[test]
fn subbuild_instances_update_and_mpd_roundtrips() {
    let mut e = engine();
    let before = e.model.flat(0);
    let out = e
        .execute(Cmd::MakeSubbuild {
            ids: vec![1, 2],
            name: "pair".into(),
        })
        .unwrap();
    let file = out["file"].as_u64().unwrap() as usize;
    let after = e.model.flat(0);
    assert_eq!(before.len(), after.len());
    for (a, b) in before.iter().zip(&after) {
        assert_eq!(
            (a.pos, a.rot, a.color, a.name.clone()),
            (b.pos, b.rot, b.color, b.name.clone())
        );
    }
    let bytes = e.model.doc.to_bytes();
    let doc = Document::parse(&bytes, "model.mpd");
    assert_eq!(doc.to_bytes(), bytes);
    assert_eq!(doc.flatten(None).len(), 2);
    e.execute(Cmd::Add {
        file: 0,
        parts: vec![PartSpec {
            name: "pair.ldr".into(),
            color: 16,
            pos: DVec3::Z * 100.0,
            rot: DMat3::IDENTITY,
            id: None,
        }],
        free: true,
    })
    .unwrap();
    e.execute(Cmd::Paint {
        ids: vec![1],
        color: 14,
    })
    .unwrap();
    assert_eq!(e.model.flat(file).len(), 2);
    assert_eq!(
        e.model.flat(0).iter().filter(|p| p.color.0 == 14).count(),
        2
    );
    assert!(
        e.execute(Cmd::Add {
            file,
            parts: vec![PartSpec {
                name: "model.ldr".into(),
                color: 16,
                pos: DVec3::ZERO,
                rot: DMat3::IDENTITY,
                id: None
            }],
            free: true
        })
        .is_err()
    );
}
#[test]
fn collision_is_rejected_and_snapshot_preserved() {
    let mut e = engine();
    let before = e.model.doc.to_bytes();
    assert!(
        e.execute(Cmd::Move {
            ids: vec![2],
            delta: -DVec3::X * 60.0,
            free: false
        })
        .is_err()
    );
    assert_eq!(e.model.doc.to_bytes(), before);
}
#[test]
fn lock_and_duplicate_id_guards() {
    let mut e = engine();
    e.execute(Cmd::Lock {
        ids: vec![1],
        value: true,
    })
    .unwrap();
    assert!(e.execute(Cmd::Remove { ids: vec![1] }).is_err());
    assert!(
        e.execute(Cmd::Move {
            ids: vec![2, 2],
            delta: DVec3::X,
            free: true
        })
        .is_err()
    );
}
#[test]
fn registry_has_unique_commands() {
    let defs = engine::registry();
    let names: std::collections::BTreeSet<_> = defs.iter().map(|d| d.name).collect();
    assert_eq!(defs.len(), names.len());
    assert_eq!(defs.len(), 16);
    for d in defs {
        assert_eq!(d.schema["type"], "object");
        assert!(d.schema["properties"].is_object());
    }
}
#[test]
fn later_agent_turn_survives_acceptance_but_not_a_conflicting_user_edit() {
    let mut e = engine();
    e.observe("agent");
    let first = e
        .propose(
            "agent".into(),
            1,
            Cmd::Move {
                ids: vec![1],
                delta: DVec3::Z * 60.0,
                free: true,
            },
        )
        .unwrap()["draft"]
        .as_u64()
        .unwrap();
    e.propose(
        "agent".into(),
        2,
        Cmd::Paint {
            ids: vec![1],
            color: 14,
        },
    )
    .unwrap();
    e.accept("agent", Some(vec![first])).unwrap();
    assert_eq!(e.drafts.ops[1].state, "pending");
    e.accept("agent", None).unwrap();
    assert_eq!(e.model.parts[&1].color, 14);
    e.propose(
        "agent".into(),
        3,
        Cmd::Paint {
            ids: vec![1],
            color: 1,
        },
    )
    .unwrap();
    e.execute(Cmd::Paint {
        ids: vec![1],
        color: 2,
    })
    .unwrap();
    assert_eq!(e.drafts.ops[2].state, "stale");
}
#[test]
fn user_occupancy_blocks_ghost_and_previews_do_not_change_base() {
    let mut e = engine();
    let bytes = e.model.doc.to_bytes();
    let mut command = add("3003.dat", DVec3::X * 120.0);
    if let Cmd::Add { free, .. } = &mut command {
        *free = false;
    }
    e.propose("agent".into(), 1, command.clone()).unwrap();
    let ghosts = e.snapshot().ghosts(lib(), 0);
    assert!(!ghosts.is_empty());
    assert_eq!(bytes, e.model.doc.to_bytes());
    e.execute(command).unwrap();
    assert_eq!(e.drafts.ops[0].state, "blocked");
    assert!(
        e.snapshot()
            .ghosts(lib(), 0)
            .iter()
            .all(|g| g.kind == engine::presentation::GhostKind::Stale)
    );
}
#[test]
fn external_edits_are_not_overwritten_and_atomic_save_roundtrips() {
    let path = std::env::temp_dir().join(format!("cad-save-test-{}.ldr", std::process::id()));
    let mut e = engine();
    let bytes = e.model.doc.to_bytes();
    e.save(Some(path.clone())).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    e.execute(Cmd::Paint {
        ids: vec![1],
        color: 14,
    })
    .unwrap();
    std::fs::write(&path, b"0 external user edit\n").unwrap();
    assert!(e.save(None).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"0 external user edit\n");
    std::fs::remove_file(path).unwrap();
}
#[test]
fn geometric_snap_and_nudge_follow_a_vertical_stud_surface() {
    let mut e = Engine::new(lib(), Document::new("test.ldr", "test"), None);
    let r = DMat3::from_rotation_x(std::f64::consts::FRAC_PI_2);
    e.execute(Cmd::Add {
        file: 0,
        parts: vec![PartSpec {
            name: "3003.dat".into(),
            pos: DVec3::ZERO,
            color: 4,
            rot: r,
            id: None,
        }],
        free: true,
    })
    .unwrap();
    let snapped = e
        .snap(
            0,
            PartSpec {
                name: "3005.dat".into(),
                pos: DVec3::ZERO,
                color: 1,
                rot: r,
                id: None,
            },
            DVec3::ZERO,
            r * DVec3::NEG_Y,
        )
        .unwrap();
    let direction = if snapped.pos.x < 0.0 {
        DVec3::X
    } else {
        DVec3::NEG_X
    };
    let id = e
        .execute(Cmd::Add {
            file: 0,
            parts: vec![snapped],
            free: false,
        })
        .unwrap()["ids"][0]
        .as_u64()
        .unwrap();
    let before = e.model.parts[&id].pos;
    e.execute(Cmd::Nudge {
        ids: vec![id],
        direction,
        normal: false,
        alt: None,
    })
    .unwrap();
    assert!((e.model.parts[&id].pos - before - direction * 20.0).length() < 1e-6);
}
#[test]
fn fast_legality_matches_full_native_relation_on_square_poses() {
    let shape = Arc::new(physics::Shape::build(&lib(), "3003.dat").unwrap());
    let a = physics::Body::new(shape.clone(), DMat3::IDENTITY, DVec3::ZERO);
    for x in [-40.0, -20.0, -10.0, 0.0, 10.0, 20.0, 40.0] {
        for y in [-30.0, -24.0, -16.0, 0.0, 16.0, 24.0, 30.0] {
            let b = physics::Body::new(shape.clone(), DMat3::IDENTITY, DVec3::new(x, y, 0.0));
            let full = physics::relation(&a, &b, false).is_none_or(|r| !r.collides());
            assert_eq!(physics::fast_legal(&a, &b), full, "{x},{y}");
        }
    }
}
#[test]
fn subbuild_creation_refuses_step_loss_and_bad_names() {
    let mut e=Engine::new(lib(),Document::parse(b"1 4 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n0 STEP\n1 4 60 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n","steps.ldr"),None);
    let before = e.model.doc.to_bytes();
    assert!(
        e.execute(Cmd::MakeSubbuild {
            ids: vec![1, 2],
            name: "pair".into()
        })
        .is_err()
    );
    assert_eq!(e.model.doc.to_bytes(), before);
    assert!(e.execute(add("../3003.dat", DVec3::ZERO)).is_err());
    assert!(
        e.execute(add("3003.dat\n1 4 3003.dat", DVec3::ZERO))
            .is_err()
    );
}
#[test]
fn draft_copy_and_subbuild_allocate_stable_ids_across_previews_and_acceptance() {
    let mut e = engine();
    e.observe("agent");
    e.propose(
        "agent".into(),
        1,
        Cmd::Copy {
            ids: vec![1],
            delta: DVec3::Z * 100.0,
            count: 2,
        },
    )
    .unwrap();
    let first = e.snapshot().effective(lib(), "agent").unwrap();
    let second = e.snapshot().effective(lib(), "agent").unwrap();
    assert_eq!(
        first.parts.keys().collect::<Vec<_>>(),
        second.parts.keys().collect::<Vec<_>>()
    );
    let copied = *first.parts.keys().last().unwrap();
    e.propose(
        "agent".into(),
        2,
        Cmd::Paint {
            ids: vec![copied],
            color: 14,
        },
    )
    .unwrap();
    e.accept("agent", None).unwrap();
    assert_eq!(e.model.parts[&copied].color, 14);
    e.observe("agent");
    e.propose(
        "agent".into(),
        3,
        Cmd::MakeSubbuild {
            ids: vec![1, 2],
            name: "stable".into(),
        },
    )
    .unwrap();
    let first = e.snapshot().effective(lib(), "agent").unwrap();
    let reference = first
        .parts
        .values()
        .find(|p| p.name == "stable.ldr")
        .unwrap()
        .id;
    assert_eq!(
        e.snapshot().effective(lib(), "agent").unwrap().parts[&reference].name,
        "stable.ldr"
    );
    e.propose(
        "agent".into(),
        4,
        Cmd::Paint {
            ids: vec![reference],
            color: 1,
        },
    )
    .unwrap();
    e.accept("agent", None).unwrap();
    assert_eq!(e.model.parts[&reference].color, 1);
}
#[test]
fn nested_subbuild_edits_check_collisions_in_all_parent_instances() {
    let src=b"0 FILE main.ldr\n1 16 0 0 0 1 0 0 0 1 0 0 0 1 middle.ldr\n1 4 100 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n0 FILE middle.ldr\n1 16 0 0 0 1 0 0 0 1 0 0 0 1 leaf.ldr\n0 FILE leaf.ldr\n1 1 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n";
    let mut e = Engine::new(lib(), Document::parse(src, "main.mpd"), None);
    let before = e.model.doc.to_bytes();
    assert_eq!(e.model.flat(0)[0].ancestors.len(), 2);
    for id in [3, 4] {
        assert!(
            e.execute(Cmd::Move {
                ids: vec![id],
                delta: DVec3::X * 100.0,
                free: false
            })
            .is_err()
        );
        assert_eq!(before, e.model.doc.to_bytes());
    }
}
