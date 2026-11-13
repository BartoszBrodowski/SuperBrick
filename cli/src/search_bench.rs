use crate::Res;
use search::{Searcher, Weights};
use serde_json::{Value, json};
use std::collections::BTreeMap;

struct Query {
    text: String,
    ids: Vec<String>,
    style: String,
}

pub fn run(args: &[String]) -> Res<bool> {
    let split = crate::opt(args, "--split").unwrap_or("dev");
    if !["dev", "heldout"].contains(&split) {
        return Err("--split must be dev or heldout".into());
    }
    let legacy = crate::flag(args, "--legacy");
    let file = crate::opt(args, "--queries").ok_or("--queries FILE.jsonl required")?;
    let text = std::fs::read_to_string(if legacy {
        search::data_dir().join("golden.tsv")
    } else {
        file.into()
    })?;
    let mut queries = Vec::new();
    for line in text
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
    {
        if legacy {
            if let Some((q, ids)) = line.split_once('\t') {
                queries.push(Query {
                    text: q.into(),
                    ids: ids.split(',').map(|s| s.trim().into()).collect(),
                    style: "legacy".into(),
                });
            }
        } else {
            let v: Value = serde_json::from_str(line)?;
            if v["split"].as_str() != Some(split) {
                continue;
            }
            queries.push(Query {
                text: v["query"].as_str().ok_or("missing query")?.into(),
                ids: vec![v["part_id"].as_str().ok_or("missing part_id")?.into()],
                style: v["style"].as_str().ok_or("missing style")?.into(),
            });
        }
    }
    if queries.is_empty() {
        return Err("empty benchmark split".into());
    }
    let mut s = search::load(&search::data_dir(), true).map_err(|e| e.to_string())?;
    let profiles: Vec<&str> = if crate::flag(args, "--grid") {
        vec![
            "baseline",
            "adaptive",
            "balanced",
            "lexical",
            "precise",
            "semantic",
            "no-priors",
        ]
    } else {
        vec![crate::opt(args, "--profile").unwrap_or("production")]
    };
    if split == "heldout" && crate::flag(args, "--grid") {
        return Err("freeze one profile on dev before evaluating heldout".into());
    }
    let reports: Vec<_> = profiles
        .iter()
        .map(|profile| {
            s.w = weights(profile);
            evaluate(
                &s,
                &queries,
                profile,
                split,
                crate::flag(args, "--failures") && split == "dev",
            )
        })
        .collect();
    println!(
        "{}",
        serde_json::to_string_pretty(
            &json!({"split":if legacy {"legacy"} else {split},"reports":reports})
        )?
    );
    Ok(true)
}

fn weights(profile: &str) -> Weights {
    let mut w = Weights {
        adaptive: false,
        bm25: 1.,
        emb: 0.2,
        pop: 1.5,
        dims: 0.6,
        canon: 1.,
        priors: true,
        synonyms: true,
        typos: true,
    };
    let (bm, emb, pop, dims, canon) = match profile {
        "balanced" => (2., 0.8, 0.6, 0.6, 0.6),
        "lexical" => (4., 1., 0.3, 0.7, 0.3),
        "precise" => (5., 0.5, 0.1, 0.8, 0.3),
        "semantic" => (2., 2., 0.3, 0.5, 0.3),
        "no-priors" => (1., 0.5, 0., 0., 0.),
        "production" => return Weights::default(),
        "adaptive" => {
            w.adaptive = true;
            return w;
        }
        _ => (w.bm25, w.emb, w.pop, w.dims, w.canon),
    };
    w.bm25 = bm;
    w.emb = emb;
    w.pop = pop;
    w.dims = dims;
    w.canon = canon;
    w
}

fn evaluate(s: &Searcher, queries: &[Query], profile: &str, split: &str, failures: bool) -> Value {
    let mut totals = [0.; 4];
    let mut times = vec![];
    let mut misses = vec![];
    let mut styles: BTreeMap<String, (usize, usize)> = BTreeMap::new();
    for q in queries {
        let (hits, info) = s.filtered(&q.text, &search::Filters::default(), 50);
        let rank = hits
            .iter()
            .position(|h| q.ids.contains(&s.cat.parts[h.idx].id))
            .map(|r| r + 1);
        for (i, k) in [1, 5, 10].iter().enumerate() {
            totals[i] += f64::from(rank.is_some_and(|r| r <= *k));
        }
        totals[3] += rank.map_or(0., |r| 1. / r as f64);
        let st = styles.entry(q.style.clone()).or_default();
        st.0 += 1;
        st.1 += usize::from(rank.is_some_and(|r| r <= 10));
        times.push(info.total_us / 1000.);
        if failures && rank.is_none_or(|r| r > 10) {
            misses.push(json!({"query":q.text,"expected":q.ids,"rank":rank,"top":hits.iter().take(5).map(|h|json!({"id":s.cat.parts[h.idx].id,"desc":s.cat.parts[h.idx].clean_desc()})).collect::<Vec<_>>()}));
        }
    }
    times.sort_by(f64::total_cmp);
    let n = queries.len() as f64;
    json!({"profile":profile,"split":split,"queries":queries.len(),"top1":totals[0]/n,"top5":totals[1]/n,"top10":totals[2]/n,"mrr50":totals[3]/n,"p95_ms":times[(times.len()-1)*95/100],"by_style":styles.iter().map(|(k,(n,h))|(k,json!({"queries":n,"top10":*h as f64 / *n as f64}))).collect::<BTreeMap<_,_>>(),"failures":misses})
}
