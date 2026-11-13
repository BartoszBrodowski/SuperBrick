//! `search`: find parts in the library by words, part number, LEGO element ID or set; the reply
//! is a labelled thumbnail grid.
use crate::args::{self, Result};
use crate::shots;
use crate::{Reply, Session};
use glam::{DMat3, DVec3};
use search::sets::{Element, Sets};
use serde_json::{json, Value};

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "Words, part numbers or LEGO element IDs: \"slope curved 2x1 inverted\", \"3001\", \"technic pin\", \"4142155\"."},
            "similar": {"type": "string", "description": "Part number to find look-alikes of (instead of query)."},
            "elements": {"type": ["array", "string"], "items": {"type": ["string", "integer"]},
                         "description": "LEGO element IDs (the 6-7 digit numbers in a booklet's parts list), up to 200: each comes back as LDraw part + colour, with thumbnails in that colour."},
            "set": {"type": "string", "description": "A set number (\"40809\" or \"40809-1\") for its full parts inventory as LDraw parts and colours, with each line's element IDs; or words to find set numbers by name (\"gingerbread house\")."},
            "color": {"type": "integer", "description": "Only parts known in this LDraw colour; thumbnails use it."},
            "limit": {"type": "integer", "description": "Results (default 12, max 36); for set and elements, thumbnails drawn (default 48, max 120)."},
            "picture": {"type": "boolean", "description": "Return the thumbnail grid (default true)."}
        },
        "additionalProperties": false
    })
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply> {
    if args.get("elements").is_some() {
        return elements(s, args);
    }
    if let Some(set) = args::str_arg(args, "set") {
        return set_inventory(s, args, set);
    }
    let limit = args::usize_arg(args, "limit", 12, 36).max(1);
    let color = args.get("color").and_then(Value::as_i64).map(|c| c as i32);
    let query = args::str_arg(args, "query").map(str::to_string);
    let similar = args::str_arg(args, "similar").map(|p| p.trim_end_matches(".dat").to_ascii_lowercase());
    let found: Vec<(String, String, String, u32)> = {
        let searcher = s.searcher()?;
        let hits = if let Some(id) = &similar {
            searcher.similar(id, limit)
        } else {
            let q = query.as_deref().ok_or("search needs \"query\", \"similar\", \"elements\" or \"set\"")?;
            let filters = search::Filters { color, tags: Vec::new(), unknown_colors: color.is_none() };
            searcher.filtered(q, &filters, limit).0
        };
        hits.into_iter()
            .filter_map(|h| searcher.cat.parts.get(h.idx))
            .map(|p| (p.id.clone(), p.clean_desc().to_string(), p.category.clone(), p.pop))
            .collect()
    };
    // Element IDs in the query: their parts come first, drawn in the element's colour.
    let resolved: Vec<Element> = query
        .as_deref()
        .filter(|q| search::sets::element_tokens(q).next().is_some())
        .and_then(|q| Sets::get().map(|t| t.elements_in(q)))
        .unwrap_or_default();
    let what = similar.as_ref().map(|p| format!("parts like {p}")).unwrap_or_else(|| format!("\"{}\"", query.unwrap_or_default()));
    if found.is_empty() {
        return Ok(Reply::fail(format!("No parts found for {what}."), json!({"parts": []})));
    }
    let colour = color.unwrap_or(71);
    let mut rows = Vec::new();
    let mut lines = Vec::new();
    let mut items = Vec::new();
    for (id, desc, category, pop) in &found {
        let element = resolved.iter().find(|e| e.part.as_deref() == Some(id.as_str()));
        let name = format!("{id}.dat");
        let size = shots::part_box(&s.lib, &name, DVec3::ZERO, DMat3::IDENTITY).map(|(lo, hi)| hi - lo);
        let dims = size.map(|d| format!("{:.1}x{:.1} studs, {:.1} plates tall", d.x / 20.0, d.z / 20.0, d.y / 8.0));
        let via = element.map(|e| format!("element {} = ", e.element.as_deref().unwrap_or("?"))).unwrap_or_default();
        let in_colour = element.map(|e| format!(" in {} ({})", e.color_name, code_text(e.color))).unwrap_or_default();
        lines.push(format!("{via}{id}: {desc}{in_colour}{}", dims.as_ref().map(|d| format!(" ({d})")).unwrap_or_default()));
        let mut row = json!({"part": id, "description": desc, "category": category, "popularity": pop,
                             "size_ldu": size.map(|d| [d.x.round(), d.y.round(), d.z.round()])});
        if let Some(e) = element {
            row["element"] = json!(e.element);
            row["color"] = json!(e.color);
            row["color_name"] = json!(e.color_name);
        }
        rows.push(row);
        items.push((name, element.and_then(|e| e.color).unwrap_or(colour), format!("{id}  {}", short(desc, 28))));
    }
    let mut text = format!("{} results for {what}: {}.", found.len(), lines.join("; "));
    let picture = picture(s, args, &items, &mut text);
    Ok(Reply::ok(text, json!({"parts": rows})).image(picture))
}

