//! `edit`: a batch of operations as one proposal (or, for `human`, one change to the model).
use super::{census, file_arg, fmt_ids, units_prop, views_prop};
use crate::args::{self, Result, Units};
use crate::shots::{self, BoxMark, Label, Role, Shot, Views};
use crate::{overlay, Ctx, Reply, Session, HUMAN};
use engine::connectors::Endpoint;
use engine::drafts::DraftTx;
use engine::{Cmd, Id, Model, PartSpec};
use glam::{DMat3, DVec3};
use std::collections::BTreeSet;
use serde_json::{json, Value};

pub fn schema() -> Value {
    json!({
        "type": "object",
        "required": ["ops"],
        "properties": {
            "ops": {"type": "array", "minItems": 1, "description":
"Operations, applied in order as one atomic batch (all or nothing). Each is an object with \"op\":
- add: {part:\"3001\", color:4, at:[x,y,z], rot:\"yaw90\"} or {parts:[{part,color,at,rot},...]} or {ldraw:\"1 15 0 -24 0 1 0 0 0 1 0 0 0 1 3001.dat\\n...\"} (LDraw lines, LDU, row order). at is the part's LDraw origin. free:true skips the collision check.
- place: {from:\"c3\", pick:2, color?} places candidate 2 of a `where` result; or {part, color, near:[x,y,z] or near_ids, rot?} snaps to the nearest legal stud seat.
- move: {ids, by:[dx,dy,dz]} or {ids, to:[x,y,z]} (the first part's origin goes there).
- rotate: {ids, axis:\"y\" or [x,y,z], angle:90, pivot?} (pivot defaults to the parts' centre).
- mirror: {ids, axis:\"x\"|\"y\"|\"z\", pivot?}.
- copy: {ids, by, count?:1}.
- remove: {ids}. paint: {ids, color}.
- connect: {source:{part,index}, target:{part,index}, pose?} joins connector to connector (indexes from project inspect); the source part and everything connected to it moves.
- hinge: {part, angle, at?, joint?, axis?:0, detach?:[joint ids]} turns the side holding `part` about a hinge/turntable/ball joint; `at` is the hinge piece. A complete hinge (one part holding both halves, e.g. 1927) is split into its halves first (automatic when it is the only one rigidly joining `part`).\n- split: {ids} replaces a shortcut part (complete hinge, turntable) by its pieces, same step.
- group / subbuild: {ids, name}. hide / lock: {ids, value?:true}.
- step: {label?} ends the construction step here (`0 STEP`). restep: {ids, after} moves parts to the end of the step that part `after` is in. add and place also take after:<part id> to put the new parts in that part's (earlier) step.
color is an LDraw number or a name (\"red\", \"light bluish gray\", \"lbg\").",
                "items": {"type": "object", "required": ["op"]}},
            "label": {"type": "string", "description": "Short description shown with the proposal (\"left wing root\")."},
            "dry_run": {"type": "boolean", "description": "Check and picture the result without proposing it."},
            "step": {"type": ["boolean", "string"], "description": "End a construction step after this batch (`0 STEP` in the LDraw, labelled with the edit's label, or this text). Use one edit per instruction step."},
            "units": units_prop(),
            "file": {"type": "integer", "description": "Model file index (0 = main)."},
            "views": views_prop(),
            "images": {"type": "boolean", "description": "Return a picture (default true)."},
            "compare": super::compare::schema(),
            "callout": {"type": "array", "items": {"type": "object"}, "description": "The booklet callout for this step: [{part, color, qty}]. The batch is refused unless the parts it adds, plus parts a `place` attaches (a sub-build), match it exactly."}
        },
        "additionalProperties": false
    })
}

/// Default colour: the one this user used last, else light bluish grey.
fn default_colour(s: &Session) -> i32 {
    s.store
        .pref(&format!("user:{}", s.user), "color")
        .ok()
        .flatten()
        .and_then(|v| v.as_i64())
        .map(|c| c as i32)
        .unwrap_or(71)
}

pub(crate) fn colour_arg(v: &Value, fallback: i32) -> Result<i32> {
    match v.get("color").or_else(|| v.get("colour")) {
        None | Some(Value::Null) => Ok(fallback),
        Some(Value::String(name)) => colour_code(name),
        Some(c) => c.as_i64().map(|c| c as i32).ok_or_else(|| "color must be an LDraw colour number or name".into()),
    }
}

/// A colour by LDraw number ("71"), name ("light bluish gray", "pearl gold") or short form ("lbg").
pub(crate) fn colour_code(name: &str) -> Result<i32> {
    static COLOURS: std::sync::OnceLock<Option<ldraw::Colors>> = std::sync::OnceLock::new();
    match COLOURS.get_or_init(|| ldraw::Library::open_default().ok().map(|l| l.colors.clone())) {
        Some(c) => c.code(name),
        None => name.trim().parse().map_err(|_| format!("unknown colour {name:?}; use an LDraw colour number")),
    }
}

fn spec(v: &Value, units: Units, colour: i32) -> Result<PartSpec> {
    let name = args::str_arg(v, "part").or_else(|| args::str_arg(v, "name")).ok_or("each part needs \"part\" (a part number such as \"3001\")")?;
    let at = v.get("at").or_else(|| v.get("pos")).ok_or("each part needs \"at\": [x, y, z]")?;
    Ok(PartSpec {
        name: args::part_name(name),
        color: colour_arg(v, colour)?,
        pos: args::point(at, units)?,
        rot: match v.get("rot") {
            None | Some(Value::Null) => DMat3::IDENTITY,
            Some(r) => args::rotation(r)?,
        },
        id: None,
    })
}

/// LDraw type-1 lines ("1 colour x y z a b c d e f g h i part.dat"), always in LDU and LDraw row order.
/// Comments, blank lines and non-part lines are skipped; colour 16 takes the default colour.
pub(crate) fn ldraw_specs(v: &Value, colour: i32) -> Result<Vec<PartSpec>> {
    let text = match v {
        Value::String(s) => s.clone(),
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("\n"),
        _ => return Err("ldraw must be a string of LDraw lines or an array of lines".into()),
    };
    let mut out = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let w: Vec<&str> = line.split_whitespace().collect();
        if w.first() != Some(&"1") {
            continue;
        }
        if w.len() < 15 {
            return Err(format!("ldraw line {}: expected 1 colour x y z a b c d e f g h i part.dat", n + 1));
        }
        let f = |i: usize| w[i].parse::<f64>().map_err(|_| format!("ldraw line {}: {:?} is not a number", n + 1, w[i]));
        let c: i32 = w[1].parse().map_err(|_| format!("ldraw line {}: bad colour {:?}", n + 1, w[1]))?;
        let mut r = [0.0; 9];
        for (k, slot) in r.iter_mut().enumerate() {
            *slot = f(5 + k)?;
        }
        out.push(PartSpec {
            name: args::part_name(&w[14..].join(" ")),
            color: if c == 16 { colour } else { c },
            pos: DVec3::new(f(2)?, f(3)?, f(4)?),
            rot: args::ldraw_rot(r),
            id: None,
        });
    }
    if out.is_empty() {
        return Err("ldraw held no part lines (type 1)".into());
    }
    Ok(out)
}

fn axis_index(v: &Value) -> Result<usize> {
    match v {
        Value::Number(n) => n.as_u64().filter(|&a| a < 3).map(|a| a as usize).ok_or_else(|| "axis must be 0, 1 or 2".into()),
        Value::String(s) => match s.trim_start_matches(['-', '+']).to_ascii_lowercase().as_str() {
            "x" => Ok(0),
            "y" | "up" | "down" => Ok(1),
            "z" => Ok(2),
            _ => Err(format!("unknown axis {s:?}: use x, y or z")),
        },
        _ => Err("axis must be \"x\", \"y\" or \"z\"".into()),
    }
}

fn endpoint(v: &Value) -> Result<Endpoint> {
    serde_json::from_value(v.clone()).map_err(|e| format!("connector endpoint {{part, leaf?, index?}}: {e}"))
}

/// What one op did, for the reply.
struct Done {
    kind: String,
    note: Option<String>,
    /// A group made by this op: its name and parts (kept as a named selection too).
    group: Option<(String, Vec<Id>)>,
    /// Parts a `place` added or moved, and whether the bench proved that seat (connector candidates).
    placed: Vec<Id>,
    bench: bool,
}

/// Resolve an op's ids against the transaction's current model.
fn op_ids(s: &Ctx, model: &Model, op: &Value, file: usize) -> Result<Vec<Id>> {
    let ids = s.ids_arg(model, op, "ids", file)?;
    if ids.is_empty() {
        return Err("needs \"ids\"".into());
    }
    Ok(ids)
}

fn centre(s: &Ctx, model: &Model, ids: &[Id]) -> DVec3 {
    match shots::ids_box(&s.lib, model, ids) {
        Some((lo, hi)) => ((lo + hi) * 0.5 / 10.0).round() * 10.0,
        None => model.parts.get(&ids[0]).map(|p| p.pos).unwrap_or_default(),
    }
}

/// The `step` argument of an edit: true (use the edit's label), a label, or false/absent.
/// Refuse the batch unless the parts it adds equal `list` ([{part, color, qty}]), as counted in a booklet callout.
fn check_callout(ctx: &Ctx, list: &Value, original: &std::collections::HashSet<Id>, moved: &std::collections::HashSet<Id>, model: &Model, file: usize) -> Result<()> {
    use super::project::{counts_from, part_key, Counts};
    let lib: &ldraw::Library = ctx.lib;
    let list = list.as_array().ok_or("callout is a list of {part, color, qty}")?;
    let want = counts_from(lib, list)?;
    let mut got = Counts::new();
    for p in model.parts.values().filter(|p| p.file == file && (!original.contains(&p.id) || moved.contains(&p.id))) {
        *got.entry((part_key(lib, &p.name), p.color)).or_default() += 1;
    }
    if want == got {
        return Ok(());
    }
    let fmt = |m: &Counts, other: &Counts| -> Vec<String> {
        m.iter().filter_map(|(k, n)| {
            let o = other.get(k).copied().unwrap_or(0);
            (*n > o).then(|| format!("{}× {} {} ({})", n - o, k.0, super::colour_name(lib, k.1), k.1))
        }).collect()
    };
    let missing = fmt(&want, &got);
    let extra = fmt(&got, &want);
    let mut t = String::from("the callout doesn't match the parts this edit adds:");
    if !missing.is_empty() {
        t.push_str(&format!(" missing {}.", missing.join(", ")));
    }
    if !extra.is_empty() {
        t.push_str(&format!(" not in the callout {}.", extra.join(", ")));
    }
    t.push_str(" Recount the callout box, or fix the parts.");
    Err(t)
}

fn step_label(v: Option<&Value>, label: &str) -> Result<Option<Option<String>>> {
    Ok(match v {
        None | Some(Value::Null) | Some(Value::Bool(false)) => None,
        Some(Value::Bool(true)) => Some((!label.is_empty()).then(|| label.to_string())),
        Some(Value::String(s)) => Some(Some(s.clone())),
        Some(_) => return Err("step: true (label it with the edit's label) or a label".into()),
    })
}


/// Apply a move or turn. When its only collisions are bench-proven joints (clips, bars, windows)
/// whose two parts move together, apply it without the stud-only collision check.
fn apply_rigid(s: &Ctx, tx: &mut DraftTx, cmd: Cmd) -> Result<()> {
    let err = match tx.apply(cmd.clone()) {
        Ok(_) => return Ok(()),
        Err(e) => e,
    };
    let moved: std::collections::HashSet<Id> = cmd.ids().iter().copied().collect();
    let unchecked = match cmd {
        Cmd::Move { ids, delta, .. } => Cmd::Move { ids, delta, free: true },
        Cmd::Rotate { ids, axis, angle, pivot, .. } => Cmd::Rotate { ids, axis, angle, pivot, free: true },
        _ => return Err(err),
    };
    let only_joints = tx
        .engine()
        .last_failure
        .as_ref()
        .and_then(|f| f.get("collisions"))
        .and_then(Value::as_array)
        .is_some_and(|list| {
            !list.is_empty()
                && list.iter().all(|c| match (c.get("moving").and_then(Value::as_u64), c.get("blocker").and_then(Value::as_u64)) {
                    (Some(m), Some(b)) => moved.contains(&m) && moved.contains(&b) && s.joints.iter().any(|j| j.joins(m, b)),
                    _ => false,
                })
        });
    if !only_joints {
        return Err(err);
    }
    tx.apply(unchecked)?;
    Ok(())
}

/// The real pieces a shortcut part stands for (a complete hinge 1927 = base 2429 + top 2430),
/// placed where the shortcut is; None for an ordinary part.
fn components(lib: &ldraw::Library, name: &str, color: i32, pos: DVec3, rot: DMat3, depth: usize) -> Option<Vec<PartSpec>> {
    if depth > 4 {
        return None;
    }
    let text = lib.read_text(name).ok()?;
    let shortcut = text.lines().take(12).any(|l| {
        let l = l.trim();
        (l.starts_with("0 !LDRAW_ORG") && (l.contains("Shortcut") || l.contains("Alias"))) || l.starts_with("0 ~Moved to")
    });
    if !shortcut {
        return None;
    }
    let mut out = Vec::new();
    for line in text.lines() {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() < 15 || t[0] != "1" {
            continue;
        }
        let n: Vec<f64> = t[2..14].iter().map(|x| x.parse().ok()).collect::<Option<_>>()?;
        let c: i32 = t[1].parse().ok()?;
        let c = if c == 16 { color } else { c };
        let sub = t[14..].join(" ");
        let local = DMat3::from_cols_array(&[n[3], n[6], n[9], n[4], n[7], n[10], n[5], n[8], n[11]]);
        let (p, r) = (pos + rot * DVec3::new(n[0], n[1], n[2]), rot * local);
        match components(lib, &sub, c, p, r, depth + 1) {
            Some(v) => out.extend(v),
            None => out.push(PartSpec { name: sub, color: c, pos: p, rot: r, id: None }),
        }
    }
    (!out.is_empty()).then_some(out)
}

fn splittable(lib: &ldraw::Library, name: &str) -> bool {
    components(lib, name, 16, DVec3::ZERO, DMat3::IDENTITY, 0).is_some_and(|v| v.len() >= 2)
}

/// Replace a shortcut part by its pieces, in the same step; the first piece keeps the id.
fn split_part(s: &Ctx, tx: &mut DraftTx, id: Id) -> Result<Vec<Id>> {
    let p = tx.model().parts.get(&id).cloned().ok_or_else(|| format!("no part #{id}"))?;
    let mut specs = components(s.lib, &p.name, p.color, p.pos, p.rot, 0)
        .filter(|v| v.len() >= 2)
        .ok_or_else(|| format!("#{id} {} is a single piece; only shortcut parts such as a complete hinge (1927) split", p.name))?;
    let anchor = tx
        .model()
        .parts
        .values()
        .filter(|q| q.id != id && q.file == p.file && q.step == p.step)
        .map(|q| q.id)
        .next();
    let before: std::collections::HashSet<Id> = tx.model().parts.keys().copied().collect();
    specs[0].id = Some(id);
    tx.apply(Cmd::Remove { ids: vec![id] })?;
    tx.apply(Cmd::Add { file: p.file, parts: specs, free: true })?;
    let mut halves: Vec<Id> = vec![id];
    halves.extend(tx.model().parts.keys().filter(|k| !before.contains(k)).copied());
    if let Some(after) = anchor {
        tx.apply(Cmd::Restep { ids: halves.clone(), after })?;
    }
    Ok(halves)
}

/// Turn one op into commands and apply them inside the transaction.
fn apply_op(s: &Ctx, tx: &mut DraftTx, op: &Value, file: usize, units: Units, colour: i32) -> Result<Done> {
    let kind = args::str_arg(op, "op").ok_or("each op needs \"op\" (add, place, move, ...)")?.to_ascii_lowercase();
    let free = op.get("free").and_then(Value::as_bool).unwrap_or(false);
    let mut placed: Vec<Id> = Vec::new();
    let mut bench = false;
    let mut note = None;
    let mut group = None;
    // `after`: the parts this op adds join the construction step of that part (their lines go
    // right after its line), instead of the step being built at the end of the file.
    let anchor = match op.get("after") {
        Some(v) if kind == "add" || kind == "place" => {
            let ids = s.resolve(tx.model(), &args::id_refs(v)?, file)?;
            Some(*ids.last().ok_or("after: no such part")?)
        }
        Some(_) => return Err("\"after\" only applies to add and place (use op restep to move existing parts)".into()),
        None => None,
    };
    let before: BTreeSet<Id> = if anchor.is_some() { tx.model().parts.keys().copied().collect() } else { BTreeSet::new() };
    match kind.as_str() {
        "add" => {
            let parts = match (op.get("ldraw"), op.get("parts").and_then(Value::as_array)) {
                (Some(lines), _) => ldraw_specs(lines, colour_arg(op, colour)?)?,
                (None, Some(list)) => list.iter().map(|p| spec(p, units, colour_arg(op, colour)?)).collect::<Result<Vec<_>>>()?,
                (None, None) => vec![spec(op, units, colour)?],
            };
            tx.apply(Cmd::Add { file, parts, free })?;
        }
        "place" => {
            if let Some(from) = args::str_arg(op, "from") {
                let set = s.store.candidates(from)?;
                let items = set.json["items"].as_array().cloned().unwrap_or_default();
                let pick = op.get("pick").and_then(Value::as_u64).unwrap_or(1) as usize;
                let item = items
                    .get(pick.wrapping_sub(1))
                    .ok_or_else(|| format!("{from} has candidates 1-{}", items.len()))?;
                let before: std::collections::HashMap<Id, u64> =
                    tx.model().parts.iter().map(|(id, p)| (*id, p.content_hash())).collect();
                bench = item["bench"].as_bool().unwrap_or(false);
                for c in item["cmds"].as_array().into_iter().flatten() {
                    let mut cmd: Cmd = serde_json::from_value(c.clone()).map_err(|e| format!("candidate {from}/{pick}: {e}"))?;
                    if let (Cmd::Add { parts, .. }, Some(col)) = (&mut cmd, op.get("color").and_then(Value::as_i64)) {
                        for p in parts.iter_mut() {
                            p.color = col as i32;
                        }
                    }
                    tx.apply(cmd)?;
                }
                placed = tx.model().parts.iter().filter(|(id, p)| before.get(*id) != Some(&p.content_hash())).map(|(id, _)| *id).collect();
                note = Some(format!("{from} #{pick}"));
            } else {
                let mut part = spec(&{
                    let mut o = op.clone();
                    if o.get("at").is_none() {
                        o["at"] = op.get("near").cloned().unwrap_or(json!([0, 0, 0]));
                    }
                    o
                }, units, colour)?;
                let near = match op.get("near_ids") {
                    Some(v) => {
                        let ids = s.resolve(tx.model(), &args::id_refs(v)?, file)?;
                        centre(s, tx.model(), &ids)
                    }
                    None => part.pos,
                };
                let facing = match op.get("facing") {
                    Some(v) => args::axis(v)?,
                    None => DVec3::ZERO,
                };
                let radius = op.get("radius").and_then(args::num).unwrap_or(60.0);
                let found = tx.engine().stud_candidates(file, &part, near, facing, &[], radius, 1)?;
                let best = found.into_iter().next().ok_or_else(|| {
                    format!("no legal stud seat for {} within {radius} LDU of {near:?}; try `where` with a larger radius", part.name)
                })?;
                note = Some(format!("seated at {:?}", units.from_ldu(best.pos)));
                part = best;
                tx.apply(Cmd::Add { file, parts: vec![part], free: false })?;
            }
        }
        "move" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let delta = if let Some(by) = op.get("by").or_else(|| op.get("delta")) {
                units.to_ldu(args::triple(by)?)
            } else if let Some(to) = op.get("to") {
                let first = tx.model().parts.get(&ids[0]).map(|p| p.pos).unwrap_or_default();
                args::point(to, units)? - first
            } else {
                return Err("move needs \"by\" or \"to\"".into());
            };
            apply_rigid(s, tx, Cmd::Move { ids, delta, free })?;
        }
        "rotate" | "turn" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let axis = args::axis(op.get("axis").unwrap_or(&json!("y")))?;
            let angle = op.get("angle").and_then(args::num).ok_or("rotate needs \"angle\" in degrees")?;
            let pivot = match op.get("pivot") {
                Some(p) => args::point(p, units)?,
                None => centre(s, tx.model(), &ids),
            };
            apply_rigid(s, tx, Cmd::Rotate { ids, axis, angle, pivot, free })?;
        }
        "mirror" | "flip" | "invert" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let axis = axis_index(op.get("axis").unwrap_or(&json!("x")))?;
            let pivot = match op.get("pivot") {
                Some(p) => args::point(p, units)?,
                None => centre(s, tx.model(), &ids),
            };
            tx.apply(Cmd::Mirror { ids, axis, pivot })?;
        }
        "copy" | "duplicate" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let delta = units.to_ldu(args::triple(op.get("by").or_else(|| op.get("delta")).ok_or("copy needs \"by\"")?)?);
            let count = op.get("count").and_then(Value::as_u64).unwrap_or(1).clamp(1, 500) as usize;
            tx.apply(Cmd::Copy { ids, delta, count })?;
        }
        "remove" | "delete" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            tx.apply(Cmd::Remove { ids })?;
        }
        "paint" | "color" | "colour" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let color = op.get("color").or_else(|| op.get("colour")).and_then(Value::as_i64).ok_or("paint needs \"color\"")? as i32;
            tx.apply(Cmd::Paint { ids, color })?;
        }
        "connect" => {
            // The source part's connected group moves; the engine wants every part of the file listed.
            let ids: Vec<Id> = tx.model().parts.values().filter(|p| p.file == file).map(|p| p.id).collect();
            let source = endpoint(op.get("source").ok_or("connect needs \"source\"")?)?;
            let target = endpoint(op.get("target").ok_or("connect needs \"target\"")?)?;
            let pose = op.get("pose").and_then(Value::as_u64).unwrap_or(0) as usize;
            tx.apply(Cmd::Connect { ids, file, source, target, pose })?;
        }
        "split" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let mut out = Vec::new();
            for id in ids {
                let halves = split_part(s, tx, id)?;
                out.push(format!("#{id} into {}", fmt_ids(halves.iter().copied())));
            }
            note = Some(format!("split {}", out.join("; ")));
        }
        "hinge" | "articulate" => {
            let clicked = op
                .get("part")
                .and_then(Value::as_u64)
                .ok_or("hinge needs \"part\": the id of a part on the side that turns")?;
            let angle = op.get("angle").and_then(args::num).ok_or("hinge needs \"angle\" in degrees")?;
            let mut at = op.get("at").and_then(Value::as_u64);
            if at == Some(clicked) {
                return Err("hinge: \"part\" is a part on the side that turns, \"at\" the hinge piece; they can't be the same".into());
            }
            let mut notes = Vec::new();
            let mut halves: Vec<Id> = Vec::new();
            if at.is_none() && op.get("joint").is_none() {
                // A complete hinge (one LDraw part holding both halves) rigidly joins the two sides:
                // if exactly one sits in the turning part's rigid chunk, that is the hinge meant.
                let analysis = engine::connectors::analyze(tx.model(), file)?;
                let chunk = analysis.graph.rigid_chunk(clicked)?;
                let model = tx.model();
                let found: Vec<Id> = chunk
                    .iter()
                    .copied()
                    .filter(|id| model.parts.get(id).is_some_and(|p| splittable(s.lib, &p.name)))
                    .collect();
                match found.len() {
                    0 => {}
                    1 => at = Some(found[0]),
                    _ => {
                        return Err(format!(
                            "#{clicked} is rigidly joined through several complete hinges/turntables ({}); pass \"at\": the one to turn",
                            fmt_ids(found.iter().copied())
                        ))
                    }
                }
            }
            if let Some(h) = at {
                let name = tx.model().parts.get(&h).map(|p| p.name.clone()).ok_or_else(|| format!("no part #{h}"))?;
                if splittable(s.lib, &name) {
                    halves = split_part(s, tx, h)?;
                    notes.push(format!("split #{h} {} into {}", name.trim_end_matches(".dat"), fmt_ids(halves.iter().copied())));
                }
            }
            let analysis = engine::connectors::analyze(tx.model(), file)?;
            let pivots = analysis.graph.pivots(clicked)?;
            let turns = |j: &engine::joints::Joint| !j.motion.rotation_axes().is_empty();
            let joint = if let Some(j) = op.get("joint").and_then(Value::as_u64) {
                if !pivots.contains(&j) {
                    return Err(format!("joint {j} does not turn part #{clicked}; joints that do: {pivots:?}"));
                }
                j
            } else if !halves.is_empty() {
                analysis
                    .graph
                    .joints
                    .iter()
                    .find(|j| turns(j) && halves.contains(&j.a) && halves.contains(&j.b))
                    .map(|j| j.id)
                    .ok_or("the connector bench found no hinge between the split halves")?
            } else if let Some(h) = at {
                analysis
                    .graph
                    .joints
                    .iter()
                    .find(|j| turns(j) && pivots.contains(&j.id) && (j.a == h || j.b == h))
                    .map(|j| j.id)
                    .ok_or_else(|| format!("#{h} has no hinge, turntable or ball joint that turns #{clicked}"))?
            } else {
                *pivots.first().ok_or_else(|| format!("part #{clicked} is not on a hinge, turntable or ball joint"))?
            };
            let art = analysis.graph.articulate(clicked, joint)?;
            let axis = op.get("axis").and_then(Value::as_u64).unwrap_or(0) as usize;
            let detach: Vec<u64> = op.get("detach").and_then(|d| serde_json::from_value(d.clone()).ok()).unwrap_or_default();
            if !art.detach.is_empty() && detach.is_empty() {
                return Err(format!(
                    "turning joint {joint} would open a loop: confirm by passing detach: {:?}",
                    art.detach
                ));
            }
            apply_rigid(s, tx, art.rotation(axis, angle, &detach)?)?;
            notes.push(format!("joint {joint}, {} parts turn", art.moving.len()));
            note = Some(notes.join("; "));
        }
        "group" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let name = args::str_arg(op, "name").ok_or("group needs \"name\"")?.to_string();
            group = Some((name.clone(), ids.clone()));
            tx.apply(Cmd::Group { ids, name })?;
        }
        "subbuild" | "make_subbuild" | "submodel" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let name = args::str_arg(op, "name").ok_or("subbuild needs \"name\"")?.to_string();
            tx.apply(Cmd::MakeSubbuild { ids, name })?;
        }
        "hide" | "lock" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let value = op.get("value").and_then(Value::as_bool).unwrap_or(true);
            tx.apply(if kind == "hide" { Cmd::Hide { ids, value } } else { Cmd::Lock { ids, value } })?;
        }
        "nudge" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let direction = args::axis(op.get("direction").ok_or("nudge needs \"direction\"")?)?;
            tx.apply(Cmd::Nudge { ids, direction, normal: false, alt: None })?;
        }
        "step" => {
            let label = args::str_arg(op, "label").map(str::to_string);
            tx.apply(Cmd::Step { file, label })?;
        }
        "restep" => {
            let ids = op_ids(s, tx.model(), op, file)?;
            let after = op.get("after").ok_or("restep needs \"after\": the part whose step the parts join")?;
            let after = *s.resolve(tx.model(), &args::id_refs(after)?, file)?.last().ok_or("after: no such part")?;
            let v = tx.apply(Cmd::Restep { ids, after })?;
            note = Some(format!("now in step {}", v["step"]));
        }
        other => return Err(format!("unknown op {other:?}")),
    }
    if let Some(after) = anchor {
        let new: Vec<Id> = tx.model().parts.keys().copied().filter(|id| !before.contains(id)).collect();
        if !new.is_empty() {
            let v = tx.apply(Cmd::Restep { ids: new, after })?;
            note = Some(format!("{}moved to the end of step {} (the step of #{after})", note.map(|n| format!("{n}, ")).unwrap_or_default(), v["step"]));
        }
    }
    Ok(Done { kind, note, group, placed, bench })
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply> {
    // `images:false` holds for this call only; the session's own setting comes back afterwards.
    let images = s.images;
    let reply = run_once(s, args);
    s.images = images;
    reply
}

