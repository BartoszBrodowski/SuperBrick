use engine::{
    Cmd, Engine,
    connectors::{self, Endpoint},
};
use glam::DVec3;
use std::{path::PathBuf, sync::Arc};
#[test]
fn fast_connect_endpoints_and_compatibility_do_not_require_graph_or_solve() {
    let doc = ldraw::Document::parse(
        b"1 4 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n1 1 100 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n",
        "studs.ldr",
    );
    let mut e = Engine::new(Arc::new(ldraw::Library::open_default().unwrap()), doc, None);
    let a: connectors::Analysis = serde_json::from_value(
        connectors::request(&e.model, 0, serde_json::json!({"action":"endpoints"})).unwrap(),
    )
    .unwrap();
    assert!(a.graph.joints.is_empty());
    assert!(!a.connectors.is_empty());
    let source = a
        .connectors
        .iter()
        .find(|c| c.part == 2 && c.label == "anti-stud")
        .unwrap()
        .endpoint();
    let d = connectors::request(
        &e.model,
        0,
        serde_json::json!({"action":"compatible","source":source}),
    )
    .unwrap();
    let target: Endpoint = serde_json::from_value(d["destinations"][0].clone()).unwrap();
    assert_eq!(target.part, 1);
    e.execute(Cmd::Connect {
        ids: vec![1, 2],
        file: 0,
        source,
        target,
        pose: 0,
    })
    .unwrap();
    assert!(
        !connectors::analyze(&e.model, 0)
            .unwrap()
            .graph
            .joints
            .is_empty()
    );
}
#[test]
fn near_technic_ball_is_explicitly_unseated_then_connect_restores_pivot() {
    let doc = ldraw::Document::parse(
        b"1 4 0 0 0 1 0 0 0 1 0 0 0 1 92013.dat\n1 1 82 0 0 -1 0 0 0 1 0 0 0 -1 57909.dat\n",
        "neck.ldr",
    );
    let mut e = Engine::new(Arc::new(ldraw::Library::open_default().unwrap()), doc, None);
    let a = connectors::analyze(&e.model, 0).unwrap();
    assert!(a.graph.joints.is_empty());
    assert_eq!(a.near_joints.len(), 1);
    let near = &a.near_joints[0];
    assert!((near.distance - 2.).abs() < 1e-6);
    e.execute(Cmd::Connect {
        ids: vec![1, 2],
        file: 0,
        source: near.target.clone(),
        target: near.source.clone(),
        pose: 0,
    })
    .unwrap();
    assert!(
        connectors::analyze(&e.model, 0)
            .unwrap()
            .graph
            .joints
            .iter()
            .any(|j| matches!(j.motion, engine::joints::Motion::Ball { .. }))
    );
}
#[test]
fn automatic_drag_reuses_free_connector_index_and_omits_occupied_studs() {
    let code = format!(
        "{}\n{}",
        include_str!("../src/connectors.py"),
        r#"
identity=tuple(np.eye(3).T.flatten())
key=((0,'3005.dat',identity,(0.,0.,0.),False),(1,'3005.dat',identity,(0.,-24.,0.),False))
grid=_drag_index(key)
assert _drag_index(key) is grid
available={(i,c) for values in grid.values() for i,c,_ in values}
a=_body('3005.dat',identity,(0.,0.,0.),1)
b=_body('3005.dat',identity,(0.,-24.,0.),2)
matches=physics.snap_links(a,b)[0]
assert matches
for match in matches:
    if match.type in ('stud','side-stud'):
        assert (0,match.ia) not in available
        assert (1,match.ib) not in available
assert _drag_index.cache_info().hits>=1
"#
    );
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let script = std::env::temp_dir().join(format!("sb-drag-test-{}.py", std::process::id()));
    std::fs::write(&script, &code).unwrap();
    let mut command = std::process::Command::new(
        std::env::var("CAD_PYTHON").unwrap_or_else(|_| "python3".into()),
    );
    ldraw::platform::no_window(&mut command);
    let output = command
    .arg(&script)
    .arg(root.join("bench-next"))
    .arg(root.join("data-next/ldcad_shadow/LDCadShadowLibrary-main"))
    .env("PYTHONDONTWRITEBYTECODE", "1")
    .stdin(std::process::Stdio::null())
    .output()
    .unwrap();
    let _ = std::fs::remove_file(&script);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}
