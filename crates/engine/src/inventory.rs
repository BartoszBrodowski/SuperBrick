//! Purchasing inventory of the assembled base document, not source-file references.
use crate::Model;
use glam::{DMat3, DVec3};
use ldraw::{Color, PartLine};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Lot {
    pub part: String,
    pub color: i32,
    pub quantity: usize,
}
#[derive(Clone, Debug, Default)]
pub struct Inventory {
    pub lots: Vec<Lot>,
    pub pieces: usize,
    pub warnings: Vec<String>,
}
impl Inventory {
    pub fn build(model: &Model) -> Self {
        let Some(root) = model.doc.main_index() else {
            return Self::default();
        };
        let mut counts = BTreeMap::new();
        let mut warnings = BTreeSet::new();
        check_refs(&model.doc, root, &mut Vec::new(), &mut warnings);
        for p in model.flat(root) {
            let part = ldraw::norm(&p.name);
            if !part.ends_with(".dat") {
                warnings.insert(format!(
                    "Unresolved model reference: {part}; resolve it before ordering."
                ));
            }
            if p.color == Color::MAIN
                || p.color == Color::EDGE
                || p.color.0 < 0
                || p.color.edge_source().is_some()
            {
                warnings.insert(format!("{} has unresolved or non-catalogue colour {}. Choose an actual manufactured colour before ordering.",part,p.color.to_field()));
            }
            *counts.entry((part, p.color.0)).or_insert(0usize) += 1;
        }
        let lots = counts
            .into_iter()
            .map(|((part, color), quantity)| Lot {
                part,
                color,
                quantity,
            })
            .collect::<Vec<_>>();
        Self {
            pieces: lots.iter().map(|l| l.quantity).sum(),
            lots,
            warnings: warnings.into_iter().collect(),
        }
    }
    pub fn unique_parts(&self) -> usize {
        self.lots
            .iter()
            .map(|l| &l.part)
            .collect::<BTreeSet<_>>()
            .len()
    }
    /// Actual LDraw codes, not BrickLink/LEGO/Rebrickable colour IDs.
    /// Wobrick documents this exact Rebrickable-style upload schema with LDraw IDs.
    pub fn upload_csv(&self) -> String {
        let mut out = String::from("Part,Color,Quantity,Is Spare\r\n");
        for l in &self.lots {
            out += &format!(
                "{},{},{},False\r\n",
                csv_field(l.part.trim_end_matches(".dat")),
                Color(l.color).to_field(),
                l.quantity
            );
        }
        out
    }
    /// One type-1 line per counted instance. Suppliers perform their own catalogue
    /// mapping; never silently treat LDraw IDs as BrickLink colour/part IDs.
    pub fn upload_ldr(&self) -> String {
        let mut out = String::from(
            "0 CAD supplier inventory - NOT an assembled model\r\n0 All base-document instances; hidden included; unaccepted drafts excluded\r\n",
        );
        let mut i = 0usize;
        for l in &self.lots {
            for _ in 0..l.quantity {
                let part = PartLine {
                    color: Color(l.color),
                    pos: DVec3::new((i % 100) as f64 * 200., 0., (i / 100) as f64 * 200.),
                    rot: DMat3::IDENTITY,
                    name: l.part.clone(),
                };
                out += &part.to_text();
                out += "\r\n";
                i += 1;
            }
        }
        out
    }
}
fn csv_field(s: &str) -> String {
    format!("\"{}\"", s.replace('"', "\"\""))
}
fn check_refs(
    doc: &ldraw::Document,
    file: usize,
    stack: &mut Vec<usize>,
    warnings: &mut BTreeSet<String>,
) {
    if stack.contains(&file) {
        warnings.insert(format!("Recursive subbuild {}: inventory is incomplete. Fix the reference loop before ordering.",doc.files[file].name));
        return;
    }
    if stack.len() >= 20 {
        warnings.insert(
            "Subbuild nesting exceeds the flattening limit; inventory may be incomplete.".into(),
        );
        return;
    }
    stack.push(file);
    for (_, part, _) in doc.files[file].parts() {
        if let Some(child) = doc.file_index(&part.name) {
            check_refs(doc, child, stack, warnings);
        }
    }
    stack.pop();
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_subbuilds_inherit_colours_and_hidden_parts_still_count() {
        let doc=ldraw::Document::parse(b"0 FILE main.ldr\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 child.ldr\n1 1 100 0 0 1 0 0 0 1 0 0 0 1 child.ldr\n0 FILE child.ldr\n1 16 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n1 14 40 0 0 1 0 0 0 1 0 0 0 1 3004.dat\n","main.mpd");
        let mut model = Model::from_document(doc, None);
        model.parts.get_mut(&1).unwrap().hidden = true;
        let inv = Inventory::build(&model);
        assert_eq!((inv.pieces, inv.unique_parts(), inv.lots.len()), (4, 2, 3));
        assert_eq!(
            inv.lots
                .iter()
                .find(|l| l.part == "3004.dat")
                .unwrap()
                .quantity,
            2
        );
        assert!(inv.warnings.is_empty());
        let exported = ldraw::Document::parse(inv.upload_ldr().as_bytes(), "inventory.ldr");
        assert_eq!(
            Inventory::build(&Model::from_document(exported, None)).lots,
            inv.lots
        );
        assert!(inv.upload_csv().contains("\"3004\",14,2,False\r\n"));
    }
    #[test]
    fn unresolved_items_are_flagged_not_omitted_or_recoloured() {
        let doc = ldraw::Document::parse(b"1 16 0 0 0 1 0 0 0 1 0 0 0 1 missing.ldr\n", "main.ldr");
        let inv = Inventory::build(&Model::from_document(doc, None));
        assert_eq!(inv.pieces, 1);
        assert_eq!(inv.lots[0].color, 16);
        assert_eq!(inv.warnings.len(), 2);
    }
    #[test]
    fn recursive_subbuilds_warn_and_direct_colour_survives_export() {
        let doc = ldraw::Document::parse(
            b"0 FILE root.ldr\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 root.ldr\n",
            "root.mpd",
        );
        assert!(
            Inventory::build(&Model::from_document(doc, None))
                .warnings
                .iter()
                .any(|s| s.contains("Recursive"))
        );
        let doc = ldraw::Document::parse(
            b"1 0x2ABCDEF 0 0 0 1 0 0 0 1 0 0 0 1 3005.dat\n",
            "main.ldr",
        );
        let inv = Inventory::build(&Model::from_document(doc, None));
        assert_eq!(inv.pieces, 1);
        assert!(!inv.warnings.is_empty());
        assert!(inv.upload_ldr().to_uppercase().contains("0X2ABCDEF"));
    }
}
