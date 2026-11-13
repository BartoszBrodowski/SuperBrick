pub mod catalog;
pub mod embed;
pub mod search;
pub mod sets;
pub mod text;

pub use search::{Hit, Searcher, Weights};
use std::path::{Path, PathBuf};
pub type Result<T> = embed::Result<T>;

#[derive(Clone, Default)]
pub struct Filters {
    pub color: Option<i32>,
    pub tags: Vec<String>,
    pub unknown_colors: bool,
}

pub fn data_dir() -> PathBuf {
    if let Some(dir) = ldraw::app_env("SEARCH_DATA") {
        return PathBuf::from(dir);
    }
    if let Some(installed) = ldraw::installed_share("search") {
        if installed.join("out/catalog.json").is_file() {
            return installed;
        }
    }
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../spikes/search")
}

pub fn load(dir: &Path, embeddings: bool) -> Result<Searcher> {
    let cat: catalog::Catalog =
        serde_json::from_slice(&std::fs::read(dir.join("out/catalog.json"))?)?;
    let mut s = Searcher::new(cat, Weights::default());
    if embeddings {
        let enc = embed::Potion::load(&dir.join("models/potion-base-8m"))?;
        use embed::Encoder;
        let b = std::fs::read(dir.join("out/emb-potion-base-8m.bin"))?;
        if b.len() < 8 {
            return Err("truncated embedding index".into());
        }
        let n = u32::from_le_bytes(b[..4].try_into()?) as usize;
        let dim = u32::from_le_bytes(b[4..8].try_into()?) as usize;
        if n != s.cat.parts.len() || dim != enc.dim() || b.len() != 8 + n * dim * 4 {
            return Err(
                "stale or truncated embedding index; rebuild with search-spike index".into(),
            );
        }
        s.set_encoder(
            Box::new(enc),
            b[8..]
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes(b.try_into().unwrap()))
                .collect(),
        );
    }
    Ok(s)
}

impl Searcher {
    /// Apply hard colour/feature filters before truncating results, including query colours.
    pub fn filtered(
        &self,
        query: &str,
        filters: &Filters,
        limit: usize,
    ) -> (Vec<Hit>, search::QueryInfo) {
        let (mut hits, info) = self.search_internal(query, self.cat.parts.len(), Some(filters));
        if query.trim().is_empty() {
            hits = self
                .cat
                .parts
                .iter()
                .enumerate()
                .filter(|(_, p)| !p.hidden())
                .map(|(idx, p)| Hit {
                    idx,
                    score: p.pop as f32,
                })
                .collect();
            hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.idx.cmp(&b.idx)));
        }
        let color = filters.color.or(info.color);
        let exact = self.exact_matches(query);
        hits.retain(|h| exact.contains(&h.idx) || allowed(&self.cat.parts[h.idx], filters, color));
        // LEGO element IDs ("4142155") name a part in a colour: put those parts first.
        let mut lead = Vec::new();
        if sets::element_tokens(query).next().is_some()
            && let Some(table) = sets::Sets::get()
        {
            for e in table.elements_in(query) {
                if let Some(i) = e.part.as_deref().and_then(|p| self.exact_matches(p).first().copied())
                    && !lead.contains(&i)
                {
                    lead.push(i);
                }
            }
        }
        hits.retain(|h| !lead.contains(&h.idx));
        hits.splice(0..0, lead.into_iter().map(|idx| Hit { idx, score: f32::MAX }));
        hits.truncate(limit);
        (hits, info)
    }
}

pub(crate) fn allowed(p: &catalog::Part, filters: &Filters, color: Option<i32>) -> bool {
    color.is_none_or(|c| p.colors.contains(&c) || (filters.unknown_colors && p.colors.is_empty()))
        && filters.tags.iter().all(|t| tags(p).contains(&t.as_str()))
}

