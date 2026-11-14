//! `project`: information and housekeeping — the non-geometric verbs.
use super::{colour_name, describe_part, file_arg, fmt_ids, ids_prop, units_prop};
use crate::args::{self, Result};
use crate::shots::{self, BoxMark, Label, Role, Shot, Views};
use crate::{overlay, Reply, Session, HUMAN};
use engine::{Id, Model};
use glam::DVec3;
use serde_json::{json, Value};

const ACTIONS: &[&str] = &[
    "info", "parts", "inspect", "drafts", "accept", "reject", "withdraw", "log", "branches", "branch", "restore",
    "checkpoint", "export", "selections", "forget", "prefs", "set", "assets", "save_asset", "colours", "inventory",
];

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "action": {"type": "string", "enum": ACTIONS,
                "description": "info: summary. parts: list parts (ids/box, offset, limit). inspect: a part's connectors and size (id or part number), with a labelled picture. drafts: pending proposals. accept (human only) / reject / withdraw: drafts by id. log: versions. branches / branch (create, name, from). restore: a version (human). checkpoint: save a version now (human). export: LDraw to a path. selections / forget (a selection). prefs / set (key, value). assets / save_asset (id, path). colours: common colours. inventory: the model's parts counted by part and colour; with against (a set number, a booklet_inventory.json path or a [{part, color, qty}] list) also what is missing, extra or in the wrong colour."},
            "against": {"description": "inventory: a set number (\"40809\"), a path to a JSON file {parts:[{part, color, qty}]} or such a list."},
            "ids": ids_prop("Parts (parts, inspect) or draft ids (accept, reject, withdraw)"),
            "part": {"type": "string", "description": "Part number for inspect."},
            "box": {"type": "object", "description": "Region for parts: {min, max}."},
            "offset": {"type": "integer"},
            "limit": {"type": "integer"},
            "path": {"type": "string", "description": "File to write (export, save_asset)."},
            "id": {"description": "Version (restore), asset id (save_asset), selection (forget)."},
            "name": {"type": "string"},
            "from": {"type": "integer", "description": "Version to branch from."},
            "key": {"type": "string"},
            "value": {},
            "note": {"type": "string", "description": "Why (reject); shown to the agent that proposed it."},
            "message": {"type": "string"},
            "units": units_prop(),
            "file": {"type": "integer"}
        },
        "required": ["action"],
        "additionalProperties": false
    })
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply> {
    let action = args::str_arg(args, "action").ok_or("project needs an action")?;
    match action {
        "info" => info(s),
        "inventory" => inventory(s, args),
        "parts" => parts(s, args),
        "inspect" => inspect(s, args),
        "drafts" => drafts(s),
        "accept" | "reject" | "withdraw" => decide(s, args, action),
        "log" => {
            let log = s.store.log(&s.branch, args::usize_arg(args, "limit", 20, 200))?;
            let lines: Vec<String> = log
                .iter()
                .map(|c| format!("v{} {} {} by {}: {}", c.id, age(c.time), c.kind, c.author, c.message))
                .collect();
            Ok(Reply::ok(
                if lines.is_empty() { "No versions yet.".into() } else { lines.join("\n") },
                json!({"versions": log.iter().map(|c| json!({"id": c.id, "kind": c.kind, "author": c.author, "message": c.message, "time": c.time, "drafts": c.drafts})).collect::<Vec<_>>()}),
            ))
        }
        "branches" => {
            let b = s.store.branches()?;
            let users = s.engine.drafts.users();
            let mut text = b.iter().map(|b| format!("{} (head v{})", b.name, b.head.unwrap_or(0))).collect::<Vec<_>>().join(", ");
            if !users.is_empty() {
                text.push_str(&format!(". Agent branches (pending drafts): {}.", users.iter().map(|u| format!("{u} ({})", s.engine.drafts.pending(u).count())).collect::<Vec<_>>().join(", ")));
            }
            Ok(Reply::ok(text, json!({"branches": b.iter().map(|b| json!({"name": b.name, "head": b.head, "base": b.base})).collect::<Vec<_>>(), "agents": users})))
        }
        "branch" => {
            human_only(s, "create branches")?;
            let name = args::str_arg(args, "name").ok_or("branch needs a name")?;
            let from = args.get("from").and_then(Value::as_i64);
            let user = s.user.clone();
            s.store.create_branch(name, from, &user)?;
            Ok(Reply::ok(format!("Created branch {name}."), json!({"branch": name})))
        }
        "restore" => {
            human_only(s, "restore versions")?;
            let id = args.get("id").and_then(Value::as_i64).ok_or("restore needs id (a version)")?;
            let user = s.user.clone();
            let branch = s.branch.clone();
            let commit = s.store.restore(&branch, id, &user)?;
            s.force_sync()?;
            let view = s.view();
            let shot = Shot { whole: true, views: Views::Auto(1), intent: crate::planner::Intent::Whole, ..Shot::new(0, format!("restored v{id}")) };
            let (img, _) = s.picture(&view, shot);
            Ok(Reply::ok(format!("Restored version {id} as v{commit}."), json!({"commit": commit})).image(img))
        }
        "checkpoint" => {
            human_only(s, "save versions")?;
            let msg = args::str_arg(args, "message").unwrap_or("checkpoint").to_string();
            let (branch, user) = (s.branch.clone(), s.user.clone());
            let commit = s.store.commit(&branch, "save", &user, &msg, &[])?;
            Ok(Reply::ok(format!("Saved version v{commit}."), json!({"commit": commit})))
        }
        "export" => {
            let path = args::str_arg(args, "path").ok_or("export needs a path (.ldr or .mpd)")?;
            let bytes = s.export()?;
            std::fs::write(path, &bytes).map_err(|e| format!("{path}: {e}"))?;
            let what = if s.is_human() { "the shared model" } else { "the model with your pending drafts" };
            Ok(Reply::ok(format!("Wrote {what} to {path} ({} bytes).", bytes.len()), json!({"path": path, "bytes": bytes.len()})))
        }
        "selections" => {
            let user = s.user.clone();
            let list = s.store.selections(if s.is_human() { None } else { Some(&user) })?;
            let text = if list.is_empty() {
                "No selections.".to_string()
            } else {
                list.iter()
                    .map(|x| format!("{}{} ({} parts, by {})", x.id, x.name.as_deref().map(|n| format!(" \"{n}\"")).unwrap_or_default(), x.parts.len(), x.user))
                    .collect::<Vec<_>>()
                    .join(", ")
            };
            Ok(Reply::ok(text, json!({"selections": list.iter().map(|x| json!({"id": x.id, "name": x.name, "count": x.parts.len(), "user": x.user})).collect::<Vec<_>>()})))
        }
        "forget" => {
            let key = args.get("id").and_then(Value::as_str).ok_or("forget needs id (a selection)")?;
            let user = s.user.clone();
            s.store.delete_selection(&user, key)?;
            Ok(Reply::ok(format!("Forgot {key}."), json!({})))
        }
        "prefs" => {
            let scope = format!("user:{}", s.user);
            let mine = s.store.prefs(&scope)?;
            let project = s.store.prefs("project")?;
            Ok(Reply::ok(format!("Yours: {}. Project: {}.", json!(mine), json!(project)), json!({"user": mine, "project": project})))
        }
        "set" => {
            let key = args::str_arg(args, "key").ok_or("set needs key")?;
            let value = args.get("value").cloned().unwrap_or(Value::Null);
            let scope = if key.starts_with("project.") { "project".to_string() } else { format!("user:{}", s.user) };
            let key = key.trim_start_matches("project.");
            s.store.set_pref(&scope, key, &value)?;
            Ok(Reply::ok(format!("Set {key} = {value} ({scope})."), json!({})))
        }
        "assets" => {
            let list = s.store.assets(None)?;
            let text = if list.is_empty() {
                "No assets (publish makes instructions and renders).".to_string()
            } else {
                list.iter().map(|a| format!("{} {} {} ({})", a.id, a.kind, a.name, a.mime)).collect::<Vec<_>>().join(", ")
            };
            Ok(Reply::ok(text, json!({"assets": list.iter().map(|a| json!({"id": a.id, "kind": a.kind, "name": a.name, "mime": a.mime, "meta": a.meta})).collect::<Vec<_>>()})))
        }
        "save_asset" => {
            let id = args.get("id").and_then(Value::as_str).ok_or("save_asset needs id")?;
            let path = args::str_arg(args, "path").ok_or("save_asset needs path")?;
            let (a, bytes) = s.store.asset(id)?;
            std::fs::write(path, &bytes).map_err(|e| format!("{path}: {e}"))?;
            Ok(Reply::ok(format!("Wrote {} {} to {path} ({} bytes).", a.kind, a.name, bytes.len()), json!({"path": path})))
        }
        "colours" | "colors" => {
            let common = [0, 1, 2, 4, 5, 6, 7, 8, 14, 15, 19, 25, 26, 27, 28, 70, 71, 72, 73, 84, 85, 191, 212, 226, 272, 288, 308, 320, 326, 378];
            let lib = s.lib.clone();
            let list: Vec<Value> = common.iter().map(|&c| json!({"code": c, "name": colour_name(&lib, c)})).collect();
            let text = common.iter().map(|&c| format!("{c} {}", colour_name(&lib, c))).collect::<Vec<_>>().join(", ");
            Ok(Reply::ok(format!("Common colours: {text}."), json!({"colours": list})))
        }
        other => Err(format!("unknown action \"{other}\"; one of {}", ACTIONS.join(", "))),
    }
}

