//! Number formatting and line splitting shared by the parsers.

use crate::model::Eol;

/// LDraw number text, as `fmt_num` in ldraw.py: integers bare, else up to 6 decimals without trailing zeros.
pub fn fmt_num(x: f64) -> String {
    let r = x.round();
    if (x - r).abs() < 1e-6 {
        return format!("{}", r as i64);
    }
    let s = format!("{x:.6}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

/// Split text into lines with their terminators (`\n`, `\r\n` or a bare `\r`).
/// A last line without a terminator gets [`Eol::None`]; a trailing terminator adds no empty line.
pub fn split_eol(text: &str) -> Vec<(&str, Eol)> {
    let b = text.as_bytes();
    let mut out = Vec::new();
    let (mut start, mut i) = (0, 0);
    while i < b.len() {
        match b[i] {
            b'\n' => {
                out.push((&text[start..i], Eol::Lf));
                i += 1;
                start = i;
            }
            b'\r' => {
                if b.get(i + 1) == Some(&b'\n') {
                    out.push((&text[start..i], Eol::CrLf));
                    i += 2;
                } else {
                    out.push((&text[start..i], Eol::Cr));
                    i += 1;
                }
                start = i;
            }
            _ => i += 1,
        }
    }
    if start < b.len() {
        out.push((&text[start..], Eol::None));
    }
    out
}

/// Lines without terminators (Python `str.splitlines` for the terminators LDraw files use).
pub fn split_lines(text: &str) -> impl Iterator<Item = &str> {
    split_eol(text).into_iter().map(|(l, _)| l)
}

/// Parse `N` floats the way Python does (`float(x)`): decimal text to f64.
pub fn floats<const N: usize>(toks: &[&str]) -> Option<[f64; N]> {
    let mut out = [0.0; N];
    if toks.len() < N {
        return None;
    }
    for (o, t) in out.iter_mut().zip(toks) {
        *o = t.parse::<f64>().ok()?;
    }
    Some(out)
}

/// Decode file bytes as ldraw.py does: UTF-8, else Latin-1. Returns (text, was_latin1).
pub fn decode(bytes: &[u8]) -> (String, bool) {
    match std::str::from_utf8(bytes) {
        Ok(s) => (s.to_string(), false),
        Err(_) => (bytes.iter().map(|&b| b as char).collect(), true),
    }
}

/// 64-bit FNV-1a, a stable hash for cache keys.
pub fn fnv1a64(data: &[u8]) -> u64 {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for &b in data {
        h ^= b as u64;
        h = h.wrapping_mul(0x0000_0100_0000_01b3);
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_num_matches_python() {
        assert_eq!(fmt_num(0.0), "0");
        assert_eq!(fmt_num(-0.0000001), "0");
        assert_eq!(fmt_num(24.0), "24");
        assert_eq!(fmt_num(-24.0000004), "-24");
        assert_eq!(fmt_num(0.5), "0.5");
        assert_eq!(fmt_num(-0.707107), "-0.707107");
        assert_eq!(fmt_num(0.7071067811865476), "0.707107");
        assert_eq!(fmt_num(1.25), "1.25");
        assert_eq!(fmt_num(0.0000015), "0.000002");
    }

    #[test]
    fn split_eol_keeps_terminators() {
        let v = split_eol("a\r\nb\nc\rd");
        assert_eq!(v, vec![("a", Eol::CrLf), ("b", Eol::Lf), ("c", Eol::Cr), ("d", Eol::None)]);
        assert_eq!(split_eol("a\n"), vec![("a", Eol::Lf)]);
        assert_eq!(split_eol("\n\n"), vec![("", Eol::Lf), ("", Eol::Lf)]);
        assert!(split_eol("").is_empty());
    }
}
