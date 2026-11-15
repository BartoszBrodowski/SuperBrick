//! `look`: render the model or some of its parts from chosen or automatic angles.
use super::{census, file_arg, ids_prop, views_prop};
use crate::args::{self, Result};
use crate::shots::{self, Role, Shot, Views};
use crate::{Reply, Session};
use serde_json::{json, Value};

pub fn schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "ids": ids_prop("Parts to look at (ids, a selection \"sN\" or \"last\"); omit for the whole model"),
            "views": views_prop(),
            "compare": super::compare::schema(),
            "isolate": {"type": "boolean", "description": "Draw only the focus parts."},
            "dim": {"type": "boolean", "description": "Grey out everything except the focus (default true when ids are given)."},
            "hide": ids_prop("Parts not to draw (to see inside)"),
            "badges": {"type": "boolean", "description": "Label focus parts with their ids (default true for up to 40 parts)."},
            "size": {"type": "array", "items": {"type": "integer"}, "minItems": 2, "maxItems": 2, "description": "Image size in pixels (default 960x720)."},
            "file": {"type": "integer"},
            "units": super::units_prop()
        },
        "additionalProperties": false
    })
}

/// Parse a `views` argument: a count, "auto", a preset name or list ("front,right"),
/// or an array of names and `[yaw, pitch]` pairs (degrees).
pub fn views_arg(v: Option<&Value>) -> Result<Option<Views>> {
    let Some(v) = v else { return Ok(None) };
    match v {
        Value::Null => Ok(None),
        Value::Number(n) => Ok(Some(Views::Auto(n.as_u64().unwrap_or(1).clamp(1, 6) as usize))),
        Value::String(s) if s == "auto" => Ok(Some(Views::Auto(2))),
        Value::String(s) => {
            let mut out = Vec::new();
            for name in s.split([',', ' ', '+']).filter(|n| !n.is_empty()) {
                out.extend(preset(name)?);
            }
            Ok(Some(Views::Angles(out)))
        }
        Value::Array(items) => {
            let mut out = Vec::new();
            for item in items {
                match item {
                    Value::String(s) => out.extend(preset(s)?),
                    Value::Array(p) if p.len() == 2 => {
                        let yaw = args::num(&p[0]).ok_or("views: yaw must be a number")? as f32;
                        let pitch = args::num(&p[1]).ok_or("views: pitch must be a number")? as f32;
                        out.push((yaw.rem_euclid(360.0), pitch.clamp(-89.5, 89.5)));
                    }
                    Value::Object(o) if o.contains_key("yaw") || o.contains_key("pitch") => {
                        let yaw = o.get("yaw").map(|v| args::num(v).ok_or("views: yaw must be a number")).transpose()?.unwrap_or(0.0) as f32;
                        let pitch = o.get("pitch").map(|v| args::num(v).ok_or("views: pitch must be a number")).transpose()?.unwrap_or(0.0) as f32;
                        out.push((yaw.rem_euclid(360.0), pitch.clamp(-89.5, 89.5)));
                    }
                    _ => return Err("views: use names (\"front\", \"iso\"), [yaw, pitch] pairs or {yaw, pitch}".into()),
                }
            }
            if out.is_empty() {
                return Ok(None);
            }
            out.truncate(6);
            Ok(Some(Views::Angles(out)))
        }
        _ => Err("views: a count, \"auto\", preset names or [yaw, pitch] pairs".into()),
    }
}

