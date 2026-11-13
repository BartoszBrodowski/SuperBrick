//! Shared BM25F over part text, optional embedding similarity, and priors (popularity, canonical part,
//! dimension match, colour availability) fused into one score.

use std::collections::{HashMap, HashSet};

use crate::{
    catalog::{Catalog, Part},
    embed::Encoder,
    text::{self, Analyzed, Dims},
};

#[derive(Clone, Copy, Debug)]
pub struct Weights {
    pub adaptive: bool,
    pub bm25: f32,
    pub emb: f32,
    pub pop: f32,
    pub dims: f32,
    pub canon: f32,
    pub priors: bool,
    pub synonyms: bool,
    pub typos: bool,
}

impl Default for Weights {
    fn default() -> Self {
        // Tuned on golden.tsv (see FINDINGS.md): embeddings are a light tie-breaker, popularity does the heavy lifting.
        Weights {
            adaptive: true,
            bm25: 1.0,
            emb: 0.2,
            pop: 1.5,
            dims: 0.6,
            canon: 1.0,
            priors: true,
            synonyms: true,
            typos: true,
        }
    }
}

/// Slang and describe-the-piece words → catalogue words, with a weight for the added tokens.
const EXPAND: &[(&str, &str, f32)] = &[
    ("cheese", "slope 1x1x2/3", 1.0),
    ("jumper", "plate 1x2 1 stud center", 0.8),
    ("macaroni", "round corner", 1.0),
    ("snot", "stud side", 0.8),
    ("liftarm", "beam", 1.0),
    ("grill", "grille", 1.0),
    ("vent", "grille", 1.0),
    ("flat", "tile", 0.6),
    ("smooth", "tile", 0.6),
    ("studless", "tile", 0.6),
    ("thin", "plate", 0.6),
    ("circle", "round", 1.0),
    ("circular", "round", 1.0),
    ("curve", "curved", 1.0),
    ("figure", "minifig", 0.8),
    ("upside", "inverted", 0.8),
    ("connector", "joiner", 0.6),
    ("rotating", "turntable", 0.8),
    ("stick", "bar", 0.8),
    ("l", "corner", 0.8),
    ("body", "torso", 0.8),
    ("man", "minifig", 0.5),
    ("triangle", "triangular", 1.0),
];
const PHRASES: &[(&str, &str, f32)] = &[
    ("no stud", "tile", 1.0),
    ("without stud", "tile", 1.0),
    ("base plate", "baseplate", 1.0),
    ("upside down", "inverted", 1.0),
];
const DECOR_WORDS: &[&str] = &[
    "print",
    "printed",
    "pattern",
    "sticker",
    "decorated",
    "logo",
    "text",
    "face",
];
const THEME_WORDS: &[&str] = &[
    "duplo",
    "primo",
    "quatro",
    "fabuland",
    "scala",
    "znap",
    "belville",
    "clikits",
    "galidor",
    "homemaker",
];

struct Field {
    weight: f32,
    b: f32,
}
const FIELDS: [Field; 4] = [
    Field {
        weight: 3.0,
        b: 0.5,
    }, // LDraw description
    Field {
        weight: 2.0,
        b: 0.5,
    }, // Rebrickable name
    Field {
        weight: 1.0,
        b: 0.3,
    }, // categories
    Field {
        weight: 1.0,
        b: 0.3,
    }, // keywords
];

pub struct Searcher {
    pub cat: Catalog,
    vocab: HashMap<String, u32>,
    df: Vec<u32>,
    /// term → (doc, saturated BM25F term weight)
    postings: Vec<Vec<(u32, f32)>>,
    idf: Vec<f32>,
    dims: Vec<Option<Dims>>,
    ids: HashMap<String, Vec<u32>>,
    enc: Option<(Box<dyn Encoder>, Vec<f32>)>,
    max_pop: f32,
    pub w: Weights,
}

#[derive(Debug, Clone)]
pub struct Hit {
    pub idx: usize,
    pub score: f32,
}

#[derive(Debug, Default, Clone)]
pub struct QueryInfo {
    pub color: Option<i32>,
    pub tokens: Vec<(String, f32)>,
    pub corrected: Vec<(String, String)>,
    pub embed_us: f64,
    pub total_us: f64,
}