fn human_only(s: &Session, what: &str) -> Result<()> {
    if s.is_human() {
        Ok(())
    } else {
        Err(format!("only the human can {what}; agents propose drafts the human accepts"))
    }
}

fn age(t: f64) -> String {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(t);
    let d = (now - t).max(0.0);
    if d < 90.0 {
        format!("{:.0}s ago", d)
    } else if d < 5400.0 {
        format!("{:.0}m ago", d / 60.0)
    } else if d < 129600.0 {
        format!("{:.0}h ago", d / 3600.0)
    } else {
        format!("{:.0}d ago", d / 86400.0)
    }
}

fn info(s: &mut Session) -> Result<Reply> {
    let view = s.view();
    let files: Vec<String> = view.doc.files.iter().map(|f| f.name.clone()).collect();
    let count = view.parts.len();
    let mine: Vec<engine::drafts::Draft> = s.engine.drafts.pending(&s.user).cloned().collect();
    let all_pending = s.engine.drafts.ops.iter().filter(|d| d.state == "pending").count();
    let sels = s.store.selections(Some(&s.user.clone()))?.len();
    let bbox = shots::ids_box(&s.lib, &view, &view.parts.keys().copied().collect::<Vec<_>>());
    let mut text = format!(
        "{} as \"{}\" on {}: {} parts in {} file{} ({}). You have {} pending draft{} ({} pending overall), {} selection{}.",
        s.store.path().display(),
        s.user,
        s.branch,
        count,
        files.len(),
        if files.len() == 1 { "" } else { "s" },
        files.join(", "),
        mine.len(),
        if mine.len() == 1 { "" } else { "s" },
        all_pending,
        sels,
        if sels == 1 { "" } else { "s" }
    );
    if let Some((lo, hi)) = bbox {
        let d = hi - lo;
        text.push_str(&format!(" Size {:.0}x{:.0}x{:.0} LDU ({:.1} x {:.1} studs, {:.1} bricks tall).", d.x, d.y, d.z, d.x / 20.0, d.z / 20.0, d.y / 24.0));
    }
    text.push_str(" Conventions: LDraw units, y points down; rotations are 9 numbers in LDraw row order.");
    Ok(Reply::ok(text, json!({"parts": count, "files": files, "pending": mine, "user": s.user, "branch": s.branch})))
}

