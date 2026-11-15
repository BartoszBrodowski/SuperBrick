//! `pattern` and `fill`: repeated parts as one edit op, added (and checked) as one batch.
//!
//! pattern: a cell (one part, a list of parts, or LDraw lines, placed relative to `at`) repeated
//! along one to three directions, with an optional running-bond offset on alternate rows.
//! fill: a box filled with one brick size, layer by layer, optionally in running bond with
//! smaller `ends` parts closing the row ends.
//! Both take colour lists (cycled per part or per row, or picked at random from a seed) and skip
//! boxes (doors, windows).
use super::edit::{colour_arg, colour_code, ldraw_specs, spec};
use crate::args::{self, Result, Units};
use engine::PartSpec;
use glam::{DMat3, DVec3};
use ldraw::{Library, Lod};
use serde_json::Value;

/// At most this many parts from one op.
pub const MAX_PARTS: usize = 5000;

/// The parts a `pattern` or `fill` op adds, and a short note ("8 x 10 = 80, 4 skipped").
pub fn expand(lib: &Library, op: &Value, units: Units, colour: i32) -> Result<(Vec<PartSpec>, String)> {
    let kind = args::str_arg(op, "op").unwrap_or("pattern").to_ascii_lowercase();
    let colours = Colours::from_op(op)?;
    let skip = skip_boxes(op, units)?;
    let (cells, note) = if kind == "fill" { fill(lib, op, units, colour)? } else { pattern(op, units, colour)? };
    let mut out = Vec::new();
    let mut skipped = 0;
    for (n, (mut part, row)) in cells.into_iter().enumerate() {
        if !skip.is_empty() {
            if let Some((lo, hi)) = body_box(lib, &part.name, part.pos, part.rot) {
                if skip.iter().any(|(a, b)| overlaps(lo, hi, *a, *b)) {
                    skipped += 1;
                    continue;
                }
            }
        }
        if let Some(c) = colours.pick(n, row) {
            part.color = c;
        }
        out.push(part);
    }
    if out.len() > MAX_PARTS {
        return Err(format!("{kind} makes {} parts; at most {MAX_PARTS} per op", out.len()));
    }
    if out.is_empty() {
        return Err(format!("{kind} makes no parts ({note}{})", if skipped > 0 { format!(", all {skipped} skipped") } else { String::new() }));
    }
    let note = if skipped > 0 { format!("{note}, {skipped} skipped") } else { note };
    Ok((out, note))
}

/// pattern: {part|parts|ldraw, at, along:[{by, count}, ...], bond?:{offset, row?}, ...}
fn pattern(op: &Value, units: Units, colour: i32) -> Result<(Vec<(PartSpec, usize)>, String)> {
    let at = match op.get("at") {
        Some(v) => args::point(v, units)?,
        None => DVec3::ZERO,
    };
    // The cell: parts relative to the cell origin.
    let cell: Vec<PartSpec> = match (op.get("ldraw"), op.get("parts").and_then(Value::as_array)) {
        (Some(lines), _) => ldraw_specs(lines, colour_arg(op, colour)?)?,
        (None, Some(list)) => list.iter().map(|p| spec(p, units, colour_arg(op, colour)?)).collect::<Result<_>>()?,
        (None, None) => {
            let mut o = op.clone();
            o["at"] = serde_json::json!([0, 0, 0]);
            vec![spec(&o, units, colour)?]
        }
    };
    let along = op.get("along").and_then(Value::as_array).ok_or(
        "pattern needs \"along\": [{by:[dx,dy,dz], count:N}, ...] (one to three directions)",
    )?;
    if along.is_empty() || along.len() > 3 {
        return Err("pattern \"along\" takes one to three directions".into());
    }
    let mut dirs = Vec::new();
    for d in along {
        let by = args::point(d.get("by").ok_or("each \"along\" entry needs \"by\": [dx,dy,dz]")?, units)?;
        let count = d.get("count").and_then(Value::as_u64).ok_or("each \"along\" entry needs \"count\"")? as usize;
        if count == 0 {
            return Err("pattern counts must be at least 1".into());
        }
        dirs.push((by, count));
    }
    let total: usize = dirs.iter().map(|d| d.1).product::<usize>().saturating_mul(cell.len());
    if total > MAX_PARTS {
        return Err(format!("pattern makes {total} parts; at most {MAX_PARTS} per op"));
    }
    // Running bond: rows with an odd index along direction `row` (default: the last) shift by `offset`.
    let (bond, bond_row) = match op.get("bond") {
        None | Some(Value::Null) | Some(Value::Bool(false)) => (None, dirs.len() - 1),
        Some(b) => {
            let offset = args::point(b.get("offset").ok_or("bond needs \"offset\": [dx,dy,dz]")?, units)?;
            let row = b.get("row").and_then(Value::as_u64).map(|r| r as usize).unwrap_or(dirs.len() - 1);
            if row >= dirs.len() {
                return Err(format!("bond row {row} is not one of the {} directions (0-based)", dirs.len()));
            }
            (Some(offset), row)
        }
    };
    let row_dir = if op.get("bond").is_some() { bond_row } else { dirs.len() - 1 };
    let mut out = Vec::new();
    let mut index = vec![0usize; dirs.len()];
    loop {
        let mut origin = at;
        for (k, (by, _)) in dirs.iter().enumerate() {
            origin += *by * index[k] as f64;
        }
        if let Some(offset) = bond {
            if index[bond_row] % 2 == 1 {
                origin += offset;
            }
        }
        for p in &cell {
            out.push((PartSpec { pos: origin + p.pos, ..p.clone() }, index[row_dir]));
        }
        // Next index, the first direction fastest.
        let mut k = 0;
        loop {
            if k == dirs.len() {
                let counts: Vec<String> = dirs.iter().map(|d| d.1.to_string()).collect();
                let note = format!("pattern {} = {}", counts.join(" x "), out.len());
                return Ok((out, note));
            }
            index[k] += 1;
            if index[k] < dirs[k].1 {
                break;
            }
            index[k] = 0;
            k += 1;
        }
    }
}

