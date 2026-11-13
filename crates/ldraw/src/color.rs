//! LDraw colour codes and the LDConfig.ldr colour table (`Colors` in ldraw.py).

use std::collections::{BTreeMap, HashMap};

/// Code `EDGE_BASE + c` means "the edge colour of colour c" (as in ldraw.py).
pub const EDGE_BASE: i32 = 10_000_000;

/// An LDraw colour code. Direct colours (`0x2RRGGBB`) are stored as `-(rgb) - 1`, edge-of-colour as
/// `EDGE_BASE + c`, matching the Python bench so results compare one to one.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Debug)]
pub struct Color(pub i32);

impl Color {
    pub const MAIN: Color = Color(16);
    pub const EDGE: Color = Color(24);

    pub fn direct(rgb: u32) -> Color {
        Color(-((rgb & 0xFF_FFFF) as i32) - 1)
    }

    pub fn direct_rgb(self) -> Option<[u8; 3]> {
        (self.0 < 0).then(|| {
            let v = (-(self.0 as i64) - 1) as u32;
            [(v >> 16) as u8, (v >> 8) as u8, v as u8]
        })
    }

    /// The code meaning "edge colour of self".
    pub fn edge_of(self) -> Color {
        Color(EDGE_BASE + self.0)
    }

    /// For an edge-of code, the colour whose edge it is.
    pub fn edge_source(self) -> Option<Color> {
        (self.0 >= EDGE_BASE).then(|| Color(self.0 - EDGE_BASE))
    }

    /// Parse a colour field of an LDraw line: a decimal code or `0x2RRGGBB` (`parse_color_code`).
    pub fn parse(s: &str) -> Option<Color> {
        let s = s.trim();
        if s.len() >= 2 && s[..2].eq_ignore_ascii_case("0x") {
            let v = u64::from_str_radix(&s[2..], 16).ok()?;
            return Some(Color::direct((v & 0xFF_FFFF) as u32));
        }
        s.parse::<i32>().ok().map(Color)
    }

    /// The text for the colour field of a written line.
    pub fn to_field(self) -> String {
        match self.direct_rgb() {
            Some([r, g, b]) => format!("0x2{r:02X}{g:02X}{b:02X}"),
            None => self.0.to_string(),
        }
    }
}

