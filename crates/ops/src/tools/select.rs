//! `select`: build a reusable selection ("sN") by ids, region, attributes or connectivity.
use super::{census, file_arg, fmt_ids, ids_prop, units_prop, views_prop};
use crate::args::{self, Result};
use crate::shots::{self, Role, Shot, Views};
use crate::{Reply, Session};
use engine::{Id, Model};
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::BTreeSet;

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ids": ids_prop("Start from these parts (ids, selections, \"last\")"),
            "box": {"type": "object", "description": "Region {min:[x,y,z], max:[x,y,z]} in the chosen units. Includes hidden parts.",
                    "properties": {"min": {"type": "array"}, "max": {"type": "array"}, "touching": {"type": "boolean", "description": "Parts that touch the box, not only those fully inside (default false)."}}},
            "near": {"type": "object", "description": "Parts whose centre is within radius of a point: {at:[x,y,z], radius}",
                     "properties": {"at": {"type": "array"}, "radius": {"type": "number"}}},
            "color": {"type": "integer", "description": "Only parts of this colour."},
            "part": {"type": "string", "description": "Only parts whose number contains this (\"3001\", \"slope\" matches descriptions too)."},
            "connected": {"type": "string", "enum": ["studs", "rigid", "touching"],
                          "description": "Grow from the parts so far: everything stud-connected (\"studs\"), everything that moves with them without bending a hinge (\"rigid\"), or every part touching them, repeatedly (\"touching\")."},
            "add": ids_prop("Union with these"),
            "minus": ids_prop("Remove these"),
            "and": ids_prop("Intersect with these"),
            "name": {"type": "string", "description": "Keep the selection under this name (pinned; never pruned)."},
            "units": units_prop(),
            "file": {"type": "integer"},
            "views": views_prop(),
            "picture": {"type": "boolean", "description": "Return a picture (default true)."}
        },
        "additionalProperties": false
    })
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply> {
    let units = s.units(args)?;
    let view = s.view();
    let file = file_arg(&view, args)?;
    let in_file: Vec<Id> = view.parts.values().filter(|p| p.file == file).map(|p| p.id).collect();
    let mut set: Option<BTreeSet<Id>> = None;
    let mut how = Vec::new();
    let narrow = |set: &mut Option<BTreeSet<Id>>, ids: BTreeSet<Id>| {
        *set = Some(match set.take() {
            None => ids,
            Some(cur) => cur.intersection(&ids).copied().collect(),
        })
    };
    if args.get("ids").is_some() {
        let ids = s.ids_arg(&view, args, "ids", file)?;
        how.push(format!("ids {}", fmt_ids(ids.iter().copied())));
        narrow(&mut set, ids.into_iter().collect());
    }
    if let Some(b) = args.get("box") {
        let lo = units.to_ldu(args::triple(b.get("min").ok_or("box needs min")?)?);
        let hi = units.to_ldu(args::triple(b.get("max").ok_or("box needs max")?)?);
        let (lo, hi) = (lo.min(hi), lo.max(hi));
        let touching = b.get("touching").and_then(Value::as_bool).unwrap_or(false);
        let eps = 0.5;
        let hit: BTreeSet<Id> = in_file
            .iter()
            .copied()
            .filter(|id| {
                let p = &view.parts[id];
                let Some((plo, phi)) = shots::part_box(&s.lib, &p.name, p.pos, p.rot) else {
                    return lo.cmple(p.pos).all() && p.pos.cmple(hi).all();
                };
                if touching {
                    plo.cmplt(hi + eps).all() && phi.cmpgt(lo - eps).all()
                } else {
                    plo.cmpge(lo - eps).all() && phi.cmple(hi + eps).all()
                }
            })
            .collect();
        how.push(format!("{} box", if touching { "touching" } else { "inside" }));
        narrow(&mut set, hit);
    }
    if let Some(n) = args.get("near") {
        let at = args::point(n.get("at").ok_or("near needs at")?, units)?;
        let r = n.get("radius").and_then(args::num).unwrap_or(1.0) * if units == args::Units::Grid { 20.0 } else { 1.0 };
        let hit: BTreeSet<Id> = in_file
            .iter()
            .copied()
            .filter(|id| {
                let p = &view.parts[id];
                let c = shots::part_box(&s.lib, &p.name, p.pos, p.rot).map(|(a, b)| (a + b) * 0.5).unwrap_or(p.pos);
                c.distance(at) <= r
            })
            .collect();
        how.push(format!("within {r:.0} LDU"));
        narrow(&mut set, hit);
    }
    if let Some(c) = args.get("color").and_then(Value::as_i64) {
        let base: Vec<Id> = set.clone().map(|s| s.into_iter().collect()).unwrap_or_else(|| in_file.clone());
        let hit = base.into_iter().filter(|id| view.parts[id].color as i64 == c).collect();
        how.push(format!("colour {c}"));
        set = Some(hit);
    }
    if let Some(q) = args::str_arg(args, "part") {
        let q = q.trim_end_matches(".dat").to_ascii_lowercase();
        let base: Vec<Id> = set.clone().map(|s| s.into_iter().collect()).unwrap_or_else(|| in_file.clone());
        let mut hit = BTreeSet::new();
        for id in base {
            let name = view.parts[&id].name.to_ascii_lowercase();
            let matches = name.contains(&q)
                || s.lib.description(&view.parts[&id].name).to_ascii_lowercase().contains(&q);
            if matches {
                hit.insert(id);
            }
        }
        how.push(format!("part ~{q}"));
        set = Some(hit);
    }
    let mut set = set.unwrap_or_default();
    if let Some(mode) = args::str_arg(args, "connected") {
        if set.is_empty() {
            return Err("connected needs seed parts (ids, box, ...)".into());
        }
        set = grow(s, &view, file, &set, mode)?;
        how.push(format!("{mode}-connected"));
    }
    if args.get("add").is_some() {
        set.extend(s.ids_arg(&view, args, "add", file)?);
        how.push("plus others".into());
    }
    if args.get("and").is_some() {
        let other: BTreeSet<Id> = s.ids_arg(&view, args, "and", file)?.into_iter().collect();
        set = set.intersection(&other).copied().collect();
        how.push("intersected".into());
    }
    if args.get("minus").is_some() {
        for id in s.ids_arg(&view, args, "minus", file)? {
            set.remove(&id);
        }
        how.push("minus others".into());
    }
    if set.is_empty() {
        return Ok(Reply::fail(format!("Nothing matched ({}).", how.join(", ")), json!({"count": 0})));
    }
    let ids: Vec<Id> = set.iter().copied().collect();
    let user = s.user.clone();
    let mut query = args.clone();
    if let Some(q) = query.as_object_mut() {
        q.remove("picture");
        q.remove("views");
    }
    let sel = s.store.create_selection(&user, args::str_arg(args, "name"), file, &ids, Some(&query))?;
    s.engine.observe_ids(&user, ids.iter().copied());
    let mut text = format!(
        "{}{}: {} parts ({}) — {}.",
        sel.id,
        sel.name.as_deref().map(|n| format!(" \"{n}\"")).unwrap_or_default(),
        ids.len(),
        census(s, &view, &ids, 8),
        how.join(", ")
    );
    if ids.len() <= 60 {
        text.push_str(&format!(" Ids {}.", fmt_ids(ids.iter().copied())));
    }
    let bbox = shots::ids_box(&s.lib, &view, &ids);
    let mut data = json!({"selection": sel.id, "count": ids.len()});
    if ids.len() <= 400 {
        data["ids"] = json!(ids);
    }
    if let Some((lo, hi)) = bbox {
        data["box"] = units.bbox(lo, hi);
    }
    let mut reply = Reply::ok(text, data);
    if args.get("picture").and_then(Value::as_bool).unwrap_or(true) {
        let mut shot = Shot::new(file, format!("{}: {} parts", sel.id, ids.len())).role(ids.iter().copied(), Role::Selected);
        shot.dim_rest = true;
        shot.id_badges = ids.len() <= 40;
        shot.views = super::look::views_arg(args.get("views"))?.unwrap_or(Views::Auto(2));
        let (img, e) = s.picture(&view, shot);
        if let Some(e) = e {
            reply.text.push(' ');
            reply.text.push_str(&e);
        }
        reply = reply.image(img);
    }
    Ok(reply)
}