/// fill: {part, from, to, rot?, bond?:true, ends?:"3004"}: layers of `part` filling the box.
fn fill(lib: &Library, op: &Value, units: Units, colour: i32) -> Result<(Vec<(PartSpec, usize)>, String)> {
    let name = args::part_name(args::str_arg(op, "part").ok_or("fill needs \"part\" (the brick to fill with)")?);
    let rot = match op.get("rot") {
        None | Some(Value::Null) => DMat3::IDENTITY,
        Some(r) => args::rotation(r)?,
    };
    let color = colour_arg(op, colour)?;
    let a = args::point(op.get("from").ok_or("fill needs \"from\" and \"to\": opposite corners of the box")?, units)?;
    let b = args::point(op.get("to").ok_or("fill needs \"from\" and \"to\": opposite corners of the box")?, units)?;
    let (lo, hi) = (a.min(b), a.max(b));
    let (plo, phi) = body_box(lib, &name, DVec3::ZERO, rot).ok_or_else(|| format!("{name}: no geometry to measure"))?;
    let size = phi - plo;
    if size.min_element() < 1.0 {
        return Err(format!("{name} has no volume to fill with"));
    }
    let ends = match args::str_arg(op, "ends") {
        Some(e) => {
            let e = args::part_name(e);
            let (elo, ehi) = body_box(lib, &e, DVec3::ZERO, rot).ok_or_else(|| format!("{e}: no geometry to measure"))?;
            Some((e, elo, ehi - elo))
        }
        None => None,
    };
    // Rows run along the longer horizontal side of the part; `across` is the other one.
    let (long, across) = if size.x >= size.z { (0usize, 2usize) } else { (2, 0) };
    let span = hi - lo;
    let fits = |extent: f64, step: f64| ((extent + 0.5) / step).floor().max(0.0) as usize;
    let layers = fits(span.y, size.y);
    let lines = fits(span[across], size[across]);
    if layers == 0 || lines == 0 || fits(span[long], size[long]) == 0 {
        return Err(format!(
            "{name} ({:.0} x {:.0} x {:.0} LDU) does not fit in the box ({:.0} x {:.0} x {:.0} LDU)",
            size.x, size.y, size.z, span.x, span.y, span.z
        ));
    }
    let bond = op.get("bond").and_then(Value::as_bool).unwrap_or(false);
    let mut out = Vec::new();
    let mut gaps = 0usize;
    for layer in 0..layers {
        // y points down: layer 0 is the bottom of the box.
        let top = hi.y - (layer + 1) as f64 * size.y;
        for line in 0..lines {
            let side = lo[across] + line as f64 * size[across];
            let place = |name: &str, low: DVec3, start: f64| {
                let mut corner = DVec3::ZERO;
                corner[long] = start;
                corner[across] = side;
                corner.y = top;
                PartSpec { name: name.to_string(), color, pos: corner - low, rot, id: None }
            };
            // Segments along the row: [start, end) to fill with ends parts, main parts between.
            let mut x = lo[long];
            let shift = if bond && layer % 2 == 1 { size[long] / 2.0 } else { 0.0 };
            let mut close = |from: f64, to: f64, out: &mut Vec<(PartSpec, usize)>| {
                let mut x = from;
                if let Some((e, elo, esize)) = &ends {
                    while x + esize[long] <= to + 0.5 {
                        out.push((place(e, *elo, x), layer));
                        x += esize[long];
                    }
                }
                if to - x > 0.5 {
                    gaps += 1;
                }
            };
            if shift > 0.0 {
                close(x, x + shift, &mut out);
                x += shift;
            }
            while x + size[long] <= hi[long] + 0.5 {
                out.push((place(&name, plo, x), layer));
                x += size[long];
            }
            close(x, hi[long], &mut out);
        }
    }
    let mut note = format!("fill {layers} layer{} of {name}{} = {}", if layers == 1 { "" } else { "s" }, if bond { " in running bond" } else { "" }, out.len());
    if gaps > 0 {
        note.push_str(&format!(", {gaps} row end{} left open{}", if gaps == 1 { "" } else { "s" }, if ends.is_none() { " (give ends:\"PART\" to close them)" } else { "" }));
    }
    Ok((out, note))
}

