//! The tools. Each takes JSON arguments and returns a [`Reply`]; `TOOLS` describes them for MCP
//! and `sb help`.
mod edit;
mod find;
mod look;
mod project;
mod publish;
mod select;
mod verify;
mod where_;

use crate::args::Result;
use crate::{Reply, Session};
use engine::{Id, Model};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub(crate) use verify::problems;

pub struct Tool {
    pub name: &'static str,
    pub summary: &'static str,
    pub schema: fn() -> Value,
}

const UNITS: &str = "\"ldu\" (default; LDraw units, y down) or \"grid\" (x,z in studs, y in plates, y up)";

pub const TOOLS: &[Tool] = &[
    Tool {
        name: "edit",
        summary: "Change the model with a batch of operations in one atomic proposal: add, place, move, rotate, mirror, copy, remove, paint, connect, hinge, group, subbuild, hide, lock. Every batch is checked for collisions; the reply shows the result from a view where the change is visible. Agents' edits are drafts the human accepts; a failed batch changes nothing and the reply shows why (clearance picture) and the nearest legal alternatives.",
        schema: edit::schema,
    },
    Tool {
        name: "where",
        summary: "Find where a part (or the selection, ids) can legally go near a point or parts: returns numbered candidate poses as a picture with ghosts and labels, saved as a candidate set (\"c3\"). Place one with edit {op:\"place\", from:\"c3\", pick:2}.",
        schema: where_::schema,
    },
    Tool {
        name: "select",
        summary: "Make a named, reusable selection (\"s4\") by ids, region box, colour, part, connectivity (everything rigidly or stud-connected to a seed, optionally stopping at hinges) or set operations on other selections. Returns the selection highlighted, with counts. Use the id anywhere ids are accepted.",
        schema: select::schema,
    },
    Tool {
        name: "look",
        summary: "Picture the model, a selection or some parts: views chosen automatically (default) or named (front, back, left, right, top, bottom, iso) or as [yaw, pitch] pairs; isolate or hide parts; id badges. Most edits already return a picture, so use this only to see something else.",
        schema: look::schema,
    },
    Tool {
        name: "search",
        summary: "Search the part library by words (\"slope curved 2x1 inverted\"), part number or similarity; returns a thumbnail grid with part numbers and descriptions.",
        schema: find::schema,
    },
    Tool {
        name: "verify",
        summary: "Check the build (or some parts): collisions, floating groups, weak links between two regions (strength = number of connections at the weakest cut). Problems come back highlighted in red with an explanation.",
        schema: verify::schema,
    },
    Tool {
        name: "project",
        summary: "Project information and housekeeping: info, parts (list in a region), inspect (a part's connectors), drafts, accept/reject/withdraw (human accepts), log, branches, restore, export (LDraw), selections, prefs, assets, colours.",
        schema: project::schema,
    },
    Tool {
        name: "publish",
        summary: "Make building instructions (PDF and step images) or a studio-quality render; stored in the project as assets and exportable to files.",
        schema: publish::schema,
    },
];

pub fn describe(tool: &str) -> Option<Value> {
    TOOLS.iter().find(|t| t.name == tool).map(|t| (t.schema)())
}

pub(crate) fn run(s: &mut Session, tool: &str, args: &Value) -> Result<Reply> {
    match tool {
        "edit" => edit::run(s, args),
        "where" => where_::run(s, args),
        "select" => select::run(s, args),
        "look" => look::run(s, args),
        "search" => find::run(s, args),
        "verify" => verify::run(s, args),
        "project" => project::run(s, args),
        "publish" => publish::run(s, args),
        other => Err(format!(
            "unknown tool {other:?}; tools: {}",
            TOOLS.iter().map(|t| t.name).collect::<Vec<_>>().join(", ")
        )),
    }
}

// ---- shared helpers ---------------------------------------------------------------------------

pub(crate) fn units_prop() -> Value {
    json!({"type":"string","enum":["ldu","grid"],"description":UNITS})
}
pub(crate) fn ids_prop(what: &str) -> Value {
    json!({"description": format!("{what}: part ids (number or list), a selection id or name (\"s3\"), \"last\" (what your previous edit changed) or \"all\""),
           "anyOf":[{"type":"integer"},{"type":"string"},{"type":"array","items":{"type":["integer","string"]}}]})
}
pub(crate) fn views_prop() -> Value {
    json!({"description":"Views: a count for automatic views (1-4), a name (front, back, left, right, top, bottom, iso) or a list of names and [yaw, pitch] pairs in degrees. yaw 0 looks from the front (-z), 90 from the right (+x); pitch 90 looks down.",
           "anyOf":[{"type":"integer"},{"type":"string"},{"type":"array"}]})
}

/// Model file the call works in (`file` index, default the main file 0).
pub(crate) fn file_arg(model: &Model, args: &Value) -> Result<usize> {
    let file = args.get("file").and_then(Value::as_u64).unwrap_or(0) as usize;
    if file >= model.doc.files.len() {
        return Err(format!("no file {file}: the project has {} file(s)", model.doc.files.len()));
    }
    Ok(file)
}

/// `[1,2,3,7,9,10]` → `#1-3, #7, #9-10`.
pub(crate) fn fmt_ids(ids: impl IntoIterator<Item = Id>) -> String {
    let mut v: Vec<Id> = ids.into_iter().collect();
    v.sort_unstable();
    v.dedup();
    if v.is_empty() {
        return "none".into();
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i < v.len() {
        let mut j = i;
        while j + 1 < v.len() && v[j + 1] == v[j] + 1 {
            j += 1;
        }
        out.push(if j > i { format!("#{}-{}", v[i], v[j]) } else { format!("#{}", v[i]) });
        i = j + 1;
    }
    if out.len() > 24 {
        let n = out.len();
        out.truncate(24);
        out.push(format!("… ({n} runs)"));
    }
    out.join(", ")
}

/// Part counts by name and colour: `3x 3001 Brick 2x4 (red), 1x ...`.
pub(crate) fn census(s: &mut Session, model: &Model, ids: &[Id], max: usize) -> String {
    let mut counts: BTreeMap<(String, i32), usize> = BTreeMap::new();
    for id in ids {
        if let Some(p) = model.parts.get(id) {
            *counts.entry((p.name.clone(), p.color)).or_default() += 1;
        }
    }
    let mut v: Vec<_> = counts.into_iter().collect();
    v.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    let n = v.len();
    let lib = s.lib.clone();
    let mut out: Vec<String> = v
        .into_iter()
        .take(max)
        .map(|((name, color), k)| format!("{k}× {} ({})", describe_part(s, &name), colour_name(&lib, color)))
        .collect();
    if n > max {
        out.push(format!("+{} more kinds", n - max));
    }
    out.join(", ")
}

pub(crate) fn describe_part(s: &mut Session, name: &str) -> String {
    let id = name.trim_end_matches(".dat").to_ascii_lowercase();
    let desc = s
        .searcher()
        .ok()
        .and_then(|sr| sr.cat.parts.iter().find(|p| p.id == id).map(|p| p.clean_desc().to_string()));
    match desc {
        Some(d) => format!("{id} {d}"),
        None => id,
    }
}

pub(crate) fn colour_name(lib: &ldraw::Library, code: i32) -> String {
    lib.colors.name(code)
}