/// Surface finish from LDConfig (for the renderer).
#[derive(Clone, Copy, PartialEq, Eq, Debug, Default)]
pub enum Finish {
    #[default]
    Solid,
    Chrome,
    Pearlescent,
    Rubber,
    MatteMetallic,
    Metal,
    Glitter,
    Speckle,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ColorDef {
    pub code: i32,
    pub ldraw_name: String,
    /// Friendly name: "light bluish gray", "trans-clear".
    pub name: String,
    pub rgb: [u8; 3],
    pub edge: [u8; 3],
    pub alpha: u8,
    pub luminance: u8,
    pub finish: Finish,
}

const COLOR_ALIASES: &[(&str, i32)] = &[
    ("lbg", 71), ("lbgray", 71), ("lightbluishgray", 71), ("lightbluishgrey", 71), ("lightgray", 71),
    ("lightgrey", 71), ("dbg", 72), ("dbgray", 72), ("darkbluishgray", 72), ("darkbluishgrey", 72),
    ("darkgray", 72), ("darkgrey", 72), ("oldlightgray", 7), ("oldlightgrey", 7), ("olddarkgray", 8),
    ("olddarkgrey", 8), ("transclear", 47), ("clear", 47), ("grey", 71), ("gray", 71),
];

#[derive(Clone, Debug, Default)]
pub struct Colors {
    pub by_code: BTreeMap<i32, ColorDef>,
    by_key: HashMap<String, i32>,
}

fn hex6(s: &str) -> Option<[u8; 3]> {
    let b = s.as_bytes().get(..6)?;
    if !b.iter().all(u8::is_ascii_hexdigit) {
        return None;
    }
    let v = u32::from_str_radix(std::str::from_utf8(b).ok()?, 16).ok()?;
    Some([(v >> 16) as u8, (v >> 8) as u8, v as u8])
}

fn leading_int(s: &str) -> Option<u32> {
    let d: String = s.chars().take_while(char::is_ascii_digit).collect();
    d.parse().ok()
}

impl Colors {
    /// Parse `!COLOUR` lines. Same acceptance as the Python regex:
    /// `!COLOUR name CODE n VALUE #rrggbb EDGE (#?rrggbb | digits)`.
    pub fn parse(text: &str) -> Colors {
        let mut by_code = BTreeMap::new();
        for line in crate::num::split_lines(text) {
            if !line.contains("!COLOUR") {
                continue;
            }
            let t: Vec<&str> = line.split_whitespace().collect();
            let Some(i) = t.iter().position(|&x| x == "!COLOUR") else { continue };
            let get = |k: usize| t.get(i + k).copied();
            let (Some(name), Some("CODE"), Some(code), Some("VALUE"), Some(val), Some("EDGE"), Some(edge)) =
                (get(1), get(2), get(3), get(4), get(5), get(6), get(7))
            else {
                continue;
            };
            if !code.bytes().all(|b| b.is_ascii_digit()) {
                continue;
            }
            let Ok(code) = code.parse::<i32>() else { continue };
            let Some(hex) = val.strip_prefix('#') else { continue };
            if hex.len() != 6 {
                continue;
            }
            let Some(rgb) = hex6(hex) else { continue };
            let e = edge.strip_prefix('#').unwrap_or(edge);
            let edge_rgb = match hex6(e) {
                Some(v) => v,
                None if e.starts_with(|c: char| c.is_ascii_digit()) => [51, 51, 51],
                None => continue,
            };
            let after = |key: &str| t.iter().position(|&x| x == key).and_then(|j| t.get(j + 1)).and_then(|v| leading_int(v));
            let alpha = after("ALPHA").map_or(255, |a| a.min(255) as u8);
            let luminance = after("LUMINANCE").map_or(0, |a| a.min(255) as u8);
            let has = |k: &str| t.contains(&k);
            let finish = if has("CHROME") {
                Finish::Chrome
            } else if has("PEARLESCENT") {
                Finish::Pearlescent
            } else if has("RUBBER") {
                Finish::Rubber
            } else if has("MATTE_METALLIC") {
                Finish::MatteMetallic
            } else if has("METAL") {
                Finish::Metal
            } else if has("GLITTER") {
                Finish::Glitter
            } else if has("SPECKLE") {
                Finish::Speckle
            } else {
                Finish::Solid
            };
            by_code.insert(
                code,
                ColorDef {
                    code,
                    ldraw_name: name.to_string(),
                    name: Self::friendly(name),
                    rgb,
                    edge: edge_rgb,
                    alpha,
                    luminance,
                    finish,
                },
            );
        }
        let mut by_key = HashMap::new();
        // Highest code first, so the lowest code wins a shared name (as in ldraw.py).
        for (code, c) in by_code.iter().rev() {
            by_key.insert(Self::key(&c.name), *code);
            by_key.insert(Self::key(&c.ldraw_name), *code);
        }
        for (k, v) in COLOR_ALIASES {
            by_key.insert(k.to_string(), *v);
        }
        Colors { by_code, by_key }
    }

    pub fn friendly(ldname: &str) -> String {
        let n = ldname.to_lowercase().replace('_', " ").replace("grey", "gray");
        match n.strip_prefix("trans ") {
            Some(rest) => format!("trans-{rest}"),
            None => n,
        }
    }

    pub fn key(s: &str) -> String {
        s.to_lowercase().replace("grey", "gray").chars().filter(|c| c.is_ascii_lowercase() || c.is_ascii_digit()).collect()
    }

    pub fn get(&self, code: i32) -> Option<&ColorDef> {
        self.by_code.get(&code)
    }

    pub fn name(&self, code: i32) -> String {
        if let Some(c) = self.by_code.get(&code) {
            return c.name.clone();
        }
        if let Some([r, g, b]) = Color(code).direct_rgb() {
            return format!("#{r:02X}{g:02X}{b:02X}");
        }
        format!("color {code}")
    }