fn parts(s: &mut Session, args: &Value) -> Result<Reply> {
    let units = s.units(args)?;
    let view = s.view();
    let file = file_arg(&view, args)?;
    let mut ids = if args.get("ids").is_some() || args.get("box").is_some() {
        let mut q = json!({});
        for k in ["ids", "box", "file", "units"] {
            if let Some(v) = args.get(k) {
                q[k] = v.clone();
            }
        }
        q["picture"] = json!(false);
        let r = super::select::run(s, &q)?;
        r.data["ids"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect::<Vec<Id>>()).unwrap_or_default()
    } else {
        view.parts.values().filter(|p| p.file == file).map(|p| p.id).collect()
    };
    ids.sort_unstable();
    let offset = args::usize_arg(args, "offset", 0, usize::MAX);
    let limit = args::usize_arg(args, "limit", 60, 400).max(1);
    let page: Vec<Id> = ids.iter().copied().skip(offset).take(limit).collect();
    let user = s.user.clone();
    s.engine.observe_ids(&user, page.iter().copied());
    s.save_observations()?;
    let rows: Vec<Value> = page
        .iter()
        .filter_map(|id| view.parts.get(id))
        .map(|p| {
            let rot = args::rot_rows(p.rot).map(|v| (v * 1e4).round() / 1e4 + 0.0);
            json!({"id": p.id, "part": p.name.trim_end_matches(".dat"), "color": p.color, "pos": units.from_ldu(p.pos), "rot": rot})
        })
        .collect();
    let lines: Vec<String> = rows
        .iter()
        .map(|r| format!("#{} {} c{} at {} rot {}", r["id"], r["part"].as_str().unwrap_or(""), r["color"], r["pos"], r["rot"]))
        .collect();
    let more = ids.len().saturating_sub(offset + page.len());
    let mut text = format!("{} of {} parts{}:\n{}", page.len(), ids.len(), if offset > 0 { format!(" from {offset}") } else { String::new() }, lines.join("\n"));
    if more > 0 {
        text.push_str(&format!("\n(+{more} more: offset {})", offset + page.len()));
    }
    Ok(Reply::ok(text, json!({"parts": rows, "total": ids.len(), "more": more})))
}