fn run_once(s: &mut Session, args: &Value) -> Result<Reply> {
    super::compare::view(args)?;
    let ops = args.get("ops").and_then(Value::as_array).cloned().ok_or("edit needs \"ops\": a list of operations")?;
    if ops.is_empty() {
        return Err("\"ops\" is empty".into());
    }
    if ops.len() > 2000 {
        return Err("at most 2000 ops per batch".into());
    }
    let units = s.units(args)?;
    let dry = args.get("dry_run").and_then(Value::as_bool).unwrap_or(false);
    let label = args::str_arg(args, "label").unwrap_or("").to_string();
    if args.get("compare").is_none() && args.get("images").and_then(Value::as_bool) == Some(false) {
        s.images = false;
    }
    let view = s.view();
    let file = file_arg(&view, args)?;
    let colour = default_colour(s);
    let user = s.user.clone();
    let turn = s.next_turn();

    // Human edits are proposals accepted at once; a publish conflict (another process changed
    // the model in between) re-reads the project and runs the batch again, once.
    for attempt in 0..2 {
        let mut failed: Option<usize> = None;
        let mut done: Vec<Done> = Vec::new();
        s.engine.drafts.next = s.engine.drafts.next.max(s.store.reserve_draft_ids(1)?);
        // Joints placed from connector seats, keyed to their relative pose (older records are
        // converted here, while their parts are still where they were put).
        let view = s.view();
        let joints: Vec<super::verify::BenchJoint> = super::verify::bench_joints(s, &view).into_iter().filter(|j| j.holds(&view)).collect();
        super::verify::record_bench_joints(s, &view, &[])?;
        let Session { engine, store, lib, user: who, last, .. } = &mut *s;
        let ctx = Ctx { lib: &*lib, store: &*store, user: who.as_str(), last: last.as_slice(), joints: &joints };
        let result = engine.propose_with(&user, turn, &label, dry, |tx| {
            let original: std::collections::HashSet<Id> = tx.model().parts.keys().copied().collect();
            // Inside a batch, "last" means what the earlier ops of this batch made or changed.
            let mut batch_last: Vec<Id> = Vec::new();
            for (i, op) in ops.iter().enumerate() {
                let before: std::collections::HashMap<Id, u64> =
                    tx.model().parts.iter().map(|(id, p)| (*id, p.content_hash())).collect();
                let c = Ctx { last: if batch_last.is_empty() { ctx.last } else { &batch_last }, ..ctx };
                let applied = apply_op(&c, tx, op, file, units, colour);
                if applied.is_ok() {
                    let changed: Vec<Id> = tx.model().parts.iter()
                        .filter(|(id, p)| before.get(*id) != Some(&p.content_hash()))
                        .map(|(id, _)| *id).collect();
                    for id in changed {
                        if !batch_last.contains(&id) {
                            batch_last.push(id);
                        }
                    }
                }
                match applied {
                    Ok(d) => done.push(d),
                    Err(e) => {
                        failed = Some(i);
                        let kind = op.get("op").and_then(Value::as_str).unwrap_or("?");
                        return Err(if ops.len() > 1 { format!("op {} ({kind}): {e}", i + 1) } else { e });
                    }
                }
            }
            // `callout`: the parts this batch adds must be exactly the booklet's callout.
            if let Some(list) = args.get("callout") {
                let moved: std::collections::HashSet<Id> = done.iter().flat_map(|d| d.placed.iter().copied()).collect();
                check_callout(&ctx, list, &original, &moved, tx.model(), file)?;
            }
            // `step`: close the batch as one construction step (`0 STEP`, labelled).
            if let Some(label) = step_label(args.get("step"), &label)? {
                tx.apply(Cmd::Step { file, label })?;
            }
            Ok(())
        });
        let p = match result {
            Ok(p) => p,
            Err(e) => return Ok(failure(s, &view, file, &ops, failed, units, colour, e)),
        };
        // Remember the colour for the next add.
        if let Some(c) = ops.iter().rev().find_map(|o| o.get("color").and_then(Value::as_i64)) {
            let _ = s.store.set_pref(&format!("user:{user}"), "color", &json!(c));
        }
        let created: Vec<Id> = p.touched.created.iter().copied().collect();
        let modified: Vec<Id> = p.touched.modified.iter().copied().collect();
        let removed: Vec<Id> = p.touched.removed.iter().copied().collect();
        let mut summary = Vec::new();
        if !created.is_empty() {
            summary.push(format!("added {} ({})", created.len(), fmt_ids(created.iter().copied())));
        }
        if !modified.is_empty() {
            summary.push(format!("changed {} ({})", modified.len(), fmt_ids(modified.iter().copied())));
        }
        if !removed.is_empty() {
            summary.push(format!("removed {} ({})", removed.len(), fmt_ids(removed.iter().copied())));
        }
        if summary.is_empty() {
            summary.push("no change".into());
        }
        if !dry {
            // A group's name also becomes a named selection, so it works as `ids` everywhere.
            for (name, ids) in done.iter().filter_map(|d| d.group.as_ref()) {
                s.store.create_selection(&user, Some(name), file, ids, Some(&json!({"group": name})))?;
            }
        }
        let notes: Vec<String> = done.iter().filter_map(|d| d.note.as_ref().map(|n| format!("{}: {n}", d.kind))).collect();
        let mut text;
        let mut data = json!({
            "created": created, "modified": modified, "removed": removed,
            "dependencies": p.dependencies, "turn": turn,
        });
        if dry {
            text = format!("Dry run (nothing proposed): {}.", summary.join("; "));
        } else if user == HUMAN && s.store.leased_by_other(&s.branch, "gui:none")? {
            // The GUI owns the working tree: hand it the change to apply as its own undo step.
            let draft = p.draft.ok_or("internal: no draft for the change")?;
            s.save_drafts(&[draft])?;
            text = format!("Queued for the open SuperBrick window, which applies it at once: {}.", summary.join("; "));
            data["draft"] = json!(draft);
            data["queued"] = json!(true);
        } else if user == HUMAN {
            let draft = p.draft.ok_or("internal: no draft for the change")?;
            s.engine.accept(HUMAN, Some(vec![draft]))?;
            let msg = if label.is_empty() { summary.join("; ") } else { label.clone() };
            match s.publish_shared("edit", &msg, &[draft]) {
                Ok(commit) => {
                    text = format!("Done: {}. Version {commit}.", summary.join("; "));
                    data["commit"] = json!(commit);
                }
                Err(e) if e.starts_with("conflict") && attempt == 0 => {
                    s.force_sync()?;
                    continue;
                }
                Err(e) => return Err(e),
            }
        } else {
            let draft = p.draft.ok_or("internal: no draft for the change")?;
            s.save_drafts(&[draft])?;
            let mut deps = String::new();
            if !p.dependencies.is_empty() {
                deps = format!(" Builds on draft{} {}.", if p.dependencies.len() > 1 { "s" } else { "" },
                    p.dependencies.iter().map(|d| d.to_string()).collect::<Vec<_>>().join(", "));
            }
            text = format!("Draft {draft} proposed: {}.{deps} It shows as pending in the GUI until the human accepts it.", summary.join("; "));
            data["draft"] = json!(draft);
        }
        if !notes.is_empty() {
            text.push_str(&format!(" ({})", notes.join("; ")));
        }
        let mut focus: Vec<Id> = created.iter().chain(&modified).copied().collect();
        if !dry {
            s.set_last(focus.clone())?;
        }
        // Problems the change introduced (new floating parts, collisions with free placement).
        let model = p.effective;
        let mut bad = Vec::new();
        // Connector seats the bench proved legal: its overlap test models clips and bars, SuperBrick's doesn't.
        let trusted: std::collections::HashSet<Id> = done.iter().filter(|d| d.bench).flat_map(|d| d.placed.iter().copied()).collect();
        if !focus.is_empty() && model.flat(file).len() <= 4000 {
            let pr = super::verify::problems_with(s, &model, file, &trusted);
            if !dry {
                let mut seats: Vec<(Id, Id)> =
                    pr.joints.iter().copied().filter(|(a, b)| trusted.contains(a) || trusted.contains(b)).collect();
                seats.extend(trusted.iter().map(|&t| (t, t)));
                super::verify::record_bench_joints(s, &model, &seats)?;
            }
            for id in &focus {
                if let Some(st) = pr.by_id.get(id) {
                    if st.iter().any(|x| x == "FLOATING" || x == "COLLISION") {
                        bad.push((*id, st.join("+")));
                    }
                }
            }
        }
        if !bad.is_empty() {
            text.push_str(&format!(
                " Warning: {} of the changed parts {}: {}.",
                bad.len(),
                if bad.len() == 1 { "has a problem" } else { "have problems" },
                bad.iter().take(12).map(|(id, st)| format!("#{id} {st}")).collect::<Vec<_>>().join(", ")
            ));
            data["problems"] = json!(bad.iter().map(|(id, st)| json!({"id": id, "status": st})).collect::<Vec<_>>());
        }
        focus.sort_unstable();
        let mut shot = Shot::new(file, if label.is_empty() { summary.join("; ") } else { label.clone() })
            .role(focus.iter().copied(), Role::New)
            .role(bad.iter().map(|b| b.0), Role::Error);
        shot.id_badges = focus.len() <= 16;
        let before_flat = shots::flat_with_ids(&p.before, file);
        for (f, id) in before_flat {
            if id.is_some_and(|i| p.touched.removed.contains(&i)) {
                shot.extra.push((f, Role::Removed));
            }
        }
        shot.intent = crate::planner::Intent::Edit;
        shot.views = super::look::views_arg(args.get("views"))?.unwrap_or(Views::Auto(if focus.len() > 40 || !bad.is_empty() { 2 } else { 1 }));
        if focus.is_empty() && removed.is_empty() {
            shot.whole = true;
            shot.intent = crate::planner::Intent::Whole;
        }
        if let Some(v) = super::compare::view(args)? {
            shot.views = Views::Angles(vec![v]);
            shot.whole = true;
            shot.intent = crate::planner::Intent::Whole;
            shot.id_badges = false;
        }
        let (img, err) = s.picture(&model, shot);
        if let Some(e) = err {
            text.push(' ');
            text.push_str(&e);
        }
        let img = super::compare::attach(args, img)?;
        let mut reply = Reply::ok(text, data).image(img);
        reply.ok = true;
        return Ok(reply);
    }
    Err("the project kept changing under this edit; try again".into())
}