/// Grow a seed set by connectivity.
pub(crate) fn grow(s: &mut Session, view: &Model, file: usize, seed: &BTreeSet<Id>, mode: &str) -> Result<BTreeSet<Id>> {
    match mode {
        "rigid" => {
            let a = engine::connectors::analyze(view, file)?;
            let mut out = BTreeSet::new();
            for id in seed {
                if a.graph.parts.contains(id) {
                    out.extend(a.graph.rigid_chunk(*id)?);
                } else {
                    out.insert(*id);
                }
            }
            Ok(out)
        }
        "studs" | "touching" => {
            let parts = shots::flat_with_ids(view, file);
            let flat: Vec<ldraw::FlatPart> = parts.iter().map(|(f, _)| f.clone()).collect();
            let report = s.engine.validator.check(&s.lib, &flat);
            let n = parts.len();
            let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
            if mode == "studs" {
                for (i, row) in report.rows.iter().enumerate() {
                    for &j in &row.links {
                        if j < n {
                            adj[i].push(j);
                            adj[j].push(i);
                        }
                    }
                }
            } else {
                for p in &report.pairs {
                    if p.a < n && p.b < n && (p.contact || p.studs > 0 || p.overlap > 0.0) {
                        adj[p.a].push(p.b);
                        adj[p.b].push(p.a);
                    }
                }
            }
            let mut seen = vec![false; n];
            let mut stack: Vec<usize> = parts
                .iter()
                .enumerate()
                .filter(|(_, (_, id))| id.is_some_and(|id| seed.contains(&id)))
                .map(|(i, _)| i)
                .collect();
            while let Some(i) = stack.pop() {
                if std::mem::replace(&mut seen[i], true) {
                    continue;
                }
                stack.extend(adj[i].iter().copied().filter(|&j| !seen[j]));
            }
            let mut out: BTreeSet<Id> = seed.clone();
            for (i, (_, id)) in parts.iter().enumerate() {
                if seen[i] {
                    if let Some(id) = id {
                        out.insert(*id);
                    }
                }
            }
            Ok(out)
        }
        other => Err(format!("connected: unknown mode \"{other}\" (studs, rigid, touching)")),
    }
}

#[allow(dead_code)]
fn centre(lo: DVec3, hi: DVec3) -> DVec3 {
    (lo + hi) * 0.5
}
