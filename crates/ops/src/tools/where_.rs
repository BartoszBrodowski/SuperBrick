//! `where`: legal places for a new part, or for existing parts to move to, near a point.
use super::edit::blank_flat;
use super::{census, describe_part, file_arg, ids_prop, units_prop, views_prop};
use crate::args::{self, Result};
use crate::shots::{self, Label, Role, Shot, Views};
use crate::{overlay, Reply, Session};
use engine::{Cmd, Id, PartSpec};
use glam::{DMat3, DVec3};
use serde_json::{json, Value};

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "part": {"type": "string", "description": "Part number to place (\"3001\"); or give ids to move existing parts."},
            "color": {"type": "integer"},
            "rot": {"description": "Orientation to try first: 9 numbers (LDraw row order) or a shorthand such as \"yaw90\"."},
            "ids": ids_prop("Existing parts to move as one rigid piece (instead of part)"),
            "near": {"type": "array", "items": {"type": "number"}, "minItems": 3, "maxItems": 3, "description": "Point to search around."},
            "near_ids": ids_prop("Search around these parts (instead of near)"),
            "facing": {"description": "Only seats whose studs point this way: \"up\", \"down\", \"x\", \"-z\" or a vector."},
            "radius": {"type": "number", "description": "Search radius in LDU (default 60, max 400)."},
            "limit": {"type": "integer", "description": "Candidates to return (default 6, max 24)."},
            "connectors": {"type": "boolean", "description": "Use every connector type (pins, axles, clips, hinges), not just studs. Automatic for parts without studs."},
            "units": units_prop(),
            "file": {"type": "integer"},
            "views": views_prop()
        },
        "additionalProperties": false
    })
}