/// Many element IDs at once (a booklet's parts list): part + colour for each.
fn elements(s: &mut Session, args: &Value) -> Result<Reply> {
    let ids: Vec<String> = match args.get("elements") {
        Some(Value::Array(a)) => a.iter().filter_map(|v| v.as_str().map(str::to_string).or_else(|| v.as_u64().map(|n| n.to_string()))).collect(),
        Some(Value::String(t)) => t.split(|c: char| !c.is_ascii_digit()).filter(|t| !t.is_empty()).map(str::to_string).collect(),
        Some(Value::Number(n)) => vec![n.to_string()],
        _ => return Err("\"elements\" is a list of element IDs".into()),
    };
    if ids.is_empty() || ids.len() > 200 {
        return Err(format!("\"elements\" takes 1-200 element IDs (got {})", ids.len()));
    }
    let table = Sets::get().ok_or("the element table (search data out/sets.bin) is not installed")?;
    let mut rows = Vec::new();
    let mut lines = Vec::new();
    let mut items = Vec::new();
    let mut unknown = Vec::new();
    for id in &ids {
        let Some(e) = table.element(id) else {
            unknown.push(id.clone());
            continue;
        };
        let desc = e.part.as_deref().map(|p| describe(s, p)).unwrap_or_default();
        lines.push(format!("{id} = {}", item_text(&e, &desc)));
        if let Some(p) = &e.part {
            items.push((format!("{p}.dat"), e.color.unwrap_or(71), format!("{id} {p}")));
        }
        let mut row = json!(e);
        row["description"] = json!(desc);
        rows.push(row);
    }
    let mut text = format!("{} of {} element IDs found: {}.", rows.len(), ids.len(), lines.join("; "));
    if !unknown.is_empty() {
        text.push_str(&format!(" Not in the table (check the digits, or the part may be newer than the data): {}.", unknown.join(", ")));
    }
    let picture = picture(s, args, &items, &mut text);
    Ok(Reply { ok: !rows.is_empty(), ..Reply::ok(text, json!({"elements": rows, "unknown": unknown})) }.image(picture))
}

