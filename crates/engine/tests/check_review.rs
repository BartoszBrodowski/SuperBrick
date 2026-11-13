use engine::{Model, validation::Validator};
use ldraw::{Document, Library};

#[test]
fn full_check_finds_unseated_stud_even_without_native_collision() {
    let lib = Library::open_default().unwrap();
    let model=Model::from_document(Document::parse(b"0 Unseated stud\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n1 1 0 -25 0 1 0 0 0 1 0 0 0 1 3005.dat\n","gap.ldr"),None);
    let original = model.doc.to_bytes();
    let mut report = Validator::default().check(&lib, &model.flat(0));
    assert_eq!(report.errors, 0);
    report.review_collisions(&model, 0).unwrap();
    assert_eq!(report.errors, 1);
    assert_eq!(report.verdict, "ILLEGAL");
    let pair = report.reviewed.iter().find(|p| p.collision).unwrap();
    assert_eq!(pair.overlap, 0.);
    assert!(pair.reasons.iter().any(|s| s.contains("not pressed home")));
    assert!(!pair.points.is_empty());
    assert_eq!(model.doc.to_bytes(), original);
}

#[test]
fn bar_keepouts_clear_native_false_positive_but_real_collision_remains() {
    let lib = Library::open_default().unwrap();
    let model=Model::from_document(Document::parse(b"0 Bar in cone\n1 6 0 0 0 1 0 0 0 1 0 0 0 1 4599a.dat\n1 6 0 4 10 1 0 0 0 0 -1 0 1 0 4589.dat\n","bar.ldr"),None);
    let mut report = Validator::default().check(&lib, &model.flat(0));
    assert!(
        report.errors > 0,
        "fixture must exercise native false positive"
    );
    let original = model.doc.to_bytes();
    report.review_collisions(&model, 0).unwrap();
    assert_eq!(report.errors, 0);
    assert_eq!(report.verdict, "UNVERIFIED");
    assert!(
        report
            .reviewed
            .iter()
            .any(|p| !p.collision && p.types.contains(&"bar".into()))
    );
    assert_eq!(model.doc.to_bytes(), original);
    let model=Model::from_document(Document::parse(b"0 Overlap\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n1 1 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n","bad.ldr"),None);
    let mut report = Validator::default().check(&lib, &model.flat(0));
    report.review_collisions(&model, 0).unwrap();
    assert!(report.errors > 0);
    assert_eq!(report.verdict, "ILLEGAL");
}
