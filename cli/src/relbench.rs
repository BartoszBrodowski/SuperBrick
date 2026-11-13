//! `cad relbench`: speed and parity of the Rust `physics::relation` port against the Python bench.
//!
//! 1. The pair scenarios in `crates/physics/tests/scenarios.json`, against `tests/fixtures/py_rel.json`
//!    (written by `crates/physics/tests/make_rel_ref.py`).
//! 2. Every pair Python's validate checked in a golden dump (`tools/golden/out/design/<model>.json`,
//!    written by `tools/golden/dump.py --models`), with the same poses, plus Rust's own broad phase.

use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, Instant},
};

use glam::{DMat3, DVec3};
use ldraw::Library;
use physics::{relation, Body, Relation, Shape, COLLIDE_MIN};
use rayon::prelude::*;
use serde_json::Value;

use crate::Res;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("..")
}

fn load(p: &Path) -> Res<Value> {
    let text = std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn v3(v: &Value) -> DVec3 {
    DVec3::new(
        v[0].as_f64().unwrap(),
        v[1].as_f64().unwrap(),
        v[2].as_f64().unwrap(),
    )
}

fn m3(v: &Value) -> DMat3 {
    let rows: Vec<f64> = (0..9).map(|i| v[i].as_f64().unwrap()).collect();
    ldraw::mesh::mat_from_rows(&rows)
}

/// Shapes built once per part name, in parallel.
fn build_shapes(
    lib: &Library,
    names: &[String],
) -> (HashMap<String, Arc<Shape>>, Duration, Vec<String>) {
    let t = Instant::now();
    let built: Vec<(String, Result<Shape, String>)> = names
        .par_iter()
        .map(|n| (n.clone(), Shape::build(lib, n).map_err(|e| e.to_string())))
        .collect();
    let el = t.elapsed();
    let mut ok = HashMap::new();
    let mut bad = Vec::new();
    for (n, s) in built {
        match s {
            Ok(s) => {
                ok.insert(n, Arc::new(s));
            }
            Err(e) => bad.push(format!("{n}: {e}")),
        }
    }
    (ok, el, bad)
}

fn time_rel(a: &Body, b: &Body) -> (Option<Relation>, f64) {
    let t = Instant::now();
    let mut n = 0u32;
    let mut out = None;
    while n < 3 || (t.elapsed() < Duration::from_millis(40) && n < 20_000) {
        out = relation(a, b, true);
        n += 1;
    }
    (out, t.elapsed().as_secs_f64() * 1e6 / n as f64)
}

struct Summary {
    collides: bool,
    overlap: f64,
    raw: f64,
    contact: bool,
    ab: usize,
    ba: usize,
    not_home: bool,
}

fn ours(r: &Relation) -> Summary {
    Summary {
        collides: r.collides(),
        overlap: r.overlap,
        raw: r.raw_overlap,
        contact: r.contact,
        ab: r.studs_ab,
        ba: r.studs_ba,
        not_home: r.not_home,
    }
}

/// Python relation as dumped by make_rel_ref.py (`ab`/`ba`/`raw`) or golden dump.py (`studs_ab`/...).
fn theirs(v: &Value) -> Summary {
    let num = |k: &str, alt: &str| {
        v.get(k)
            .or_else(|| v.get(alt))
            .and_then(Value::as_f64)
            .unwrap_or(0.0)
    };
    let not_home = v["conflicts"]
        .as_array()
        .is_some_and(|c| c.iter().any(|x| x.to_string().contains("not pressed home")));
    let overlap = num("overlap", "overlap");
    Summary {
        collides: overlap >= COLLIDE_MIN,
        overlap,
        raw: num("raw", "raw_overlap"),
        contact: v["contact"].as_bool().unwrap_or(false),
        ab: num("ab", "studs_ab") as usize,
        ba: num("ba", "studs_ba") as usize,
        not_home,
    }
}

fn diff(a: &Summary, b: &Summary) -> Vec<&'static str> {
    let mut d = Vec::new();
    if (a.overlap - b.overlap).abs() > 1e-6 {
        d.push("overlap");
    }
    if (a.raw - b.raw).abs() > 1e-6 {
        d.push("raw");
    }
    if a.contact != b.contact {
        d.push("contact");
    }
    if a.ab != b.ab || a.ba != b.ba {
        d.push("studs");
    }
    if a.not_home != b.not_home {
        d.push("not_home");
    }
    d
}

