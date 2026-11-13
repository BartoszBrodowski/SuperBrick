//! `search`: find parts in the library; the reply is a labelled thumbnail grid.
use crate::args::{self, Result};
use crate::shots;
use crate::{Reply, Session};
use glam::{DMat3, DVec3};
use serde_json::{json, Value};

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "query": {"type": "string", "description": "Words or part numbers: \"slope curved 2x1 inverted\", \"3001\", \"technic pin\"."},
            "similar": {"type": "string", "description": "Part number to find look-alikes of (instead of query)."},
            "color": {"type": "integer", "description": "Only parts known in this LDraw colour; thumbnails use it."},
            "limit": {"type": "integer", "description": "Results (default 12, max 36)."},
            "picture": {"type": "boolean", "description": "Return the thumbnail grid (default true)."}
        },
        "additionalProperties": false
    })
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply> {
    let limit = args::usize_arg(args, "limit", 12, 36).max(1);
    let color = args.get("color").and_then(Value::as_i64).map(|c| c as i32);
    let query = args::str_arg(args, "query").map(str::to_string);
    let similar = args::str_arg(args, "similar").map(|p| p.trim_end_matches(".dat").to_ascii_lowercase());
    let found: Vec<(String, String, String, u32)> = {
        let searcher = s.searcher()?;
        let hits = if let Some(id) = &similar {
            searcher.similar(id, limit)
        } else {
            let q = query.as_deref().ok_or("search needs \"query\" or \"similar\"")?;
            let filters = search::Filters { color, tags: Vec::new(), unknown_colors: color.is_none() };
            searcher.filtered(q, &filters, limit).0
        };
        hits.into_iter()
            .filter_map(|h| searcher.cat.parts.get(h.idx))
            .map(|p| (p.id.clone(), p.clean_desc().to_string(), p.category.clone(), p.pop))
            .collect()
    };
    let what = similar.as_ref().map(|p| format!("parts like {p}")).unwrap_or_else(|| format!("\"{}\"", query.unwrap_or_default()));
    if found.is_empty() {
        return Ok(Reply::fail(format!("No parts found for {what}."), json!({"parts": []})));
    }
    let colour = color.unwrap_or(71);
    let mut rows = Vec::new();
    let mut lines = Vec::new();
    let mut items = Vec::new();
    for (id, desc, category, pop) in &found {
        let name = format!("{id}.dat");
        let size = shots::part_box(&s.lib, &name, DVec3::ZERO, DMat3::IDENTITY).map(|(lo, hi)| hi - lo);
        let dims = size.map(|d| format!("{:.1}x{:.1} studs, {:.1} plates tall", d.x / 20.0, d.z / 20.0, d.y / 8.0));
        lines.push(format!("{id}: {desc}{}", dims.as_ref().map(|d| format!(" ({d})")).unwrap_or_default()));
        rows.push(json!({"part": id, "description": desc, "category": category, "popularity": pop,
                         "size_ldu": size.map(|d| [d.x.round(), d.y.round(), d.z.round()])}));
        items.push((name, colour, format!("{id}  {}", short(desc, 28))));
    }
    let mut text = format!("{} results for {what}: {}.", found.len(), lines.join("; "));
    let mut reply_images = None;
    if args.get("picture").and_then(Value::as_bool).unwrap_or(true) && s.images {
        let cols = if items.len() <= 4 { items.len() as u32 } else if items.len() <= 9 { 3 } else if items.len() <= 16 { 4 } else { 6 };
        let cell = if cols <= 3 { 240 } else if cols == 4 { 200 } else { 160 };
        match shots::thumbnail_grid(&s.lib, &items, cell, cols.max(1)) {
            Ok(p) => reply_images = Some(p.into()),
            Err(e) => text.push_str(&format!(" (no picture: {e})")),
        }
    }
    Ok(Reply::ok(text, json!({"parts": rows})).image(reply_images))
}

fn short(s: &str, n: usize) -> String {
    if s.chars().count() <= n {
        s.to_string()
    } else {
        format!("{}…", s.chars().take(n - 1).collect::<String>())
    }
}
