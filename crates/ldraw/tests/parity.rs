//! Parity with the Python bench (bench-next/ldraw.py), from a fixture written by
//! `python3 crates/ldraw/tests/make_geom_ref.py`. Skips itself when the fixture or the LDraw
//! library is missing, so the crate still tests on a machine without either.

use std::path::{Path, PathBuf};

use ldraw::{Document, Library, Lod, Mesh};
use serde_json::Value;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../..")
}

fn setup() -> Option<(Library, Value)> {
    let fx = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/py_ref.json");
    let Ok(text) = std::fs::read_to_string(&fx) else {
        eprintln!("skip: {} missing (run make_geom_ref.py)", fx.display());
        return None;
    };
    let lib = match Library::open_default() {
        Ok(l) => l,
        Err(e) => {
            eprintln!("skip: no LDraw library ({e})");
            return None;
        }
    };
    Some((lib, serde_json::from_str(&text).expect("fixture is JSON")))
}

fn f(v: &Value) -> f64 {
    v.as_f64().expect("number")
}

fn vec3(v: &Value) -> [f64; 3] {
    [f(&v[0]), f(&v[1]), f(&v[2])]
}

fn hist(codes: impl Iterator<Item = i32>) -> Vec<(i64, i64)> {
    let mut m = std::collections::BTreeMap::new();
    for c in codes {
        *m.entry(c as i64).or_insert(0i64) += 1;
    }
    m.into_iter().collect()
}

fn ref_hist(v: &Value) -> Vec<(i64, i64)> {
    v.as_array().unwrap().iter().map(|p| (p[0].as_i64().unwrap(), p[1].as_i64().unwrap())).collect()
}

fn norm(name: &str) -> String {
    name.trim().to_ascii_lowercase().replace('\\', "/")
}

/// Differences between a Rust mesh and the Python summary, as readable strings.
fn compare_mesh(m: &Mesh, r: &Value) -> Vec<String> {
    let mut bad = Vec::new();
    let count = |k: &str| r[k].as_u64().unwrap() as usize;
    for (k, ours) in [
        ("tris", m.tris.len()),
        ("tstud", m.tstud.iter().filter(|&&s| s).count()),
        ("lines", m.lines.len()),
        ("cond", m.cond.len()),
        ("studs", m.studs.len()),
    ] {
        let theirs = if k == "studs" { r[k].as_array().unwrap().len() } else { count(k) };
        if ours != theirs {
            bad.push(format!("{k}: {ours} vs python {theirs}"));
        }
    }
    if !bad.is_empty() {
        return bad;
    }
    for (k, ours) in [
        ("tcol", hist(m.tcol.iter().map(|c| c.0))),
        ("lcol", hist(m.lcol.iter().map(|c| c.0))),
        ("ccol", hist(m.ccol.iter().map(|c| c.0))),
    ] {
        if ours != ref_hist(&r[k]) {
            bad.push(format!("{k} histogram differs"));
        }
    }
    // Order-free checksums over every triangle vertex, in f64 from the f32 vertices.
    let (mut sum, mut sq) = ([0f64; 3], 0f64);
    for t in &m.tris {
        for v in t {
            for a in 0..3 {
                let x = v[a] as f64;
                sum[a] += x;
                sq += x * x;
            }
        }
    }
    let scale = f(&r["abs"]).max(1.0);
    let rs = vec3(&r["sum"]);
    if (0..3).any(|a| (sum[a] - rs[a]).abs() > 1e-5 * scale) || (sq - f(&r["sq"])).abs() > 1e-5 * f(&r["sq"]).max(1.0) {
        bad.push(format!("vertex checksum {sum:?} vs python {rs:?}"));
    }
    let mut lsum = [0f64; 3];
    for l in &m.lines {
        for v in l {
            for a in 0..3 {
                lsum[a] += v[a] as f64;
            }
        }
    }
    let rl = vec3(&r["lsum"]);
    if (0..3).any(|a| (lsum[a] - rl[a]).abs() > 1e-5 * scale) {
        bad.push(format!("line checksum {lsum:?} vs python {rl:?}"));
    }
    let (lo, hi) = m.bbox(true);
    let (rlo, rhi) = (vec3(&r["bbox"][0]), vec3(&r["bbox"][1]));
    if (0..3).any(|a| (lo[a] as f64 - rlo[a]).abs() > 1e-3 || (hi[a] as f64 - rhi[a]).abs() > 1e-3) {
        bad.push(format!("bbox {lo:?}..{hi:?} vs python {rlo:?}..{rhi:?}"));
    }
    // Studs as a multiset: each Python stud must match a distinct Rust stud.
    let mut used = vec![false; m.studs.len()];
    for s in r["studs"].as_array().unwrap() {
        let v: Vec<f64> = (0..6).map(|i| f(&s[i])).collect();
        let hollow = s[6].as_bool().unwrap();
        let hit = m.studs.iter().enumerate().position(|(i, st)| {
            !used[i]
                && st.hollow == hollow
                && (0..3).all(|a| (st.pos[a] as f64 - v[a]).abs() < 1e-3 && (st.dir[a] as f64 - v[3 + a]).abs() < 1e-4)
        });
        match hit {
            Some(i) => used[i] = true,
            None => {
                bad.push(format!("python stud {v:?} hollow={hollow} has no match"));
                break;
            }
        }
    }
    bad
}