fn scenarios(lib: &Library) -> Res<bool> {
    let sc = load(&root().join("crates/physics/tests/scenarios.json"))?;
    let py = load(&root().join("crates/physics/tests/fixtures/py_rel.json"))?;
    let list = sc["scenarios"]
        .as_array()
        .ok_or("scenarios.json: no scenarios")?;
    let mut names: Vec<String> = list
        .iter()
        .flat_map(|s| {
            [
                s["a"][0].as_str().unwrap().to_string(),
                s["b"][0].as_str().unwrap().to_string(),
            ]
        })
        .collect();
    names.sort();
    names.dedup();
    let (shapes, el, bad) = build_shapes(lib, &names);
    if !bad.is_empty() {
        return Err(format!("shape build failed: {}", bad.join("; ")).into());
    }
    println!(
        "shapes: {} built in {:.1} ms (parallel)",
        shapes.len(),
        el.as_secs_f64() * 1e3
    );
    for (n, s) in &shapes {
        let p = &py["shapes"][n.as_str()];
        if p.is_null() {
            continue;
        }
        let lo: Vec<i64> = (0..3).map(|i| p["lo"][i].as_i64().unwrap()).collect();
        let hi: Vec<i64> = (0..3).map(|i| p["hi"][i].as_i64().unwrap()).collect();
        let anti = p["anti"].as_array().map_or(0, Vec::len);
        let studs = p["studs"].as_u64().unwrap_or(0) as usize;
        let same = lo == s.lo && hi == s.hi && anti == s.anti.len() && studs == s.studs.len();
        if !same {
            println!(
                "  shape {n}: rust lo {:?} hi {:?} studs {} anti {} ≠ python lo {lo:?} hi {hi:?} studs {studs} anti {anti}",
                s.lo,
                s.hi,
                s.studs.len(),
                s.anti.len()
            );
        }
    }
    let py_by: HashMap<&str, &Value> = py["scenarios"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| (s["name"].as_str().unwrap(), s))
        .collect();
    println!(
        "\n{:<22} {:>9} {:>10} {:>8}   {:<34} {}",
        "scenario",
        "rust µs",
        "python µs",
        "speedup",
        "rust: overlap raw studs contact",
        "vs python"
    );
    let mut all_ok = true;
    for s in list {
        let name = s["name"].as_str().unwrap();
        let body = |side: &Value| {
            Body::new(
                shapes[side[0].as_str().unwrap()].clone(),
                m3(&side[2]),
                v3(&side[1]),
            )
        };
        let (a, b) = (body(&s["a"]), body(&s["b"]));
        let (rel, us) = time_rel(&a, &b);
        let p = py_by.get(name).copied();
        let pus = p.and_then(|p| p["us"].as_f64()).unwrap_or(f64::NAN);
        let pres = p.map(|p| &p["result"]);
        let (desc, verdict) = match (&rel, pres) {
            (None, Some(Value::Null)) => ("none".to_string(), "same (none)".to_string()),
            (None, _) => (
                "none".to_string(),
                "DIFF: python found a relation".to_string(),
            ),
            (Some(_), Some(Value::Null)) => {
                ("some".to_string(), "DIFF: python found none".to_string())
            }
            (Some(r), Some(pr)) => {
                let (o, t) = (ours(r), theirs(pr));
                let d = diff(&o, &t);
                let desc = format!(
                    "{:>6} {:>6} {}+{} {}{}",
                    o.overlap,
                    o.raw,
                    o.ab,
                    o.ba,
                    o.contact,
                    if o.not_home { " not-home" } else { "" }
                );
                let narrow = pr["narrow"].as_bool().unwrap_or(false);
                let v = if d.is_empty() {
                    "same".to_string()
                } else {
                    format!(
                        "DIFF {} (python {} {} {}+{}{}){}",
                        d.join(","),
                        t.overlap,
                        t.raw,
                        t.ab,
                        t.ba,
                        if t.not_home { " not-home" } else { "" },
                        if narrow {
                            " — python used its narrow phase, not ported"
                        } else {
                            ""
                        }
                    )
                };
                (desc, v)
            }
            (Some(_), None) => ("some".to_string(), "no python result".to_string()),
        };
        if verdict.starts_with("DIFF") && !verdict.contains("narrow phase") {
            all_ok = false;
        }
        println!(
            "{name:<22} {us:>9.1} {pus:>10.0} {:>7.0}×   {desc:<34} {verdict}",
            pus / us
        );
    }
    Ok(all_ok)
}

