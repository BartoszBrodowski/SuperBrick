use engine::{Model, PartSpec};
use glam::{DMat3, DVec3};
use ldraw::Document;
use serde_json::json;
use std::path::PathBuf;
use store::Store;

fn lib() -> std::sync::Arc<ldraw::Library> {
    static LIB: std::sync::OnceLock<std::sync::Arc<ldraw::Library>> = std::sync::OnceLock::new();
    LIB.get_or_init(|| std::sync::Arc::new(ldraw::Library::open_default().expect("LDraw library")))
        .clone()
}

fn tmp(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sb-store-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    for ext in ["", "-wal", "-shm"] {
        let _ = std::fs::remove_file(format!("{}{ext}", p.display()));
    }
    p
}

const MPD: &str = "0 FILE main.ldr\r\n0 Main\r\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3001.dat\r\n1 1 0 -24 0 1 0 0 0 1 0 0 0 1 sub.ldr\r\n0 NOFILE\r\n0 FILE sub.ldr\r\n0 Sub\r\n1 15 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\r\n1 15 40 0 0 1 0 0 0 1 0 0 0 1 3003.dat\r\n0 NOFILE\r\n";

#[test]
fn ldr_round_trips_byte_exact_with_ids() {
    let p = tmp("rt.sb");
    let doc = Document::parse(MPD.as_bytes(), "main.ldr");
    let s = Store::import(&p, doc, "human").unwrap();
    assert_eq!(s.export_ldr("main").unwrap(), MPD.as_bytes());
    let (_, m) = s.load("main").unwrap();
    let ids: Vec<_> = m.parts.keys().copied().collect();
    assert_eq!(ids.len(), 4);
    // A fresh connection sees the same ids.
    let s2 = Store::open(&p).unwrap();
    let (_, m2) = s2.load("main").unwrap();
    assert_eq!(m2.parts.keys().copied().collect::<Vec<_>>(), ids);
}

#[test]
fn real_model_round_trips() {
    let src = PathBuf::from(std::env::var("HOME").unwrap()).join("np.ldr");
    let Ok(bytes) = std::fs::read(&src) else {
        return;
    };
    let p = tmp("np.sb");
    let s = Store::import(&p, Document::parse(&bytes, "np.ldr"), "human").unwrap();
    assert_eq!(s.export_ldr("main").unwrap(), bytes);
}

#[test]
fn ids_survive_edits_and_new_ids_come_from_reserved_blocks() {
    let p = tmp("ids.sb");
    let mut s = Store::import(&p, Document::parse(MPD.as_bytes(), "main.ldr"), "human").unwrap();
    let (w, model) = s.load("main").unwrap();
    let lib = lib();
    let mut e = engine::Engine::from_model(lib, model);
    e.set_id_reserver(Store::id_reserver(p.clone()));
    let removed = *e.model.parts.keys().next().unwrap();
    e.model.parts.remove(&removed);
    let spec = PartSpec {
        name: "3024.dat".into(),
        color: 4,
        pos: DVec3::new(0.0, -100.0, 0.0),
        rot: DMat3::IDENTITY,
        id: None,
    };
    let v = e
        .execute(engine::Cmd::Add { file: 0, parts: vec![spec], free: true })
        .unwrap();
    let id = v["ids"][0].as_u64().unwrap();
    assert!(id >= 5, "new id {id} must not reuse imported ids");
    s.publish("main", &e.model, Some(w.seq), "human").unwrap();
    let (_, back) = s.load("main").unwrap();
    assert!(back.parts.contains_key(&id));
    // Another process reserving ids never gets this one.
    let mut other = Store::open(&p).unwrap();
    let (a, b) = other.reserve_ids(10).unwrap();
    assert!(a > id && b == a + 10);
}

