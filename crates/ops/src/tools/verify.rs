//! `verify`: the checker's verdict (collisions, conflicts, floating and loose parts) and
//! connection strength, with pictures.
use super::{census, file_arg, fmt_ids, ids_prop, views_prop};
use crate::args::Result;
use crate::shots::{self, BoxMark, Label, Role, Shot, Views};
use crate::{overlay, Reply, Session};
use engine::strength;
use engine::checker::Report;
use engine::{Id, Model};
use glam::DVec3;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

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

/// One collision or conflict between two pieces (part ids; equal when both are inside one submodel).
#[derive(Clone, Debug)]
pub(crate) struct Clash {
    pub a: Id,
    pub b: Id,
    pub a_part: String,
    pub b_part: String,
    /// "collision" or "conflict".
    pub kind: String,
    pub overlap: f64,
    pub reason: String,
    pub bounds: Option<(DVec3, DVec3)>,
    pub at: Option<DVec3>,
}

/// The checker's findings for one model file, keyed by part id.
pub(crate) struct Problems {
    /// Statuses other than OK, per part id.
    pub by_id: BTreeMap<Id, Vec<String>>,
    pub clashes: Vec<Clash>,
    pub floating: Vec<Vec<Id>>,
    pub loose: Vec<Id>,
    pub unverified: Vec<Id>,
    /// Flat index → part id.
    pub ids: Vec<Option<Id>>,
    pub report: Report,
}

/// Run the checker on `file` of `model`.
pub(crate) fn problems(_s: &mut Session, model: &Model, file: usize) -> Result<Problems> {
    let report = engine::checker::check(model, file).map_err(|e| format!("checker: {e}"))?;
    let mut ids = vec![None; report.pieces];
    for r in &report.rows {
        if let Some(slot) = ids.get_mut(r.index()) {
            *slot = r.id;
        }
    }
    let flat = model.flat(file);
    let name = |i: usize| flat.get(i).map(|f| f.name.trim_end_matches(".dat").to_string()).unwrap_or_default();
    let mut by_id: BTreeMap<Id, Vec<String>> = BTreeMap::new();
    let (mut loose, mut unverified) = (BTreeSet::new(), BTreeSet::new());
    for r in &report.rows {
        let Some(id) = r.id else { continue };
        for st in r.status.iter().filter(|s| s.as_str() != "OK") {
            let e = by_id.entry(id).or_default();
            if !e.contains(st) {
                e.push(st.clone());
            }
            match st.as_str() {
                "LOOSE" => drop(loose.insert(id)),
                "UNVERIFIED" => drop(unverified.insert(id)),
                _ => {}
            }
        }
    }
    let mut clashes = Vec::new();
    let mut seen = BTreeSet::new();
    for (i, j, h) in report.hits() {
        let (Some(a), Some(b)) = (ids.get(i).copied().flatten(), ids.get(j).copied().flatten()) else { continue };
        if !seen.insert((a.min(b), a.max(b), h.kind.clone(), name(i).min(name(j)), name(i).max(name(j)))) {
            continue;
        }
        // The other piece gets the status too (the checker lists it on the later piece only).
        let e = by_id.entry(b).or_default();
        let st = if h.kind == "collision" { "COLLISION" } else { "CONFLICT" };
        if !e.iter().any(|x| x == st) {
            e.push(st.into());
        }
        clashes.push(Clash {
            a,
            b,
            a_part: name(i),
            b_part: name(j),
            kind: h.kind.clone(),
            overlap: h.overlap,
            reason: h.reason.clone(),
            bounds: h.bounds.map(|[lo, hi]| (DVec3::from(lo), DVec3::from(hi))),
            at: h.at.map(DVec3::from),
        });
    }
    let mut floating = Vec::new();
    for g in &report.groups {
        if g.iter().any(|&uid| report.row(uid).is_some_and(|r| r.has("FLOATING"))) {
            let set: BTreeSet<Id> = g.iter().filter_map(|&uid| ids.get(uid - 1).copied().flatten()).collect();
            floating.push(set.into_iter().collect());
        }
    }
    Ok(Problems { by_id, clashes, floating, loose: loose.into_iter().collect(), unverified: unverified.into_iter().collect(), ids, report })
}