/// A set's inventory, or set numbers matching words.
fn set_inventory(s: &mut Session, args: &Value, set: &str) -> Result<Reply> {
    let table = Sets::get().ok_or("the set table (search data out/sets.bin) is not installed")?;
    let Some(inv) = table.inventory(set) else {
        let found = table.find(set, 20);
        if found.is_empty() {
            return Ok(Reply::fail(format!("No set \"{set}\"."), json!({"sets": []})));
        }
        let lines: Vec<String> = found.iter().map(|(n, name, y)| format!("{n} {name} ({y})")).collect();
        return Ok(Reply::ok(format!("Sets matching \"{set}\": {}.", lines.join("; ")),
                            json!({"sets": found.iter().map(|(n, name, y)| json!({"set": n, "name": name, "year": y})).collect::<Vec<_>>()})));
    };
    let mut rows = inv.rows.clone();
    rows.sort_by(|a, b| a.spare.cmp(&b.spare).then(b.qty.cmp(&a.qty)).then(a.item.part.cmp(&b.item.part)));
    let pieces: u32 = rows.iter().filter(|r| !r.spare).map(|r| r.qty).sum();
    let spares: u32 = rows.iter().filter(|r| r.spare).map(|r| r.qty).sum();
    let mut lines = Vec::new();
    let mut items = Vec::new();
    let mut booklet = Vec::new();
    let mut flagged = Vec::new();
    for r in rows.iter().filter(|r| !r.spare) {
        let e = &r.item;
        lines.push(format!("{}× {}", r.qty, item_text(e, "")));
        match (&e.part, e.color) {
            (Some(p), Some(c)) => {
                booklet.push(json!({"part": p, "color": c, "qty": r.qty}));
                items.push((format!("{p}.dat"), c, format!("{}× {p}", r.qty)));
            }
            _ => flagged.push(format!("{} ({})", e.rb_part, e.color_name)),
        }
    }
    let mut text = format!(
        "Set {} \"{}\" ({}): {} lines, {pieces} pieces + {spares} spares (Rebrickable counts {}; minifigure parts are not listed). {}.",
        inv.set, inv.name, inv.year, rows.iter().filter(|r| !r.spare).count(), inv.num_parts, lines.join("; ")
    );
    if !flagged.is_empty() {
        text.push_str(&format!(" No LDraw part or colour for: {}.", flagged.join(", ")));
    }
    text.push_str(" Printed parts are given as their unprinted base. data.booklet_inventory is ready for research/booklet_inventory.json.");
    let picture = picture(s, args, &items, &mut text);
    Ok(Reply::ok(text, json!({"set": inv.set, "name": inv.name, "year": inv.year, "rows": rows,
                              "booklet_inventory": {"parts": booklet}})).image(picture))
}

fn item_text(e: &Element, desc: &str) -> String {
    let part = match &e.part {
        Some(p) if e.printed => format!("{p} (print {}, unprinted base shown)", e.rb_part),
        Some(p) => p.clone(),
        None => format!("no LDraw part (Rebrickable {})", e.rb_part),
    };
    let desc = if desc.is_empty() { String::new() } else { format!(" {desc}") };
    format!("{part}{desc} {} ({})", e.color_name, code_text(e.color))
}

fn code_text(c: Option<i32>) -> String {
    c.map(|c| format!("colour {c}")).unwrap_or_else(|| "no LDraw colour".into())
}

fn describe(s: &mut Session, part: &str) -> String {
    s.searcher()
        .ok()
        .and_then(|sr| sr.exact_matches(part).first().map(|&i| short(sr.cat.parts[i].clean_desc(), 40)))
        .unwrap_or_default()
}

fn picture(s: &mut Session, args: &Value, items: &[(String, i32, String)], text: &mut String) -> Option<crate::Image> {
    if !args.get("picture").and_then(Value::as_bool).unwrap_or(true) || !s.images || items.is_empty() {
        return None;
    }
    let cap = if args.get("set").is_some() || args.get("elements").is_some() { args::usize_arg(args, "limit", 48, 120) } else { items.len() };
    let items = &items[..items.len().min(cap.max(1))];
    let cols = if items.len() <= 4 { items.len() as u32 } else if items.len() <= 9 { 3 } else if items.len() <= 16 { 4 } else if items.len() <= 36 { 6 } else { 8 };
    let cell = if cols <= 3 { 240 } else if cols == 4 { 200 } else if cols == 6 { 160 } else { 140 };
    match shots::thumbnail_grid(&s.lib, items, cell, cols.max(1)) {
        Ok(p) => Some(p.into()),
        Err(e) => {
            text.push_str(&format!(" (no picture: {e})"));
            None
        }
    }
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}