pub fn doc_text(p: &Part) -> String {
    let mut s = p.clean_desc().to_string();
    if let Some(r) = &p.rb_name
        && !r.eq_ignore_ascii_case(p.clean_desc())
    {
        s.push_str(" | ");
        s.push_str(r);
    }
    s
}

impl Searcher {
    /// Literal catalogue IDs (including .dat) are navigation, not a fuzzy query.
    pub fn exact_matches(&self, query: &str) -> Vec<usize> {
        let mut matches = Vec::new();
        for token in query.split(|c: char| !c.is_ascii_alphanumeric() && c != '.') {
            let token = token.trim_end_matches('.').to_ascii_lowercase();
            let id = token.strip_suffix(".dat").unwrap_or(&token);
            if id.len() < 3 || !id.starts_with(|c: char| c.is_ascii_digit()) {
                continue;
            }
            if let Some(indices) = self.ids.get(id) {
                for &i in indices {
                    if self.cat.parts[i as usize].id.eq_ignore_ascii_case(id)
                        && !matches.contains(&(i as usize))
                    {
                        matches.push(i as usize);
                    }
                }
            }
        }
        matches
    }
    pub fn new(cat: Catalog, w: Weights) -> Searcher {
        const K1: f32 = 1.2;
        let n = cat.parts.len();
        let fields_of = |p: &Part| -> [Analyzed; 4] {
            [
                text::analyze(p.clean_desc()),
                text::analyze(p.rb_name.as_deref().unwrap_or("")),
                text::analyze(&format!(
                    "{} {}",
                    p.category,
                    p.rb_cat.as_deref().unwrap_or("")
                )),
                text::analyze(&p.keywords.join(" ")),
            ]
        };
        let analyzed: Vec<[Analyzed; 4]> = cat.parts.iter().map(fields_of).collect();
        let mut avg = [0f32; 4];
        for a in &analyzed {
            for f in 0..4 {
                avg[f] += a[f].tokens.len() as f32;
            }
        }
        avg.iter_mut()
            .for_each(|x| *x = (*x / n.max(1) as f32).max(1.0));
        let mut vocab: HashMap<String, u32> = HashMap::new();
        let mut tf: Vec<HashMap<u32, f32>> = Vec::new();
        for (d, a) in analyzed.iter().enumerate() {
            let mut per: HashMap<u32, f32> = HashMap::new();
            for (f, fa) in a.iter().enumerate() {
                let norm = 1.0 - FIELDS[f].b + FIELDS[f].b * fa.tokens.len() as f32 / avg[f];
                for t in &fa.tokens {
                    let next = vocab.len() as u32;
                    let id = *vocab.entry(t.clone()).or_insert(next);
                    *per.entry(id).or_default() += FIELDS[f].weight / norm;
                }
            }
            for (t, v) in per {
                if tf.len() <= t as usize {
                    tf.resize_with(t as usize + 1, HashMap::new);
                }
                tf[t as usize].insert(d as u32, v);
            }
        }
        let df: Vec<u32> = tf.iter().map(|m| m.len() as u32).collect();
        let idf: Vec<f32> = df
            .iter()
            .map(|&d| (1.0 + (n as f32 - d as f32 + 0.5) / (d as f32 + 0.5)).ln())
            .collect();
        let postings: Vec<Vec<(u32, f32)>> = tf
            .into_iter()
            .map(|m| {
                let mut v: Vec<(u32, f32)> = m
                    .into_iter()
                    .map(|(d, x)| (d, x * (K1 + 1.0) / (x + K1)))
                    .collect();
                v.sort_by_key(|p| p.0);
                v
            })
            .collect();
        let dims = analyzed
            .iter()
            .map(|a| a[0].dims.first().cloned())
            .collect();
        let mut ids: HashMap<String, Vec<u32>> = HashMap::new();
        for (i, p) in cat.parts.iter().enumerate() {
            ids.entry(p.id.clone()).or_default().push(i as u32);
            let stem = p.id.trim_end_matches(|c: char| c.is_ascii_alphabetic());
            if stem.len() < p.id.len() && stem.len() >= 3 {
                ids.entry(stem.to_string()).or_default().push(i as u32);
            }
        }
        let max_pop = cat
            .parts
            .iter()
            .map(|p| p.pop as f32 + 0.3 * p.print_pop as f32)
            .fold(1.0, f32::max);
        Searcher {
            cat,
            vocab,
            df,
            postings,
            idf,
            dims,
            ids,
            enc: None,
            max_pop,
            w,
        }
    }

