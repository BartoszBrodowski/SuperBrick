//! S6 search spike.
//!
//!   search-spike catalog                      build out/catalog.json (LDraw headers + Rebrickable)
//!   search-spike index --enc NAME             embed every part with models/NAME → out/emb-NAME.bin
//!   search-spike check-golden                 show what each golden answer is, to catch wrong answers
//!   search-spike eval [--enc NAME|none] [--no-priors] [--no-synonyms] [--no-typos] [--fails]
//!   search-spike query [--enc NAME] TEXT...

mod catalog;
mod embed;
mod search;
mod text;

use std::{
    error::Error,
    io::{Read, Write},
    path::{Path, PathBuf},
    time::Instant,
};

use search::{Searcher, Weights};

type Res<T> = Result<T, Box<dyn Error + Send + Sync>>;

fn base() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn opt(args: &[String], name: &str) -> Option<String> {
    args.iter().position(|a| a == name).and_then(|i| args.get(i + 1)).cloned()
}

fn max_rss_mb() -> f64 {
    let mut u: libc::rusage = unsafe { std::mem::zeroed() };
    unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut u) };
    u.ru_maxrss as f64 / (1024.0 * 1024.0) // bytes on macOS
}

fn load_catalog() -> Res<catalog::Catalog> {
    let p = base().join("out/catalog.json");
    let text = std::fs::read_to_string(&p).map_err(|e| format!("{}: {e} (run `search-spike catalog` first)", p.display()))?;
    Ok(serde_json::from_str(&text)?)
}

fn emb_path(name: &str) -> PathBuf {
    base().join(format!("out/emb-{name}.bin"))
}

fn write_emb(path: &Path, n: usize, dim: usize, v: &[f32]) -> Res<()> {
    let mut f = std::io::BufWriter::new(std::fs::File::create(path)?);
    f.write_all(&(n as u32).to_le_bytes())?;
    f.write_all(&(dim as u32).to_le_bytes())?;
    for x in v {
        f.write_all(&x.to_le_bytes())?;
    }
    Ok(())
}

fn read_emb(path: &Path) -> Res<(usize, usize, Vec<f32>)> {
    let mut b = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut b)?;
    let n = u32::from_le_bytes(b[0..4].try_into()?) as usize;
    let dim = u32::from_le_bytes(b[4..8].try_into()?) as usize;
    let v = b[8..].chunks_exact(4).map(|c| f32::from_le_bytes(c.try_into().unwrap())).collect();
    Ok((n, dim, v))
}

fn searcher(args: &[String], w: Weights) -> Res<(Searcher, f64)> {
    let t = Instant::now();
    let mut s = Searcher::new(load_catalog()?, w);
    if let Some(name) = opt(args, "--enc").filter(|n| n != "none") {
        let enc = embed::load(&name, &base().join("models"))?;
        let (n, dim, v) = read_emb(&emb_path(&name)).map_err(|e| format!("{e} (run `search-spike index --enc {name}`)"))?;
        if n != s.cat.parts.len() || dim != enc.dim() {
            return Err(format!("out/emb-{name}.bin is stale; re-run index").into());
        }
        s.set_encoder(enc, v);
    }
    Ok((s, t.elapsed().as_secs_f64() * 1e3))
}

fn weights(args: &[String]) -> Weights {
    let mut w = Weights::default();
    if args.iter().any(|a| a == "--no-priors") {
        w.priors = false;
    }
    if args.iter().any(|a| a == "--no-synonyms") {
        w.synonyms = false;
    }
    if args.iter().any(|a| a == "--no-typos") {
        w.typos = false;
    }
    for (flag, slot) in [("--w-emb", &mut w.emb as *mut f32), ("--w-pop", &mut w.pop), ("--w-bm25", &mut w.bm25)] {
        if let Some(v) = opt(args, flag).and_then(|s| s.parse().ok()) {
            unsafe { *slot = v };
        }
    }
    w
}

struct Golden {
    query: String,
    ids: Vec<String>,
    kind: String,
}