fn inspect(s: &mut Session, args: &Value) -> Result<Reply> {
    let units = s.units(args)?;
    let view = s.view();
    let file = file_arg(&view, args)?;
    let (model, id, file) = if let Some(part) = args::str_arg(args, "part") {
        let name = args::part_name(part);
        let text = format!("0 inspect\n0 Name: inspect.ldr\n1 71 0 0 0 1 0 0 0 1 0 0 0 1 {name}\n");
        let doc = ldraw::Document::parse(text.as_bytes(), "inspect.ldr");
        let m = Model::from_document(doc, None);
        let id = *m.parts.keys().next().ok_or("could not load the part")?;
        (m, id, 0)
    } else {
        let ids = s.ids_arg(&view, args, "ids", file)?;
        let id = *ids.first().ok_or("inspect needs ids (one part) or part")?;
        (view, id, file)
    };
    let p = model.parts.get(&id).ok_or("unknown part")?.clone();
    let what = describe_part(s, &p.name);
    let analysis = engine::connectors::analyze(&model, file)?;
    let conns: Vec<_> = analysis.connectors.iter().filter(|c| c.part == id).collect();
    let bbox = shots::part_box(&s.lib, &p.name, p.pos, p.rot);
    let mut shot = Shot::new(file, format!("#{id} {what}")).role([id], Role::Selected);
    shot.dim_rest = true;
    shot.views = Views::Angles(vec![(35.0, 28.0), (215.0, -25.0)]);
    let mut lines = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for (i, c) in conns.iter().enumerate() {
        let key = ((c.position * 2.0).round().to_array().map(|v| v as i64), c.label.clone());
        let first = seen.insert(key);
        let pos = units.from_ldu(c.position);
        if lines.len() < 48 {
            lines.push(format!(
                "c{i} {} at [{:.0}, {:.0}, {:.0}] axis [{:.0}, {:.0}, {:.0}]",
                c.label, pos[0], pos[1], pos[2], c.axis.x, c.axis.y, c.axis.z
            ));
        }
        if first && shot.labels.len() < 24 {
            shot.labels.push(Label { text: format!("c{i} {}", short_label(&c.label)), at: c.position, color: overlay::PALETTE[i % overlay::PALETTE.len()] });
        }
    }
    let mut text = format!("#{id} {what}: {} connectors.", conns.len());
    let mut data = json!({"id": id, "part": p.name, "connectors": conns.iter().enumerate().map(|(i, c)| json!({"index": i, "label": c.label, "at": units.from_ldu(c.position), "axis": [c.axis.x, c.axis.y, c.axis.z], "leaf": c.leaf, "connector": c.index})).collect::<Vec<_>>()});
    if let Some((lo, hi)) = bbox {
        let d = hi - lo;
        text.push_str(&format!(
            " Size {:.0}x{:.0}x{:.0} LDU ({:.1} x {:.1} studs, {:.2} plates tall); box {} to {}.",
            d.x, d.y, d.z, d.x / 20.0, d.z / 20.0, d.y / 8.0,
            json!(units.from_ldu(lo)), json!(units.from_ldu(hi))
        ));
        shot.boxes.push(BoxMark { lo, hi, color: overlay::GREY, text: format!("{:.0}×{:.0}×{:.0}", d.x, d.y, d.z) });
        data["box"] = units.bbox(lo, hi);
    }
    if !lines.is_empty() {
        text.push(' ');
        text.push_str(&lines.join("; "));
    }
    text.push_str(&format!(" Origin [{:.0}, {:.0}, {:.0}], rot {:?}.", p.pos.x, p.pos.y, p.pos.z, args::rot_rows(p.rot).map(|v| v.round() as i64)));
    let (img, e) = s.picture(&model, shot);
    if let Some(e) = e {
        text.push(' ');
        text.push_str(&e);
    }
    let _ = DVec3::ZERO;
    Ok(Reply::ok(text, data).image(img))
}