    pub fn vocab_len(&self) -> usize {
        self.vocab.len()
    }

    pub fn set_encoder(&mut self, enc: Box<dyn Encoder>, corpus: Vec<f32>) {
        assert_eq!(corpus.len(), self.cat.parts.len() * enc.dim());
        self.enc = Some((enc, corpus));
    }

    pub fn encoder_name(&self) -> &str {
        self.enc.as_ref().map_or("none", |(e, _)| e.name())
    }

    /// Nearest catalogue neighbours, not restricted by the current picker colour.
    /// Uses stored semantic vectors plus text/shape-family signals; excludes self/aliases.
    pub fn similar(&self, id: &str, limit: usize) -> Vec<Hit> {
        let Some(source) = self.exact_matches(id).first().copied() else {
            return vec![];
        };
        let part = &self.cat.parts[source];
        let (lexical, _) = self.search(part.clean_desc(), 200);
        let max = lexical.first().map_or(1., |h| h.score.abs().max(1e-6));
        let lex: HashMap<usize, f32> = lexical
            .into_iter()
            .map(|h| (h.idx, h.score / max))
            .collect();
        let mut hits: Vec<Hit> = self
            .cat
            .parts
            .iter()
            .enumerate()
            .filter_map(|(i, p)| {
                if i == source || p.hidden() || p.alias() {
                    return None;
                }
                let semantic = self.enc.as_ref().map_or(0., |(enc, corpus)| {
                    let dim = enc.dim();
                    corpus[source * dim..(source + 1) * dim]
                        .iter()
                        .zip(&corpus[i * dim..(i + 1) * dim])
                        .map(|(a, b)| a * b)
                        .sum::<f32>()
                });
                if self.enc.is_none() && !lex.contains_key(&i) {
                    return None;
                }
                let mut score = 0.8 * semantic + 0.2 * lex.get(&i).copied().unwrap_or(0.);
                if !part.category.is_empty() && p.category == part.category {
                    score += 0.08;
                }
                if let (Some(a), Some(b)) = (&self.dims[source], &self.dims[i]) {
                    if a.fp == b.fp {
                        score += 0.05;
                    }
                }
                if p.decorated() != part.decorated() {
                    score -= 0.12;
                }
                // Don't fill an entire "similar shapes" grid with the source's printed variants.
                if p.id.starts_with(&part.id) || part.id.starts_with(&p.id) {
                    score -= 0.12;
                }
                Some(Hit { idx: i, score })
            })
            .collect();
        hits.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.idx.cmp(&b.idx)));
        hits.truncate(limit);
        hits
    }

    /// Colour words come out of the query and become a filter; the longest name wins.
    fn take_color(&self, words: &mut Vec<String>) -> Option<i32> {
        // Colours inside a print/sticker/design describe its appearance, not the
        // manufactured base colour. Only a leading colour phrase is a filter.
        if words.iter().any(|t| DECOR_WORDS.contains(&t.as_str())) {
            return None;
        }
        let mut best: Option<(usize, usize, i32)> = None; // (len, start, code)
        for (code, names) in &self.cat.colors {
            for name in names {
                let nw: Vec<&str> = name.split(' ').collect();
                if nw.is_empty() || nw.len() > words.len() {
                    continue;
                }
                for s in 0..=words.len() - nw.len() {
                    if words[s..s + nw.len()].iter().zip(&nw).all(|(a, b)| a == b)
                        && best.is_none_or(|(l, _, _)| nw.len() > l)
                    {
                        best = Some((nw.len(), s, *code));
                    }
                }
            }
        }
        let (len, start, code) = best?;
        if !words[..start].iter().all(|w| {
            ["a", "an", "the", "find", "show", "me", "need", "want", "i"].contains(&w.as_str())
        }) {
            return None;
        }
        words.drain(start..start + len);
        Some(code)
    }

    fn correct(&self, w: &str) -> Option<String> {
        if self.vocab.contains_key(w) || w.len() < 4 || !w.chars().all(|c| c.is_ascii_alphabetic())
        {
            return None;
        }
        let max = if w.len() >= 7 { 2 } else { 1 };
        // Adjacent-key transpositions are one typo, not two substitutions.
        let bytes = w.as_bytes();
        for i in 0..bytes.len().saturating_sub(1) {
            let mut swapped = bytes.to_vec();
            swapped.swap(i, i + 1);
            let s = String::from_utf8(swapped).ok()?;
            if self.vocab.contains_key(&s) {
                return Some(s);
            }
        }
        let mut best: Option<(usize, u32, &str)> = None;
        for (t, &id) in &self.vocab {
            if t.len().abs_diff(w.len()) > max || !t.chars().all(|c| c.is_ascii_alphabetic()) {
                continue;
            }
            let d = text::edit_distance(w, t, max);
            let df = self.df[id as usize];
            if d <= max && best.is_none_or(|(bd, bdf, _)| d < bd || (d == bd && df > bdf)) {
                best = Some((d, df, t));
            }
        }
        best.map(|b| b.2.to_string())
    }

    pub fn search(&self, query: &str, limit: usize) -> (Vec<Hit>, QueryInfo) {
        self.search_internal(query, limit, None)
    }
    pub(crate) fn search_internal(
        &self,
        query: &str,
        limit: usize,
        filters: Option<&crate::Filters>,
    ) -> (Vec<Hit>, QueryInfo) {
        let t0 = std::time::Instant::now();
        let mut info = QueryInfo::default();
        let exact: HashSet<u32> = self
            .exact_matches(query)
            .into_iter()
            .map(|i| i as u32)
            .collect();
        let mut raw = text::raw_tokens(query);
        info.color = self.take_color(&mut raw);
        let joined = raw.join(" ");

        // Typed part numbers.
        let mut id_hits: HashSet<u32> = HashSet::new();
        for t in &raw {
            if t.len() >= 3
                && t.chars().next().is_some_and(|c| c.is_ascii_digit())
                && let Some(v) = self.ids.get(t.as_str())
            {
                id_hits.extend(v);
            }
        }
        if self.w.typos {
            for t in raw.iter_mut() {
                if EXPAND.iter().any(|(word, _, _)| t == word) {
                    continue;
                }
                if let Some(c) = self.correct(t) {
                    info.corrected.push((t.clone(), c.clone()));
                    *t = c;
                }
            }
        }
        let a = text::analyze_tokens(&raw);
        let mut qtok: Vec<(String, f32)> = a.tokens.iter().map(|t| (t.clone(), 1.0)).collect();
        let mut extra_text = String::new();
        if self.w.synonyms {
            for (w, exp, wt) in EXPAND {
                if raw.iter().any(|t| t == w) {
                    qtok.extend(text::analyze(exp).tokens.into_iter().map(|t| (t, *wt)));
                    extra_text.push(' ');
                    extra_text.push_str(exp);
                }
            }
            for (ph, exp, wt) in PHRASES {
                if joined.contains(ph) {
                    qtok.extend(text::analyze(exp).tokens.into_iter().map(|t| (t, *wt)));
                    extra_text.push(' ');
                    extra_text.push_str(exp);
                }
            }
        }
        let wants_decor = raw.iter().any(|t| DECOR_WORDS.contains(&t.as_str()));
        let words = raw.join(" ");
        let feature = |s: &str| raw.iter().any(|t| t == s);
        let family = if feature("pin") && !feature("brick") && !feature("gear") {
            Some("technic pin")
        } else if feature("liftarm") || feature("beam") {
            Some("technic beam")
        } else if feature("torso") || (feature("minifig") && feature("body")) {
            Some("minifig torso")
        } else if feature("tile") || words.contains("no studs") {
            Some("tile")
        } else {
            None
        };
        let wants_theme: HashSet<&str> = raw
            .iter()
            .filter(|t| THEME_WORDS.contains(&t.as_str()))
            .map(String::as_str)
            .collect();
        let qdims = a.dims.first().cloned();

        // BM25F.
        let n = self.cat.parts.len();
        let mut bm = vec![0f32; n];
        let mut seen: HashSet<&str> = HashSet::new();
        for (t, wt) in &qtok {
            if !seen.insert(t.as_str()) {
                continue;
            }
            if let Some(&id) = self.vocab.get(t) {
                let idf = self.idf[id as usize] * wt;
                for &(d, s) in &self.postings[id as usize] {
                    bm[d as usize] += idf * s;
                }
            }
        }
        // Embedding similarity.
        let mut cos: Option<Vec<f32>> = None;
        if let Some((enc, corpus)) = &self.enc {
            let te = std::time::Instant::now();
            let qtext = format!("{}{}", raw.join(" "), extra_text);
            if let Ok(v) = enc.encode(&[qtext]) {
                let q = &v[0];
                let dim = enc.dim();
                cos = Some(
                    (0..n)
                        .map(|i| {
                            corpus[i * dim..(i + 1) * dim]
                                .iter()
                                .zip(q)
                                .map(|(a, b)| a * b)
                                .sum()
                        })
                        .collect(),
                );
            }
            info.embed_us = te.elapsed().as_secs_f64() * 1e6;
        }

        // Candidates: the top of each list plus typed ids.
        const K: usize = 300;
        let top = |v: &[f32]| -> Vec<u32> {
            let mut idx: Vec<u32> = (0..n as u32)
                .filter(|&i| {
                    v[i as usize] > 0.0
                        && filters.is_none_or(|f| {
                            crate::allowed(&self.cat.parts[i as usize], f, f.color.or(info.color))
                        })
                })
                .collect();
            if idx.len() > K {
                idx.select_nth_unstable_by(K, |&a, &b| v[b as usize].total_cmp(&v[a as usize]));
                idx.truncate(K);
            }
            idx
        };
        let mut cand: HashSet<u32> = top(&bm).into_iter().collect();
        if let Some(c) = &cos {
            cand.extend(top(c));
        }
        cand.extend(&id_hits);
        cand.extend(&exact);
        let cand: Vec<u32> = cand
            .into_iter()
            .filter(|&i| {
                exact.contains(&i)
                    || ((!self.cat.parts[i as usize].hidden() || id_hits.contains(&i))
                        && filters.is_none_or(|f| {
                            crate::allowed(&self.cat.parts[i as usize], f, f.color.or(info.color))
                        }))
            })
            .collect();
        let bm_max = cand.iter().map(|&i| bm[i as usize]).fold(1e-6, f32::max);
        let (c_hi, c_lo) = match &cos {
            Some(c) => {
                let mut v: Vec<f32> = cand.iter().map(|&i| c[i as usize]).collect();
                v.sort_by(|a, b| b.total_cmp(a));
                (
                    v.first().copied().unwrap_or(1.0),
                    v.last().copied().unwrap_or(0.0),
                )
            }
            None => (1.0, 0.0),
        };

        let mut w = self.w;
        // Canonical/popular pieces are useful defaults for short generic queries.
        // Specific descriptions must be allowed to retrieve rare prints and mechanisms.
        let informative = a
            .tokens
            .iter()
            .filter(|t| t.chars().all(char::is_alphabetic))
            .collect::<HashSet<_>>()
            .len();
        let rare = a
            .tokens
            .iter()
            .filter(|t| {
                self.vocab
                    .get(*t)
                    .is_some_and(|&i| self.df[i as usize] < 100)
            })
            .count();
        if w.adaptive && (wants_decor || informative >= 6 || rare >= 2) {
            w.bm25 = 4.;
            w.emb = 1.;
            w.pop = 0.3;
            w.dims = 0.7;
            w.canon = 0.3;
        }
        let mut hits: Vec<Hit> = cand
            .iter()
            .map(|&i| {
                let p = &self.cat.parts[i as usize];
                let mut s = w.bm25 * bm[i as usize] / bm_max;
                if let Some(c) = &cos {
                    s += w.emb * ((c[i as usize] - c_lo) / (c_hi - c_lo).max(1e-6)).max(0.0);
                }
                if id_hits.contains(&i) {
                    s += 10.0
                        + if raw.iter().any(|t| *t == p.id) {
                            1.0
                        } else {
                            0.0
                        };
                }
                if w.priors {
                    let pop = (1.0 + p.pop as f32 + 0.3 * p.print_pop as f32).ln()
                        / (1.0 + self.max_pop).ln();
                    s += w.pop * pop;
                    if let Some(q) = &qdims {
                        s += w.dims
                            * match &self.dims[i as usize] {
                                Some(d)
                                    if d.fp == q.fp && (q.full.is_none() || d.full == q.full) =>
                                {
                                    1.0
                                }
                                Some(d) if d.fp == q.fp && q.full.is_none() => 0.4,
                                Some(d) if q.full.is_some() && d.full == q.full => 0.8,
                                _ => -1.0,
                            };
                    }
                    let mut canon = 0.0;
                    if p.decorated() && !wants_decor {
                        canon -= 0.5;
                    }
                    if p.alias() {
                        canon -= 0.3;
                    }
                    if p.kind.contains("Physical_Colour") || p.desc.starts_with('_') {
                        canon -= 0.3;
                    }
                    if p.clean_desc().contains("Obsolete") {
                        canon -= 0.2;
                    }
                    let d = p.clean_desc().to_lowercase();
                    // Prefer the requested family and distinguish shape modifiers rather than
                    // letting popularity turn a curved slope into an inverted one or a full pin into half a pin.
                    if let Some(family) = family {
                        if d.starts_with(family) {
                            canon += 0.65;
                        } else {
                            canon -= 0.7;
                        }
                        if d == family && !feature("friction") {
                            canon += if family == "minifig torso" { 1.6 } else { 0.5 };
                        }
                    }
                    for (modifier, wanted) in [
                        (
                            "inverted",
                            feature("inverted") || words.contains("upside down"),
                        ),
                        ("curved", feature("curved")),
                        ("round", feature("round")),
                        ("clip", feature("clip")),
                        (
                            "corner",
                            feature("corner") || feature("quarter") || words.contains("l shaped"),
                        ),
                    ] {
                        if wanted && !d.contains(modifier) {
                            canon -= 0.6;
                        }
                        if !wanted && d.contains(modifier) {
                            canon -= 0.25;
                        }
                        if wanted && d.contains(modifier) {
                            canon += 0.15;
                        }
                    }
                    for (wanted, matching) in [
                        (feature("friction"), d.contains("friction")),
                        (feature("tooth"), d.contains("tooth")),
                        (
                            words.contains("hole in the stud") || feature("open"),
                            d.contains("open stud"),
                        ),
                    ] {
                        if wanted {
                            canon += if matching { 0.45 } else { -0.65 };
                        }
                    }
                    if words.contains("no studs") && feature("square") {
                        if let Some(dims) = &self.dims[i as usize] {
                            canon += if dims.fp == "1x1" { 0.65 } else { -0.4 };
                        }
                    }
                    if feature("snot") && !d.contains("side") {
                        canon -= 0.8;
                    }
                    if !feature("stop") && d.contains("with stop") {
                        canon -= 0.2;
                    }
                    if !feature("thick") && d.contains("thick c-clip") {
                        canon -= 0.15;
                    }
                    if family == Some("technic pin") && !words.contains("1/2") && d.contains("1/2")
                    {
                        canon -= 0.5;
                    }
                    if family == Some("technic beam") {
                        let length = raw.iter().rev().find(|t| t.parse::<u32>().is_ok());
                        if let Some(length) = length {
                            let desc = text::raw_tokens(&d);
                            if desc.get(2) == Some(length) {
                                canon += 1.0;
                            } else {
                                canon -= 0.7;
                            }
                            if desc.len() == 3 {
                                canon += 0.65;
                            }
                        }
                    }
                    if p.moved_to.is_some() {
                        canon -= 2.0;
                    }
                    if THEME_WORDS
                        .iter()
                        .any(|t| d.contains(t) && !wants_theme.contains(t))
                    {
                        canon -= 0.4;
                    }
                    s += w.canon * canon;
                }
                if let Some(c) = info.color {
                    if p.colors.is_empty() {
                        s -= 0.1;
                    } else if !p.colors.contains(&c) {
                        s -= 0.6;
                    }
                }
                Hit {
                    idx: i as usize,
                    score: s,
                }
            })
            .collect();
        hits.sort_by(|a, b| {
            exact
                .contains(&(b.idx as u32))
                .cmp(&exact.contains(&(a.idx as u32)))
                .then(b.score.total_cmp(&a.score))
                .then(a.idx.cmp(&b.idx))
        });
        hits.truncate(limit);
        info.tokens = qtok;
        info.total_us = t0.elapsed().as_secs_f64() * 1e6;
        (hits, info)
    }
}