fn model(lib: &Library, golden: &Path) -> Res<bool> {
    let g = load(golden)?;
    let parts = g["parts"].as_array().ok_or("golden: no parts")?;
    let pairs = g["pairs"].as_array().ok_or("golden: no pairs")?;
    let py_ms = g["validate_ms"].as_f64().unwrap_or(f64::NAN);
    let mut names: Vec<String> = parts
        .iter()
        .map(|p| p["part"].as_str().unwrap().to_string())
        .collect();
    names.sort();
    names.dedup();
    let (shapes, el, bad) = build_shapes(lib, &names);
    println!(
        "\n{}: {} parts, {} unique shapes built in {:.0} ms (parallel){}",
        golden.file_stem().unwrap().to_string_lossy(),
        parts.len(),
        shapes.len(),
        el.as_secs_f64() * 1e3,
        if bad.is_empty() {
            String::new()
        } else {
            format!(", {} failed", bad.len())
        }
    );
    let mut bodies: HashMap<i64, Body> = HashMap::new();
    for p in parts {
        if let Some(s) = shapes.get(p["part"].as_str().unwrap()) {
            bodies.insert(
                p["uid"].as_i64().unwrap(),
                Body::new(s.clone(), m3(&p["rot"]), v3(&p["pos"])),
            );
        }
    }

    // Same pairs as Python, one thread then all threads.
    let jobs: Vec<(&Body, &Body, &Value)> = pairs
        .iter()
        .filter_map(|p| {
            Some((
                bodies.get(&p["a"].as_i64()?)?,
                bodies.get(&p["b"].as_i64()?)?,
                &p["rel"],
            ))
        })
        .collect();
    let t = Instant::now();
    let rels: Vec<Option<Relation>> = jobs.iter().map(|(a, b, _)| relation(a, b, true)).collect();
    let one = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let _par: Vec<Option<Relation>> = jobs
        .par_iter()
        .map(|(a, b, _)| relation(a, b, true))
        .collect();
    let par = t.elapsed().as_secs_f64() * 1e3;
    let points: usize = rels.iter().flatten().map(|r| r.points).sum();
    println!(
        "same {} pairs as python: rust {one:.0} ms on 1 thread ({:.1} µs/pair), {par:.0} ms on {} threads; python validate {:.0} ms → {:.0}× / {:.0}×",
        jobs.len(),
        one * 1e3 / jobs.len().max(1) as f64,
        rayon::current_num_threads(),
        py_ms,
        py_ms / one,
        py_ms / par
    );
    println!(
        "  {} grid points sampled ({:.0} per pair)",
        points,
        points as f64 / jobs.len().max(1) as f64
    );

    // Agreement on what the editor needs: collides, stud links, contact.
    let (mut same_col, mut same_studs, mut same_contact, mut narrow, mut n) = (0, 0, 0, 0, 0);
    let mut col_only_rust = Vec::new();
    let mut col_only_py = Vec::new();
    let mut stud_diff = Vec::new();
    for ((a, b, pr), r) in jobs.iter().zip(&rels) {
        n += 1;
        let t = if pr.is_null() { None } else { Some(theirs(pr)) };
        let o = r.as_ref().map(ours);
        let (oc, tc) = (
            o.as_ref().is_some_and(|o| o.collides),
            t.as_ref().is_some_and(|t| t.collides),
        );
        let (os, ts) = (
            o.as_ref().map_or((0, 0), |o| (o.ab, o.ba)),
            t.as_ref().map_or((0, 0), |t| (t.ab, t.ba)),
        );
        let (ok, tk) = (
            o.as_ref().is_some_and(|o| o.contact),
            t.as_ref().is_some_and(|t| t.contact),
        );
        if pr["narrow"].as_bool().unwrap_or(false) {
            narrow += 1;
        }
        same_col += (oc == tc) as usize;
        same_studs += (os == ts) as usize;
        same_contact += (ok == tk) as usize;
        let label = format!("{}+{}", a.shape.name, b.shape.name);
        if oc && !tc {
            col_only_rust.push(label.clone());
        }
        if tc && !oc {
            col_only_py.push(label.clone());
        }
        if os != ts {
            stud_diff.push(format!("{label} {os:?} vs {ts:?}"));
        }
    }
    let pct = |x: usize| 100.0 * x as f64 / n.max(1) as f64;
    println!(
        "  agreement: collides {:.2}%  studs {:.2}%  contact {:.2}%   (python used its narrow phase on {narrow} pairs)",
        pct(same_col),
        pct(same_studs),
        pct(same_contact)
    );
    let show = |what: &str, v: &[String]| {
        if !v.is_empty() {
            let mut c: HashMap<&str, usize> = HashMap::new();
            for x in v {
                *c.entry(x.as_str()).or_default() += 1;
            }
            let mut c: Vec<_> = c.into_iter().collect();
            c.sort_by(|a, b| b.1.cmp(&a.1));
            let s: Vec<String> = c
                .iter()
                .take(8)
                .map(|(k, n)| {
                    if *n > 1 {
                        format!("{k} ×{n}")
                    } else {
                        k.to_string()
                    }
                })
                .collect();
            println!("  {what} ({}): {}", v.len(), s.join(" · "));
        }
    };
    show("collides only in rust", &col_only_rust);
    show("collides only in python", &col_only_py);
    show("stud links differ", &stud_diff);

    // Rust's own broad phase: sweep on x over full boxes, then relation on every touching pair.
    let list: Vec<&Body> = bodies.values().collect();
    let t = Instant::now();
    let mut order: Vec<usize> = (0..list.len()).collect();
    order.sort_by(|&i, &j| list[i].flo.x.total_cmp(&list[j].flo.x));
    let mut cand = Vec::new();
    for (k, &i) in order.iter().enumerate() {
        for &j in &order[k + 1..] {
            if list[j].flo.x > list[i].fhi.x + 1.0 {
                break;
            }
            let (a, b) = (list[i], list[j]);
            if (a.flo - 1.0).cmple(b.fhi).all() && (b.flo - 1.0).cmple(a.fhi).all() {
                cand.push((i, j));
            }
        }
    }
    let bp = t.elapsed().as_secs_f64() * 1e3;
    let t = Instant::now();
    let rel: Vec<Option<Relation>> = cand
        .par_iter()
        .map(|&(i, j)| relation(list[i], list[j], true))
        .collect();
    let rp = t.elapsed().as_secs_f64() * 1e3;
    let cols = rel.iter().flatten().filter(|r| r.collides()).count();
    let links: usize = rel.iter().flatten().map(|r| r.links.len()).sum();
    println!(
        "own broad phase: {} candidate pairs in {bp:.1} ms, relations in {rp:.0} ms ({} threads): {cols} collisions, {links} stud links",
        cand.len(),
        rayon::current_num_threads()
    );
    Ok(true)
}

pub fn cmd_relbench(args: &[String]) -> Res<bool> {
    let lib = Library::open_default()?;
    let ok = scenarios(&lib)?;
    let models: Vec<PathBuf> = {
        let given: Vec<PathBuf> = crate::positional(args).iter().map(PathBuf::from).collect();
        if given.is_empty() {
            ["tortoise", "skilltest_helmet"]
                .iter()
                .map(|m| root().join(format!("tools/golden/out/design/{m}.json")))
                .filter(|p| p.exists())
                .collect()
        } else {
            given
        }
    };
    for m in &models {
        model(&lib, m)?;
    }
    Ok(ok)
}
