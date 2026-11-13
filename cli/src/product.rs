use crate::Res;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::Instant,
};
fn native(path: &Path) -> Res<engine::validation::Report> {
    let doc = ldraw::Document::load(path)?;
    let flat = doc.flatten(path.parent());
    let lib = ldraw::Library::open_default()?;
    Ok(engine::validation::Validator::default().check(&lib, &flat))
}
pub fn validate(args: &[String]) -> Res<bool> {
    let path = Path::new(args.first().ok_or("MODEL required")?);
    let t = Instant::now();
    let mut report = native(path)?;
    if args.iter().any(|s| s == "--review") {
        let model =
            engine::Model::from_document(ldraw::Document::load(path)?, Some(path.to_path_buf()));
        report.review_collisions(&model, model.doc.main_index().unwrap_or(0))?;
    }
    println!("{}", serde_json::to_string_pretty(&report)?);
    eprintln!("native validation: {:.2}s", t.elapsed().as_secs_f64());
    Ok(report.errors == 0)
}
pub fn parity(args: &[String]) -> Res<bool> {
    let path = Path::new(args.first().ok_or("MODEL required")?).canonicalize()?;
    let ours = serde_json::to_value(native(&path)?)?;
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut cmd = ldraw::runtime::python_command()?;
    cmd.arg(root.join("bench-next/validate.py"))
        .arg(&path)
        .arg("--json");
    cmd.env(
        "BENCH_SHADOW",
        root.join("data-next/ldcad_shadow/LDCadShadowLibrary-main"),
    );
    let output = cmd.output()?;
    let reference: Value = serde_json::from_slice(&output.stdout).map_err(|e| {
        format!(
            "Python validator did not return JSON: {e}\n{}",
            String::from_utf8_lossy(&output.stderr)
        )
    })?;
    let mut differences = vec![];
    for key in ["verdict", "groups", "link_types"] {
        if ours[key] != reference[key] {
            differences.push(json!({"field":key,"native":ours[key],"python":reference[key]}));
        }
    }
    for (i, (a, b)) in ours["rows"]
        .as_array()
        .unwrap()
        .iter()
        .zip(reference["rows"].as_array().ok_or("Python rows missing")?)
        .enumerate()
    {
        if a["status"] != b["status"] {
            differences.push(json!({"part":i+1,"native":a["status"],"python":b["status"]}));
        }
    }
    if ours["pieces"] != reference["pieces"] {
        differences
            .push(json!({"field":"pieces","native":ours["pieces"],"python":reference["pieces"]}));
    }
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"pass":differences.is_empty(),"differences":differences,"limitations":ours["limitations"]})
        )?
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
    eprintln!("Saved user transactions. Native connector validation remains partial.");
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
