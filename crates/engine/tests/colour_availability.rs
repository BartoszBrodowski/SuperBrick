use engine::{Cmd, Engine};
use std::sync::Arc;

#[test]
fn paint_warns_without_rejecting_and_checker_keeps_colour_separate_from_errors() {
    let lib = Arc::new(ldraw::Library::open_default().unwrap());
    let doc = ldraw::Document::parse(
        b"0 Colour warning test\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n",
        "colour.ldr",
    );
    let mut e = Engine::new(lib.clone(), doc, None);
    let original = e.model.doc.to_bytes();
    let result = e
        .execute(Cmd::Paint {
            ids: vec![1],
            color: 999999,
        })
        .unwrap();
    assert!(!result["warnings"].as_array().unwrap().is_empty());
    assert_eq!(e.model.parts[&1].color, 999999);
    let report = engine::checker::check(&e.model, 0).unwrap();
    assert_eq!(report.errors, 0);
    assert_eq!(report.colour_warnings.len(), 1);
    assert_eq!(report.colour_warnings[0].pieces, vec![1]);
    e.undo(false).unwrap();
    assert_eq!(e.model.doc.to_bytes(), original);
}