fn golden() -> Res<Vec<Golden>> {
    let mut out = Vec::new();
    let mut kind = String::from("misc");
    for line in std::fs::read_to_string(base().join("golden.tsv"))?.lines() {
        if let Some(k) = line.strip_prefix("## ") {
            kind = k.trim().to_string();
            continue;
        }
        if line.trim().is_empty() || line.starts_with('#') {
            continue;
        }
        let mut f = line.split('\t');
        let (Some(q), Some(ids)) = (f.next(), f.next()) else { continue };
        out.push(Golden { query: q.trim().to_string(), ids: ids.split(',').map(|s| s.trim().to_lowercase()).collect(), kind: kind.clone() });
    }
    Ok(out)
}

fn cmd_catalog() -> Res<()> {
    let t = Instant::now();
    let lib = ldraw::Library::open_default().map_err(|e| e.to_string())?;
    let cat = catalog::build(&lib, &base().join("data/rebrickable")).map_err(|e| e.to_string())?;
    std::fs::create_dir_all(base().join("out"))?;
    std::fs::write(base().join("out/catalog.json"), serde_json::to_string(&cat)?)?;
    println!("{}", serde_json::to_string_pretty(&cat.stats)?);
    println!("catalog: {} parts in {:.1} s", cat.parts.len(), t.elapsed().as_secs_f64());
    Ok(())
}

fn cmd_index(args: &[String]) -> Res<()> {
    let name = opt(args, "--enc").ok_or("--enc NAME")?;
    let cat = load_catalog()?;
    let t = Instant::now();
    let enc = embed::load(&name, &base().join("models"))?;
    let load_ms = t.elapsed().as_secs_f64() * 1e3;
    let texts: Vec<String> = cat.parts.iter().map(search::doc_text).collect();
    let t = Instant::now();
    use rayon::prelude::*;
    let chunks: Vec<Vec<Vec<f32>>> = texts.par_chunks(512).map(|c| enc.encode(c).expect("encode")).collect();
    let flat: Vec<f32> = chunks.into_iter().flatten().flatten().collect();
    let secs = t.elapsed().as_secs_f64();
    write_emb(&emb_path(&name), texts.len(), enc.dim(), &flat)?;
    println!(
        "{name}: dim {}, model load {load_ms:.0} ms, embedded {} parts in {secs:.1} s ({:.0}/s), corpus {:.1} MB, max RSS {:.0} MB",
        enc.dim(),
        texts.len(),
        texts.len() as f64 / secs,
        flat.len() as f64 * 4.0 / 1e6,
        max_rss_mb()
    );
    Ok(())
}

fn cmd_check_golden() -> Res<()> {
    let cat = load_catalog()?;
    let by: std::collections::HashMap<&str, &catalog::Part> = cat.parts.iter().map(|p| (p.id.as_str(), p)).collect();
    let mut bad = 0;
    for g in golden()? {
        let descs: Vec<String> = g
            .ids
            .iter()
            .map(|id| match by.get(id.as_str()) {
                Some(p) if p.hidden() => {
                    bad += 1;
                    format!("{id} HIDDEN {} → {:?}", p.desc, p.moved_to)
                }
                Some(p) => format!("{id} \"{}\" pop {}", p.clean_desc(), p.pop),
                None => {
                    bad += 1;
                    format!("{id} MISSING")
                }
            })
            .collect();
        println!("{:42} {}", g.query, descs.join(" | "));
    }
    println!("{bad} golden answers missing or hidden");
    Ok(())
}

