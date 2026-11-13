//! Argument conventions shared by every tool: units, rotations, id references.
//!
//! * **Units.** `"ldu"` (default) is raw LDraw: x, y, z with y pointing *down*. `"grid"` is
//!   studs for x and z (1 = 20 LDU) and plates for y (1 = 8 LDU) with y pointing *up*, so
//!   `[0, 3, 0]` is one brick higher. Rotations are unaffected by units.
//! * **Rotations.** Nine numbers in LDraw row order (`a b c d e f g h i` as written in a type-1
//!   line), or a shorthand: `"identity"`, `"yaw90"`, `"pitch-90"`, `"roll180"`, or products such
//!   as `"yaw90*pitch-90"` (applied left to right as matrix products). `yaw` turns about the
//!   vertical LDraw Y axis, `pitch` about X, `roll` about Z.
//! * **Ids.** A number, a list, a selection (`"s4"` or its name), `"last"` (what the previous
//!   edit changed) or `"all"`; lists may mix them.
use engine::Id;
use glam::{DMat3, DVec3};
use serde_json::Value;

pub type Result<T> = std::result::Result<T, String>;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Units {
    Ldu,
    Grid,
}

impl Units {
    pub fn from_args(args: &Value) -> Result<Units> {
        match args.get("units").and_then(Value::as_str) {
            None | Some("ldu") => Ok(Units::Ldu),
            Some("grid") | Some("studs") => Ok(Units::Grid),
            Some(other) => Err(format!("units must be \"ldu\" or \"grid\", not {other:?}")),
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Units::Ldu => "ldu",
            Units::Grid => "grid",
        }
    }
    /// A position or offset given in these units, as LDU.
    pub fn to_ldu(self, v: [f64; 3]) -> DVec3 {
        match self {
            Units::Ldu => DVec3::from(v),
            Units::Grid => DVec3::new(v[0] * 20.0, -v[1] * 8.0, v[2] * 20.0),
        }
    }
    /// An LDU position or offset in these units (rounded to 1/1000).
    pub fn from_ldu(self, v: DVec3) -> [f64; 3] {
        let r = |x: f64| (x * 1000.0).round() / 1000.0 + 0.0;
        match self {
            Units::Ldu => [r(v.x), r(v.y), r(v.z)],
            Units::Grid => [r(v.x / 20.0), r(-v.y / 8.0), r(v.z / 20.0)],
        }
    }
    /// A box `lo..hi` in these units, as `{min, max, size}` (size in studs/plates or LDU).
    pub fn bbox(self, lo: DVec3, hi: DVec3) -> Value {
        let (a, b) = (self.from_ldu(lo), self.from_ldu(hi));
        let min = [a[0].min(b[0]), a[1].min(b[1]), a[2].min(b[2])];
        let max = [a[0].max(b[0]), a[1].max(b[1]), a[2].max(b[2])];
        let r = |x: f64| (x * 100.0).round() / 100.0 + 0.0;
        serde_json::json!({
            "min": min, "max": max,
            "size": [r(max[0]-min[0]), r(max[1]-min[1]), r(max[2]-min[2])],
            "units": self.name(),
        })
    }
}

pub fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|x| x.is_finite())
}

/// Three numbers as an array (or an object with x, y, z).
pub fn triple(v: &Value) -> Result<[f64; 3]> {
    if let Some(a) = v.as_array() {
        if a.len() == 3 {
            if let (Some(x), Some(y), Some(z)) = (num(&a[0]), num(&a[1]), num(&a[2])) {
                return Ok([x, y, z]);
            }
        }
    } else if v.is_object() {
        if let (Some(x), Some(y), Some(z)) = (num(&v["x"]), num(&v["y"]), num(&v["z"])) {
            return Ok([x, y, z]);
        }
    }
    Err(format!("expected three finite numbers, got {v}"))
}

pub fn point(v: &Value, units: Units) -> Result<DVec3> {
    Ok(units.to_ldu(triple(v)?))
}

/// A unit axis: `"x"`, `"-y"`, `"up"`, `"down"` or three numbers (LDraw axes).
pub fn axis(v: &Value) -> Result<DVec3> {
    if let Some(s) = v.as_str() {
        let a = match s.trim().to_ascii_lowercase().as_str() {
            "x" | "+x" => DVec3::X,
            "-x" => -DVec3::X,
            "y" | "+y" | "down" => DVec3::Y,
            "-y" | "up" | "vertical" => -DVec3::Y,
            "z" | "+z" => DVec3::Z,
            "-z" => -DVec3::Z,
            other => return Err(format!("unknown axis {other:?} (use x, y, z, -x, up, ... or [x,y,z])")),
        };
        return Ok(a);
    }
    let a = DVec3::from(triple(v)?);
    if a.length() < 1e-9 {
        return Err("axis must be non-zero".into());
    }
    Ok(a.normalize())
}

/// LDraw row order (as written in a type-1 line) to a matrix.
pub fn ldraw_rot(r: [f64; 9]) -> DMat3 {
    DMat3::from_cols_array(&r).transpose()
}

/// A matrix to LDraw row order, rounded so `0.0000001` noise does not leak into replies.
pub fn rot_rows(m: DMat3) -> [f64; 9] {
    let a = m.transpose().to_cols_array();
    a.map(|x| (x * 1e6).round() / 1e6 + 0.0)
}