struct Candidate {
    cmds: Vec<Cmd>,
    /// Parts to draw as the ghost.
    ghosts: Vec<PartSpec>,
    /// Where the label goes (the ghost's middle).
    at: DVec3,
    /// The (first) part's LDraw origin after the move — what `add at` takes.
    origin: DVec3,
    note: String,
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply> {
    let units = s.units(args)?;
    let view = s.view();
    let file = file_arg(&view, args)?;
    let limit = args::usize_arg(args, "limit", 6, 24).max(1);
    let radius = args.get("radius").and_then(args::num).unwrap_or(60.0).clamp(5.0, 400.0);
    let ids = s.ids_arg(&view, args, "ids", file)?;
    let near = if let Some(n) = args.get("near") {
        args::point(n, units)?
    } else if args.get("near_ids").is_some() {
        let around = s.ids_arg(&view, args, "near_ids", file)?;
        let (lo, hi) = shots::ids_box(&s.lib, &view, &around).ok_or("near_ids: no geometry")?;
        (lo + hi) * 0.5
    } else {
        return Err("where needs \"near\" (a point) or \"near_ids\"".into());
    };
    let facing = match args.get("facing") {
        Some(v) => args::axis(v)?,
        None => DVec3::ZERO,
    };
    let saved = std::mem::replace(&mut s.engine.model, view.clone());
    // Ask for extra seats: the ones a symmetry of the part makes identical are dropped.
    let found = find(s, &view, file, &ids, args, units, near, facing, radius, (limit * 4).min(96));
    s.engine.model = saved;
    let (mut cands, what) = found?;
    let mut seen = std::collections::HashSet::new();
    cands.retain(|c| shape_key(&s.lib, &c.ghosts).is_none_or(|k| seen.insert(k)));
    cands.truncate(limit);
    if cands.is_empty() {
        let mut text = format!("No legal place for {what} within {radius:.0} LDU of the point.");
        text.push_str(" Try a larger radius, another rotation, connectors:true, or look at the area.");
        let shot = Shot { labels: vec![Label { text: "searched here".into(), at: near, color: overlay::ORANGE }], ..Shot::new(file, "no legal place") };
        let (img, _) = s.picture(&view, shot);
        return Ok(Reply::fail(text, json!({"candidates": []})).image(img));
    }
    let items: Vec<Value> = cands
        .iter()
        .enumerate()
        .map(|(i, c)| json!({"label": (i + 1).to_string(), "at": units.from_ldu(c.origin), "note": c.note, "cmds": c.cmds}))
        .collect();
    let tree = s.store.working(&s.branch)?.tree;
    let user = s.user.clone();
    let set = s.store.create_candidates(&user, &json!({"file": file, "units": units.name(), "what": what, "items": items}), &tree)?;
    let mut shot = Shot::new(file, format!("{set}: {} places for {what}", cands.len()));
    for (i, c) in cands.iter().enumerate() {
        let extra = c
            .ghosts
            .iter()
            .map(|g| (ldraw::FlatPart { name: g.name.clone(), color: ldraw::Color(g.color), pos: g.pos, rot: g.rot, ..blank_flat() }, Role::Ghost))
            .collect();
        shot.items.push(shots::Item { label: format!("{}", i + 1), extra, at: c.at, note: c.note.clone(), ..Default::default() });
    }
    shot.intent = crate::planner::Intent::Item;
    if !ids.is_empty() {
        shot = shot.role(ids.iter().copied(), Role::Selected);
    }
    shot.views = super::look::views_arg(args.get("views"))?.unwrap_or(Views::Auto(if cands.len() > 3 { 2 } else { 1 }));
    let lines: Vec<String> = cands
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let g = units.from_ldu(c.origin);
            format!("{}: [{:.0}, {:.0}, {:.0}]{}", i + 1, g[0], g[1], g[2], if c.note.is_empty() { String::new() } else { format!(" {}", c.note) })
        })
        .collect();
    let mut text = format!(
        "{} legal places for {what} (set {set}), nearest first (part origin): {}. Place one with edit {{op:\"place\", from:\"{set}\", pick:N}}.",
        cands.len(),
        lines.join("; ")
    );
    let (img, e) = s.picture(&view, shot);
    if let Some(e) = e {
        text.push(' ');
        text.push_str(&e);
    }
    Ok(Reply::ok(text, json!({"set": set, "candidates": items.iter().map(|i| json!({"label": i["label"], "at": i["at"], "note": i["note"]})).collect::<Vec<_>>()})).image(img))
}

