//! Shared analyzer for part text and queries, so "2 x 2", "2x2", "2 by 2" and "two by two" all
//! become the token `2x2`, and "2x1" is the same footprint as "1x2".

pub const NUM_WORDS: &[(&str, &str)] = &[
    ("one", "1"),
    ("two", "2"),
    ("three", "3"),
    ("four", "4"),
    ("five", "5"),
    ("six", "6"),
    ("seven", "7"),
    ("eight", "8"),
    ("nine", "9"),
    ("ten", "10"),
    ("twelve", "12"),
    ("sixteen", "16"),
    ("single", "1"),
];

const STOP: &[&str] = &[
    "a", "an", "the", "of", "for", "to", "that", "is", "it", "its", "has", "have", "piece", "part",
    "lego", "and", "shaped", "thing", "block", "small", "tiny", "big", "little", "one's",
];

pub fn normalize(s: &str) -> String {
    s.to_lowercase()
        .replace("grey", "gray")
        .replace("centre", "center")
        .replace("colour", "color")
        .replace("tyre", "tire")
        .replace("minifigure", "minifig")
}

fn is_num(t: &str) -> bool {
    t.chars().next().is_some_and(|c| c.is_ascii_digit())
        && t.chars()
            .all(|c| c.is_ascii_digit() || c == '/' || c == '.')
}

fn num_val(t: &str) -> f64 {
    match t.split_once('/') {
        Some((a, b)) => a.parse::<f64>().unwrap_or(0.0) / b.parse::<f64>().unwrap_or(1.0).max(1e-9),
        None => t.parse().unwrap_or(0.0),
    }
}

/// Lowercased words and numbers, with glued dimensions ("2x4", "1x2x2/3") split into
/// number / "x" pieces and number words turned into digits.
pub fn raw_tokens(s: &str) -> Vec<String> {
    let mut out = Vec::new();
    for w in normalize(s).split(|c: char| !(c.is_alphanumeric() || c == '/' || c == '.')) {
        let w = w.trim_matches(['.', '/']);
        if w.is_empty() {
            continue;
        }
        if w.contains('x')
            && w.chars().any(|c| c.is_ascii_digit())
            && w.split('x').all(|p| p.is_empty() || is_num(p))
        {
            for (i, p) in w.split('x').enumerate() {
                if i > 0 {
                    out.push("x".to_string());
                }
                if !p.is_empty() {
                    out.push(p.to_string());
                }
            }
        } else if let Some((_, d)) = NUM_WORDS.iter().find(|(a, _)| *a == w) {
            out.push(d.to_string());
        } else {
            out.push(w.to_string());
        }
    }
    out
}

fn stem(w: &str) -> String {
    match w {
        "teeth" => return "tooth".into(),
        "axles" | "studs" | "sides" | "bricks" | "plates" | "tiles" | "slopes" | "holes"
        | "pins" | "fingers" | "wheels" | "bars" | "clips" | "corners" | "beams" | "gears"
        | "doors" | "windows" | "arches" | "legs" | "hands" | "ends" => {}
        _ => return w.to_string(),
    }
    w.strip_suffix("es")
        .filter(|s| s.ends_with("ch"))
        .map(str::to_string)
        .unwrap_or_else(|| w.trim_end_matches('s').to_string())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Dims {
    /// Footprint, smaller side first: "1x2".
    pub fp: String,
    /// Footprint plus the remaining dimensions as written: "1x2x2/3".
    pub full: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Analyzed {
    pub tokens: Vec<String>,
    pub dims: Vec<Dims>,
    /// Plain words and numbers (no dimension tokens), for typo correction.
    pub words: Vec<String>,
}

pub fn analyze(s: &str) -> Analyzed {
    analyze_tokens(&raw_tokens(s))
}

pub fn analyze_tokens(t: &[String]) -> Analyzed {
    let mut a = Analyzed::default();
    let mut i = 0;
    while i < t.len() {
        if is_num(&t[i]) {
            let mut d = vec![t[i].clone()];
            let mut j = i + 1;
            while j + 1 < t.len() && matches!(t[j].as_str(), "x" | "by" | "*") && is_num(&t[j + 1])
            {
                d.push(t[j + 1].clone());
                j += 2;
            }
            if d.len() >= 2 {
                let (x, y) = if num_val(&d[0]) <= num_val(&d[1]) {
                    (&d[0], &d[1])
                } else {
                    (&d[1], &d[0])
                };
                let fp = format!("{x}x{y}");
                let full = (d.len() > 2).then(|| format!("{fp}x{}", d[2..].join("x")));
                a.tokens.push(fp.clone());
                if let Some(f) = &full {
                    a.tokens.push(f.clone());
                }
                a.dims.push(Dims { fp, full });
                i = j;
                continue;
            }
        }
        let w = &t[i];
        i += 1;
        if w == "x" || STOP.contains(&w.as_str()) {
            continue;
        }
        let s = stem(w);
        a.words.push(s.clone());
        a.tokens.push(s);
    }
    a
}

/// Damerau-free Levenshtein distance with an early exit above `max`.
pub fn edit_distance(a: &str, b: &str, max: usize) -> usize {
    let (a, b): (Vec<char>, Vec<char>) = (a.chars().collect(), b.chars().collect());
    if a.len().abs_diff(b.len()) > max {
        return max + 1;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        let mut best = cur[0];
        for j in 1..=b.len() {
            let sub = prev[j - 1] + usize::from(a[i - 1] != b[j - 1]);
            cur[j] = sub.min(prev[j] + 1).min(cur[j - 1] + 1);
            best = best.min(cur[j]);
        }
        if best > max {
            return max + 1;
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dimensions_normalize() {
        for q in [
            "2x2 brick",
            "2 x 2 brick",
            "brick 2 by 2",
            "two by two brick",
            "BRICK 2X2",
        ] {
            let a = analyze(q);
            assert!(a.tokens.contains(&"2x2".to_string()), "{q}: {:?}", a.tokens);
            assert!(
                a.tokens.contains(&"brick".to_string()),
                "{q}: {:?}",
                a.tokens
            );
        }
        assert_eq!(analyze("Slope Brick 45 2 x 1").dims[0].fp, "1x2");
        let a = analyze("Slope Brick 31 1 x 1 x 2/3");
        assert_eq!(
            a.dims[0],
            Dims {
                fp: "1x1".into(),
                full: Some("1x1x2/3".into())
            }
        );
        assert_eq!(analyze("technic axle 3").tokens, ["technic", "axle", "3"]);
    }

    #[test]
    fn typo_distance() {
        assert_eq!(edit_distance("brik", "brick", 2), 1);
        assert_eq!(edit_distance("tyle", "tile", 2), 1);
        assert!(edit_distance("plate", "brick", 2) > 2);
    }
}
