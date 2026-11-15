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
    let r = call(&mut human, "project", json!({"action": "accept", "ids": "all"}));
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
    assert!(call(&mut human, "project", json!({"action": "accept", "ids": [drafts[0]]})).ok);
    let r = call(&mut human, "project", json!({"action": "reject", "ids": [drafts[1]], "note": "too tall"}));
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
    // The checker records a collision on the later part's row only; verify lists it once,
    // later piece first (red), the piece it meets second (amber).
    let dir = std::env::temp_dir().join(format!("sb-ops-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let ldr = dir.join("overlap.ldr");
    std::fs::write(&ldr, "0 overlap\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3001.dat\n1 1 10 0 0 1 0 0 0 1 0 0 0 1 3001.dat\n").unwrap();
    let path = dir.join("overlap.sb");
    let _ = std::fs::remove_file(&path);
    create_project(&path, "overlap", Some(&ldr)).unwrap();
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    let r = call(&mut agent, "verify", json!({}));
    assert!(!r.ok, "{}", r.text);
    let pairs = r.data["collisions"].as_array().unwrap();
    assert_eq!(pairs.len(), 1, "{}", r.data);
    assert_eq!((pairs[0]["a"].as_u64(), pairs[0]["b"].as_u64()), (Some(2), Some(1)));
    assert_eq!(r.data["verdict"], "ILLEGAL");
    assert!(r.text.contains("RESULT: ILLEGAL"), "{}", r.text);
}

#[test]
fn verify_lists_what_chosen_parts_connect_to() {
    let path = project("connections");
    let mut s = Session::open(&path, "claude", lib()).unwrap();
    let r = call(&mut s, "edit", json!({"images": false, "ops": [brick([0.0, 0.0, 0.0]), brick([0.0, -24.0, 0.0])]}));
    assert!(r.ok, "{}", r.text);
    let r = call(&mut s, "verify", json!({"ids": [2]}));
    assert!(r.ok, "{}", r.text);
    assert!(r.text.contains("Connections:") && r.text.contains("#2 ") && r.text.contains("#1 (8 stud)"), "{}", r.text);
    let c = r.data["connections"].as_array().unwrap();
    assert_eq!(c.len(), 1, "{}", r.data);
    assert_eq!((c[0]["id"].as_u64(), c[0]["with"].as_u64(), c[0]["types"]["stud"].as_u64()), (Some(2), Some(1), Some(8)));
}

#[test]
fn steps_export_and_argument_checks() {
    let path = project("steps");
    let mut s = Session::open(&path, "claude", lib()).unwrap();
    // One edit per instruction step: the export has a STEP after each.
    let r = call(&mut s, "edit", json!({"label": "step 1", "step": true, "images": false, "ops": [brick([0.0, 0.0, 0.0])]}));
    assert!(r.ok, "{}", r.text);
    let r = call(&mut s, "edit", json!({"label": "step 2", "step": true, "images": false, "ops": [brick([0.0, -24.0, 0.0])]}));
    assert!(r.ok, "{}", r.text);
    let first = r.data["created"].as_array().map(|a| a[0].as_u64().unwrap());
    // A part added later can join step 1 (after the first brick), named colours work.
    let r = call(&mut s, "edit", json!({"images": false, "ops": [{"op": "add", "part": "3001", "color": "light bluish gray", "at": [80, 0, 0], "after": 1}]}));
    assert!(r.ok, "{}", r.text);
    // A removed part leaves no placeholder in the export.
    let r = call(&mut s, "edit", json!({"images": false, "ops": [{"op": "remove", "ids": first.map(|i| vec![i]).unwrap_or(vec![2])}]}));
    assert!(r.ok, "{}", r.text);
    let out = path.with_extension("ldr");
    let r = call(&mut s, "project", json!({"action": "export", "path": out}));
    assert!(r.ok, "{}", r.text);
    let text = std::fs::read_to_string(&out).unwrap();
    assert!(!text.contains("// removed"), "{text}");
    let lines: Vec<&str> = text.lines().map(str::trim).filter(|l| l.starts_with("1 ") || *l == "0 STEP").collect();
    assert_eq!(lines.len(), 3 + 2 - 1, "{text}");
    assert!(lines[0].starts_with("1 4 ") && lines[1].starts_with("1 71 ") && lines[2] == "0 STEP", "{text}");
    // Rotation as a string of 9 numbers; unknown arguments and non-boolean flags are refused.
    let r = call(&mut s, "edit", json!({"dry_run": true, "images": false, "ops": [{"op": "add", "part": "3001", "at": [200, 0, 0], "rot": "0 0 1 0 1 0 -1 0 0"}]}));
    assert!(r.ok, "{}", r.text);
    assert!(!call(&mut s, "look", json!({"isolate": [5, 6]})).ok);
    assert!(!call(&mut s, "look", json!({"zoom": 2})).ok);
    // Withdraw without ids is refused rather than withdrawing everything.
    let r = call(&mut s, "project", json!({"action": "withdraw"}));
    assert!(!r.ok && r.text.contains("ids"), "{}", r.text);
}

#[test]
fn element_ids_sets_and_inventory() {
    if search::sets::Sets::get().is_none() {
        eprintln!("skipped: no out/sets.bin in the search data");
        return;
    }
    let path = project("inventory");
    let mut s = Session::open(&path, "claude", lib()).unwrap();
    // 300121 is the classic red 2x4 brick; 4211398 a light bluish grey 1x2 plate.
    let r = call(&mut s, "search", json!({"elements": ["300121", "4211398", "999999"], "picture": false}));
    assert!(r.ok);
    let found = r.data["elements"].as_array().unwrap();
    assert_eq!(found.len(), 2);
    assert_eq!(found[0]["part"], "3001");
    assert_eq!(found[0]["color"], 4);
    assert_eq!(found[1]["part"], "3023b");
    assert_eq!(found[1]["color"], 71);
    assert_eq!(r.data["unknown"], json!(["999999"]));
    // An element number inside a word query puts that part first.
    let r = call(&mut s, "search", json!({"query": "4211398", "picture": false}));
    assert!(r.text.contains("element 4211398 = 3023b"), "{}", r.text);

    let r = call(&mut s, "search", json!({"set": "40809", "picture": false}));
    assert!(r.ok && r.text.contains("Festive Gingerbread House"), "{}", r.text);
    let parts = r.data["booklet_inventory"]["parts"].as_array().unwrap();
    assert!(parts.len() > 100);

    // The old number 3023 counts as 3023b; colours can be names.
    assert!(call(&mut s, "edit", json!({"ops": [
        {"op": "add", "part": "3023", "color": "bright pink", "at": [0, 0, 0]},
        {"op": "add", "part": "3001", "color": 4, "at": [100, 0, 0]}], "images": false})).ok);
    let r = call(&mut s, "project", json!({"action": "inventory", "against": [
        {"part": "3023b", "color": 29, "qty": 1}, {"part": "3024", "color": 15, "qty": 2}]}));
    assert!(r.ok);
    assert_eq!(r.data["matched"], 1);
    assert_eq!(r.data["missing"], json!([{"part": "3024", "color": 15, "qty": 2}]));
    assert_eq!(r.data["extra"], json!([{"part": "3001", "color": 4, "qty": 1}]));
    let r = call(&mut s, "project", json!({"action": "inventory", "against": "40809"}));
    assert!(r.ok && r.text.contains("set 40809-1"), "{}", r.text);
}

#[test]
fn inventory_of_some_steps() {
    let path = project("inventory-steps");
    let mut s = Session::open(&path, "claude", lib()).unwrap();
    assert!(call(&mut s, "edit", json!({"images": false, "step": true, "ops": [brick([0.0, 0.0, 0.0])]})).ok);
    assert!(call(&mut s, "edit", json!({"images": false, "step": true, "ops": [
        {"op": "add", "part": "3023", "color": 15, "at": [100, 0, 0]},
        {"op": "add", "part": "3023", "color": 15, "at": [100, 0, 40]}]})).ok);
    let r = call(&mut s, "project", json!({"action": "inventory", "steps": "2"}));
    assert!(r.ok, "{}", r.text);
    assert!(r.text.starts_with("2 pieces in 1 kinds in 1 step(s)") && r.text.contains("Plate 1 x 2"), "{}", r.text);
    let r = call(&mut s, "project", json!({"action": "inventory", "steps": [1, 2]}));
    assert!(r.text.starts_with("3 pieces in 2 kinds"), "{}", r.text);
    assert!(!call(&mut s, "project", json!({"action": "inventory", "steps": "3-1"})).ok);
}

#[test]
fn callout_and_booklet_compare() {
    let path = project("callout");
    let mut s = Session::open(&path, "claude", lib()).unwrap();
    // The parts a step adds must match the booklet's callout; a miscount changes nothing.
    let r = call(&mut s, "edit", json!({"images": false, "callout": [{"part": "3001", "color": 4, "qty": 2}], "ops": [brick([0.0, 0.0, 0.0])]}));
    assert!(!r.ok && r.text.contains("missing 1× 3001"), "{}", r.text);
    let r = call(&mut s, "edit", json!({"images": false, "callout": [{"part": "3001", "color": "red", "qty": 1}], "ops": [brick([0.0, 0.0, 0.0])]}));
    assert!(r.ok, "{}", r.text);
    // compare: a page image beside the model, one picture even with images:false.
    let page = path.with_extension("png");
    let img = render::RgbaImage { width: 200, height: 100, pixels: vec![255; 200 * 100 * 4] };
    std::fs::write(&page, render::encode_png_fast(&img)).unwrap();
    let r = call(&mut s, "edit", json!({"images": false, "compare": {"page": page, "box": [0.0, 0.0, 1.0, 1.0]}, "ops": [brick([0.0, -24.0, 0.0])]}));
    assert!(r.ok, "{}", r.text);
    assert_eq!(r.images.len(), 1);
    assert!(r.images[0].width > r.images[0].height);
    // A missing page is refused before anything is proposed.
    let before = call(&mut s, "project", json!({"action": "drafts"})).text;
    let r = call(&mut s, "edit", json!({"compare": {"page": "/no/such/page.png"}, "ops": [brick([0.0, -48.0, 0.0])]}));
    assert!(!r.ok);
    assert_eq!(call(&mut s, "project", json!({"action": "drafts"})).text, before);
}

#[test]
fn batch_last_accumulates_and_subbuild_callout_counts_moved_parts() {
    let path = project("subbuild");
    let mut s = Session::open(&path, "claude", lib()).unwrap();
    assert!(call(&mut s, "edit", json!({"images": false, "ops": [brick([0.0, 0.0, 0.0])]})).ok);
    // Built off to the side: "last" inside the batch covers both plates, so the group holds both.
    let r = call(&mut s, "edit", json!({"images": false, "ops": [
        {"op": "add", "part": "3024", "color": "yellow", "at": [400, 0, 0]},
        {"op": "add", "part": "3024", "color": "yellow", "at": [400, -8, 0]},
        {"op": "group", "ids": "last", "name": "pair"}]}));
    assert!(r.ok);
    let r = call(&mut s, "where", json!({"ids": "pair", "near": [0, -24, 0], "limit": 2}));
    assert!(r.ok, "{}", r.text);
    assert!(r.text.contains("2 parts"), "{}", r.text);
    let set = r.data["set"].as_str().unwrap().to_string();
    // Attaching the sub-build carries its callout: a miscount is refused, the right count passes.
    let wrong = call(&mut s, "edit", json!({"images": false, "callout": [{"part": "3024", "color": "yellow", "qty": 1}],
        "ops": [{"op": "place", "from": set, "pick": 1}]}));
    assert!(!wrong.ok);
    let right = call(&mut s, "edit", json!({"images": false, "callout": [{"part": "3024", "color": "yellow", "qty": 2}],
        "ops": [{"op": "place", "from": set, "pick": 1}]}));
    assert!(right.ok, "{}", right.text);
}

#[test]
fn pattern_wall_with_running_bond_colours_and_a_door() {
    let path = project("pattern");
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    // A 1x4 wall, 4 bricks long and 4 high, running bond, a door gap, two colours by row.
    let wall = json!({"op": "pattern", "part": "3010", "color": 4, "at": [0, 0, 0],
        "along": [{"by": [80, 0, 0], "count": 4}, {"by": [0, -24, 0], "count": 4}],
        "bond": {"offset": [40, 0, 0]}, "colors": [4, 15], "colors_by": "row",
        "skip": [{"from": [70, 24, -10], "to": [170, -24, 10]}]});
    let r = call(&mut agent, "edit", json!({"ops": [wall], "images": false}));
    assert!(r.ok, "{}", r.text);
    let view = agent.view();
    let flat = view.flat(0);
    // 16 bricks; the door box (the bottom two rows, x 70..170) removes 2 + 3 = 5 of them.
    assert_eq!(flat.len(), 11, "{}", r.text);
    assert!(r.text.contains("5 skipped"), "{}", r.text);
    for p in &flat {
        let row = (-p.pos.y / 24.0).round() as i32;
        let x0 = if row % 2 == 1 { 40.0 } else { 0.0 };
        assert_eq!(((p.pos.x - x0) / 80.0).fract(), 0.0, "bond offset on odd rows: {:?}", p.pos);
        assert_eq!(p.color.0, if row % 2 == 0 { 4 } else { 15 }, "colour by row");
    }
}

#[test]
fn pattern_cells_grid_units_and_seeded_colours() {
    let path = project("pattern-grid");
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    // A two-part cell (1x2 brick + 1x2 plate on top) repeated 3 x 2 in grid units.
    let op = json!({"op": "pattern", "at": [0, 0, 0],
        "parts": [{"part": "3004", "at": [0, 0, 0]}, {"part": "3023", "at": [0, 3, 0]}],
        "along": [{"by": [2, 0, 0], "count": 3}, {"by": [0, 0, 1], "count": 2}],
        "colors": ["red", "blue", "yellow"], "seed": 7});
    let r = call(&mut agent, "edit", json!({"ops": [op], "units": "grid", "images": false}));
    assert!(r.ok, "{}", r.text);
    let flat = agent.view().flat(0);
    assert_eq!(flat.len(), 12);
    let xs: std::collections::BTreeSet<i64> = flat.iter().map(|p| p.pos.x.round() as i64).collect();
    assert_eq!(xs.into_iter().collect::<Vec<_>>(), vec![0, 40, 80], "grid steps of 2 studs");
    assert!(flat.iter().any(|p| p.name.starts_with("3023") && (p.pos.y + 24.0).abs() < 1e-6), "plates on the bricks");
    // The same seed picks the same colours again.
    let colours: Vec<i32> = flat.iter().map(|p| p.color.0).collect();
    let path2 = project("pattern-grid2");
    let mut again = Session::open(&path2, "claude", lib()).unwrap();
    let r = call(&mut again, "edit", json!({"ops": [op], "units": "grid", "images": false}));
    assert!(r.ok);
    assert_eq!(again.view().flat(0).iter().map(|p| p.color.0).collect::<Vec<_>>(), colours);
    assert!(colours.iter().any(|&c| c != colours[0]), "random colours vary: {colours:?}");
}

#[test]
fn fill_box_in_running_bond_with_ends_and_one_check_per_batch() {
    let path = project("fill");
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    // 8 studs long, 1 deep, 3 bricks high: 1x4 bricks, odd layers shifted by 2 studs and closed with 1x2s.
    let op = json!({"op": "fill", "part": "3010", "color": 1, "from": [0, 0, -10], "to": [160, -72, 10],
        "bond": true, "ends": "3004"});
    let r = call(&mut agent, "edit", json!({"ops": [op], "images": false}));
    assert!(r.ok, "{}", r.text);
    let flat = agent.view().flat(0);
    let long = flat.iter().filter(|p| p.name.starts_with("3010")).count();
    let ends = flat.iter().filter(|p| p.name.starts_with("3004")).count();
    assert_eq!((long, ends), (2 + 1 + 2, 2), "{}", r.text);
    // A batch is checked once: a brick in the way and the op moving it away pass together...
    let blocker = json!({"op": "add", "part": "3001", "at": [300, 0, 0]});
    let r = call(&mut agent, "edit", json!({"ops": [blocker], "images": false}));
    assert!(r.ok, "{}", r.text);
    let id = agent.last.first().copied().unwrap();
    let r = call(&mut agent, "edit", json!({"ops": [
        {"op": "add", "part": "3001", "at": [300, 0, 0]},
        {"op": "move", "ids": [id], "by": [0, 0, 100]}], "images": false}));
    assert!(r.ok, "a clash undone later in the batch is fine: {}", r.text);
    // ...and a clash that stays names the op it comes from.
    let r = call(&mut agent, "edit", json!({"ops": [
        {"op": "add", "part": "3003", "at": [-200, 0, 0]},
        {"op": "pattern", "part": "3001", "at": [0, 0, 0], "along": [{"by": [80, 0, 0], "count": 2}]}],
        "images": false}));
    assert!(!r.ok);
    assert!(r.text.contains("op 2 (pattern)"), "{}", r.text);
}

#[test]
fn corner_anchors_ops_files_and_dropped_clashes() {
    let path = project("corner");
    let mut agent = Session::open(&path, "claude", lib()).unwrap();
    // Corner (0,0,0) in grid units: a 2x4 brick sitting on y=0, spanning x 0..4 and z 0..2 studs.
    let ops = json!([{"op": "add", "part": "3001", "at": [0, 0, 0], "anchor": "corner"},
                     {"op": "add", "parts": [{"part": "3001", "at": [0, 3, 0], "rot": "yaw90", "anchor": "corner"}]}]);
    let file = path.with_extension("ops.json");
    std::fs::write(&file, serde_json::to_string(&json!({"ops": ops, "units": "grid"})).unwrap()).unwrap();
    let r = call(&mut agent, "edit", json!({"ops_file": file.to_str().unwrap(), "images": false}));
    assert!(r.ok, "{}", r.text);
    let flat = agent.view().flat(0);
    let at = |i: usize| flat[i].pos.to_array().map(|v| v.round() as i64);
    assert_eq!(at(0), [40, -24, 20], "origin = corner + half the footprint, top of the brick");
    assert_eq!(at(1), [20, -48, 40], "turned: 2 studs along x, 4 along z, one brick up");
    // drop_clashes leaves out the brick that would overlap the first one, keeps the free one.
    let r = call(&mut agent, "edit", json!({"units": "grid", "images": false, "ops": [{"op": "add", "drop_clashes": true,
        "parts": [{"part": "3001", "at": [1, 0, 0], "anchor": "corner"}, {"part": "3001", "at": [10, 0, 0], "anchor": "corner"}]}]}));
    assert!(r.ok, "{}", r.text);
    assert_eq!(agent.view().flat(0).len(), 3);
    assert!(r.text.contains("1 of 2 dropped"), "{}", r.text);
}