#[test]
fn meshes_match_python() {
    let Some((lib, fx)) = setup() else { return };
    let parts = fx["parts"].as_object().unwrap();
    let mut failures = Vec::new();
    let mut checked = 0;
    for (name, entry) in parts {
        for (lod_name, lod) in [("std", Lod::Std), ("low", Lod::Low)] {
            let r = &entry[lod_name];
            match (lib.mesh(name, lod), r.is_null()) {
                (Err(_), true) => {}
                (Err(e), false) => failures.push(format!("{name} [{lod_name}]: python meshes it, rust says {e}")),
                (Ok(_), true) => failures.push(format!("{name} [{lod_name}]: python can't resolve it, rust can")),
                (Ok(m), false) => {
                    checked += 1;
                    for b in compare_mesh(&m, r) {
                        failures.push(format!("{name} [{lod_name}]: {b}"));
                    }
                }
            }
        }
    }
    eprintln!("checked {checked} part meshes, {} differences", failures.len());
    for x in failures.iter().take(40) {
        eprintln!("  {x}");
    }
    assert!(failures.is_empty());
}

#[test]
fn flatten_matches_python() {
    let Some((_lib, fx)) = setup() else { return };
    let models = fx["models"].as_object().unwrap();
    let mut failures = Vec::new();
    let mut parts = 0;
    for (rel, refs) in models {
        let path = root().join(rel);
        let doc = match Document::load(&path) {
            Ok(d) => d,
            Err(e) => {
                failures.push(format!("{rel}: {e}"));
                continue;
            }
        };
        let ours = doc.flatten(path.parent());
        let theirs = refs.as_array().unwrap();
        if ours.len() != theirs.len() {
            failures.push(format!("{rel}: {} parts vs python {}", ours.len(), theirs.len()));
            continue;
        }
        for (i, (p, r)) in ours.iter().zip(theirs).enumerate() {
            let rot = ldraw::mesh::mat_to_rows(&p.rot);
            let rpos = vec3(&r[2]);
            let rrot: Vec<f64> = (0..9).map(|k| f(&r[3][k])).collect();
            let why = if norm(&p.name) != norm(r[0].as_str().unwrap()) {
                Some(format!("name {} vs {}", p.name, r[0]))
            } else if p.color.0 as i64 != r[1].as_i64().unwrap() {
                Some(format!("colour {} vs {}", p.color.0, r[1]))
            } else if (0..3).any(|a| (p.pos[a] - rpos[a]).abs() > 1e-6 * rpos[a].abs().max(1.0)) {
                Some(format!("pos {:?} vs {rpos:?}", p.pos))
            } else if (0..9).any(|k| (rot[k] - rrot[k]).abs() > 1e-9) {
                Some(format!("rot {rot:?} vs {rrot:?}"))
            } else if p.step as i64 != r[4].as_i64().unwrap() {
                Some(format!("step {} vs {}", p.step, r[4]))
            } else if p.path != r[5].as_str().unwrap() {
                Some(format!("path {:?} vs {}", p.path, r[5]))
            } else {
                None
            };
            if let Some(w) = why {
                failures.push(format!("{rel} part {i}: {w}"));
                break;
            }
        }
        parts += ours.len();
    }
    eprintln!("checked {} models, {parts} flattened parts, {} differences", models.len(), failures.len());
    for x in failures.iter().take(40) {
        eprintln!("  {x}");
    }
    assert!(failures.is_empty());
}