fn short_label(l: &str) -> String {
    l.split([' ', ':']).next().unwrap_or(l).chars().take(10).collect()
}

fn drafts(s: &mut Session) -> Result<Reply> {
    let human = s.is_human();
    let user = s.user.clone();
    let list: Vec<_> = s
        .engine
        .drafts
        .ops
        .iter()
        .filter(|d| (human || d.session == user) && matches!(d.state.as_str(), "pending" | "stale" | "blocked"))
        .cloned()
        .collect();
    if list.is_empty() {
        return Ok(Reply::ok(if human { "No pending drafts." } else { "You have no pending drafts." }, json!({"drafts": []})));
    }
    let lines: Vec<String> = list
        .iter()
        .map(|d| {
            format!(
                "draft {} by {} turn {} [{}]: {} (+{} ~{} -{}){}{}",
                d.id,
                d.session,
                d.turn,
                d.state,
                if d.label.is_empty() { format!("{} op(s)", d.commands.len()) } else { d.label.clone() },
                d.touched.created.len(),
                d.touched.modified.len(),
                d.touched.removed.len(),
                if d.dependencies.is_empty() { String::new() } else { format!(" after {:?}", d.dependencies) },
                if d.reason.is_empty() { String::new() } else { format!(" — {}", d.reason) }
            )
        })
        .collect();
    let data = json!({"drafts": list.iter().map(|d| json!({"id": d.id, "user": d.session, "turn": d.turn, "state": d.state, "label": d.label,
        "created": d.touched.created, "modified": d.touched.modified, "removed": d.touched.removed, "dependencies": d.dependencies, "reason": d.reason, "note": d.note})).collect::<Vec<_>>()});
    // Picture: the shared model with the listed drafts' changes highlighted.
    let mut shot = Shot::new(0, format!("{} pending drafts", list.len()));
    let changed: Vec<Id> = list.iter().flat_map(|d| d.touched.created.iter().chain(d.touched.modified.iter()).copied()).collect();
    shot = shot.role(changed.iter().copied(), Role::New);
    shot.views = Views::Auto(1);
    let view = s.view();
    let model = if human {
        // With one proposing agent, show its view; otherwise the shared model.
        let users: Vec<String> = list.iter().map(|d| d.session.clone()).collect::<std::collections::BTreeSet<_>>().into_iter().collect();
        if users.len() == 1 { s.engine.effective_model(&users[0]) } else { view }
    } else {
        view
    };
    let (img, _) = s.picture(&model, shot);
    Ok(Reply::ok(lines.join("\n"), data).image(img))
}

