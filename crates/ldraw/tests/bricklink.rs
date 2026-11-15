//! LDraw → BrickLink part numbers for supplier exports. Skips without an LDraw library.

use ldraw::Library;

#[test]
fn bricklink_numbers_follow_moves_and_header_keywords() {
    let lib = match Library::open_default() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("skip: no LDraw library ({e})");
            return;
        }
    };
    for (ldraw, bricklink) in [
        ("3023b.dat", "3023"),  // renamed in LDraw, BrickLink kept the old number
        ("3023.dat", "3023"),   // ~Moved to 3023b, then back to BrickLink's number
        ("6141.dat", "4073"),   // LDraw's 1x1 round plate is BrickLink 4073
        ("4073.dat", "4073"),   // ~Moved to 6141
        ("3001.dat", "3001"),   // no keyword: same number
    ] {
        assert_eq!(lib.bricklink_number(ldraw), bricklink, "{ldraw}");
    }
}
