//! `verify`: collisions, floating groups and connection strength, with pictures.
use super::{census, file_arg, fmt_ids, ids_prop, views_prop};
use crate::args::Result;
use crate::shots::{self, BoxMark, Label, Role, Shot, Views};
use crate::{overlay, Reply, Session};
use engine::strength;
use engine::validation::Report;
use engine::{Id, Model};
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ids": ids_prop("Only report problems involving these parts"),
            "between": {"type": "object", "description": "Strength between two regions: {a: ids, b: ids}. Reports how many connections hold them together at the weakest cut, and where.",
                        "properties": {"a": {}, "b": {}}},
            "weakest": {"description": "Find the weakest point of the group containing these ids (or true for the largest group)."},
            "file": {"type": "integer"},
            "views": views_prop()
        },
        "additionalProperties": false
    })
}

/// Validator findings keyed by part id.
pub(crate) struct Problems {
    pub by_id: BTreeMap<Id, Vec<String>>,
    pub collisions: Vec<(Id, Id, f64)>,
    pub floating: Vec<Vec<Id>>,
    pub groups: usize,
    pub verdict: String,
    pub pieces: usize,
    pub limitations: Vec<String>,
    /// Flat index → part id.
    pub ids: Vec<Option<Id>>,
    pub report: Report,
    /// Overlaps not counted as collisions: joints the connector bench proved (clips, bars, pins).
    pub excused: Vec<(Id, Id)>,
    /// Touching or overlapping pairs held by such a joint (they connect groups for floating checks).
    pub joints: Vec<(Id, Id)>,
}

const BENCH_JOINTS: &str = "bench_joints";

/// Joints the connector bench proved legal although the stud-only overlap test sees a collision:
/// A joint the connector bench proved (clip, bar, pin, window in frame): the two parts and their
/// relative pose when it was made. It holds while that relative pose is unchanged, so moving or
/// turning both parts together keeps it.
#[derive(Clone, Copy, Debug)]
pub(crate) struct BenchJoint {
    pub a: Id,
    pub b: Id,
    rel: [f64; 12],
}

fn rel_pose(pa: &engine::Part, pb: &engine::Part) -> [f64; 12] {
    let rt = pa.rot.transpose();
    let r = (rt * pb.rot).to_cols_array();
    let t = rt * (pb.pos - pa.pos);
    [r[0], r[1], r[2], r[3], r[4], r[5], r[6], r[7], r[8], t.x, t.y, t.z]
}

impl BenchJoint {
    fn new(model: &Model, a: Id, b: Id) -> Option<BenchJoint> {
        let (pa, pb) = (model.parts.get(&a)?, model.parts.get(&b)?);
        Some(BenchJoint { a, b, rel: rel_pose(pa, pb) })
    }
    pub fn holds(&self, model: &Model) -> bool {
        match (model.parts.get(&self.a), model.parts.get(&self.b)) {
            (Some(pa), Some(pb)) => rel_pose(pa, pb).iter().zip(self.rel).all(|(x, y)| (x - y).abs() < 0.05),
            _ => false,
        }
    }
    pub fn joins(&self, a: Id, b: Id) -> bool {
        (self.a == a && self.b == b) || (self.a == b && self.b == a)
    }
}

/// Parts whose boxes come within 2 LDU of part `id` (same model file).
fn neighbours(s: &Session, model: &Model, id: Id) -> Vec<Id> {
    let Some(p) = model.parts.get(&id) else { return vec![] };
    let Some((lo, hi)) = shots::part_box(&s.lib, &p.name, p.pos, p.rot) else { return vec![] };
    let (lo, hi) = (lo - DVec3::splat(2.0), hi + DVec3::splat(2.0));
    model
        .parts
        .values()
        .filter(|o| o.id != id && o.file == p.file)
        .filter(|o| {
            shots::part_box(&s.lib, &o.name, o.pos, o.rot)
                .is_some_and(|(l2, h2)| lo.cmple(h2).all() && hi.cmpge(l2).all())
        })
        .map(|o| o.id)
        .collect()
}