pub fn tags(p: &catalog::Part) -> Vec<&'static str> {
    let d = p.clean_desc().to_lowercase();
    let mut out = Vec::new();
    for (tag, terms) in [
        ("SNOT", &["stud on side", "studs on side", "bracket"][..]),
        ("hinge", &["hinge", "turntable"][..]),
        ("clip", &["clip"][..]),
        ("bar", &["bar", "handle"][..]),
        ("technic", &["technic"][..]),
        ("tile", &["tile"][..]),
        ("jumper", &["stud in center", "stud in centre"][..]),
    ] {
        if terms.iter().any(|t| d.contains(t)) {
            out.push(tag);
        }
    }
    if p.decorated() {
        out.push("print");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn literal_ids_always_win_even_if_filters_exclude_them() {
        let s = load(&data_dir(), false).unwrap();
        let filter = Filters {
            color: Some(-1000),
            tags: vec!["tile".into()],
            unknown_colors: false,
        };
        for q in ["15068", "15068.dat", "PART: 15068", "15068.DAT"] {
            let hits = s.filtered(q, &filter, 10).0;
            assert_eq!(s.cat.parts[hits[0].idx].id, "15068", "{q}");
        }
        let plain = s.filtered("curved slope", &filter, 10).0;
        assert!(
            plain.is_empty(),
            "Non-ID queries must still obey explicit filters"
        );
    }

    #[test]
    fn print_colours_are_not_manufactured_colour_filters() {
        let s = Searcher::new(
            catalog::Catalog {
                parts: vec![catalog::Part {
                    id: "3039p99".into(),
                    desc: "Slope 2 x 2 with Red and Blue Flight Controls Pattern".into(),
                    colors: vec![0],
                    ..Default::default()
                }],
                colors: vec![(4, vec!["red".into()]), (1, vec!["blue".into()])],
                ..Default::default()
            },
            Weights::default(),
        );
        let (hits, info) = s.filtered(
            "slope with red blue flight controls pattern",
            &Filters::default(),
            10,
        );
        assert_eq!(hits.len(), 1);
        assert_eq!(info.color, None);
        let (hits, info) = s.filtered("red slope", &Filters::default(), 10);
        assert!(hits.is_empty());
        assert_eq!(info.color, Some(4));
    }

    #[test]
    fn similar_grid_uses_semantics_excludes_source_and_is_deterministic() {
        struct Encoder;
        impl embed::Encoder for Encoder {
            fn name(&self) -> &str {
                "test"
            }
            fn dim(&self) -> usize {
                2
            }
            fn encode(&self, texts: &[String]) -> embed::Result<Vec<Vec<f32>>> {
                Ok(vec![vec![1., 0.]; texts.len()])
            }
        }
        let mut parts = vec![];
        for i in 0..12 {
            parts.push(catalog::Part {
                id: format!("{}", 15068 + i),
                desc: if i == 11 {
                    "Minifig hat".into()
                } else {
                    "Slope Brick Curved 2 x 2".into()
                },
                category: if i == 11 {
                    "Minifig".into()
                } else {
                    "Slope".into()
                },
                ..Default::default()
            });
        }
        let mut s = Searcher::new(
            catalog::Catalog {
                parts,
                ..Default::default()
            },
            Weights::default(),
        );
        let corpus = (0..12)
            .flat_map(|i| if i == 11 { vec![0., 1.] } else { vec![1., 0.] })
            .collect();
        s.set_encoder(Box::new(Encoder), corpus);
        let hits = s.similar("15068", 8);
        assert_eq!(hits.len(), 8);
        assert!(hits.iter().all(|h| h.idx != 0 && h.idx != 11));
        assert_eq!(
            hits.iter().map(|h| h.idx).collect::<Vec<_>>(),
            s.similar("15068", 8)
                .iter()
                .map(|h| h.idx)
                .collect::<Vec<_>>()
        );
        assert!(s.similar("no-such-part", 8).is_empty());
    }
    #[test]
    fn filters_are_hard_and_empty_queries_work() {
        let parts = vec![
            catalog::Part {
                id: "3003".into(),
                desc: "Brick 2 x 2".into(),
                colors: vec![4],
                ..Default::default()
            },
            catalog::Part {
                id: "x".into(),
                desc: "Tile 2 x 2".into(),
                colors: vec![1],
                ..Default::default()
            },
        ];
        let s = Searcher::new(
            catalog::Catalog {
                parts,
                ..Default::default()
            },
            Weights::default(),
        );
        let f = Filters {
            color: Some(4),
            ..Default::default()
        };
        assert_eq!(s.filtered("2x2", &f, 20).0.len(), 1);
        assert_eq!(s.filtered("", &f, 20).0.len(), 1);
        // "No colour" clears only the manufactured-colour restriction.
        // Parts offered in other colours must remain discoverable.
        assert_eq!(s.filtered("", &Filters::default(), 20).0.len(), 2);
        assert_eq!(s.filtered("2x2", &Filters::default(), 20).0.len(), 2);
        assert!(
            s.filtered(
                "",
                &Filters {
                    tags: vec!["hinge".into()],
                    ..f
                },
                20
            )
            .0
            .is_empty()
        );
    }
}