/// Colour lists: `colors:[...]` cycled per part (`colors_by:"row"`: per row), or picked at
/// random with `seed`.
struct Colours {
    list: Vec<i32>,
    by_row: bool,
    seed: Option<u64>,
}

impl Colours {
    fn from_op(op: &Value) -> Result<Colours> {
        let list = match op.get("colors").or_else(|| op.get("colours")) {
            None | Some(Value::Null) => Vec::new(),
            Some(Value::Array(a)) => a
                .iter()
                .map(|c| match c {
                    Value::String(s) => colour_code(s),
                    c => c.as_i64().map(|c| c as i32).ok_or_else(|| "colors are LDraw numbers or names".to_string()),
                })
                .collect::<Result<_>>()?,
            Some(_) => return Err("colors must be a list of colours".into()),
        };
        let by_row = match args::str_arg(op, "colors_by") {
            None | Some("part") | Some("cell") => false,
            Some("row") => true,
            Some(o) => return Err(format!("colors_by must be \"part\" or \"row\", not {o:?}")),
        };
        Ok(Colours { list, by_row, seed: op.get("seed").and_then(Value::as_u64) })
    }

    fn pick(&self, n: usize, row: usize) -> Option<i32> {
        if self.list.is_empty() {
            return None;
        }
        let k = if self.by_row { row } else { n };
        let i = match self.seed {
            Some(seed) => (splitmix(seed ^ (k as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15)) % self.list.len() as u64) as usize,
            None => k % self.list.len(),
        };
        Some(self.list[i])
    }
}

fn splitmix(mut z: u64) -> u64 {
    z = z.wrapping_add(0x9E37_79B9_7F4A_7C15);
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// `skip:[{from, to}, ...]`: boxes in the edit's units.
fn skip_boxes(op: &Value, units: Units) -> Result<Vec<(DVec3, DVec3)>> {
    let Some(list) = op.get("skip") else { return Ok(Vec::new()) };
    let list = list.as_array().ok_or("skip is a list of boxes: [{from:[x,y,z], to:[x,y,z]}]")?;
    list.iter()
        .map(|b| {
            let a = args::point(b.get("from").ok_or("each skip box needs \"from\" and \"to\"")?, units)?;
            let c = args::point(b.get("to").ok_or("each skip box needs \"from\" and \"to\"")?, units)?;
            Ok((a.min(c), a.max(c)))
        })
        .collect()
}

/// Boxes overlapping by more than 0.5 LDU on every axis (touching faces do not count).
fn overlaps(alo: DVec3, ahi: DVec3, blo: DVec3, bhi: DVec3) -> bool {
    (0..3).all(|k| ahi[k].min(bhi[k]) - alo[k].max(blo[k]) > 0.5)
}

/// World box of a placed part's body, studs left out.
fn body_box(lib: &Library, name: &str, pos: DVec3, rot: DMat3) -> Option<(DVec3, DVec3)> {
    let mesh = lib.mesh(name, Lod::Low).ok()?;
    let (lo, hi) = mesh.bbox(true);
    if !lo.is_finite() || !hi.is_finite() || lo == hi {
        return None;
    }
    let (lo, hi) = (lo.as_dvec3(), hi.as_dvec3());
    let mut a = DVec3::splat(f64::INFINITY);
    let mut b = DVec3::splat(f64::NEG_INFINITY);
    for i in 0..8 {
        let c = DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let w = rot * c + pos;
        a = a.min(w);
        b = b.max(w);
    }
    // Round away float noise so pitches come out exact (80, 24, 40 ...).
    let r = |v: DVec3| (v * 100.0).round() / 100.0;
    Some((r(a), r(b)))
}
