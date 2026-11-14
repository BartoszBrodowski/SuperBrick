//! LEGO element IDs (the 6–7 digit numbers in instruction-booklet parts lists, each one a part in
//! one colour) and set inventories, from Rebrickable's data, mapped onto LDraw parts and colours.
//!
//! `build` turns the Rebrickable CSVs into `out/sets.bin` (offline, with `sb-dev search-sets`);
//! `Sets::get` loads it lazily at run time.

use std::{
    collections::HashMap,
    error::Error,
    io::{Read, Write},
    path::Path,
    sync::OnceLock,
};

use ldraw::Library;
use serde::Serialize;

use crate::catalog::{self, Catalog, Shapes};

const MAGIC: &[u8] = b"SBSETS1\n";
type Res<T> = Result<T, Box<dyn Error>>;

/// A part in a colour, as LDraw sees it.
#[derive(Serialize, Clone, Debug, PartialEq)]
pub struct Element {
    /// The LEGO element ID, when the item came from one.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub element: Option<String>,
    /// LDraw part number (no ".dat"); None when no LDraw part matches.
    pub part: Option<String>,
    /// The item is printed; `part` is then the unprinted base shape.
    pub printed: bool,
    /// LDraw colour code; None when the colour has no LDraw equivalent.
    pub color: Option<i32>,
    pub color_name: String,
    /// Rebrickable's part number and colour id.
    pub rb_part: String,
    pub rb_color: i32,
}

/// One line of a set inventory.
#[derive(Serialize, Clone, Debug)]
pub struct Row {
    #[serde(flatten)]
    pub item: Element,
    pub qty: u32,
    pub spare: bool,
    /// Element IDs that make this part in this colour (what the booklet's parts list prints).
    pub elements: Vec<String>,
}

#[derive(Serialize, Clone, Debug)]
pub struct Inventory {
    pub set: String,
    pub name: String,
    pub year: u32,
    /// Rebrickable's piece count for the set.
    pub num_parts: u32,
    pub rows: Vec<Row>,
}

struct RbPart {
    num: String,
    ldraw: Option<String>,
    printed: bool,
}

struct SetRec {
    num: String,
    name: String,
    year: u32,
    num_parts: u32,
    /// (part index, Rebrickable colour, quantity, spare)
    rows: Vec<(u32, i32, u32, bool)>,
}

pub struct Sets {
    parts: Vec<RbPart>,
    colors: HashMap<i32, (Option<i32>, String)>,
    elements: HashMap<String, (u32, i32)>,
    by_item: HashMap<(u32, i32), Vec<String>>,
    /// (LDraw part without ".dat", LDraw colour) → element IDs, ascending.
    by_ldraw: HashMap<(String, i32), Vec<String>>,
    sets: Vec<SetRec>,
    by_num: HashMap<String, usize>,
}

/// Every token that looks like an element ID (6–8 digits; shorter numbers are part numbers).
pub fn element_tokens(query: &str) -> impl Iterator<Item = &str> {
    query
        .split(|c: char| !c.is_ascii_digit())
        .filter(|t| (6..=8).contains(&t.len()))
}