#[test]
fn compare_and_swap_detects_conflicts() {
    let p = tmp("cas.sb");
    let mut a = Store::create(&p, "cas").unwrap();
    let mut b = Store::open(&p).unwrap();
    let (w, m) = a.load("main").unwrap();
    let mut m1 = m.clone();
    m1.doc.files[0].lines.push(ldraw::Line::new("0 // a", ldraw::Eol::Lf));
    let m1 = Model::from_document(m1.doc, None);
    a.publish("main", &m1, Some(w.seq), "human").unwrap();
    let err = b.publish("main", &m, Some(w.seq), "human").unwrap_err();
    assert!(err.contains("conflict"), "{err}");
}

#[test]
fn writes_succeed_while_another_connection_reads() {
    let p = tmp("busy.sb");
    let mut a = Store::create(&p, "busy").unwrap();
    let b = Store::open(&p).unwrap();
    // Hold a read transaction open on b.
    b.connection().execute_batch("BEGIN; SELECT * FROM working;").unwrap();
    a.set_pref("project", "colour", &json!(4)).unwrap();
    b.connection().execute_batch("COMMIT").unwrap();
    assert_eq!(b.pref("project", "colour").unwrap(), Some(json!(4)));
}

#[test]
fn concurrent_writers_retry_instead_of_failing() {
    let p = tmp("retry.sb");
    Store::create(&p, "retry").unwrap();
    let threads: Vec<_> = (0..4)
        .map(|k| {
            let p = p.clone();
            std::thread::spawn(move || {
                let mut s = Store::open(&p).unwrap();
                for i in 0..25 {
                    s.set_pref(&format!("user:t{k}"), &format!("k{i}"), &json!(i)).unwrap();
                    s.reserve_ids(16).unwrap();
                }
            })
        })
        .collect();
    for t in threads {
        t.join().unwrap();
    }
    let s = Store::open(&p).unwrap();
    for k in 0..4 {
        assert_eq!(s.prefs(&format!("user:t{k}")).unwrap().len(), 25);
    }
}

#[test]
fn commits_dedupe_and_gc_drops_unreferenced_blobs() {
    let p = tmp("gc.sb");
    let mut s = Store::create(&p, "gc").unwrap();
    let c0 = s.log("main", 10).unwrap()[0].id;
    assert_eq!(s.commit("main", "save", "human", "again", &[]).unwrap(), c0);
    let (_, m) = s.load("main").unwrap();
    let mut doc = m.doc.clone();
    doc.files[0].lines.push(ldraw::Line::new("0 // scratch", ldraw::Eol::Lf));
    s.publish("main", &Model::from_document(doc, None), None, "human").unwrap();
    // Put main back: the scratch tree is now unreferenced.
    s.publish("main", &m, None, "human").unwrap();
    let blobs = |s: &Store| -> i64 {
        s.connection().query_row("SELECT COUNT(*) FROM blobs", [], |r| r.get(0)).unwrap()
    };
    let before = blobs(&s);
    s.maintain("main").unwrap();
    assert!(blobs(&s) < before);
    assert_eq!(s.export_ldr("main").unwrap(), m.doc.to_bytes());
}

#[test]
fn branches_and_restore() {
    let p = tmp("br.sb");
    let mut s = Store::import(&p, Document::parse(MPD.as_bytes(), "main.ldr"), "human").unwrap();
    let first = s.log("main", 1).unwrap()[0].id;
    s.create_branch("variant", None, "human").unwrap();
    let (_, m) = s.load("variant").unwrap();
    assert_eq!(m.parts.len(), 4);
    let mut doc = m.doc.clone();
    doc.files[0].lines.truncate(2);
    s.publish("main", &Model::from_document(doc, None), None, "human").unwrap();
    s.commit("main", "save", "human", "cut", &[]).unwrap();
    // main.ldr lost its two lines; sub.ldr keeps its two parts.
    assert_eq!(s.load("main").unwrap().1.parts.len(), 2);
    s.restore("main", first, "human").unwrap();
    assert_eq!(s.load("main").unwrap().1.parts.len(), 4);
    assert!(s.branches().unwrap().iter().any(|b| b.name == "variant"));
}

