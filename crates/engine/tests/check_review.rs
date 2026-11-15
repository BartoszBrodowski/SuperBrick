//! The checker is the bench validator running in SuperBrick's Python service.
use engine::{Model, checker};
use ldraw::Document;

fn model(text: &[u8]) -> Model {
    Model::from_document(Document::parse(text, "t.ldr"), None)
}

#[test]
fn unseated_stud_is_a_conflict() {
    let m = model(b"0 Unseated stud\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n1 1 0 -25 0 1 0 0 0 1 0 0 0 1 3005.dat\n");
    let original = m.doc.to_bytes();
    let report = checker::check(&m, 0).unwrap();
    assert_eq!(report.verdict, "ILLEGAL");
    let hit = report.rows.iter().flat_map(|r| &r.hits).find(|h| h.kind == "conflict").unwrap();
    assert!(hit.reason.contains("not pressed home"), "{}", hit.reason);
    assert!(hit.at.is_some());
    assert_eq!(m.doc.to_bytes(), original);
}

#[test]
fn bar_in_clip_cone_is_a_link_but_solid_overlap_collides() {
    let m = model(b"0 Bar in cone\n1 6 0 0 0 1 0 0 0 1 0 0 0 1 4599a.dat\n1 6 0 4 10 1 0 0 0 0 -1 0 1 0 4589.dat\n");
    let report = checker::check(&m, 0).unwrap();
    assert_eq!(report.count("COLLISION"), 0, "{}", report.text);
    assert!(report.link_types.contains_key("bar"), "{}", report.text);
    let m = model(b"0 Overlap\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n1 1 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n");
    let report = checker::check(&m, 0).unwrap();
    assert_eq!(report.verdict, "ILLEGAL");
    assert_eq!(report.count("COLLISION"), 1);
    let hit = report.rows[1].hits.first().unwrap();
    assert!(hit.overlap > 0. && hit.bounds.is_some());
    assert_eq!(report.rows[1].id, Some(2));
}

#[test]
fn stacked_bricks_are_verified() {
    let m = model(
        b"0 Steps\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3001.dat\n0 STEP\n1 1 0 -24 0 1 0 0 0 1 0 0 0 1 3001.dat\n0 STEP\n",
    );
    let report = checker::check(&m, 0).unwrap();
    assert_eq!(report.verdict, "VERIFIED", "{}", report.text);
    assert!(report.rows.iter().all(|r| r.is_ok()));
    assert_eq!(report.link_types.get("stud"), Some(&8)); // eight studs
}

#[test]
fn pieces_connected_to_nothing_float() {
    let m = model(
        b"0 test\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n1 4 0 -24 0 1 0 0 0 1 0 0 0 1 3003.dat\n1 4 400 -200 0 1 0 0 0 1 0 0 0 1 3003.dat\n",
    );
    let report = checker::check(&m, 0).unwrap();
    let floating: Vec<_> = report.rows.iter().filter(|r| r.has("FLOATING")).map(|r| r.uid).collect();
    assert_eq!(floating, vec![3], "{}", report.text);
}

#[test]
fn bench_asserts_are_honoured() {
    let text = b"1 4 0 0 0 1 0 0 0 1 0 0 0 1 3003.dat\n1 4 0 -200 0 1 0 0 0 1 0 0 0 1 3003.dat\n";
    let report = checker::check(&model(&[b"0 No assert\n".as_slice(), text].concat()), 0).unwrap();
    assert_eq!(report.count("FLOATING"), 2, "{}", report.text);
    let m = model(&[b"0 Assert\n0 !BENCH ASSERT 1 2 glued by the designer\n".as_slice(), text].concat());
    let report = checker::check(&m, 0).unwrap();
    assert_eq!(report.count("FLOATING"), 0, "{}", report.text);
}