/// Named views: yaw 0 looks from the front (-Z), 90 from the right (+X); pitch > 0 looks down.
pub fn preset(name: &str) -> Result<Vec<(f32, f32)>> {
    Ok(match name.to_ascii_lowercase().as_str() {
        "front" => vec![(0.0, 0.0)],
        "back" => vec![(180.0, 0.0)],
        "right" => vec![(90.0, 0.0)],
        "left" => vec![(270.0, 0.0)],
        "top" => vec![(0.0, 89.5)],
        "bottom" | "under" => vec![(0.0, -89.5)],
        "iso" => vec![(35.0, 28.0)],
        "iso-back" | "back-iso" => vec![(215.0, 28.0)],
        "low" => vec![(35.0, -20.0)],
        "around" | "turntable" => vec![(35.0, 25.0), (125.0, 25.0), (215.0, 25.0), (305.0, 25.0)],
        "sides" => vec![(0.0, 0.0), (90.0, 0.0), (180.0, 0.0), (270.0, 0.0)],
        "ortho" | "three" => vec![(0.0, 0.0), (90.0, 0.0), (0.0, 89.5)],
        other => return Err(format!("unknown view \"{other}\" (front, back, right, left, top, bottom, iso, iso-back, low, around, sides, three)")),
    })
}

pub fn run(s: &mut Session, args: &Value) -> Result<Reply> {
    let units = s.units(args)?;
    let view = s.view();
    let file = file_arg(&view, args)?;
    let ids = s.ids_arg(&view, args, "ids", file)?;
    let hide = s.ids_arg(&view, args, "hide", file)?;
    let isolate = args.get("isolate").and_then(serde_json::Value::as_bool).unwrap_or(false);
    let dim = args.get("dim").and_then(serde_json::Value::as_bool).unwrap_or(!ids.is_empty());
    let mut shot = Shot::new(
        file,
        if ids.is_empty() { "whole model".to_string() } else { format!("{} parts", ids.len()) },
    );
    if let Some(size) = args.get("size").and_then(Value::as_array) {
        let w = size.first().and_then(Value::as_u64).unwrap_or(960).clamp(128, 2048) as u32;
        let h = size.get(1).and_then(Value::as_u64).unwrap_or(720).clamp(128, 2048) as u32;
        shot.size = (w, h);
    }
    shot.whole = ids.is_empty();
    shot = shot.role(ids.iter().copied(), Role::Selected);
    shot = shot.role(hide.iter().copied(), Role::Hidden);
    if isolate {
        let keep: std::collections::BTreeSet<_> = ids.iter().copied().collect();
        let rest: Vec<_> = view.parts.keys().copied().filter(|id| !keep.contains(id)).collect();
        shot = shot.role(rest, Role::Hidden);
    }
    shot.dim_rest = dim && !isolate && !ids.is_empty();
    shot.id_badges = args.get("badges").and_then(Value::as_bool).unwrap_or(!ids.is_empty() && ids.len() <= 40);
    shot.views = views_arg(args.get("views"))?.unwrap_or(Views::Auto(2));
    if let Some(v) = super::compare::view(args)? {
        shot.views = Views::Angles(vec![v]);
    }
    shot.intent = if ids.is_empty() { crate::planner::Intent::Whole } else { crate::planner::Intent::Inspect };
    let focus = if ids.is_empty() {
        view.parts.keys().copied().collect::<Vec<_>>()
    } else {
        ids.clone()
    };
    let bbox = shots::ids_box(&s.lib, &view, &focus);
    let mut text = if ids.is_empty() {
        format!("Whole model: {} parts ({}).", view.parts.len(), census(s, &view, &focus, 6))
    } else {
        format!("{} parts: {}.", ids.len(), census(s, &view, &ids, 8))
    };
    if let Some((lo, hi)) = bbox {
        let b = units.bbox(lo, hi);
        text.push_str(&format!(" Box {} to {} ({}).", b["min"], b["max"], b["units"].as_str().unwrap_or("")));
    }
    let (img, e) = s.picture(&view, shot);
    let img = super::compare::attach(args, img)?;
    if let Some(e) = e {
        text.push(' ');
        text.push_str(&e);
    }
    let mut data = json!({"count": focus.len()});
    if let Some((lo, hi)) = bbox {
        data["box"] = units.bbox(lo, hi);
    }
    Ok(Reply::ok(text, data).image(img))
}