#[allow(clippy::too_many_arguments)]
fn find(
    s: &mut Session,
    view: &engine::Model,
    file: usize,
    ids: &[Id],
    args: &Value,
    units: crate::args::Units,
    near: DVec3,
    facing: DVec3,
    radius: f64,
    limit: usize,
) -> Result<(Vec<Candidate>, String)> {
    let _ = units;
    if !ids.is_empty() {
        let what = if ids.len() == 1 {
            format!("#{}", ids[0])
        } else {
            format!("{} parts ({})", ids.len(), census(s, view, ids, 3))
        };
        let req = json!({"action": "drag", "moving": ids, "source": {"part": ids[0]}, "near": near,
                         "all": true, "radius": radius, "limit": limit, "budget": 4096});
        let res = engine::connectors::request(view, file, req)?;
        let mut out = Vec::new();
        for c in res["candidates"].as_array().into_iter().flatten() {
            let r: DMat3 = serde_json::from_value(c["rotation"].clone()).map_err(|e| e.to_string())?;
            let t: DVec3 = serde_json::from_value(c["translation"].clone()).map_err(|e| e.to_string())?;
            let mut cmds = Vec::new();
            let (axis, angle) = args::axis_angle(r);
            if angle.abs() > 1e-6 {
                cmds.push(Cmd::Rotate { ids: ids.to_vec(), axis, angle, pivot: DVec3::ZERO, free: true });
            }
            cmds.push(Cmd::Move { ids: ids.to_vec(), delta: t, free: false });
            let ghosts: Vec<PartSpec> = ids
                .iter()
                .filter_map(|id| view.parts.get(id))
                .map(|p| PartSpec { name: p.name.clone(), color: p.color, pos: r * p.pos + t, rot: r * p.rot, id: None })
                .collect();
            let at = ghosts.iter().map(|g| g.pos).sum::<DVec3>() / ghosts.len().max(1) as f64;
            let note = if angle.abs() > 1e-6 { format!("turned {angle:.0}°") } else { String::new() };
            let origin = ghosts.first().map_or(at, |g| g.pos);
            out.push(Candidate { cmds, ghosts, at, origin, note });
        }
        return Ok((out, what));
    }
    let name = args::part_name(args::str_arg(args, "part").ok_or("where needs \"part\" or \"ids\"")?);
    let color = args.get("color").and_then(Value::as_i64).unwrap_or(71) as i32;
    let rot = match args.get("rot") {
        Some(r) => args::rotation(r)?,
        None => DMat3::IDENTITY,
    };
    let what = describe_part(s, &name);
    let spec = PartSpec { name: name.clone(), color, pos: near, rot, id: None };
    let use_connectors =
        args.get("connectors").and_then(Value::as_bool).unwrap_or(false) || s.engine.needs_connector_drag(&name);
    let mut out = Vec::new();
    if use_connectors {
        let req = json!({"action": "drag", "moving": [], "part": spec, "near": near,
                         "all": true, "radius": radius, "limit": limit, "budget": 4096});
        let res = engine::connectors::request(view, file, req)?;
        for c in res["candidates"].as_array().into_iter().flatten() {
            let pose: engine::connectors::Pose = serde_json::from_value(c["poses"][0].clone()).map_err(|e| e.to_string())?;
            let p = PartSpec { pos: pose.pos, rot: pose.rot, ..spec.clone() };
            out.push(candidate_for(s, file, p, rot));
        }
    } else {
        for p in s.engine.stud_candidates(file, &spec, near, facing, &[], radius, limit)? {
            out.push(candidate_for(s, file, p, rot));
        }
    }
    Ok((out, what))
}

fn candidate_for(s: &Session, file: usize, p: PartSpec, wanted: DMat3) -> Candidate {
    let at = shots::part_box(&s.lib, &p.name, p.pos, p.rot).map(|(lo, hi)| (lo + hi) * 0.5).unwrap_or(p.pos);
    let turned = (p.rot - wanted).to_cols_array().iter().any(|v| v.abs() > 1e-6);
    let note = if turned { format!("rot {:?}", args::rot_rows(p.rot).map(|v| v.round() as i64)) } else { String::new() };
    let origin = p.pos;
    Candidate { cmds: vec![Cmd::Add { file, parts: vec![p.clone()], free: false }], ghosts: vec![p], at, origin, note }
}

/// Fingerprint of the space a candidate fills (vertex positions and colours, 1/4 LDU grid).
/// Seats that differ only by a symmetry of the part (a 1x1 plate turned 90°) share it.
fn shape_key(lib: &ldraw::Library, ghosts: &[PartSpec]) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let mut keys = Vec::with_capacity(ghosts.len());
    for g in ghosts {
        let mesh = lib.mesh(&g.name, ldraw::Lod::Std).ok()?;
        let mut pts: Vec<(i64, i64, i64, i32)> = Vec::with_capacity(mesh.tris.len() * 3);
        for (tri, col) in mesh.tris.iter().zip(&mesh.tcol) {
            for v in tri {
                let q = ((g.rot * v.as_dvec3() + g.pos) * 4.0).round();
                pts.push((q.x as i64, q.y as i64, q.z as i64, col.0));
            }
        }
        pts.sort_unstable();
        pts.dedup();
        let mut h = std::collections::hash_map::DefaultHasher::new();
        (g.color, &pts).hash(&mut h);
        keys.push(h.finish());
    }
    keys.sort_unstable();
    let mut h = std::collections::hash_map::DefaultHasher::new();
    keys.hash(&mut h);
    Some(h.finish())
}