impl Sets {
    /// The installed table (`out/sets.bin` in the search data), loaded once; None if missing.
    pub fn get() -> Option<&'static Sets> {
        static SETS: OnceLock<Option<Sets>> = OnceLock::new();
        SETS.get_or_init(|| Sets::load(&crate::data_dir().join("out/sets.bin")).ok())
            .as_ref()
    }

    pub fn load(path: &Path) -> Res<Sets> {
        let bytes = std::fs::read(path)?;
        let body = bytes.strip_prefix(MAGIC).ok_or("not a SuperBrick sets table")?;
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(body).read_to_end(&mut raw)?;
        let mut r = Reader { b: &raw, i: 0 };
        let mut colors = HashMap::new();
        for _ in 0..r.u()? {
            let id = r.i()? as i32;
            let code = r.i()?;
            let name = r.s()?;
            colors.insert(id, ((code != NONE).then_some(code as i32), name));
        }
        let mut parts = Vec::new();
        for _ in 0..r.u()? {
            let num = r.s()?;
            let ldraw = r.s()?;
            let printed = r.u()? == 1;
            parts.push(RbPart {
                num,
                ldraw: (!ldraw.is_empty()).then_some(ldraw),
                printed,
            });
        }
        let mut elements = HashMap::new();
        let mut by_item: HashMap<(u32, i32), Vec<String>> = HashMap::new();
        for _ in 0..r.u()? {
            let id = r.s()?;
            let part = r.u()? as u32;
            let color = r.i()? as i32;
            by_item.entry((part, color)).or_default().push(id.clone());
            elements.insert(id, (part, color));
        }
        let mut sets = Vec::new();
        for _ in 0..r.u()? {
            let num = r.s()?;
            let name = r.s()?;
            let year = r.u()? as u32;
            let num_parts = r.u()? as u32;
            let n = r.u()?;
            let mut rows = Vec::with_capacity(n as usize);
            for _ in 0..n {
                rows.push((r.u()? as u32, r.i()? as i32, r.u()? as u32, r.u()? == 1));
            }
            sets.push(SetRec {
                num,
                name,
                year,
                num_parts,
                rows,
            });
        }
        let by_num = sets
            .iter()
            .enumerate()
            .map(|(i, s)| (s.num.to_lowercase(), i))
            .collect();
        let mut by_ldraw: HashMap<(String, i32), Vec<String>> = HashMap::new();
        for (&(part, color), ids) in &by_item {
            let (Some(ldraw), Some(code)) = (
                parts[part as usize].ldraw.as_ref(),
                colors.get(&color).and_then(|c| c.0),
            ) else {
                continue;
            };
            by_ldraw
                .entry((ldraw.clone(), code))
                .or_default()
                .extend(ids.iter().cloned());
        }
        for ids in by_ldraw.values_mut() {
            ids.sort_by_key(|id| (id.len(), id.clone()));
            ids.dedup();
        }
        Ok(Sets {
            parts,
            colors,
            elements,
            by_item,
            by_ldraw,
            sets,
            by_num,
        })
    }

    fn item(&self, part: u32, color: i32, element: Option<&str>) -> Element {
        let p = &self.parts[part as usize];
        let (code, name) = self
            .colors
            .get(&color)
            .cloned()
            .unwrap_or((None, format!("Rebrickable colour {color}")));
        Element {
            element: element.map(str::to_string),
            part: p.ldraw.clone(),
            printed: p.printed,
            color: code,
            color_name: name,
            rb_part: p.num.clone(),
            rb_color: color,
        }
    }

    /// What a LEGO element ID is: the LDraw part and colour.
    pub fn element(&self, id: &str) -> Option<Element> {
        let id = id.trim();
        let &(part, color) = self.elements.get(id)?;
        Some(self.item(part, color, Some(id)))
    }

    /// LEGO element IDs for an LDraw part (with or without ".dat") in an LDraw colour,
    /// ascending (newest last). Empty when Rebrickable lists none.
    pub fn elements_for_ldraw(&self, part: &str, color: i32) -> &[String] {
        let part = part.trim_end_matches(".dat");
        self.by_ldraw
            .get(&(part.to_string(), color))
            .map_or(&[], Vec::as_slice)
    }

    /// The element IDs among the query's tokens, resolved.
    pub fn elements_in(&self, query: &str) -> Vec<Element> {
        element_tokens(query).filter_map(|t| self.element(t)).collect()
    }

    /// The set's full number: "40809" → "40809-1"; "40809-1" stays.
    pub fn resolve(&self, num: &str) -> Option<&str> {
        let n = num.trim().to_lowercase();
        let n = n.strip_prefix("set").unwrap_or(&n).trim();
        if let Some(&i) = self.by_num.get(n) {
            return Some(&self.sets[i].num);
        }
        self.by_num
            .get(&format!("{n}-1"))
            .map(|&i| self.sets[i].num.as_str())
    }

    /// A set's parts (Rebrickable's newest inventory version; minifigures are not included).
    pub fn inventory(&self, num: &str) -> Option<Inventory> {
        let full = self.resolve(num)?;
        let s = &self.sets[self.by_num[&full.to_lowercase()]];
        let rows = s
            .rows
            .iter()
            .map(|&(part, color, qty, spare)| Row {
                item: self.item(part, color, None),
                qty,
                spare,
                elements: self.by_item.get(&(part, color)).cloned().unwrap_or_default(),
            })
            .collect();
        Some(Inventory {
            set: s.num.clone(),
            name: s.name.clone(),
            year: s.year,
            num_parts: s.num_parts,
            rows,
        })
    }

    /// Sets whose name contains every word: (number, name, year).
    pub fn find(&self, words: &str, limit: usize) -> Vec<(String, String, u32)> {
        let words: Vec<String> = words.split_whitespace().map(str::to_lowercase).collect();
        self.sets
            .iter()
            .filter(|s| {
                let name = s.name.to_lowercase();
                !words.is_empty() && words.iter().all(|w| name.contains(w.as_str()))
            })
            .take(limit)
            .map(|s| (s.num.clone(), s.name.clone(), s.year))
            .collect()
    }
}

