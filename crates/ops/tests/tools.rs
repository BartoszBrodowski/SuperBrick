//! End-to-end tests of the agent tools on real `.sb` projects.

use ldraw::Library;
use ops::{create_project, Session};
use serde_json::{json, Value};
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

fn lib() -> Arc<Library> {
    static LIB: OnceLock<Arc<Library>> = OnceLock::new();
    LIB.get_or_init(|| Arc::new(Library::open_default().expect("LDraw library")))
        .clone()
}

fn project(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("sb-ops-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join(format!("{name}.sb"));
    let _ = std::fs::remove_file(&path);
    for ext in ["sb-wal", "sb-shm"] {
        let _ = std::fs::remove_file(path.with_extension(ext));
    }
    create_project(&path, name, None).unwrap();
    path
}

fn call(s: &mut Session, tool: &str, args: Value) -> ops::Reply {
    let r = s.call(tool, &args);
    eprintln!("{tool} {args} -> ok={} {}", r.ok, r.text);
    r
}

fn brick(at: [f64; 3]) -> Value {
    json!({"op": "add", "part": "3001", "color": 4, "at": at})
}

#[test]
fn human_edits_directly_and_agent_proposes() {
    let path = project("propose");
    let mut human = Session::open(&path, "human", lib()).unwrap();
    let r = call(&mut human, "edit", json!({"ops": [brick([0.0, 0.0, 0.0])], "images": false}));
    assert!(r.ok, "{}", r.text);
    assert_eq!(human.engine.model.parts.len(), 1);

    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    assert_eq!(agent.engine.model.parts.len(), 1, "agent sees the human's edit");
    let r = call(&mut agent, "edit", json!({"ops": [brick([0.0, -24.0, 0.0])], "label": "second", "images": false}));
    assert!(r.ok, "{}", r.text);
    assert_eq!(agent.engine.model.parts.len(), 1, "a proposal does not change the shared model");
    assert_eq!(agent.view().parts.len(), 2, "the agent's own view includes its draft");

    // A second process for the same user sees the draft (persistence across restarts).
    let mut again = Session::open(&path, "claude", lib()).unwrap();
    assert_eq!(again.engine.drafts.pending("claude").count(), 1);
    assert_eq!(again.view().parts.len(), 2);

    // The human accepts it.
    let r = call(&mut human, "project", json!({"action": "accept", "user": "claude"}));
    assert!(r.ok, "{}", r.text);
    assert_eq!(human.engine.model.parts.len(), 2);
    let r = call(&mut again, "project", json!({"action": "info"}));
    assert!(r.ok);
    assert_eq!(again.engine.model.parts.len(), 2, "accepted work reaches other sessions");
    assert_eq!(again.engine.drafts.pending("claude").count(), 0);
}

#[test]
fn agents_cannot_accept() {
    let path = project("noaccept");
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    call(&mut agent, "edit", json!({"ops": [brick([0.0, 0.0, 0.0])], "images": false}));
    let r = call(&mut agent, "project", json!({"action": "accept"}));
    assert!(!r.ok);
}

#[test]
fn batch_dry_run_and_atomic_failure() {
    let path = project("batch");
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    let r = call(
        &mut agent,
        "edit",
        json!({"ops": [brick([0.0, 0.0, 0.0]), brick([0.0, -24.0, 0.0]), brick([0.0, -48.0, 0.0])], "dry_run": true}),
    );
    assert!(r.ok, "{}", r.text);
    assert_eq!(agent.engine.drafts.ops.len(), 0, "dry run proposes nothing");
    assert!(!r.images.is_empty(), "dry run still shows the result");

    // The third brick overlaps the first: the whole batch fails, with a picture.
    let r = call(
        &mut agent,
        "edit",
        json!({"ops": [brick([0.0, 0.0, 0.0]), brick([0.0, -24.0, 0.0]), brick([10.0, 0.0, 0.0])]}),
    );
    assert!(!r.ok);
    assert!(r.text.contains("collides"), "{}", r.text);
    assert!(!r.images.is_empty(), "a failure shows where it collided");
    assert_eq!(agent.engine.drafts.ops.len(), 0, "nothing of a failed batch is kept");
    assert_eq!(agent.view().parts.len(), 0);
}

#[test]
fn rotation_is_ldraw_row_order() {
    let path = project("rot");
    let mut human = Session::open(&path, "human", lib()).unwrap();
    // Row order: x' = z, z' = -x (a quarter turn about y).
    let r = call(
        &mut human,
        "edit",
        json!({"ops": [{"op": "add", "part": "3001", "color": 1, "at": [0, 0, 0], "rot": [0, 0, 1, 0, 1, 0, -1, 0, 0]}], "images": false}),
    );
    assert!(r.ok, "{}", r.text);
    let p = human.engine.model.parts.values().next().unwrap();
    let line = p.line();
    let text = line.to_text();
    assert!(text.contains("0 0 1 0 1 0 -1 0 0"), "{text}");
}

#[test]
fn where_select_look_verify() {
    let path = project("tools");
    let mut human = Session::open(&path, "human", lib()).unwrap();
    call(&mut human, "edit", json!({"ops": [{"op": "add", "part": "3003", "color": 4, "at": [0, 0, 0]}], "images": false}));

    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    let r = call(&mut agent, "where", json!({"part": "3005", "color": 1, "near": [0, -24, 0]}));
    assert!(r.ok, "{}", r.text);
    assert!(!r.images.is_empty());
    let set = r.data["set"].as_str().expect("candidate set id").to_string();
    let r = call(&mut agent, "edit", json!({"ops": [{"op": "place", "from": set, "pick": 1}]}));
    assert!(r.ok, "{}", r.text);
    assert_eq!(agent.view().parts.len(), 2);

    let r = call(&mut agent, "select", json!({"color": 1, "name": "blue bits"}));
    assert!(r.ok, "{}", r.text);
    let sel = r.data["selection"].as_str().unwrap().to_string();
    let r = call(&mut agent, "look", json!({"ids": sel, "views": 2}));
    assert!(r.ok, "{}", r.text);
    assert!(!r.images.is_empty());

    let r = call(&mut agent, "verify", json!({}));
    assert!(r.ok, "{}", r.text);

    // A floating brick is reported.
    let r = call(&mut agent, "edit", json!({"ops": [{"op": "add", "part": "3001", "color": 14, "at": [200, -200, 0]}]}));
    assert!(r.ok, "{}", r.text);
    assert!(r.text.to_lowercase().contains("float"), "{}", r.text);
}

#[test]
fn search_returns_thumbnails() {
    if !search::data_dir().join("parts.json").exists() && !search::data_dir().exists() {
        eprintln!("search data missing; skipped");
        return;
    }
    let path = project("search");
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    let r = call(&mut agent, "search", json!({"query": "curved slope 2x1", "limit": 6}));
    assert!(r.ok, "{}", r.text);
    assert_eq!(r.images.len(), 1);
}

#[test]
fn ldraw_lines_and_human_notes_reach_the_agent() {
    let path = project("notes");
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    // Two bricks from LDraw lines (row-order rotation; the second turned 90° about y).
    let lines = "0 a comment\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3001.dat\n1 1 100 0 0 0 0 1 0 1 0 -1 0 0 3001.dat\n";
    let r = call(&mut agent, "edit", json!({"ops": [{"op": "add", "ldraw": lines}], "label": "two", "images": false}));
    assert!(r.ok, "{}", r.text);
    let view = agent.view();
    assert_eq!(view.parts.len(), 2);
    let turned = view.parts.values().find(|p| p.color == 1).unwrap();
    assert_eq!(ops::args::rot_rows(turned.rot), [0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0, 0.0, 0.0]);
    let r = call(&mut agent, "edit", json!({"ops": [{"op": "add", "ldraw": "1 4 0 -24 0 1 0 0 0 1 0 0 0 1 3001.dat"}], "label": "top", "images": false}));
    assert!(r.ok, "{}", r.text);

    // The human accepts the first and rejects the second with a note.
    let mut human = Session::open(&path, "human", lib()).unwrap();
    let drafts: Vec<u64> = human.engine.drafts.pending("claude").map(|d| d.id).collect();
    assert_eq!(drafts.len(), 2);
    assert!(call(&mut human, "project", json!({"action": "accept", "user": "claude", "ids": [drafts[0]]})).ok);
    let r = call(&mut human, "project", json!({"action": "reject", "user": "claude", "ids": [drafts[1]], "note": "too tall"}));
    assert!(r.ok, "{}", r.text);

    // The agent hears both decisions once, on its next call.
    let r = call(&mut agent, "project", json!({"action": "info"}));
    assert!(r.text.contains("From the human"), "{}", r.text);
    assert!(r.text.contains("accepted") && r.text.contains("too tall"), "{}", r.text);
    assert_eq!(r.data["from_human"].as_array().unwrap().len(), 2);
    let r = call(&mut agent, "project", json!({"action": "info"}));
    assert!(!r.text.contains("From the human"), "told only once: {}", r.text);
}

#[test]
fn verify_reports_collisions_listed_on_one_row() {
    // The validator records a collision on the later part's row only.
    let dir = std::env::temp_dir().join(format!("sb-ops-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ldr = dir.join("overlap.ldr");
    std::fs::write(&ldr, "0 overlap\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3001.dat\n1 1 10 0 0 1 0 0 0 1 0 0 0 1 3001.dat\n").unwrap();
    let path = dir.join("overlap.sb");
    let _ = std::fs::remove_file(&path);
    create_project(&path, "overlap", Some(&ldr)).unwrap();
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    let r = call(&mut agent, "verify", json!({"images": false}));
    assert!(!r.ok, "{}", r.text);
    let pairs = r.data["collisions"].as_array().unwrap();
    assert_eq!(pairs.len(), 1, "{}", r.data);
    assert_eq!((pairs[0]["a"].as_u64(), pairs[0]["b"].as_u64()), (Some(1), Some(2)));
}
