use crate::Res;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
/// The checker's report for an LDraw file (the bench validator inside SuperBrick's service).
fn check(path: &Path) -> Res<engine::checker::Report> {
    let model = engine::Model::from_document(ldraw::Document::load(path)?, Some(path.to_path_buf()));
    Ok(engine::checker::check(&model, model.doc.main_index().unwrap_or(0))?)
}
pub fn validate(args: &[String]) -> Res<bool> {
    let path = Path::new(args.first().ok_or("MODEL required")?);
    let t = Instant::now();
    let report = check(path)?;
    if args.iter().any(|s| s == "--json") {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("{}", report.text);
    }
    eprintln!("check: {:.2}s", t.elapsed().as_secs_f64());
    Ok(report.errors == 0)
}
/// Runs `bench-next/validate.py` as the reference. Unless `--raw`, it reads connectors with
/// SuperBrick's corrections (`app/src/cad_snap_rules.py`), as the checker does; `--raw` shows
/// what the uncorrected bench says.
fn bench_reference(path: &Path, raw: bool) -> Res<Value> {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let bench = root.join("bench-next");
    let rules = root.join("cad/app/src");
    let script = format!(
        "import sys; sys.path[:0]=[{:?},{:?}]\nimport snaps, cad_snap_rules\n{}import validate\nsys.argv=['validate.py']+sys.argv[1:]\nvalidate.main()",
        bench.to_string_lossy(),
        rules.to_string_lossy(),
        if raw { "" } else { "cad_snap_rules.apply(snaps)\n" }
    );
    let mut cmd = ldraw::runtime::python_command()?;
    cmd.arg("-c").arg(script).arg(path).arg("--json");
    cmd.env(
        "BENCH_SHADOW",
        root.join("data-next/ldcad_shadow/LDCadShadowLibrary-main"),
    );
    let output = cmd.output()?;
    serde_json::from_slice(&output.stdout).map_err(|e| {
        format!(
            "Python validator did not return JSON: {e}\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
        .into()
    })
}
pub fn parity(args: &[String]) -> Res<bool> {
    let path = Path::new(args.first().ok_or("MODEL required")?).canonicalize()?;
    let raw = args.iter().any(|s| s == "--raw");
    let t = Instant::now();
    let ours = serde_json::to_value(check(&path)?)?;
    let took = t.elapsed().as_secs_f64();
    let reference = bench_reference(&path, raw)?;
    let mut differences = vec![];
    for key in ["verdict", "pieces", "steps", "errors", "warnings", "unverified", "groups", "link_types"] {
        if ours[key] != reference[key] {
            differences.push(json!({"field":key,"superbrick":ours[key],"bench":reference[key]}));
        }
    }
    let edges = |v: &Value| -> std::collections::BTreeSet<String> {
        v["links"].as_array().into_iter().flatten().map(|l| l.to_string()).collect()
    };
    let (a, b) = (edges(&ours), edges(&reference));
    if a != b {
        differences.push(json!({"field":"links","only_superbrick":a.difference(&b).collect::<Vec<_>>(),"only_bench":b.difference(&a).collect::<Vec<_>>()}));
    }
    let by_uid = |v: &Value| -> std::collections::BTreeMap<u64, Value> {
        v["rows"].as_array().into_iter().flatten().map(|r| (r["uid"].as_u64().unwrap_or(0), r.clone())).collect()
    };
    let (ra, rb) = (by_uid(&ours), by_uid(&reference));
    let mut statuses: std::collections::BTreeMap<String, [usize; 2]> = Default::default();
    for uid in ra.keys().chain(rb.keys()).collect::<std::collections::BTreeSet<_>>() {
        let (x, y) = (&ra.get(uid).cloned().unwrap_or(Value::Null), &rb.get(uid).cloned().unwrap_or(Value::Null));
        for s in x["status"].as_array().into_iter().flatten() {
            statuses.entry(s.as_str().unwrap_or("").into()).or_default()[0] += 1;
        }
        for s in y["status"].as_array().into_iter().flatten() {
            statuses.entry(s.as_str().unwrap_or("").into()).or_default()[1] += 1;
        }
        for key in ["step", "status", "detail", "links"] {
            if x[key] != y[key] {
                differences.push(json!({"part":uid,"field":key,"superbrick":x[key],"bench":y[key]}));
            }
        }
    }
    println!(
        "{}",
        serde_json::to_string_pretty(&json!({
            "model": path,
            "pass": differences.is_empty(),
            "verdict": [ours["verdict"], reference["verdict"]],
            "statuses (superbrick, bench)": statuses,
            "link_types": ours["link_types"],
            "check_seconds": took,
            "differences": differences.len(),
            "first_differences": differences.iter().take(20).collect::<Vec<_>>(),
        }))?
    );
    Ok(differences.is_empty())
}
pub fn search(args: &[String]) -> Res<bool> {
    let s = search::load(&search::data_dir(), true).map_err(|e| e.to_string())?;
    let (hits, info) = s.search(&args.join(" "), 20);
    for h in hits {
        let p = &s.cat.parts[h.idx];
        println!("{:8} {:.3} {}", p.id, h.score, p.clean_desc());
    }
    eprintln!("{:.2} ms", info.total_us / 1000.0);
    Ok(true)
}
pub fn commands() -> Res<bool> {
    println!(
        "{}",
        serde_json::to_string_pretty(
            &engine::registry()
                .iter()
                .map(|d| json!({"name":d.name,"key":d.key,"schema":d.schema}))
                .collect::<Vec<_>>()
        )?
    );
    Ok(true)
}
pub fn apply(args: &[String]) -> Res<bool> {
    let source = PathBuf::from(args.first().ok_or("MODEL required")?);
    let ops = crate::opt(args, "--ops").ok_or("--ops JSON file required")?;
    let out = PathBuf::from(crate::opt(args, "--out").ok_or("--out NEWFILE required")?);
    if out.exists() {
        return Err("refusing to overwrite an existing output; choose a new file".into());
    }
    let cmds: Vec<engine::Cmd> = serde_json::from_slice(&std::fs::read(ops)?)?;
    let mut e = engine::Engine::new(
        std::sync::Arc::new(ldraw::Library::open_default()?),
        ldraw::Document::load(&source)?,
        Some(source),
    );
    for cmd in cmds {
        println!("{}", e.execute(cmd)?);
    }
    e.save(Some(out))?;
    eprintln!("Saved user transactions.");
    Ok(true)
}
pub fn search_eval() -> Res<bool> {
    let s = search::load(&search::data_dir(), true).map_err(|e| e.to_string())?;
    let text = std::fs::read_to_string(search::data_dir().join("golden.tsv"))?;
    let mut n = 0;
    let mut top1 = 0;
    let mut mrr = 0.0;
    let mut times = vec![];
    let mut failures = vec![];
    for line in text
        .lines()
        .filter(|l| !l.starts_with('#') && !l.trim().is_empty())
    {
        let Some((q, ids)) = line.split_once('\t') else {
            continue;
        };
        let ids: Vec<_> = ids.split(',').map(str::trim).collect();
        let (hits, info) = s.search(q, 50);
        times.push(info.total_us / 1000.0);
        n += 1;
        let rank = hits
            .iter()
            .position(|h| ids.contains(&s.cat.parts[h.idx].id.as_str()));
        if rank == Some(0) {
            top1 += 1;
        } else {
            failures.push(json!({"query":q,"expected":ids,"rank":rank.map(|r|r+1),"got":hits.iter().take(3).map(|h|format!("{} {}",s.cat.parts[h.idx].id,s.cat.parts[h.idx].clean_desc())).collect::<Vec<_>>()}));
        }
        mrr += rank.map_or(0.0, |r| 1.0 / (r + 1) as f64);
    }
    if n == 0 {
        return Err("empty golden corpus".into());
    }
    times.sort_by(f64::total_cmp);
    let top1 = top1 as f64 / n as f64;
    let mrr = mrr / n as f64;
    let p95 = times[(times.len() - 1) * 95 / 100];
    let pass = top1 >= 0.9 && mrr >= 0.9 && p95 <= 30.0;
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"pass":pass,"queries":n,"top1":top1,"mrr":mrr,"p95_ms":p95,"failures":failures})
        )?
    );
    Ok(pass)
}