const NONE: i64 = -999_999;

struct Reader<'a> {
    b: &'a [u8],
    i: usize,
}

impl Reader<'_> {
    fn u(&mut self) -> Res<u64> {
        let mut v = 0u64;
        for shift in (0..64).step_by(7) {
            let byte = *self.b.get(self.i).ok_or("truncated sets table")?;
            self.i += 1;
            v |= ((byte & 0x7f) as u64) << shift;
            if byte & 0x80 == 0 {
                return Ok(v);
            }
        }
        Err("bad varint in sets table".into())
    }
    fn i(&mut self) -> Res<i64> {
        let v = self.u()?;
        Ok(((v >> 1) as i64) ^ -((v & 1) as i64))
    }
    fn s(&mut self) -> Res<String> {
        let n = self.u()? as usize;
        let s = self.b.get(self.i..self.i + n).ok_or("truncated sets table")?;
        self.i += n;
        Ok(String::from_utf8(s.to_vec())?)
    }
}

#[derive(Default)]
struct Writer(Vec<u8>);

impl Writer {
    fn u(&mut self, mut v: u64) {
        loop {
            let byte = (v & 0x7f) as u8;
            v >>= 7;
            if v == 0 {
                self.0.push(byte);
                return;
            }
            self.0.push(byte | 0x80);
        }
    }
    fn i(&mut self, v: i64) {
        self.u(((v << 1) ^ (v >> 63)) as u64);
    }
    fn s(&mut self, s: &str) {
        self.u(s.len() as u64);
        self.0.extend_from_slice(s.as_bytes());
    }
}