pub fn rotation(v: &Value) -> Result<DMat3> {
    if v.is_null() {
        return Ok(DMat3::IDENTITY);
    }
    if let Some(a) = v.as_array() {
        if a.len() != 9 {
            return Err("rot needs 9 numbers in LDraw row order".into());
        }
        let mut r = [0.0; 9];
        for (i, x) in a.iter().enumerate() {
            r[i] = num(x).ok_or("rot entries must be finite numbers")?;
        }
        let m = ldraw_rot(r);
        if (m.determinant().abs() - 1.0).abs() > 1e-3 {
            return Err("rot must be a rotation (|det| = 1)".into());
        }
        return Ok(m);
    }
    let s = v.as_str().ok_or("rot must be 9 numbers or a shorthand such as \"yaw90\"")?;
    shorthand(s)
}

pub fn shorthand(s: &str) -> Result<DMat3> {
    let mut m = DMat3::IDENTITY;
    for term in s.split(['*', ' ']).filter(|t| !t.is_empty()) {
        let t = term.to_ascii_lowercase();
        if t == "identity" || t == "id" || t == "none" {
            continue;
        }
        let (name, deg) = ["yaw", "pitch", "roll"]
            .iter()
            .find_map(|n| t.strip_prefix(n).map(|d| (*n, d)))
            .ok_or_else(|| format!("unknown rotation term {term:?} (yawN, pitchN, rollN)"))?;
        let deg: f64 = deg.parse().map_err(|_| format!("bad angle in {term:?}"))?;
        let r = deg.to_radians();
        let q = match name {
            "yaw" => DMat3::from_rotation_y(r),
            "pitch" => DMat3::from_rotation_x(r),
            _ => DMat3::from_rotation_z(r),
        };
        m *= snap(q);
    }
    Ok(m)
}

/// Clean right-angle rotations to exact 0/±1 entries.
fn snap(m: DMat3) -> DMat3 {
    let a = m.to_cols_array().map(|x| {
        let r = x.round();
        if (x - r).abs() < 1e-9 { r + 0.0 } else { x }
    });
    DMat3::from_cols_array(&a)
}

/// Axis-angle of a rotation matrix (degrees). Identity gives `(Y, 0)`.
pub fn axis_angle(m: DMat3) -> (DVec3, f64) {
    let q = glam::DQuat::from_mat3(&m);
    let (axis, angle) = q.to_axis_angle();
    if angle.abs() < 1e-9 || !axis.is_finite() {
        (DVec3::Y, 0.0)
    } else {
        (axis, angle.to_degrees())
    }
}

/// What an id argument referred to, before resolution against the session.
#[derive(Clone, Debug, PartialEq)]
pub enum IdRef {
    Id(Id),
    Named(String),
}

pub fn id_refs(v: &Value) -> Result<Vec<IdRef>> {
    let mut out = Vec::new();
    match v {
        Value::Null => {}
        Value::Number(n) => out.push(IdRef::Id(n.as_u64().ok_or("ids are positive integers")?)),
        Value::String(s) => {
            for part in s.split([',', ' ']).filter(|p| !p.is_empty()) {
                match part.trim_start_matches('#').parse::<u64>() {
                    Ok(n) => out.push(IdRef::Id(n)),
                    Err(_) => out.push(IdRef::Named(part.to_string())),
                }
            }
        }
        Value::Array(a) => {
            for x in a {
                out.extend(id_refs(x)?);
            }
        }
        Value::Object(_) => return Err("ids must be numbers, a selection id such as \"s3\", or \"last\"".into()),
        Value::Bool(_) => return Err("ids must be numbers".into()),
    }
    Ok(out)
}

pub fn str_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(Value::as_str).filter(|s| !s.is_empty())
}

pub fn usize_arg(args: &Value, key: &str, default: usize, max: usize) -> usize {
    args.get(key)
        .and_then(Value::as_u64)
        .map(|n| (n as usize).min(max))
        .unwrap_or(default)
}

/// Normalise a part name: `3001` → `3001.dat`; submodel names are kept.
pub fn part_name(s: &str) -> String {
    let s = s.trim();
    let lower = s.to_ascii_lowercase();
    if lower.ends_with(".dat") || lower.ends_with(".ldr") || lower.ends_with(".mpd") {
        s.to_string()
    } else {
        format!("{s}.dat")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn grid_units_flip_y_and_scale() {
        let g = Units::Grid;
        assert_eq!(g.to_ldu([1.0, 3.0, -2.0]), DVec3::new(20.0, -24.0, -40.0));
        assert_eq!(g.from_ldu(DVec3::new(20.0, -24.0, -40.0)), [1.0, 3.0, -2.0]);
    }

    #[test]
    fn rotations_parse_rows_and_shorthands() {
        let m = rotation(&json!([0, 0, 1, 0, 1, 0, -1, 0, 0])).unwrap();
        // LDraw row order: x' = z, so the matrix maps X to -Z.
        assert!((m * DVec3::X - DVec3::new(0.0, 0.0, -1.0)).length() < 1e-9);
        let y = shorthand("yaw90").unwrap();
        assert_eq!(rot_rows(y), rot_rows(DMat3::from_rotation_y(90f64.to_radians())));
        assert!(shorthand("yaw90*pitch-90").is_ok());
        assert!(shorthand("spin3").is_err());
        assert_eq!(rot_rows(rotation(&json!("identity")).unwrap()), rot_rows(DMat3::IDENTITY));
    }

    #[test]
    fn id_refs_mix_numbers_and_names() {
        let r = id_refs(&json!([3, "s4", "last", "#7"])).unwrap();
        assert_eq!(
            r,
            vec![IdRef::Id(3), IdRef::Named("s4".into()), IdRef::Named("last".into()), IdRef::Id(7)]
        );
    }
}
