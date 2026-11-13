//! The shared part catalog: every LDraw part's header joined with Rebrickable names, categories,
//! popularity (how many set inventories use it) and the colours it exists in.

use std::{
    collections::{HashMap, HashSet},
    error::Error,
    io::BufReader,
    path::Path,
    time::Instant,
};

use ldraw::Library;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Clone, Debug, Default)]
pub struct Part {
    /// LDraw file name without `.dat`, lowercase.
    pub id: String,
    /// LDraw description with its marker prefix (`~` hidden, `=` alias, `_` physical colour).
    pub desc: String,
    pub category: String,
    pub keywords: Vec<String>,
    /// `!LDRAW_ORG` type: Part, Shortcut, Part Alias, ...
    pub kind: String,
    pub moved_to: Option<String>,
    pub rb_name: Option<String>,
    pub rb_cat: Option<String>,
    /// Inventory rows (set × colour) using this exact shape, or a mould/alias variant of it.
    pub pop: u32,
    /// Inventory rows of printed or patterned versions credited to this base shape.
    pub print_pop: u32,
    /// LDraw colour codes the shape exists in, per Rebrickable.
    pub colors: Vec<i32>,
}

impl Part {
    pub fn clean_desc(&self) -> &str {
        self.desc.trim_start_matches(['~', '=', '_', '|']).trim()
    }
    pub fn hidden(&self) -> bool {
        self.desc.starts_with('~') || self.moved_to.is_some()
    }
    pub fn alias(&self) -> bool {
        self.desc.starts_with('=') || self.kind.contains("Alias")
    }
    /// Printed, patterned or sticker parts: wanted only when the query asks for decoration.
    pub fn decorated(&self) -> bool {
        let d = self.clean_desc();
        let stem = self.id.trim_start_matches(|c: char| c.is_ascii_digit());
        d.starts_with("Sticker")
            || d.contains("Pattern")
            || d.contains(" Print")
            || (stem.contains('p') && !stem.starts_with('c'))
    }
}

#[derive(Serialize, Deserialize, Default)]
pub struct Catalog {
    pub parts: Vec<Part>,
    /// (LDraw code, normalized names) for query colour words.
    pub colors: Vec<(i32, Vec<String>)>,
    pub stats: serde_json::Value,
}

fn csv_rows(
    dir: &Path,
    name: &str,
) -> Result<
    (
        HashMap<String, usize>,
        csv::Reader<flate2::read::GzDecoder<BufReader<std::fs::File>>>,
    ),
    Box<dyn Error>,
> {
    let f = std::fs::File::open(dir.join(format!("{name}.csv.gz")))?;
    let mut r = csv::Reader::from_reader(flate2::read::GzDecoder::new(BufReader::new(f)));
    let cols = r
        .headers()?
        .iter()
        .enumerate()
        .map(|(i, h)| (h.to_string(), i))
        .collect();
    Ok((cols, r))
}

pub fn norm_color(s: &str) -> String {
    s.to_lowercase()
        .replace("grey", "gray")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect()
}

/// Words of a colour name: "Light_Bluish_Grey" → ["light", "bluish", "gray"].
pub fn color_words(s: &str) -> Vec<String> {
    s.to_lowercase()
        .replace("grey", "gray")
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|w| !w.is_empty())
        .map(str::to_string)
        .collect()
}