/// Recorded joints. Older records (a, b, hash a, hash b) are converted while both parts are unmoved;
/// (a, a, ..) meant "a sits on a bench seat" and becomes joints to the parts around it.
pub(crate) fn bench_joints(s: &Session, model: &Model) -> Vec<BenchJoint> {
    let v = s.store.pref("project", BENCH_JOINTS).ok().flatten().unwrap_or(Value::Null);
    let mut out = Vec::new();
    for e in v.as_array().into_iter().flatten() {
        if let (Some(a), Some(b), Some(rel)) = (e.get("a").and_then(Value::as_u64), e.get("b").and_then(Value::as_u64), e.get("rel").and_then(Value::as_array)) {
            if rel.len() == 12 {
                let mut r = [0.0; 12];
                for (k, x) in rel.iter().enumerate() {
                    r[k] = x.as_f64().unwrap_or(f64::NAN);
                }
                out.push(BenchJoint { a, b, rel: r });
            }
            continue;
        }
        let n = |k: usize| e.get(k).and_then(Value::as_u64);
        let (Some(a), Some(b), Some(ha), Some(hb)) = (n(0), n(1), n(2), n(3)) else { continue };
        let same = |id: Id, h: u64| model.parts.get(&id).is_some_and(|p| p.content_hash() == h);
        if !(same(a, ha) && same(b, hb)) {
            continue;
        }
        if a == b {
            out.extend(neighbours(s, model, a).into_iter().filter_map(|o| BenchJoint::new(model, a, o)));
        } else if let Some(j) = BenchJoint::new(model, a, b) {
            out.push(j);
        }
    }
    out
}

/// Remember bench-proven joints (pairs of part ids in `model`; (a, a) = a sits on a bench seat,
/// joined to whatever touches it) so collisions and floating checks don't report them.
pub(crate) fn record_bench_joints(s: &mut Session, model: &Model, pairs: &[(Id, Id)]) -> Result<()> {
    let mut all = bench_joints(s, model);
    let mut add = Vec::new();
    for &(a, b) in pairs {
        if a == b {
            add.extend(neighbours(s, model, a).into_iter().map(|o| (a, o)));
        } else {
            add.push((a, b));
        }
    }
    for (a, b) in add {
        let Some(j) = BenchJoint::new(model, a, b) else { continue };
        all.retain(|e| !e.joins(a, b));
        all.push(j);
    }
    all.retain(|j| model.parts.contains_key(&j.a) && model.parts.contains_key(&j.b));
    let keep = all.len().saturating_sub(4000);
    let v = json!(all[keep..].iter().map(|j| json!({"a": j.a, "b": j.b, "rel": j.rel.to_vec()})).collect::<Vec<_>>());
    s.store.set_pref("project", BENCH_JOINTS, &v)
}

pub(crate) fn problems(s: &mut Session, model: &Model, file: usize) -> Problems {
    problems_with(s, model, file, &HashSet::new())
}