/// A failed batch: say why, show the clash, and offer the nearest legal alternatives.
#[allow(clippy::too_many_arguments)]
fn failure(s: &mut Session, view: &Model, file: usize, ops: &[Value], failed: Option<usize>, units: Units, colour: i32, err: String) -> Reply {
    // A part the batch was adding has no id worth showing: call it "new".
    let mut err = err;
    let mut fail = s.engine.last_failure.clone();
    if let Some(list) = fail.as_mut().and_then(|f| f.get_mut("collisions")).and_then(Value::as_array_mut) {
        for c in list {
            if let Some(m) = c["moving"].as_u64().filter(|m| !view.parts.contains_key(m)) {
                let name = c["moving_part"].as_str().unwrap_or("?").to_string();
                err = err.replace(&format!("{name} #{m}"), &format!("new {name}"));
                c["moving"] = Value::Null;
                c["new"] = json!(true);
            }
        }
    }
    let mut text = format!("Nothing changed: {err}");
    let mut data = json!({"error": err});
    let mut shot = Shot::new(file, "failed: what is in the way");
    let mut alternatives: Option<String> = None;
    let op = failed.and_then(|i| ops.get(i));
    // The attempted parts, drawn in red where they would have gone.
    let mut attempted: Vec<PartSpec> = Vec::new();
    if let Some(op) = op {
        let kind = op.get("op").and_then(Value::as_str).unwrap_or("");
        if kind == "add" || (kind == "place" && op.get("from").is_none()) {
            match (op.get("ldraw"), op.get("parts").and_then(Value::as_array)) {
                (Some(lines), _) => attempted.extend(ldraw_specs(lines, colour).unwrap_or_default()),
                (None, Some(list)) => attempted.extend(list.iter().filter_map(|p| spec(p, units, colour).ok())),
                (None, None) => attempted.extend(spec(op, units, colour).ok()),
            }
        } else if let Ok(ids) = s.ids_arg(view, op, "ids", file) {
            shot = shot.role(ids, Role::Error);
        }
    }
    for p in &attempted {
        shot.extra.push((
            ldraw::FlatPart { name: p.name.clone(), color: ldraw::Color(4), pos: p.pos, rot: p.rot, ..blank_flat() },
            Role::Error,
        ));
    }
    let mut clashes: Vec<String> = Vec::new();
    if let Some(f) = &fail {
        data["failure"] = f.clone();
        for (k, c) in f["collisions"].as_array().into_iter().flatten().take(6).enumerate() {
            let letter = (b'A' + k as u8) as char;
            if let Some(b) = c["blocker"].as_u64() {
                shot.roles.insert(b, Role::Blocker);
            }
            if let Some(m) = c["moving"].as_u64() {
                if view.parts.contains_key(&m) {
                    shot.roles.insert(m, Role::Error);
                }
            }
            if let (Some(lo), Some(hi)) = (c["box"][0].as_array(), c["box"][1].as_array()) {
                let v = |a: &Vec<Value>| DVec3::new(a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0), a[2].as_f64().unwrap_or(0.0));
                let clear = c["clear_by"].as_array().map(|a| {
                    let d = DVec3::new(a[0].as_f64().unwrap_or(0.0), a[1].as_f64().unwrap_or(0.0), a[2].as_f64().unwrap_or(0.0));
                    let g = units.from_ldu(d);
                    format!(", clears if moved by [{:.0}, {:.0}, {:.0}] {}", g[0], g[1], g[2], units.name())
                });
                shot.boxes.push(BoxMark { lo: v(lo), hi: v(hi), color: overlay::ORANGE, text: letter.to_string() });
                let blocker = c["blocker"].as_u64().map(|b| format!("#{b} ")).unwrap_or_default();
                clashes.push(format!(
                    "{letter}: {blocker}{} overlaps by {:.0} LDU{}",
                    c["blocker_part"].as_str().unwrap_or("?").trim_end_matches(".dat"),
                    c["depth_ldu"].as_f64().unwrap_or(0.0),
                    clear.unwrap_or_default()
                ));
            }
        }
        shot.legend = vec![(overlay::RED, "attempted".into()), (overlay::ORANGE, "in the way".into())];
    }
    // Nearest legal seats for a single failed part.
    if attempted.len() == 1 {
        let part = attempted[0].clone();
        let eff = s.view();
        let saved = std::mem::replace(&mut s.engine.model, eff);
        let found = s.engine.stud_candidates(file, &part, part.pos, DVec3::ZERO, &[], 160.0, 3);
        s.engine.model = saved;
        if let Ok(found) = found {
            if !found.is_empty() {
                let items: Vec<Value> = found
                    .iter()
                    .enumerate()
                    .map(|(i, p)| {
                        json!({"label": (i + 1).to_string(), "at": units.from_ldu(p.pos), "rot": args::rot_rows(p.rot),
                               "cmds": [Cmd::Add { file, parts: vec![p.clone()], free: false }]})
                    })
                    .collect();
                let tree = s.store.working(&s.branch).map(|w| w.tree).unwrap_or_default();
                if let Ok(id) = s.store.create_candidates(&s.user.clone(), &json!({"file": file, "units": units.name(), "items": items}), &tree) {
                    // Far seats would zoom the clash out of sight: list them, draw only near ones.
                    let near = found.iter().all(|p| (p.pos - part.pos).length() <= 80.0);
                    for (i, p) in found.iter().enumerate().filter(|_| near) {
                        shot.extra.push((
                            ldraw::FlatPart { name: p.name.clone(), color: ldraw::Color(p.color), pos: p.pos, rot: p.rot, ..blank_flat() },
                            Role::Ghost,
                        ));
                        let (lo, hi) = shots::part_box(&s.lib, &p.name, p.pos, p.rot).unwrap_or((p.pos, p.pos));
                        shot.labels.push(Label { text: format!("{}", i + 1), at: (lo + hi) * 0.5, color: overlay::BLUE });
                    }
                    let list: Vec<String> = found
                        .iter()
                        .enumerate()
                        .map(|(i, p)| {
                            let g = units.from_ldu(p.pos);
                            let d = (p.pos - part.pos).length();
                            format!("{} at [{:.0}, {:.0}, {:.0}] ({:.0} LDU away)", i + 1, g[0], g[1], g[2], d)
                        })
                        .collect();
                    alternatives = Some(format!(
                        " Nearest legal seats (blue, set {id}): {}. Use {{op:\"place\", from:\"{id}\", pick:N}}.",
                        list.join("; ")
                    ));
                    data["candidates"] = json!(id);
                    if near {
                        shot.legend.push((overlay::BLUE, format!("legal seats ({id})")));
                    }
                }
            }
        }
    }
    if !clashes.is_empty() {
        text.push_str(&format!(" Clashes (orange boxes): {}.", clashes.join("; ")));
    }
    if let Some(a) = alternatives {
        text.push_str(&a);
    }
    if !clashes.is_empty() {
        text.push_str(
            " If this is a clip, bar, pin, hinge or other non-stud seat, this check can't see the joint: find the seat with \
             where {part, near_ids, connectors:true} and place it from there (those seats are checked by the connector bench).",
        );
    }
    if !shot.roles.is_empty() || !shot.extra.is_empty() || !shot.boxes.is_empty() {
        let n = census(s, view, &shot.roles.keys().copied().collect::<Vec<_>>(), 4);
        if !n.is_empty() {
            text.push_str(&format!(" In the way: {n}."));
        }
        shot.views = Views::Auto(2);
        shot.intent = crate::planner::Intent::Error;
        shot.dim_rest = true;
        let (img, e) = s.picture(view, shot);
        if let Some(e) = e {
            text.push(' ');
            text.push_str(&e);
        }
        return Reply::fail(text, data).image(img);
    }
    Reply::fail(text, data)
}

pub(crate) fn blank_flat() -> ldraw::FlatPart {
    ldraw::FlatPart {
        name: String::new(),
        color: ldraw::Color(16),
        pos: DVec3::ZERO,
        rot: DMat3::IDENTITY,
        step: 1,
        path: String::new(),
        src: (0, 0, 0),
        root_line: usize::MAX,
        ancestors: Vec::new(),
    }
}