fn cmd_eval(args: &[String]) -> Res<()> {
    let w = weights(args);
    let (s, load_ms) = searcher(args, w)?;
    let gold = golden()?;
    let show = args.iter().any(|a| a == "--fails");
    let (mut top1, mut top5, mut mrr) = (0usize, 0usize, 0f64);
    let mut lat = Vec::new();
    let mut emb_lat = Vec::new();
    let mut by_kind: std::collections::BTreeMap<String, (usize, usize)> = Default::default();
    let mut fails = Vec::new();
    // Warm up caches once so the first query's page faults don't count.
    let _ = s.search("brick 2x2", 10);
    for g in &gold {
        let (hits, info) = s.search(&g.query, 50);
        lat.push(info.total_us / 1e3);
        emb_lat.push(info.embed_us / 1e3);
        let rank = hits.iter().position(|h| g.ids.contains(&s.cat.parts[h.idx].id));
        let e = by_kind.entry(g.kind.clone()).or_default();
        e.1 += 1;
        if rank == Some(0) {
            top1 += 1;
            e.0 += 1;
        } else if show {
            let got: Vec<String> = hits.iter().take(3).map(|h| format!("{} \"{}\"", s.cat.parts[h.idx].id, s.cat.parts[h.idx].clean_desc())).collect();
            fails.push(format!("  {:38} want {:16} rank {:>3}  got {}", g.query, g.ids.join(","), rank.map_or("-".into(), |r| (r + 1).to_string()), got.join(" · ")));
        }
        if rank.is_some_and(|r| r < 5) {
            top5 += 1;
        }
        mrr += rank.map_or(0.0, |r| 1.0 / (r as f64 + 1.0));
    }
    let n = gold.len() as f64;
    let pct = |v: &mut Vec<f64>, p: f64| {
        v.sort_by(|a, b| a.total_cmp(b));
        v[((v.len() as f64 - 1.0) * p).round() as usize]
    };
    let label = format!(
        "{}{}{}{}",
        s.encoder_name(),
        if w.priors { "" } else { " -priors" },
        if w.synonyms { "" } else { " -synonyms" },
        if w.typos { "" } else { " -typos" }
    );
    println!(
        "{label:28} top1 {:5.1}%  top5 {:5.1}%  MRR {:.3}  latency p50 {:.2} ms p95 {:.2} ms (embed p50 {:.2} ms)  load {:.0} ms  max RSS {:.0} MB",
        100.0 * top1 as f64 / n,
        100.0 * top5 as f64 / n,
        mrr / n,
        pct(&mut lat, 0.5),
        pct(&mut lat, 0.95),
        pct(&mut emb_lat, 0.5),
        load_ms,
        max_rss_mb()
    );
    let kinds: Vec<String> = by_kind.iter().map(|(k, (a, b))| format!("{k} {a}/{b}")).collect();
    println!("{:28} {}", "", kinds.join(" · "));
    for f in fails {
        println!("{f}");
    }
    Ok(())
}

fn cmd_query(args: &[String]) -> Res<()> {
    let (s, load_ms) = searcher(args, weights(args))?;
    let mut skip = false;
    let words: Vec<&str> = args
        .iter()
        .filter(|a| {
            if skip {
                skip = false;
                return false;
            }
            if a.starts_with("--w-") || *a == "--enc" {
                skip = true;
                return false;
            }
            !a.starts_with("--")
        })
        .map(String::as_str)
        .collect();
    let (hits, info) = s.search(&words.join(" "), 10);
    println!("loaded in {load_ms:.0} ms; query {:.2} ms (embed {:.2} ms); colour {:?}; corrected {:?}", info.total_us / 1e3, info.embed_us / 1e3, info.color, info.corrected);
    println!("tokens {:?}", info.tokens);
    for h in hits {
        let p = &s.cat.parts[h.idx];
        println!("{:7.3}  {:14} {:55} pop {:6} colours {}", h.score, p.id, p.clean_desc(), p.pop, p.colors.len());
    }
    Ok(())
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let rest = &args[1.min(args.len())..];
    let r = match args.first().map(String::as_str) {
        Some("catalog") => cmd_catalog(),
        Some("index") => cmd_index(rest),
        Some("check-golden") => cmd_check_golden(),
        Some("eval") => cmd_eval(rest),
        Some("query") => cmd_query(rest),
        _ => Err("usage: search-spike catalog | index --enc NAME | check-golden | eval [--enc NAME] | query TEXT".into()),
    };
    if let Err(e) = r {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}