/// Like `problems`, also excusing overlaps that touch `seated` (parts just placed on a bench-proven seat).
pub(crate) fn problems_with(s: &mut Session, model: &Model, file: usize, seated: &HashSet<Id>) -> Problems {
    let parts = shots::flat_with_ids(model, file);
    let flat: Vec<ldraw::FlatPart> = parts.iter().map(|(f, _)| f.clone()).collect();
    let ids: Vec<Option<Id>> = parts.iter().map(|(_, id)| *id).collect();
    let report = s.engine.validator.check(&s.lib, &flat);
    // Overlaps between parts the connector bench joins (clip, bar, pin, window in frame) are by design.
    let linked: HashSet<(Id, Id)> = if report.rows.iter().any(|r| !r.collides.is_empty()) {
        engine::connectors::analyze(model, file)
            .map(|a| {
                a.graph
                    .joints
                    .iter()
                    .filter(|j| j.label != "stud")
                    .map(|j| (j.a.min(j.b), j.a.max(j.b)))
                    .collect()
            })
            .unwrap_or_default()
    } else {
        HashSet::new()
    };
    let recorded: Vec<BenchJoint> = bench_joints(s, model).into_iter().filter(|j| j.holds(model)).collect();
    // A part placed on a bench-proven seat is attached even when the stud-only check sees no link.
    let seat_ok = |id: Id| seated.contains(&id) || recorded.iter().any(|j| j.a == id || j.b == id);
    let proven = |a: Id, b: Id| {
        seated.contains(&a) || seated.contains(&b) || recorded.iter().any(|j| j.joins(a, b)) || linked.contains(&(a.min(b), a.max(b)))
    };
    let overlap: HashMap<(usize, usize), f64> = report.pairs.iter().map(|p| ((p.a.min(p.b), p.a.max(p.b)), p.overlap)).collect();
    let mut by_id: BTreeMap<Id, Vec<String>> = BTreeMap::new();
    let mut collisions = Vec::new();
    // The validator lists a collision on one row only (the later part), so pairs
    // are deduplicated rather than filtered by index order.
    let mut seen: BTreeSet<(usize, usize)> = BTreeSet::new();
    for (i, row) in report.rows.iter().enumerate() {
        let Some(id) = ids.get(i).copied().flatten() else { continue };
        for st in &row.status {
            if st == "COLLISION" || st == "FLOATING" {
                let e = by_id.entry(id).or_default();
                if !e.contains(st) {
                    e.push(st.clone());
                }
            }
        }
        for &j in &row.collides {
            let key = (i.min(j), i.max(j));
            if j == i || !seen.insert(key) {
                continue;
            }
            if let Some(other) = ids.get(j).copied().flatten() {
                if other != id {
                    let (a, b) = if id < other { (id, other) } else { (other, id) };
                    collisions.push((a, b, overlap.get(&key).copied().unwrap_or(0.0)));
                    by_id.entry(other).or_default().retain(|s| s != "COLLISION");
                    by_id.entry(other).or_default().push("COLLISION".into());
                }
            }
        }
    }
    let (excused, kept): (Vec<(Id, Id, f64)>, Vec<(Id, Id, f64)>) = collisions.into_iter().partition(|&(a, b, _)| proven(a, b));
    let collisions = kept;
    let still: HashSet<Id> = collisions.iter().flat_map(|&(a, b, _)| [a, b]).collect();
    for &(a, b, _) in &excused {
        for id in [a, b] {
            if !still.contains(&id) {
                if let Some(e) = by_id.get_mut(&id) {
                    e.retain(|x| x != "COLLISION");
                }
            }
        }
    }
    by_id.retain(|_, v| !v.is_empty());
    let excused: Vec<(Id, Id)> = excused.into_iter().map(|(a, b, _)| (a, b)).collect();
    let mut joints: Vec<(Id, Id)> = Vec::new();
    for p in &report.pairs {
        if !(p.contact || p.overlap > 0.0) {
            continue;
        }
        let (Some(a), Some(b)) = (ids.get(p.a).copied().flatten(), ids.get(p.b).copied().flatten()) else { continue };
        if a != b && proven(a, b) && !joints.contains(&(a.min(b), a.max(b))) {
            joints.push((a.min(b), a.max(b)));
        }
    }
    let mut floating = Vec::new();
    for g in &report.groups {
        let rows: Vec<usize> = g.iter().map(|uid| uid.saturating_sub(1)).collect();
        if rows.iter().any(|&r| report.rows.get(r).is_some_and(|row| row.status.iter().any(|s| s == "FLOATING"))) {
            let set: BTreeSet<Id> = rows.iter().filter_map(|&r| ids.get(r).copied().flatten()).collect();
            // A bench-proven joint (window in its frame, bar in a clip) to a part outside the group holds it.
            let mut held = joints.iter().any(|(a, b)| set.contains(a) != set.contains(b))
                || recorded.iter().any(|j| set.contains(&j.a) != set.contains(&j.b));
            if !held {
                let boxes: Vec<(usize, (DVec3, DVec3))> =
                    flat.iter().enumerate().filter_map(|(i, f)| shots::flat_box(&s.lib, f).map(|b| (i, b))).collect();
                held = boxes.iter().any(|&(i, (lo, hi))| {
                    let Some(id) = ids.get(i).copied().flatten() else { return false };
                    set.contains(&id)
                        && seat_ok(id)
                        && boxes.iter().any(|&(j, (lo2, hi2))| {
                            ids.get(j).copied().flatten().is_some_and(|o| !set.contains(&o))
                                && (lo - DVec3::splat(2.0)).cmple(hi2).all()
                                && (hi + DVec3::splat(2.0)).cmpge(lo2).all()
                        })
                });
            }
            if held {
                for id in &set {
                    if let Some(e) = by_id.get_mut(id) {
                        e.retain(|x| x != "FLOATING");
                    }
                }
                continue;
            }
            floating.push(set.into_iter().collect());
        }
    }
    by_id.retain(|_, v| !v.is_empty());
    Problems {
        by_id,
        collisions,
        floating,
        groups: report.groups.len(),
        verdict: report.verdict.clone(),
        pieces: report.pieces,
        limitations: report.limitations.clone(),
        ids,
        report,
        excused,
        joints,
    }
}