#[test]
fn leases_block_live_holders_and_allow_takeover_when_stale() {
    let p = tmp("lease.sb");
    let mut a = Store::create(&p, "lease").unwrap();
    let mut b = Store::open(&p).unwrap();
    assert!(a.acquire_lease("main", "gui-a").unwrap());
    assert!(!b.acquire_lease("main", "gui-b").unwrap());
    assert!(b.leased_by_other("main", "gui-b").unwrap());
    // Age the lease past the stale limit.
    a.connection()
        .execute("UPDATE leases SET renewed=renewed-60", [])
        .unwrap();
    assert!(b.acquire_lease("main", "gui-b").unwrap());
    assert_eq!(a.lease("main").unwrap().unwrap().holder, "gui-b");
}

#[test]
fn selections_candidates_prefs_assets_and_changes() {
    let p = tmp("rec.sb");
    let mut s = Store::create(&p, "rec").unwrap();
    let seq0 = s.last_change().unwrap();
    let sel = s.create_selection("claude", Some("belt"), 0, &[3, 4, 5], None).unwrap();
    assert_eq!(s.selection("claude", "belt").unwrap().parts, vec![3, 4, 5]);
    assert_eq!(s.selection("other", &sel.id).unwrap().id, sel.id);
    let scratch = s.create_selection("claude", None, 0, &[1], Some(&json!({"box":[0,0,0,1,1,1]}))).unwrap();
    assert!(scratch.query.is_some());
    let tree = s.working("main").unwrap().tree;
    let c = s.create_candidates("claude", &json!({"poses":[1,2]}), &tree).unwrap();
    assert_eq!(s.candidates(&c).unwrap().json["poses"][1], 2);
    s.set_pref("user:human", "colour", &json!(1)).unwrap();
    assert_eq!(s.pref("user:human", "colour").unwrap(), Some(json!(1)));
    let png = [137u8, 80, 78, 71, 1, 2, 3];
    let a = s.put_asset("render", "front.png", "image/png", &png, Some(&tree), &json!({})).unwrap();
    assert_eq!(s.asset(&a).unwrap().1, png);
    assert_eq!(s.asset("front.png").unwrap().0.id, a);
    let kinds: Vec<_> = s.changes_since(seq0).unwrap().into_iter().map(|c| c.kind).collect();
    assert!(kinds.contains(&"selection".to_string()) && kinds.contains(&"asset".to_string()));
}

#[test]
fn drafts_persist_and_reload() {
    let p = tmp("drafts.sb");
    let mut s = Store::import(&p, Document::parse(MPD.as_bytes(), "main.ldr"), "human").unwrap();
    let (_, model) = s.load("main").unwrap();
    let lib = lib();
    let mut e = engine::Engine::from_model(lib.clone(), model.clone());
    e.drafts.next = s.reserve_draft_ids(1).unwrap();
    e.observe("claude");
    let id = *e.model.parts.keys().next().unwrap();
    let v = e
        .propose("claude".into(), 1, engine::Cmd::Paint { ids: vec![id], color: 2 })
        .unwrap();
    assert_eq!(v["state"], "pending");
    s.save_drafts("main", &e.drafts.ops).unwrap();
    s.save_observations("claude", "main", &e.drafts.seen["claude"]).unwrap();

    let s2 = Store::open(&p).unwrap();
    let mut e2 = engine::Engine::from_model(lib, s2.load("main").unwrap().1);
    e2.load_drafts(s2.load_drafts("main", true).unwrap());
    e2.drafts.seen.insert("claude".into(), s2.load_observations("claude", "main").unwrap());
    let eff = e2.effective_model("claude");
    assert_eq!(eff.parts[&id].color, 2);
    assert_eq!(e2.model.parts[&id].color, model.parts[&id].color);
}