/// Build the table from Rebrickable's CSV dumps (colors, parts, part_relationships, elements,
/// sets, inventories, inventory_parts) and the part catalogue. Returns the file bytes and stats.
pub fn build(lib: &Library, rb_dir: &Path, cat: &Catalog) -> Res<(Vec<u8>, serde_json::Value)> {
    let by_id: HashMap<String, usize> = cat
        .parts
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.clone(), i))
        .collect();
    let (rb_color, ..) = catalog::rb_colours(lib, rb_dir)?;
    let (mold, print_parent) = catalog::relationships(rb_dir)?;
    let shapes = Shapes {
        by_id: &by_id,
        parts: &cat.parts,
        mold: &mold,
    };

    let mut w = Writer::default();
    let (cols, mut r) = catalog::csv_rows(rb_dir, "colors")?;
    let mut colors = Vec::new();
    for rec in r.records() {
        let rec = rec?;
        let id: i32 = rec[cols["id"]].parse()?;
        colors.push((id, rec[cols["name"]].to_string()));
    }
    w.u(colors.len() as u64);
    for (id, name) in &colors {
        w.i(*id as i64);
        w.i(rb_color.get(id).map_or(NONE, |c| *c as i64));
        w.s(name);
    }

    let mut parts: Vec<(String, Option<String>, bool)> = Vec::new();
    let mut index: HashMap<String, u32> = HashMap::new();
    let mut intern = |num: &str| -> u32 {
        if let Some(&i) = index.get(num) {
            return i;
        }
        let (target, _, printed) = shapes.map_or_print(&num.to_lowercase(), &print_parent);
        let ldraw = target.map(|j| cat.parts[j].id.clone());
        parts.push((num.to_string(), ldraw, printed));
        index.insert(num.to_string(), parts.len() as u32 - 1);
        parts.len() as u32 - 1
    };

    let (cols, mut r) = catalog::csv_rows(rb_dir, "elements")?;
    let mut elements = Vec::new();
    for rec in r.records() {
        let rec = rec?;
        let Ok(color) = rec[cols["color_id"]].parse::<i32>() else {
            continue;
        };
        elements.push((
            rec[cols["element_id"]].to_string(),
            intern(&rec[cols["part_num"]]),
            color,
        ));
    }
    elements.sort();

    let (cols, mut r) = catalog::csv_rows(rb_dir, "sets")?;
    let mut sets: Vec<SetRec> = Vec::new();
    for rec in r.records() {
        let rec = rec?;
        sets.push(SetRec {
            num: rec[cols["set_num"]].to_string(),
            name: rec[cols["name"]].to_string(),
            year: rec[cols["year"]].parse().unwrap_or(0),
            num_parts: rec[cols["num_parts"]].parse().unwrap_or(0),
            rows: Vec::new(),
        });
    }
    sets.sort_by(|a, b| a.num.cmp(&b.num));
    let set_index: HashMap<String, usize> = sets
        .iter()
        .enumerate()
        .map(|(i, s)| (s.num.clone(), i))
        .collect();
    let (cols, mut r) = catalog::csv_rows(rb_dir, "inventories")?;
    let mut newest: HashMap<usize, (u32, String)> = HashMap::new();
    for rec in r.records() {
        let rec = rec?;
        let Some(&s) = set_index.get(&rec[cols["set_num"]]) else {
            continue;
        };
        let version: u32 = rec[cols["version"]].parse().unwrap_or(1);
        let id = rec[cols["id"]].to_string();
        if newest.get(&s).is_none_or(|(v, _)| version > *v) {
            newest.insert(s, (version, id));
        }
    }
    let wanted: HashMap<String, usize> = newest.into_iter().map(|(s, (_, id))| (id, s)).collect();
    let (cols, mut r) = catalog::csv_rows(rb_dir, "inventory_parts")?;
    let (ic, pc, cc, qc, sc) = (
        cols["inventory_id"],
        cols["part_num"],
        cols["color_id"],
        cols["quantity"],
        cols["is_spare"],
    );
    let mut rec = csv::StringRecord::new();
    let mut rows = 0usize;
    while r.read_record(&mut rec)? {
        let Some(&s) = wanted.get(&rec[ic]) else {
            continue;
        };
        let (Ok(color), Ok(qty)) = (rec[cc].parse::<i32>(), rec[qc].parse::<u32>()) else {
            continue;
        };
        let part = intern(&rec[pc]);
        sets[s].rows.push((part, color, qty, &rec[sc] == "True"));
        rows += 1;
    }

    w.u(parts.len() as u64);
    for (num, ldraw, printed) in &parts {
        w.s(num);
        w.s(ldraw.as_deref().unwrap_or(""));
        w.u(*printed as u64);
    }
    w.u(elements.len() as u64);
    for (id, part, color) in &elements {
        w.s(id);
        w.u(*part as u64);
        w.i(*color as i64);
    }
    w.u(sets.len() as u64);
    for s in &sets {
        w.s(&s.num);
        w.s(&s.name);
        w.u(s.year as u64);
        w.u(s.num_parts as u64);
        w.u(s.rows.len() as u64);
        for &(part, color, qty, spare) in &s.rows {
            w.u(part as u64);
            w.i(color as i64);
            w.u(qty as u64);
            w.u(spare as u64);
        }
    }
    let mut out = MAGIC.to_vec();
    let mut z = flate2::write::ZlibEncoder::new(&mut out, flate2::Compression::best());
    z.write_all(&w.0)?;
    z.finish()?;
    let mapped = parts.iter().filter(|p| p.1.is_some()).count();
    let stats = serde_json::json!({
        "colours": colors.len(),
        "colours_mapped": colors.iter().filter(|(id, _)| rb_color.contains_key(id)).count(),
        "parts": parts.len(),
        "parts_mapped": mapped,
        "elements": elements.len(),
        "sets": sets.len(),
        "inventory_rows": rows,
        "raw_bytes": w.0.len(),
    });
    Ok((out, stats))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn varints_round_trip() {
        let mut w = Writer::default();
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64] {
            w.u(v);
        }
        for v in [0i64, -1, 1, -999_999, 9999] {
            w.i(v);
        }
        w.s("40809-1");
        let mut r = Reader { b: &w.0, i: 0 };
        for v in [0u64, 1, 127, 128, 300, u32::MAX as u64] {
            assert_eq!(r.u().unwrap(), v);
        }
        for v in [0i64, -1, 1, -999_999, 9999] {
            assert_eq!(r.i().unwrap(), v);
        }
        assert_eq!(r.s().unwrap(), "40809-1");
    }

    #[test]
    fn element_tokens_skip_part_numbers() {
        let t: Vec<&str> = element_tokens("3001 4142155, 300121 and 30414").collect();
        assert_eq!(t, ["4142155", "300121"]);
    }
}
