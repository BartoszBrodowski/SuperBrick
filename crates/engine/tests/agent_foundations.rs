//! Foundations the agent operations build on: stable ids, batched drafts, structured
//! placement failures, FLOATING detection and candidate enumeration.
use engine::{Cmd, Engine, Model, PartSpec};
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
fn engine_from(text: &str) -> Engine {
    Engine::new(lib(), Document::parse(text.as_bytes(), "model.ldr"), None)
}
fn engine() -> Engine {
    engine_from(
        "0 test\r\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\r\n1 1 60 0 0 1 0 0 0 1 0 0 0 1 3003.dat\r\n",
    )
}
fn spec(name: &str, pos: DVec3) -> PartSpec {
    PartSpec {
        name: name.into(),
        pos,
        color: 4,
        rot: DMat3::IDENTITY,
        id: None,
    }
}
fn add(name: &str, pos: DVec3, free: bool) -> Cmd {
    Cmd::Add {
        file: 0,
        parts: vec![spec(name, pos)],
        free,
    }
}

#[test]
fn ids_survive_a_save_and_reload_through_the_id_map() {
    let mut e = engine();
    e.execute(Cmd::Remove { ids: vec![1] }).unwrap();
    let out = e.execute(add("3003.dat", DVec3::new(0.0, -24.0, 60.0), true)).unwrap();
    let new = out["ids"][0].as_u64().unwrap();
    assert_eq!(new, 3, "ids are never reused");
    let bytes = e.model.doc.to_bytes();
    let map = e.model.id_map();
    let back = Model::from_document_with_ids(Document::parse(&bytes, "model.ldr"), None, &map);
    let ids: Vec<_> = back.parts.keys().copied().collect();
    assert_eq!(ids, vec![2, 3]);
    assert_eq!(back.parts[&3].pos, e.model.parts[&3].pos);
}

#[test]
fn a_batch_is_one_draft_and_a_dry_run_stores_nothing() {
    let mut e = engine();
    e.observe("agent");
    let dry = e
        .propose_with("agent", 1, "two bricks", true, |tx| {
            tx.apply(add("3003.dat", DVec3::new(0.0, -24.0, 0.0), false))?;
            tx.apply(add("3003.dat", DVec3::new(60.0, -24.0, 0.0), false))
        })
        .unwrap();
    assert!(dry.draft.is_none());
    assert!(e.drafts.ops.is_empty());
    assert_eq!(dry.effective.parts.len(), 4);

    let p = e
        .propose_with("agent", 1, "two bricks, one painted", false, |tx| {
            let a = tx.apply(add("3003.dat", DVec3::new(0.0, -24.0, 0.0), false))?;
            tx.apply(add("3003.dat", DVec3::new(60.0, -24.0, 0.0), false))?;
            let id = a["ids"][0].as_u64().unwrap();
            tx.apply(Cmd::Paint {
                ids: vec![id],
                color: 14,
            })
        })
        .unwrap();
    assert!(p.draft.is_some());
    assert_eq!(e.drafts.ops.len(), 1);
    assert_eq!(e.drafts.ops[0].commands.len(), 3);
    assert_eq!(p.touched.created.len(), 2);
    // The user model is untouched until acceptance.
    assert_eq!(e.model.parts.len(), 2);
    e.accept("agent", None).unwrap();
    assert_eq!(e.model.parts.len(), 4);
    let first = *p.touched.created.iter().next().unwrap();
    assert_eq!(e.model.parts[&first].color, 14);
}

#[test]
fn a_failed_batch_leaves_no_draft_and_explains_the_collision() {
    let mut e = engine();
    e.observe("agent");
    let err = e
        .propose_with("agent", 1, "", false, |tx| {
            tx.apply(add("3003.dat", DVec3::new(0.0, -24.0, 0.0), false))?;
            // Sits inside brick #1.
            tx.apply(add("3003.dat", DVec3::new(10.0, 0.0, 0.0), false))
        })
        .err()
        .unwrap();
    assert!(err.contains("placement collides"), "{err}");
    assert!(e.drafts.ops.is_empty());
    let detail = e.last_failure.clone().expect("structured failure");
    let hit = &detail["collisions"][0];
    assert_eq!(hit["blocker"], 1);
    assert!(hit["depth_ldu"].as_f64().unwrap() > 0.0);
}

#[test]
fn stud_candidates_enumerate_distinct_legal_seats() {
    let mut e = engine();
    let part = spec("3003.dat", DVec3::ZERO);
    let seats = e
        .stud_candidates(0, &part, DVec3::new(30.0, -24.0, 0.0), DVec3::ZERO, &[], 60.0, 200)
        .unwrap();
    assert!(seats.len() > 4, "only {} seats", seats.len());
    let mut keys: Vec<_> = seats
        .iter()
        .map(|s| s.pos.to_array().map(|v| (v * 10.0).round() as i64))
        .collect();
    keys.sort();
    keys.dedup();
    assert_eq!(keys.len(), seats.len(), "seats must be distinct");
    // Every seat must be accepted by the strict edit check.
    for s in seats.iter().take(8) {
        let mut probe = engine();
        probe
            .execute(Cmd::Add {
                file: 0,
                parts: vec![s.clone()],
                free: false,
            })
            .unwrap();
    }
}

#[test]
fn read_only_views_refuse_edits() {
    let mut e = engine();
    e.read_only = true;
    assert!(e.execute(Cmd::Paint { ids: vec![1], color: 2 }).is_err());
    assert!(e.execute(Cmd::Select { ids: vec![1] }).is_ok());
    e.observe("agent");
    assert!(
        e.propose_with("agent", 1, "", false, |tx| tx.apply(Cmd::Paint {
            ids: vec![1],
            color: 2
        }))
        .is_err()
    );
}