    /// Code for a colour name, number or `#rrggbb` (`Colors.code`).
    pub fn code(&self, s: &str) -> Result<i32, String> {
        let s = s.trim();
        let digits = s.strip_prefix('-').unwrap_or(s);
        if !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()) {
            return s.parse::<i32>().map_err(|e| e.to_string());
        }
        if let Some(h) = s.strip_prefix('#')
            && h.len() == 6
            && let Some([r, g, b]) = hex6(h)
        {
            return Ok(Color::direct(u32::from_be_bytes([0, r, g, b])).0);
        }
        self.by_key.get(&Self::key(s)).copied().ok_or_else(|| {
            format!(
                "Unknown colour '{s}'. Examples: red, black, white, tan, light bluish gray (lbg), \
                 dark bluish gray (dbg), trans-clear, or an LDraw colour number."
            )
        })
    }

    pub fn rgb(&self, code: i32) -> [u8; 3] {
        if let Some(c) = self.by_code.get(&code) {
            return c.rgb;
        }
        Color(code).direct_rgb().unwrap_or([128, 128, 128])
    }

    pub fn edge_rgb(&self, code: i32) -> [u8; 3] {
        self.by_code.get(&code).map_or([51, 51, 51], |c| c.edge)
    }

    pub fn alpha(&self, code: i32) -> u8 {
        self.by_code.get(&code).map_or(255, |c| c.alpha)
    }

    /// Display colour of a flattened code, resolving edge-of codes (for rendering).
    pub fn display_rgba(&self, c: Color) -> [u8; 4] {
        if let Some(src) = c.edge_source() {
            let [r, g, b] = self.edge_rgb(src.0);
            return [r, g, b, 255];
        }
        let [r, g, b] = self.rgb(c.0);
        [r, g, b, self.alpha(c.0)]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "0 !COLOUR Black CODE 0 VALUE #1B2A34 EDGE #808080\r\n\
        0 !COLOUR Trans_Clear CODE 47 VALUE #FCFCFC EDGE #C3C3C3 ALPHA 128\n\
        0 !COLOUR Light_Bluish_Grey CODE 71 VALUE #A0A5A9 EDGE #333333\n\
        0 !COLOUR Chrome_Gold CODE 334 VALUE #BBA53D EDGE #BBB23D CHROME\n\
        0 !COLOUR Edge_Code CODE 999 VALUE #123456 EDGE 0\n\
        0 !COLOUR Broken CODE 998 VALUE #12345 EDGE #333333\n";

    #[test]
    fn parses_ldconfig_lines() {
        let c = Colors::parse(SAMPLE);
        assert_eq!(c.by_code.len(), 5);
        assert_eq!(c.rgb(0), [0x1B, 0x2A, 0x34]);
        assert_eq!(c.alpha(47), 128);
        assert_eq!(c.name(47), "trans-clear");
        assert_eq!(c.name(71), "light bluish gray");
        assert_eq!(c.edge_rgb(999), [51, 51, 51]);
        assert_eq!(c.get(334).unwrap().finish, Finish::Chrome);
        assert_eq!(c.code("Light Bluish Grey"), Ok(71));
        assert_eq!(c.code("lbg"), Ok(71));
        assert_eq!(c.code("trans-clear"), Ok(47));
        assert_eq!(c.code("-5"), Ok(-5));
        assert_eq!(c.code("#FF0000"), Ok(Color::direct(0xFF0000).0));
        assert!(c.code("plaid").is_err());
    }

    #[test]
    fn colour_fields() {
        assert_eq!(Color::parse("16"), Some(Color::MAIN));
        let d = Color::parse("0x2FF8000").unwrap();
        assert_eq!(d.direct_rgb(), Some([0xFF, 0x80, 0x00]));
        assert_eq!(d.to_field(), "0x2FF8000");
        assert_eq!(Color(4).to_field(), "4");
        assert_eq!(Color::parse("x"), None);
        assert_eq!(Color(4).edge_of().edge_source(), Some(Color(4)));
    }
}