fn decide(s: &mut Session, args: &Value, action: &str) -> Result<Reply> {
    let ids: Vec<u64> = match args.get("ids") {
        Some(Value::Number(n)) => vec![n.as_u64().ok_or("draft ids are numbers")?],
        Some(Value::Array(a)) => a.iter().map(|v| v.as_u64().ok_or_else(|| "draft ids are numbers".to_string())).collect::<Result<_>>()?,
        Some(Value::String(s)) if s == "all" => Vec::new(),
        None => return Err(format!("{action} needs \"ids\": draft ids (a number or a list) or \"all\"")),
        _ => return Err("ids: draft ids (number or list) or \"all\"".into()),
    };
    let user = s.user.clone();
    match action {
        "accept" => {
            human_only(s, "accept drafts")?;
            let targets: Vec<(String, u64)> = s
                .engine
                .drafts
                .ops
                .iter()
                .filter(|d| d.state == "pending" && (ids.is_empty() || ids.contains(&d.id)))
                .map(|d| (d.session.clone(), d.id))
                .collect();
            if targets.is_empty() {
                return Err("no pending drafts with those ids".into());
            }
            let mut users: Vec<String> = targets.iter().map(|t| t.0.clone()).collect();
            users.dedup();
            let mut accepted = Vec::new();
            for u in users {
                let mine: Vec<u64> = targets.iter().filter(|t| t.0 == u).map(|t| t.1).collect();
                s.engine.accept(&u, Some(mine.clone()))?;
                accepted.extend(mine);
            }
            let all: Vec<u64> = s.engine.drafts.ops.iter().map(|d| d.id).collect();
            s.save_drafts(&all)?;
            let commit = s.publish_shared("accept", &format!("accepted drafts {}", fmt_ids(accepted.iter().copied())), &accepted)?;
            Ok(Reply::ok(format!("Accepted drafts {} as version v{commit}.", fmt_ids(accepted.iter().copied())), json!({"accepted": accepted, "commit": commit})))
        }
        _ => {
            let mine: Vec<u64> = s
                .engine
                .drafts
                .ops
                .iter()
                .filter(|d| (s.is_human() || d.session == user) && matches!(d.state.as_str(), "pending" | "stale" | "blocked"))
                .filter(|d| ids.is_empty() || ids.contains(&d.id))
                .map(|d| d.id)
                .collect();
            if mine.is_empty() {
                return Err("no live drafts of yours with those ids".into());
            }
            let note = args::str_arg(args, "note").unwrap_or("");
            let by_user: Vec<(String, u64)> = s.engine.drafts.ops.iter().filter(|d| mine.contains(&d.id)).map(|d| (d.session.clone(), d.id)).collect();
            for (u, id) in &by_user {
                if action == "withdraw" {
                    s.engine.withdraw(u, vec![*id])?;
                } else if note.is_empty() {
                    s.engine.reject(u, vec![*id])?;
                } else {
                    s.engine.reject_with_note(u, vec![*id], note)?;
                }
            }
            let all: Vec<u64> = s.engine.drafts.ops.iter().map(|d| d.id).collect();
            s.save_drafts(&all)?;
            let blocked: Vec<u64> = s.engine.drafts.ops.iter().filter(|d| d.state == "blocked").map(|d| d.id).collect();
            let mut text = format!("{} drafts {}.", if action == "withdraw" { "Withdrew" } else { "Rejected" }, fmt_ids(mine.iter().copied()));
            if !blocked.is_empty() {
                text.push_str(&format!(" Now blocked (they built on these): {}.", fmt_ids(blocked.iter().copied())));
            }
            let _ = HUMAN;
            Ok(Reply::ok(text, json!({"done": mine, "blocked": blocked})))
        }
    }
}

pub(crate) type Counts = std::collections::BTreeMap<(String, i32), u32>;

/// Part number without ".dat", after following the library's "~Moved to" redirections (3023 -> 3023b).
pub(crate) fn part_key(lib: &ldraw::Library, name: &str) -> String {
    let n = lib.canonical_part(name.trim()).to_lowercase();
    n.strip_suffix(".dat").unwrap_or(&n).to_string()
}

/// "a; b; c" with at most `max` items, then "… and N more".
fn capped(items: Vec<String>, max: usize) -> String {
    let n = items.len();
    let mut t = items.into_iter().take(max).collect::<Vec<_>>().join("; ");
    if n > max {
        t.push_str(&format!("; … and {} more (all in data)", n - max));
    }
    t
}

pub(crate) fn counts_from(lib: &ldraw::Library, list: &[Value]) -> Result<Counts> {
    let mut want = Counts::new();
    for v in list {
        let part = v.get("part").and_then(Value::as_str).ok_or("each inventory entry needs \"part\"")?;
        let color = match v.get("color") {
            Some(Value::Number(n)) => n.as_i64().ok_or("colour must be a whole number")? as i32,
            Some(Value::String(name)) => super::edit::colour_code(name)?,
            _ => return Err("each entry needs \"color\" (a number or a name)".into()),
        };
        let qty = v.get("qty").or_else(|| v.get("quantity")).and_then(Value::as_u64).unwrap_or(1) as u32;
        *want.entry((part_key(lib, part), color)).or_default() += qty;
    }
    Ok(want)
}