/// Connection graph edges between flat parts: weight = studs engaged (1 for other links).
pub(crate) fn connection_edges(report: &Report) -> Vec<(usize, usize, f64)> {
    engine::strength::edges(report)
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply> {
    let view = s.view();
    let file = file_arg(&view, args)?;
    if args.get("between").is_some() || args.get("weakest").is_some() {
        return strength_run(s, &view, file, args);
    }
    let focus: BTreeSet<Id> = s.ids_arg(&view, args, "ids", file)?.into_iter().collect();
    let pr = problems(s, &view, file);
    let relevant = |id: &Id| focus.is_empty() || focus.contains(id);
    let collisions: Vec<_> = pr.collisions.iter().filter(|(a, b, _)| relevant(a) || relevant(b)).cloned().collect();
    let floating: Vec<_> = pr.floating.iter().filter(|g| g.iter().any(relevant)).cloned().collect();
    let mut lines = Vec::new();
    let name = |id: &Id| view.parts.get(id).map(|p| p.name.trim_end_matches(".dat").to_string()).unwrap_or_default();
    // Problems shown in their own panel get a letter (collisions) or F1.. (floating groups).
    const PANELS: usize = 5;
    let shown_c = collisions.len().min(PANELS);
    let shown_f = floating.len().min(PANELS - shown_c);
    let letter = |k: usize| ((b'A' + k as u8) as char).to_string();
    for (k, (a, b, o)) in collisions.iter().take(12).enumerate() {
        let tag = if k < shown_c { format!("{}: ", letter(k)) } else { String::new() };
        lines.push(format!("{tag}#{a} {} collides with #{b} {} (overlap about {:.0} LDU³)", name(a), name(b), o));
    }
    for (k, g) in floating.iter().take(6).enumerate() {
        let tag = if k < shown_f { format!("F{}: ", k + 1) } else { String::new() };
        lines.push(format!(
            "{tag}{} part{} ({}) not connected to the rest and not resting on the ground: {}",
            g.len(),
            if g.len() == 1 { "" } else { "s" },
            census(s, &view, g, 3),
            fmt_ids(g.iter().copied())
        ));
    }
    let joints = pr.excused.iter().filter(|(a, b)| relevant(a) || relevant(b)).count();
    if joints > 0 {
        lines.push(format!(
            "{joints} clip/bar/pin joint{} placed from connector seats overlap by design and {} not counted as collisions",
            if joints == 1 { "" } else { "s" },
            if joints == 1 { "is" } else { "are" }
        ));
    }
    let scope = if focus.is_empty() { format!("{} pieces", pr.pieces) } else { format!("{} chosen parts", focus.len()) };
    let ok = collisions.is_empty() && floating.is_empty();
    let mut text = if ok {
        format!("No collisions or floating parts among {scope} ({} connected groups).", pr.groups)
    } else {
        format!(
            "{} collision{}, {} floating group{} among {scope}: {}.",
            collisions.len(),
            if collisions.len() == 1 { "" } else { "s" },
            floating.len(),
            if floating.len() == 1 { "" } else { "s" },
            lines.join("; ")
        )
    };
    if collisions.len() > 12 {
        text.push_str(&format!(" (+{} more collisions)", collisions.len() - 12));
    }
    if let Some(l) = pr.limitations.first() {
        text.push_str(&format!(" Note: {l}"));
    }
    let data = json!({
        "ok": ok, "verdict": pr.verdict, "pieces": pr.pieces, "groups": pr.groups,
        "collisions": collisions.iter().map(|(a, b, o)| json!({"a": a, "b": b, "overlap": o.round()})).collect::<Vec<_>>(),
        "floating": floating,
    });
    let mut shot = Shot::new(file, if ok { "no problems".to_string() } else { format!("{} problems", lines.len()) });
    let bad: BTreeSet<Id> = collisions.iter().flat_map(|(a, b, _)| [*a, *b]).chain(floating.iter().flatten().copied()).collect();
    if bad.is_empty() {
        shot.whole = focus.is_empty();
        shot = shot.role(focus.iter().copied(), Role::Selected);
        shot.views = super::look::views_arg(args.get("views"))?.unwrap_or(Views::Auto(1));
        shot.intent = crate::planner::Intent::Whole;
    } else {
        // An overview that says where, then one close-up per problem.
        shot.dim_rest = true;
        shot.id_badges = true;
        shot.overview = true;
        shot.title = format!("{} problem{}", lines.len(), if lines.len() == 1 { "" } else { "s" });
        for (k, (a, b, o)) in collisions.iter().take(shown_c).enumerate() {
            let boxes = (shots::ids_box(&s.lib, &view, &[*a]), shots::ids_box(&s.lib, &view, &[*b]));
            let (at, overlap_box) = match boxes {
                (Some((alo, ahi)), Some((blo, bhi))) => {
                    let (lo, hi) = (alo.max(blo), ahi.min(bhi));
                    ((lo + hi) * 0.5, ((hi - lo).min_element() > 0.5).then_some(BoxMark { lo, hi, color: overlay::RED, text: String::new() }))
                }
                (Some((lo, hi)), None) | (None, Some((lo, hi))) => ((lo + hi) * 0.5, None),
                (None, None) => (DVec3::ZERO, None),
            };
            shot.items.push(shots::Item {
                label: letter(k),
                at,
                roles: vec![(*a, Role::Error), (*b, Role::Blocker)],
                note: format!("#{a} {} (red) × #{b} {} (orange) · overlap ≈{:.0} LDU³", name(a), name(b), o),
                boxes: overlap_box.into_iter().collect(),
                color: Some(overlay::RED),
                ..Default::default()
            });
        }
        for (k, g) in floating.iter().take(shown_f).enumerate() {
            let at = shots::ids_box(&s.lib, &view, g).map_or(DVec3::ZERO, |(lo, hi)| (lo + hi) * 0.5);
            shot.items.push(shots::Item {
                label: format!("F{}", k + 1),
                at,
                roles: g.iter().map(|id| (*id, Role::Error)).collect(),
                note: format!("floating: {} part{} ({})", g.len(), if g.len() == 1 { "" } else { "s" }, census(s, &view, g, 3)),
                color: Some(overlay::RED),
                ..Default::default()
            });
        }
        if collisions.len() + floating.len() > shown_c + shown_f {
            text.push_str(&format!(" The picture shows the first {}; the rest are listed above.", shown_c + shown_f));
        }
    }
    let (img, e) = s.picture(&view, shot);
    if let Some(e) = e {
        text.push(' ');
        text.push_str(&e);
    }
    Ok(if ok { Reply::ok(text, data) } else { Reply::fail(text, data) }.image(img))
}

fn strength_run(s: &mut Session, view: &Model, file: usize, args: &Value) -> Result<Reply> {
    let pr = problems(s, view, file);
    let n = pr.ids.len();
    let edges = connection_edges(&pr.report);
    let rows_of = |ids: &BTreeSet<Id>| -> Vec<usize> {
        pr.ids.iter().enumerate().filter(|(_, id)| id.is_some_and(|id| ids.contains(&id))).map(|(i, _)| i).collect()
    };
    let (cut, title, a_ids) = if let Some(b) = args.get("between") {
        let a: BTreeSet<Id> = s.resolve(view, &crate::args::id_refs(b.get("a").ok_or("between needs a")?)?, file)?.into_iter().collect();
        let bb: BTreeSet<Id> = s.resolve(view, &crate::args::id_refs(b.get("b").ok_or("between needs b")?)?, file)?.into_iter().collect();
        if a.is_empty() || bb.is_empty() {
            return Err("between: a and b must each name parts".into());
        }
        (strength::min_cut(n, &edges, &rows_of(&a), &rows_of(&bb)), "strength between a and b".to_string(), a)
    } else {
        let w = &args["weakest"];
        let seed: BTreeSet<Id> = if w.as_bool() == Some(true) {
            BTreeSet::new()
        } else {
            s.resolve(view, &crate::args::id_refs(w)?, file)?.into_iter().collect()
        };
        let seed_rows = rows_of(&seed);
        let group = pr
            .report
            .groups
            .iter()
            .map(|g| g.iter().map(|u| u.saturating_sub(1)).collect::<Vec<_>>())
            .filter(|g| seed_rows.is_empty() || g.iter().any(|r| seed_rows.contains(r)))
            .max_by_key(|g| g.len())
            .ok_or("no connected group found")?;
        if group.len() > 1500 {
            return Err(format!("the group has {} pieces; name a smaller region with ids for weakest", group.len()));
        }
        let c = strength::weakest_cut(n, &edges, &group).ok_or("the group has only one piece")?;
        // Report the smaller side as "a".
        let side: BTreeSet<Id> = group.iter().filter(|&&r| c.side[r]).filter_map(|&r| pr.ids[r]).collect();
        let other: BTreeSet<Id> = group.iter().filter(|&&r| !c.side[r]).filter_map(|&r| pr.ids[r]).collect();
        let a = if side.len() <= other.len() { side } else { other };
        (c, format!("weakest point of a {}-piece group", group.len()), a)
    };
    let mut cut_parts = BTreeSet::new();
    let mut pairs = Vec::new();
    for &(u, v, w) in &cut.edges {
        if let (Some(a), Some(b)) = (pr.ids[u], pr.ids[v]) {
            cut_parts.insert(a);
            cut_parts.insert(b);
            pairs.push((a, b, w));
        }
    }
    let value = cut.value;
    let verdict = match value as i64 {
        0 => "not connected at all",
        1 => "very weak (one connection)",
        2..=3 => "weak",
        4..=7 => "moderate",
        _ => "sturdy",
    };
    let mut text = format!(
        "{title}: {value:.0} connection{} at the weakest cut — {verdict}. The cut separates {} part{} ({}).",
        if value == 1.0 { "" } else { "s" },
        a_ids.len(),
        if a_ids.len() == 1 { "" } else { "s" },
        census(s, view, &a_ids.iter().copied().collect::<Vec<_>>(), 4)
    );
    if !pairs.is_empty() {
        text.push_str(&format!(
            " Cut links: {}.",
            pairs.iter().take(12).map(|(a, b, w)| format!("#{a}–#{b} ({w:.0})")).collect::<Vec<_>>().join(", ")
        ));
    }
    let mut shot = Shot::new(file, format!("{title}: {value:.0}"))
        .role(a_ids.iter().copied(), Role::Selected)
        .role(cut_parts.iter().copied(), Role::Error);
    shot.dim_rest = false;
    shot.id_badges = cut_parts.len() <= 16;
    for (a, b, w) in pairs.iter().take(8) {
        if let (Some((alo, ahi)), Some((blo, bhi))) = (shots::ids_box(&s.lib, view, &[*a]), shots::ids_box(&s.lib, view, &[*b])) {
            shot.labels.push(Label { text: format!("{w:.0}"), at: ((alo + ahi) * 0.5 + (blo + bhi) * 0.5) * 0.5, color: overlay::RED });
        }
    }
    shot.legend = vec![(overlay::RED, "cut".into()), (overlay::BLUE, "separated side".into())];
    shot.views = super::look::views_arg(args.get("views"))?.unwrap_or(Views::Auto(2));
    let (img, e) = s.picture(view, shot);
    if let Some(e) = e {
        text.push(' ');
        text.push_str(&e);
    }
    let data = json!({"strength": value, "verdict": verdict, "side": a_ids,
                      "cut": pairs.iter().map(|(a, b, w)| json!({"a": a, "b": b, "connections": w})).collect::<Vec<_>>()});
    Ok(Reply::ok(text, data).image(img))
}
