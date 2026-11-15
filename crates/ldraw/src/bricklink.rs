//! BrickLink catalogue IDs for LDraw colours. BrickLink's own LDraw importer maps some parts to
//! numbers it has since retired (LDraw 3062 → "3062b"), so supplier exports write BrickLink IDs
//! directly; part numbers come from `Library::bricklink_number`.
use std::collections::HashMap;
use std::sync::LazyLock;

static COLOURS: LazyLock<HashMap<i32, u32>> = LazyLock::new(|| {
    include_str!("../data/bricklink_colours.tsv")
        .lines()
        .filter(|l| !l.starts_with('#'))
        .filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?.parse().ok()?, f.next()?.parse().ok()?))
        })
        .collect()
});

/// BrickLink colour ID for an LDraw colour code, if it has one.
pub fn colour(ldraw: i32) -> Option<u32> {
    COLOURS.get(&ldraw).copied()
}

#[cfg(test)]
mod tests {
    #[test]
    fn common_colours_map_to_bricklink_ids() {
        for (ldraw, bl) in [(0, 11), (1, 7), (4, 5), (15, 1), (19, 2), (71, 86), (72, 85), (320, 59), (23, 109), (89, 97)] {
            assert_eq!(super::colour(ldraw), Some(bl), "LDraw {ldraw}");
        }
        assert_eq!(super::colour(16), None);
    }
}