/// The model's parts by part and colour, optionally checked against a set or a parts list.
fn inventory(s: &mut Session, args: &Value) -> Result<Reply> {
    let view = s.view();
    let file = file_arg(&view, args)?;
    let mut have = Counts::new();
    for p in view.flat(file) {
        *have.entry((part_key(&s.lib, &p.name), p.color.0)).or_default() += 1;
    }
    let total: u32 = have.values().sum();
    let line = |(p, c): &(String, i32), n: u32| format!("{n}× {p} {} ({c})", colour_name(&s.lib, *c));
    let rows = |m: &Counts| m.iter().map(|((p, c), n)| json!({"part": p, "color": c, "qty": n})).collect::<Vec<_>>();
    let Some(against) = args.get("against") else {
        let lines: Vec<String> = have.iter().map(|(k, n)| line(k, *n)).collect();
        return Ok(Reply::ok(
            format!("{total} pieces in {} kinds: {}.", have.len(), capped(lines, 60)),
            json!({"pieces": total, "parts": rows(&have)}),
        ));
    };
    let (want, source) = match against {
        Value::Array(list) => (counts_from(&s.lib, list)?, "the given list".to_string()),
        Value::Object(o) => (counts_from(&s.lib, o.get("parts").and_then(Value::as_array).ok_or("against: {parts: [...]}")?)?, "the given list".to_string()),
        Value::String(t) if t.ends_with(".json") => {
            let j: Value = serde_json::from_slice(&std::fs::read(t).map_err(|e| format!("{t}: {e}"))?).map_err(|e| format!("{t}: {e}"))?;
            let list = j.get("parts").and_then(Value::as_array).or_else(|| j.as_array()).ok_or(format!("{t}: expected {{\"parts\": [...]}}"))?;
            (counts_from(&s.lib, list)?, t.clone())
        }
        Value::String(t) => {
            let table = search::sets::Sets::get().ok_or("the set table (search data out/sets.bin) is not installed")?;
            let inv = table.inventory(t).ok_or(format!("no set \"{t}\""))?;
            let mut want = Counts::new();
            for r in inv.rows.iter().filter(|r| !r.spare) {
                if let (Some(p), Some(c)) = (&r.item.part, r.item.color) {
                    *want.entry((part_key(&s.lib, p), c)).or_default() += r.qty;
                }
            }
            (want, format!("set {} \"{}\"", inv.set, inv.name))
        }
        _ => return Err("against: a set number, a .json path or a list of {part, color, qty}".into()),
    };
    let mut missing = Counts::new();
    let mut extra = Counts::new();
    let mut matched = 0;
    for (k, &n) in &want {
        let h = have.get(k).copied().unwrap_or(0);
        matched += n.min(h);
        if n > h {
            missing.insert(k.clone(), n - h);
        }
    }
    for (k, &h) in &have {
        let n = want.get(k).copied().unwrap_or(0);
        if h > n {
            extra.insert(k.clone(), h - n);
        }
    }
    // Pair up the same part in another colour, and a printed/variant part for its base shape.
    let mut hints = Vec::new();
    for ((p, c), n) in &missing {
        for ((q, d), m) in &extra {
            if q == p && d != c {
                hints.push(format!("{p}: {m} in {} ({d}) where the list has {n} in {} ({c})", colour_name(&s.lib, *d), colour_name(&s.lib, *c)));
            } else if q != p && d == c && (q.starts_with(p.as_str()) || p.starts_with(q.as_str())) {
                hints.push(format!("{q} ({m}) may stand for {p} ({n}) in {}", colour_name(&s.lib, *c)));
            }
        }
    }
    let want_total: u32 = want.values().sum();
    let mut text = format!("{total} pieces in the model, {want_total} in {source}: {matched} match.");
    if missing.is_empty() && extra.is_empty() {
        text.push_str(" Everything matches.");
    } else {
        if !missing.is_empty() {
            text.push_str(&format!(" Missing ({}): {}.", missing.values().sum::<u32>(), capped(missing.iter().map(|(k, n)| line(k, *n)).collect(), 40)));
        }
        if !extra.is_empty() {
            text.push_str(&format!(" Extra ({}): {}.", extra.values().sum::<u32>(), capped(extra.iter().map(|(k, n)| line(k, *n)).collect(), 40)));
        }
        if !hints.is_empty() {
            text.push_str(&format!(" Likely the same piece: {}.", capped(hints.clone(), 20)));
        }
    }
    Ok(Reply::ok(text, json!({"pieces": total, "expected": want_total, "matched": matched,
                              "missing": rows(&missing), "extra": rows(&extra), "hints": hints})))
}