/// Connection graph edges between flat parts: weight = studs engaged (1 for other links).
pub(crate) fn connection_edges(report: &Report) -> Vec<(usize, usize, f64)> {
    engine::strength::edges(report)
}

/// "a, b and c" style counts of the statuses that are not OK: "2 collisions, 1 floating group".
fn tally(parts: &[(usize, &str)]) -> String {
    let v: Vec<String> = parts
        .iter()
        .filter(|(n, _)| *n > 0)
        .map(|&(n, what)| format!("{n} {what}{}", if n == 1 { "" } else { "s" }))
        .collect();
    v.join(", ")
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply> {
    let view = s.view();
    let file = file_arg(&view, args)?;
    if args.get("between").is_some() || args.get("weakest").is_some() {
        return strength_run(s, &view, file, args);
    }
    let focus: BTreeSet<Id> = s.ids_arg(&view, args, "ids", file)?.into_iter().collect();
    let pr = problems(s, &view, file)?;
    let report = &pr.report;
    let relevant = |id: &Id| focus.is_empty() || focus.contains(id);
    let clashes: Vec<&Clash> = pr.clashes.iter().filter(|c| relevant(&c.a) || relevant(&c.b)).collect();
    let floating: Vec<_> = pr.floating.iter().filter(|g| g.iter().any(relevant)).cloned().collect();
    let loose: Vec<Id> = pr.loose.iter().copied().filter(relevant).collect();
    let unverified: Vec<Id> = pr.unverified.iter().copied().filter(relevant).collect();
    let unchecked = report.count("UNCHECKED");
    let n_coll = clashes.iter().filter(|c| c.kind == "collision").count();
    let n_conf = clashes.len() - n_coll;
    let mut lines = Vec::new();
    // Problems shown in their own panel get a letter (collisions, conflicts) or F1.. (floating groups).
    const PANELS: usize = 5;
    let shown_c = clashes.len().min(PANELS);
    let shown_f = floating.len().min(PANELS - shown_c);
    let letter = |k: usize| ((b'A' + k as u8) as char).to_string();
    for (k, c) in clashes.iter().take(12).enumerate() {
        let tag = if k < shown_c { format!("{}: ", letter(k)) } else { String::new() };
        let inside = if c.a == c.b { " (inside the same submodel)" } else { "" };
        lines.push(if c.kind == "collision" {
            format!("{tag}COLLISION #{} {} with #{} {}{inside}, overlap about {:.0} LDU³", c.a, c.a_part, c.b, c.b_part, c.overlap)
        } else {
            format!("{tag}CONFLICT #{} {} with #{} {}{inside}: {}", c.a, c.a_part, c.b, c.b_part, c.reason)
        });
    }
    for (k, g) in floating.iter().take(6).enumerate() {
        let tag = if k < shown_f { format!("F{}: ", k + 1) } else { String::new() };
        lines.push(format!(
            "{tag}FLOATING {} part{} ({}) connected to nothing (not touching, or only resting): {}",
            g.len(),
            if g.len() == 1 { "" } else { "s" },
            census(s, &view, g, 3),
            fmt_ids(g.iter().copied())
        ));
    }
    let short = |v: &[Id]| {
        let mut t = fmt_ids(v.iter().copied().take(12));
        if v.len() > 12 {
            t.push_str(&format!(" (+{} more)", v.len() - 12));
        }
        t
    };
    let mut warn = Vec::new();
    if !loose.is_empty() {
        warn.push(format!("LOOSE {} (not connected to the model built so far at their step; held only by later parts or a separate group): {}", loose.len(), short(&loose)));
    }
    if !unverified.is_empty() {
        warn.push(format!("UNVERIFIED {} (attached by a connection the checker cannot prove): {}", unverified.len(), short(&unverified)));
    }
    if unchecked > 0 {
        warn.push(format!("UNCHECKED {unchecked} (no collision geometry)"));
    }
    let scope = if focus.is_empty() { format!("{} pieces", report.pieces) } else { format!("{} chosen parts", focus.len()) };
    let errors = clashes.len() + floating.len();
    let ok = errors == 0;
    let links = report.link_types.iter().map(|(k, n)| format!("{n} {k}")).collect::<Vec<_>>().join(", ");
    let mut text = format!(
        "RESULT: {} — {} pieces, {} connected group{}, {} step{}; links: {}.",
        report.verdict_words(),
        report.pieces,
        report.groups.len(),
        if report.groups.len() == 1 { "" } else { "s" },
        report.steps,
        if report.steps == 1 { "" } else { "s" },
        if links.is_empty() { "none".to_string() } else { links }
    );
    if ok {
        text.push_str(&format!(" No collisions, conflicts or floating parts among {scope}."));
    } else {
        text.push_str(&format!(
            " {} among {scope}: {}.",
            tally(&[(n_coll, "collision"), (n_conf, "conflict"), (floating.len(), "floating group")]),
            lines.join("; ")
        ));
        if clashes.len() > 12 {
            text.push_str(&format!(" (+{} more collisions/conflicts)", clashes.len() - 12));
        }
    }
    if !warn.is_empty() {
        text.push_str(&format!(" Warnings: {}.", warn.join("; ")));
    }
    for w in report.colour_warnings.iter().take(3) {
        text.push_str(&format!(" {}", w.message));
    }
    let statuses: BTreeMap<&str, usize> =
        engine::checker::STATUSES.iter().map(|&st| (st, report.count(st))).filter(|(_, n)| *n > 0).collect();
    let data = json!({
        "ok": ok, "verdict": report.verdict, "pieces": report.pieces, "groups": report.groups.len(), "steps": report.steps,
        "statuses": statuses, "link_types": report.link_types,
        "collisions": clashes.iter().filter(|c| c.kind == "collision").map(|c| json!({"a": c.a, "b": c.b, "overlap": c.overlap.round()})).collect::<Vec<_>>(),
        "conflicts": clashes.iter().filter(|c| c.kind == "conflict").map(|c| json!({"a": c.a, "b": c.b, "reason": c.reason})).collect::<Vec<_>>(),
        "floating": floating,
        "loose": loose,
        "unverified": unverified,
    });
    let mut shot = Shot::new(file, if ok { "no problems".to_string() } else { format!("{} problems", lines.len()) });
    let bad: BTreeSet<Id> = clashes.iter().flat_map(|c| [c.a, c.b]).chain(floating.iter().flatten().copied()).collect();
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
        for (k, c) in clashes.iter().take(shown_c).enumerate() {
            let boxes = (shots::ids_box(&s.lib, &view, &[c.a]), shots::ids_box(&s.lib, &view, &[c.b]));
            let (at, mark) = match (c.bounds, boxes) {
                (Some((lo, hi)), _) => (c.at.unwrap_or((lo + hi) * 0.5), (c.kind == "collision" && (hi - lo).min_element() > 0.5).then_some((lo, hi))),
                (None, (Some((lo, hi)), _) | (None, Some((lo, hi)))) => ((lo + hi) * 0.5, None),
                (None, (None, None)) => (DVec3::ZERO, None),
            };
            let what = if c.kind == "collision" { format!("overlap ≈{:.0} LDU³", c.overlap) } else { c.reason.clone() };
            shot.items.push(shots::Item {
                label: letter(k),
                at,
                roles: if c.a == c.b { vec![(c.a, Role::Error)] } else { vec![(c.a, Role::Error), (c.b, Role::Blocker)] },
                note: format!("{} #{} {} (red) × #{} {} (orange) · {what}", c.kind, c.a, c.a_part, c.b, c.b_part),
                boxes: mark.map(|(lo, hi)| BoxMark { lo, hi, color: overlay::RED, text: String::new() }).into_iter().collect(),
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
        if clashes.len() + floating.len() > shown_c + shown_f {
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
    let pr = problems(s, view, file)?;
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