pub fn build(lib: &Library, rb_dir: &Path) -> Result<Catalog, Box<dyn Error>> {
    let t = Instant::now();
    let names: Vec<&str> = lib.part_names().filter(|n| !n.contains('/')).collect();
    let mut parts: Vec<Part> = names
        .par_iter()
        .filter_map(|n| {
            let text = lib.read_text(&format!("parts/{n}")).ok()?;
            let mut p = Part {
                id: n.to_lowercase().trim_end_matches(".dat").to_string(),
                ..Default::default()
            };
            for (i, line) in text.lines().enumerate() {
                let l = line.trim();
                if l.is_empty() {
                    continue;
                }
                if !l.starts_with('0') {
                    break;
                }
                let body = l[1..].trim();
                if i == 0 {
                    p.desc = body.to_string();
                } else if let Some(c) = body.strip_prefix("!CATEGORY") {
                    p.category = c.trim().to_string();
                } else if let Some(k) = body.strip_prefix("!KEYWORDS") {
                    p.keywords.extend(
                        k.split(',')
                            .map(|s| s.trim().to_string())
                            .filter(|s| !s.is_empty()),
                    );
                } else if let Some(o) = body.strip_prefix("!LDRAW_ORG") {
                    p.kind = o
                        .split_whitespace()
                        .take_while(|w| !w.starts_with("UPDATE") && !w.starts_with("ORIGINAL"))
                        .collect::<Vec<_>>()
                        .join(" ");
                }
            }
            if p.category.is_empty() {
                p.category = p
                    .clean_desc()
                    .split_whitespace()
                    .next()
                    .unwrap_or("")
                    .to_string();
            }
            p.moved_to = lib
                .moved_to(n)
                .map(|m| m.to_lowercase().trim_end_matches(".dat").to_string());
            Some(p)
        })
        .collect();
    parts.sort_by(|a, b| a.id.cmp(&b.id));
    let header_ms = t.elapsed().as_secs_f64() * 1e3;
    let by_id: HashMap<String, usize> = parts
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.clone(), i))
        .collect();

    // Follow LDraw "~Moved to" chains to the part that is actually drawn.
    let resolve_moved = |id: &str| -> Option<usize> {
        let mut cur = *by_id.get(id)?;
        for _ in 0..8 {
            match &parts[cur].moved_to {
                Some(m) => cur = *by_id.get(m.as_str())?,
                None => return Some(cur),
            }
        }
        None
    };

    // Rebrickable colours → LDraw codes: by name, else by identical RGB and transparency.
    let mut ld_by_name: HashMap<String, i32> = HashMap::new();
    for (code, c) in &lib.colors.by_code {
        ld_by_name.entry(norm_color(&c.ldraw_name)).or_insert(*code);
        ld_by_name.entry(norm_color(&c.name)).or_insert(*code);
    }
    let (cols, mut r) = csv_rows(rb_dir, "colors")?;
    let mut rb_color: HashMap<i32, i32> = HashMap::new();
    let (mut by_name, mut by_rgb, mut unmapped_colors) = (0, 0, Vec::new());
    for rec in r.records() {
        let rec = rec?;
        let id: i32 = rec[cols["id"]].parse()?;
        let name = &rec[cols["name"]];
        let trans = &rec[cols["is_trans"]] == "True";
        if let Some(&code) = ld_by_name.get(&norm_color(name)) {
            rb_color.insert(id, code);
            by_name += 1;
            continue;
        }
        let rgb = u32::from_str_radix(&rec[cols["rgb"]], 16).unwrap_or(u32::MAX);
        let hit = lib.colors.by_code.values().find(|c| {
            let v = ((c.rgb[0] as u32) << 16) | ((c.rgb[1] as u32) << 8) | c.rgb[2] as u32;
            v == rgb && (c.alpha < 255) == trans
        });
        match hit {
            Some(c) => {
                rb_color.insert(id, c.code);
                by_rgb += 1;
            }
            None => unmapped_colors.push(name.to_string()),
        }
    }

    let (cols, mut r) = csv_rows(rb_dir, "part_categories")?;
    let mut cats: HashMap<String, String> = HashMap::new();
    for rec in r.records() {
        let rec = rec?;
        cats.insert(rec[cols["id"]].to_string(), rec[cols["name"]].to_string());
    }
    let (cols, mut r) = csv_rows(rb_dir, "parts")?;
    let mut rb_parts: HashMap<String, (String, String)> = HashMap::new();
    for rec in r.records() {
        let rec = rec?;
        let cat = cats
            .get(&rec[cols["part_cat_id"]])
            .cloned()
            .unwrap_or_default();
        rb_parts.insert(
            rec[cols["part_num"]].to_lowercase(),
            (rec[cols["name"]].to_string(), cat),
        );
    }
    let (cols, mut r) = csv_rows(rb_dir, "part_relationships")?;
    let mut mold: HashMap<String, Vec<String>> = HashMap::new();
    let mut print_parent: HashMap<String, String> = HashMap::new();
    for rec in r.records() {
        let rec = rec?;
        let (ty, child, parent) = (
            &rec[cols["rel_type"]],
            rec[cols["child_part_num"]].to_lowercase(),
            rec[cols["parent_part_num"]].to_lowercase(),
        );
        match ty {
            "M" | "A" => {
                mold.entry(child.clone()).or_default().push(parent.clone());
                mold.entry(parent).or_default().push(child);
            }
            "P" | "T" => {
                print_parent.insert(child, parent);
            }
            _ => {}
        }
    }
    let t_inv = Instant::now();
    let (cols, mut r) = csv_rows(rb_dir, "inventory_parts")?;
    let (pc, cc) = (cols["part_num"], cols["color_id"]);
    let mut usage: HashMap<String, (u32, HashSet<i32>)> = HashMap::new();
    let mut rec = csv::StringRecord::new();
    while r.read_record(&mut rec)? {
        let e = usage.entry(rec[pc].to_lowercase()).or_default();
        e.0 += 1;
        if let Ok(c) = rec[cc].parse::<i32>() {
            e.1.insert(c);
        }
    }
    let inv_ms = t_inv.elapsed().as_secs_f64() * 1e3;

    // Rebrickable part → LDraw part, in stages; the stage says how much to trust it.
    let strip_letter = |p: &str| -> Option<String> {
        let s = p.trim_end_matches(|c: char| c.is_ascii_alphabetic());
        (s.len() < p.len() && !s.is_empty()).then(|| s.to_string())
    };
    let map_shape = |p: &str| -> Option<(usize, &'static str)> {
        if let Some(i) = by_id.get(p) {
            return resolve_moved(p).map(|j| (j, if *i == j { "exact" } else { "moved" }));
        }
        if let Some(s) = strip_letter(p)
            && let Some(j) = resolve_moved(&s)
        {
            return Some((j, "mould letter"));
        }
        for m in mold.get(p).into_iter().flatten() {
            if let Some(j) = resolve_moved(m) {
                return Some((j, "mould/alternate rel"));
            }
        }
        None
    };
    let mut stage_rows: HashMap<&'static str, u64> = HashMap::new();
    let mut stage_parts: HashMap<&'static str, u64> = HashMap::new();
    let mut colors: Vec<HashSet<i32>> = vec![HashSet::new(); parts.len()];
    let mut pops = vec![(0u32, 0u32); parts.len()];
    let total_rows: u64 = usage.values().map(|u| u.0 as u64).sum();
    for (p, (n, cs)) in &usage {
        let (target, stage, printed) = match map_shape(p) {
            Some((j, st)) => (Some(j), st, false),
            None => match print_parent.get(p).and_then(|par| map_shape(par)) {
                Some((j, _)) => (Some(j), "print → base shape", true),
                None => (None, "unmapped", false),
            },
        };
        *stage_rows.entry(stage).or_default() += *n as u64;
        *stage_parts.entry(stage).or_default() += 1;
        if let Some(j) = target {
            if printed {
                pops[j].1 += n;
            } else {
                pops[j].0 += n;
            }
            colors[j].extend(cs.iter().filter_map(|c| rb_color.get(c)));
        }
    }
    for (p, (a, b)) in parts.iter_mut().zip(pops) {
        p.pop = a;
        p.print_pop = b;
    }
    let mut named = 0;
    for p in parts.iter_mut() {
        let hit = rb_parts
            .get(&p.id)
            .or_else(|| strip_letter(&p.id).and_then(|s| rb_parts.get(&s)));
        if let Some((name, cat)) = hit {
            p.rb_name = Some(name.clone());
            p.rb_cat = Some(cat.clone());
            named += 1;
        }
    }
    for (p, c) in parts.iter_mut().zip(colors) {
        let mut v: Vec<i32> = c.into_iter().collect();
        v.sort();
        p.colors = v;
    }

    // Colour words for queries, from LDraw names plus Rebrickable names that mapped.
    let mut cw: HashMap<i32, HashSet<String>> = HashMap::new();
    for (code, c) in &lib.colors.by_code {
        if matches!(code, 16 | 24) {
            continue;
        }
        cw.entry(*code)
            .or_default()
            .insert(color_words(&c.ldraw_name).join(" "));
        cw.entry(*code)
            .or_default()
            .insert(color_words(&c.name).join(" "));
    }
    let (cols, mut r) = csv_rows(rb_dir, "colors")?;
    for rec in r.records() {
        let rec = rec?;
        if let Ok(id) = rec[cols["id"]].parse::<i32>()
            && let Some(code) = rb_color.get(&id)
        {
            cw.entry(*code)
                .or_default()
                .insert(color_words(&rec[cols["name"]]).join(" "));
        }
    }
    for (alias, code) in [
        ("lbg", 71),
        ("light gray", 71),
        ("dbg", 72),
        ("dark gray", 72),
        ("gray", 71),
        ("clear", 47),
        ("transparent", 47),
        ("trans", 47),
    ] {
        cw.entry(code).or_default().insert(alias.to_string());
    }
    let mut color_list: Vec<(i32, Vec<String>)> = cw
        .into_iter()
        .map(|(k, v)| (k, v.into_iter().filter(|s| !s.is_empty()).collect()))
        .collect();
    color_list.sort();

    let pct = |n: u64| format!("{:.1}%", 100.0 * n as f64 / total_rows.max(1) as f64);
    let stats = serde_json::json!({
        "ldraw_parts": parts.len(),
        "ldraw_hidden_or_moved": parts.iter().filter(|p| p.hidden()).count(),
        "with_rebrickable_name": named,
        "with_popularity": parts.iter().filter(|p| p.pop > 0).count(),
        "with_colours": parts.iter().filter(|p| !p.colors.is_empty()).count(),
        "header_read_ms": header_ms.round(),
        "inventory_read_ms": inv_ms.round(),
        "rb_parts_used": usage.len(),
        "rb_inventory_rows": total_rows,
        "inventory_rows_by_stage": stage_rows.iter().map(|(k, v)| (k.to_string(), pct(*v))).collect::<HashMap<_, _>>(),
        "rb_parts_by_stage": stage_parts.iter().map(|(k, v)| (k.to_string(), *v)).collect::<HashMap<_, _>>(),
        "rb_colours_by_name": by_name,
        "rb_colours_by_rgb": by_rgb,
        "rb_colours_unmapped": unmapped_colors,
    });
    Ok(Catalog {
        parts,
        colors: color_list,
        stats,
    })
}