#[test]
fn helper_exits_quietly_when_native_parent_closes_response_pipe() {
    use std::{
        io::Write,
        process::{Command, Stdio},
    };
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..");
    let script =
        ldraw::runtime::helper_script("connectors", include_str!("../src/connectors.py")).unwrap();
    let mut command = Command::new(std::env::var("CAD_PYTHON").unwrap_or_else(|_| "python3".into()));
    ldraw::platform::no_window(&mut command);
    let mut child = command
        .arg("-u")
        .arg(script)
        .arg(root.join("bench-next"))
        .arg(root.join("data-next/ldcad_shadow/LDCadShadowLibrary-main"))
        .env("PYTHONDONTWRITEBYTECODE", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    drop(child.stdout.take());
    child
        .stdin
        .as_mut()
        .unwrap()
        .write_all(b"{\"action\":\"unknown\",\"parts\":[],\"roots\":[]}\n")
        .unwrap();
    drop(child.stdin.take());
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        result.stderr.is_empty(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
}
fn fixture(name: &str) -> Engine {
    let path = if name == "lock_44301_44302" {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/lock_centered.ldr")
    } else {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../../bench-next/tests/connections")
            .join(format!("{name}.ldr"))
    };
    Engine::new(
        Arc::new(ldraw::Library::open_default().unwrap()),
        ldraw::Document::load(&path).unwrap(),
        None,
    )
}
#[test]
fn pins_axles_and_hinge_fingers_can_mate_from_separate_positions() {
    for (name, label) in [
        ("pin_in_brick_hole", "pin-hole"),
        ("axle_in_cross_block", "axle"),
        ("lock_44301_44302", "hinge"),
    ] {
        let mut e = fixture(name);
        let original = connectors::analyze(&e.model, 0).unwrap();
        assert!(
            original
                .graph
                .joints
                .iter()
                .any(|j| j.label.contains(label)),
            "{name}: {:?}",
            original.graph
        );
        e.execute(Cmd::Move {
            ids: vec![2],
            delta: DVec3::new(140., 0., 0.),
            free: true,
        })
        .unwrap();
        let before = e.model.doc.to_bytes();
        e.execute(Cmd::Connect {
            ids: vec![1, 2],
            file: 0,
            source: Endpoint {
                part: 2,
                leaf: None,
                index: None,
            },
            target: Endpoint {
                part: 1,
                leaf: None,
                index: None,
            },
            pose: 0,
        })
        .unwrap();
        let after = connectors::analyze(&e.model, 0).unwrap();
        assert!(
            after.graph.joints.iter().any(|j| j.label.contains(label)),
            "{name}: {:?}",
            after.graph
        );
        e.undo(false).unwrap();
        assert_eq!(e.model.doc.to_bytes(), before);
    }
}

#[test]
fn drag_snaps_pins_axles_and_new_parts_without_connect_mode() {
    for (name, label) in [
        ("pin_in_brick_hole", "pin-hole"),
        ("axle_in_cross_block", "axle"),
    ] {
        let mut e = fixture(name);
        e.execute(Cmd::Move {
            ids: vec![2],
            delta: DVec3::X * 140.,
            free: true,
        })
        .unwrap();
        let p = &e.model.parts[&2];
        let part = engine::PartSpec {
            name: p.name.clone(),
            color: p.color,
            pos: p.pos,
            rot: p.rot,
            id: None,
        };
        let a = connectors::analyze(&e.model, 0).unwrap();
        let bytes = e.model.doc.to_bytes();
        let preview = a
            .connectors
            .iter()
            .filter(|c| c.part == 1)
            .find_map(|c| e.full_drag_preview(0, &[2], part.clone(), c.position).ok())
            .expect("drag should find a legal pin/axle pose");
        assert_eq!(bytes, e.model.doc.to_bytes());
        e.execute(preview.0).unwrap();
        assert!(
            connectors::analyze(&e.model, 0)
                .unwrap()
                .graph
                .joints
                .iter()
                .any(|j| j.label.contains(label))
        );
        e.execute(Cmd::Remove { ids: vec![2] }).unwrap();
        let preview = a
            .connectors
            .iter()
            .filter(|c| c.part == 1)
            .find_map(|c| e.full_drag_preview(0, &[], part.clone(), c.position).ok())
            .expect("library part should snap too");
        e.execute(preview.0).unwrap();
        assert!(
            connectors::analyze(&e.model, 0)
                .unwrap()
                .graph
                .joints
                .iter()
                .any(|j| j.label.contains(label))
        );
    }
}

#[test]
fn drag_uses_arbitrarily_tilted_stud_frames() {
    let lib = Arc::new(ldraw::Library::open_default().unwrap());
    let mut e=Engine::new(lib,ldraw::Document::parse(b"0 tilted\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n1 1 200 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n","tilted.ldr"),None);
    e.execute(Cmd::Rotate {
        ids: vec![1],
        axis: DVec3::Z,
        angle: 37.,
        pivot: DVec3::ZERO,
        free: true,
    })
    .unwrap();
    let p = &e.model.parts[&2];
    let part = engine::PartSpec {
        name: p.name.clone(),
        color: p.color,
        pos: p.pos,
        rot: p.rot,
        id: None,
    };
    let a = connectors::analyze(&e.model, 0).unwrap();
    let preview = a
        .connectors
        .iter()
        .filter(|c| c.part == 1)
        .find_map(|c| e.full_drag_preview(0, &[2], part.clone(), c.position).ok())
        .unwrap();
    e.execute(preview.0).unwrap();
    assert!(
        connectors::analyze(&e.model, 0)
            .unwrap()
            .graph
            .joints
            .iter()
            .any(|j| j.label.contains("stud"))
    );
    assert!(
        (e.model.parts[&2].rot - glam::DMat3::IDENTITY)
            .to_cols_array()
            .iter()
            .any(|v| v.abs() > 0.1)
    );
}

#[test]
fn destination_filter_only_returns_endpoints_with_a_verified_mate() {
    let mut e = fixture("pin_in_brick_hole");
    e.execute(Cmd::Move {
        ids: vec![2],
        delta: DVec3::X * 140.,
        free: true,
    })
    .unwrap();
    let a = connectors::analyze(&e.model, 0).unwrap();
    let targets: Vec<_> = a.connectors.iter().map(|c| c.endpoint()).collect();
    let result = connectors::request(
        &e.model,
        0,
        serde_json::json!({"action":"destinations","source":{"part":2},"targets":targets}),
    )
    .unwrap();
    let endpoints: Vec<Endpoint> = serde_json::from_value(result["destinations"].clone()).unwrap();
    assert!(!endpoints.is_empty());
    for target in endpoints {
        assert_eq!(target.part, 1);
        let result = connectors::request(
            &e.model,
            0,
            serde_json::json!({"action":"connect","source":{"part":2},"target":target}),
        );
        assert!(result.is_ok());
    }
}

#[test]
fn hidden_pinholes_are_not_offered_as_destinations() {
    let mut e = fixture("pin_in_brick_hole");
    e.execute(Cmd::Move {
        ids: vec![2],
        delta: DVec3::X * 140.,
        free: true,
    })
    .unwrap();
    e.execute(Cmd::Hide {
        ids: vec![1],
        value: true,
    })
    .unwrap();
    let analysis = engine::connectors::analyze(&e.model, 0).unwrap();
    let targets: Vec<_> = analysis.connectors.iter().map(|c| c.endpoint()).collect();
    let result = engine::connectors::request(
        &e.model,
        0,
        serde_json::json!({"action":"destinations","source":{"part":2},"targets":targets}),
    )
    .unwrap();
    assert!(result["destinations"].as_array().unwrap().is_empty());
}
#[test]
fn auto_hinge_mating_keeps_the_plate_facing_the_same_way() {
    for world_tilt in [0., 90.] {
        let doc = ldraw::Document::parse(b"0 Hinge orientation regression\n1 7 0 0 0 1 0 0 0 1 0 0 0 1 44302a.dat\n1 7 0 0 100 1 0 0 0 1 0 0 0 1 44301a.dat\n", "hinge.ldr");
        let mut e = Engine::new(Arc::new(ldraw::Library::open_default().unwrap()), doc, None);
        if world_tilt != 0. {
            e.execute(Cmd::Rotate {
                ids: vec![1, 2],
                axis: DVec3::Z,
                angle: world_tilt,
                pivot: DVec3::ZERO,
                free: true,
            })
            .unwrap();
        }
        let up = e.model.parts[&2].rot * DVec3::NEG_Y;
        e.execute(Cmd::Connect {
            ids: vec![1, 2],
            file: 0,
            source: Endpoint {
                part: 2,
                leaf: None,
                index: None,
            },
            target: Endpoint {
                part: 1,
                leaf: None,
                index: None,
            },
            pose: 0,
        })
        .unwrap();
        assert!(
            (e.model.parts[&2].rot * DVec3::NEG_Y).dot(up) > 0.999,
            "Auto must not tip/flip the hinge plate to save a little translation (world tilt {world_tilt})"
        );
        let world = glam::DMat3::from_rotation_z(world_tilt.to_radians());
        assert!(
            e.model.parts[&2]
                .pos
                .distance(world * DVec3::new(60., 0., 0.))
                < 0.001,
            "The central ratchet must lie between the two fork arms, not 12 LDU beside them: {:?}",
            e.model.parts[&2].pos
        );
        assert!(
            connectors::analyze(&e.model, 0)
                .unwrap()
                .graph
                .joints
                .iter()
                .any(|j| j.label.contains("hinge"))
        );
    }
}
#[test]
fn hinge_rotation_is_a_validated_undo_transaction() {
    let mut e = fixture("lock_44301_44302");
    let analysis = connectors::analyze(&e.model, 0).unwrap();
    let joint = analysis
        .graph
        .joints
        .iter()
        .find(|j| j.label.contains("hinge"))
        .unwrap();
    let before = e.model.doc.to_bytes();
    e.execute(Cmd::Articulate {
        ids: vec![1, 2],
        file: 0,
        clicked: 2,
        joint: joint.id,
        axis: 0,
        angle: 15.,
        detach: vec![],
    })
    .unwrap();
    assert_ne!(before, e.model.doc.to_bytes());
    assert!(
        connectors::analyze(&e.model, 0)
            .unwrap()
            .graph
            .joints
            .iter()
            .any(|j| j.label.contains("hinge"))
    );
    e.undo(false).unwrap();
    assert_eq!(before, e.model.doc.to_bytes());
}
#[test]
fn paired_coaxial_hinges_rotate_together_without_breaking_either_bearing() {
    let path =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/parallel_hinges.ldr");
    for tilt in [0., 90.] {
        let mut e = Engine::new(
            Arc::new(ldraw::Library::open_default().unwrap()),
            ldraw::Document::load(&path).unwrap(),
            None,
        );
        if tilt != 0. {
            e.execute(Cmd::Rotate {
                ids: (1..=6).collect(),
                axis: DVec3::X,
                angle: tilt,
                pivot: DVec3::ZERO,
                free: true,
            })
            .unwrap();
        }
        let a = connectors::analyze(&e.model, 0).unwrap();
        let hinges: Vec<_> = a
            .graph
            .joints
            .iter()
            .filter(|j| j.label.contains("hinge"))
            .collect();
        assert_eq!(hinges.len(), 2, "{:?}", a.graph);
        let plan = a.graph.articulate(6, hinges[0].id).unwrap();
        assert_eq!(plan.moving, [3, 4, 6].into_iter().collect());
        assert!(plan.detach.is_empty(), "{:?}", plan);
        let before = e.model.doc.to_bytes();
        let fixed: Vec<_> = [1, 2, 5]
            .iter()
            .map(|id| e.model.parts[id].clone())
            .collect();
        e.execute(Cmd::Articulate {
            ids: (1..=6).collect(),
            file: 0,
            clicked: 6,
            joint: hinges[0].id,
            axis: 0,
            angle: 15.,
            detach: vec![],
        })
        .unwrap();
        assert_ne!(e.model.doc.to_bytes(), before);
        for p in fixed {
            assert_eq!(e.model.parts[&p.id].pos, p.pos);
            assert_eq!(e.model.parts[&p.id].rot, p.rot);
        }
        let after = connectors::analyze(&e.model, 0).unwrap();
        assert_eq!(
            after
                .graph
                .joints
                .iter()
                .filter(|j| j.label.contains("hinge"))
                .count(),
            2
        );
        e.undo(false).unwrap();
        assert_eq!(e.model.doc.to_bytes(), before);
    }
}
#[test]
fn both_ball_socket_families_expose_three_axes_and_rotate() {
    for name in ["ball_14417_14418", "ball_14419_angled"] {
        let mut e = fixture(name);
        let a = connectors::analyze(&e.model, 0).unwrap();
        let joint = a
            .graph
            .joints
            .iter()
            .find(|j| matches!(j.motion, engine::joints::Motion::Ball { .. }))
            .expect(name);
        assert_eq!(a.graph.articulate(2, joint.id).unwrap().axes.len(), 3);
        let mut accepted = false;
        for axis in 0..3 {
            for angle in [-15., 15.] {
                if e.execute(Cmd::Articulate {
                    ids: vec![1, 2],
                    file: 0,
                    clicked: 2,
                    joint: joint.id,
                    axis,
                    angle,
                    detach: vec![],
                })
                .is_ok()
                {
                    accepted = true;
                    break;
                }
            }
            if accepted {
                break;
            }
        }
        assert!(accepted, "no legal ball rotation: {name}");
    }
}
#[test]
fn ball_connect_keeps_the_moving_piece_turned_as_it_is() {
    // A ball joint turns freely, so connecting must not force the socket's frame onto the
    // ball's quarter twists. A mirrored leg has a left-handed socket frame; every frame-aligned
    // pose swung it into its neighbour and the connect was refused.
    let (c, s) = (33f64.to_radians().cos(), 33f64.to_radians().sin());
    let text = format!(
        "1 4 0 0 0 1 0 0 0 1 0 0 0 1 57909.dat\n1 1 160 -30 40 -1 0 0 0 {c} {s} 0 {s} {mc}  92013.dat\n",
        mc = -c
    );
    let doc = ldraw::Document::parse(text.as_bytes(), "roll.ldr");
    let mut e = Engine::new(Arc::new(ldraw::Library::open_default().unwrap()), doc, None);
    let before = e.model.parts[&2].rot;
    let a: connectors::Analysis = serde_json::from_value(
        connectors::request(&e.model, 0, serde_json::json!({"action":"endpoints"})).unwrap(),
    )
    .unwrap();
    let socket = a.connectors.iter().find(|c| c.part == 2 && c.label == "ball socket").unwrap();
    let ball = a.connectors.iter().find(|c| c.part == 1 && c.label == "ball").unwrap();
    e.execute(Cmd::Connect {
        ids: vec![1, 2],
        file: 0,
        source: socket.endpoint(),
        target: ball.endpoint(),
        pose: 0,
    })
    .unwrap();
    let after = &e.model.parts[&2];
    assert!((after.rot - before).to_cols_array().iter().all(|d| d.abs() < 1e-6), "{:?}", after.rot);
    assert!(
        connectors::analyze(&e.model, 0)
            .unwrap()
            .graph
            .joints
            .iter()
            .any(|j| matches!(j.motion, engine::joints::Motion::Ball { .. }))
    );
}
