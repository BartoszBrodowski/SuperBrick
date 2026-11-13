//! Catalogue observations are purchasing warnings, never geometric errors.
use serde::Serialize;
use std::{collections::BTreeMap, sync::OnceLock};

#[derive(Clone, Debug, Serialize)]
pub struct ColourWarning {
    pub part: String,
    pub color: i32,
    pub pieces: Vec<usize>,
    pub message: String,
}
pub fn assess(name: &str, color: i32, colors: Option<&[i32]>) -> Option<String> {
    match colors {
        Some(c) if !c.is_empty() && c.contains(&color) => None,
        Some(c) if !c.is_empty() => Some(format!(
            "{name}: colour {color} is not listed as manufactured in the available catalogue. Paint is allowed; verify with a supplier."
        )),
        _ => Some(format!(
            "{name}: availability in colour {color} is unknown (no part-colour data)."
        )),
    }
}
fn catalogue() -> &'static BTreeMap<String, Vec<i32>> {
    static DATA: OnceLock<BTreeMap<String, Vec<i32>>> = OnceLock::new();
    DATA.get_or_init(|| {
        std::fs::read(search::data_dir().join("out/catalog.json"))
            .ok()
            .and_then(|b| serde_json::from_slice::<search::catalog::Catalog>(&b).ok())
            .map(|c| {
                c.parts
                    .into_iter()
                    .map(|p| (p.id.to_ascii_lowercase(), p.colors))
                    .collect()
            })
            .unwrap_or_default()
    })
}
pub fn warning(name: &str, color: i32) -> Option<String> {
    let id = name
        .to_ascii_lowercase()
        .trim_end_matches(".dat")
        .to_string();
    assess(name, color, catalogue().get(&id).map(Vec::as_slice))
}
pub fn check(parts: &[ldraw::FlatPart]) -> Vec<ColourWarning> {
    let mut lots: BTreeMap<(String, i32), Vec<usize>> = BTreeMap::new();
    for (i, p) in parts.iter().enumerate() {
        lots.entry((p.name.clone(), p.color.0))
            .or_default()
            .push(i + 1);
    }
    lots.into_iter()
        .filter_map(|((part, color), pieces)| {
            warning(&part, color).map(|message| ColourWarning {
                part,
                color,
                pieces,
                message,
            })
        })
        .collect()
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn available_unlisted_and_unknown_are_distinct() {
        assert!(assess("brick", 4, Some(&[4, 1])).is_none());
        assert!(
            assess("brick", 7, Some(&[4, 1]))
                .unwrap()
                .contains("not listed")
        );
        assert!(assess("brick", 7, Some(&[])).unwrap().contains("unknown"));
        assert!(assess("brick", 7, None).unwrap().contains("unknown"));
    }
}
