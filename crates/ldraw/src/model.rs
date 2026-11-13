//! LDraw models (.ldr / .mpd) as a lossless line list.
//!
//! Every line keeps its exact text and terminator, so writing an unedited document gives back the same
//! bytes, and an edit only rewrites the lines it touched. Interpretation matches `parse_ldr_text` and
//! `flatten_model` in ldraw.py, with one intended difference: comment lines before an MPD's first
//! `0 FILE` are a preamble, not an empty main model.

use std::{
    collections::{HashMap, HashSet},
    io,
    path::Path,
};

use glam::{DMat3, DVec3};

use crate::{
    color::Color,
    library::norm,
    mesh::{mat_from_rows, mat_to_rows},
    num::{decode, floats, fmt_num, split_eol},
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Eol {
    Lf,
    CrLf,
    Cr,
    /// Last line of a file without a terminator.
    None,
}

impl Eol {
    pub fn as_str(self) -> &'static str {
        match self {
            Eol::Lf => "\n",
            Eol::CrLf => "\r\n",
            Eol::Cr => "\r",
            Eol::None => "",
        }
    }
}

/// A type-1 line: a part or submodel placed with colour, position and rotation (LDraw frame).
#[derive(Clone, Debug, PartialEq)]
pub struct PartLine {
    pub color: Color,
    pub pos: DVec3,
    pub rot: DMat3,
    /// File name as written (`3001.dat`, `s\3001s01.dat`, `wing.ldr`).
    pub name: String,
}

impl PartLine {
    /// Parse a type-1 line; `None` when it is not one (as ldraw.py skips it).
    pub fn parse(line: &str) -> Option<PartLine> {
        let t: Vec<&str> = line.split_whitespace().collect();
        if t.len() < 15 || t[0] != "1" {
            return None;
        }
        let color = Color::parse(t[1])?;
        let v = floats::<12>(&t[2..14])?;
        Some(PartLine {
            color,
            pos: DVec3::new(v[0], v[1], v[2]),
            rot: mat_from_rows(&v[3..12]),
            name: t[14..].join(" "),
        })
    }

    /// Line text as `Ref.line()` writes it.
    pub fn to_text(&self) -> String {
        let mut s = format!("1 {}", self.color.to_field());
        for x in self
            .pos
            .to_array()
            .into_iter()
            .chain(mat_to_rows(&self.rot))
        {
            s.push(' ');
            s.push_str(&fmt_num(x));
        }
        s.push(' ');
        s.push_str(&self.name);
        s
    }
}

#[derive(Clone, Debug, PartialEq)]
pub enum LineKind {
    Blank,
    /// Any other `0` line (header, comment, meta command).
    Meta,
    /// `0 FILE name`
    File(String),
    /// `0 NOFILE`
    NoFile,
    /// `0 !DATA name` (embedded texture data in an MPD)
    Data(String),
    /// `0 STEP`
    Step,
    /// `0 ROTSTEP ...` (also a step boundary)
    RotStep,
    Part(PartLine),
    /// Line, triangle, quad or conditional line (types 2-5).
    Geometry(u8),
    /// Anything that does not parse (kept verbatim).
    Other,
}

impl LineKind {
    pub fn classify(text: &str) -> LineKind {
        let t: Vec<&str> = text.split_whitespace().collect();
        let Some(&first) = t.first() else {
            return LineKind::Blank;
        };
        match first {
            "0" => {
                let w = t.get(1).map(|s| s.to_ascii_uppercase()).unwrap_or_default();
                match w.as_str() {
                    "FILE" if t.len() >= 3 => LineKind::File(t[2..].join(" ")),
                    "NOFILE" => LineKind::NoFile,
                    "STEP" => LineKind::Step,
                    "ROTSTEP" => LineKind::RotStep,
                    "!DATA" if t.len() >= 3 => LineKind::Data(t[2..].join(" ")),
                    _ => LineKind::Meta,
                }
            }
            "1" => PartLine::parse(text).map_or(LineKind::Other, LineKind::Part),
            "2" | "3" | "4" | "5" => LineKind::Geometry(first.as_bytes()[0] - b'0'),
            _ => LineKind::Other,
        }
    }
}

#[derive(Clone, Debug)]
pub struct Line {
    /// Exact text as read (without terminator); for dirty part lines, regenerated on write.
    pub text: String,
    pub eol: Eol,
    pub kind: LineKind,
    /// Edited since load: part lines are re-formatted from `kind` when written.
    pub dirty: bool,
}

impl Line {
    pub fn new(text: &str, eol: Eol) -> Line {
        Line {
            text: text.to_string(),
            eol,
            kind: LineKind::classify(text),
            dirty: false,
        }
    }

    pub fn part(p: PartLine, eol: Eol) -> Line {
        Line {
            text: p.to_text(),
            eol,
            kind: LineKind::Part(p),
            dirty: true,
        }
    }

    /// The text that will be written.
    pub fn out_text(&self) -> std::borrow::Cow<'_, str> {
        match (&self.kind, self.dirty) {
            (LineKind::Part(p), true) => p.to_text().into(),
            _ => self.text.as_str().into(),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FileKind {
    Model,
    Data,
}

/// One model (a whole .ldr, or one `0 FILE` section of an MPD).
#[derive(Clone, Debug)]
pub struct ModelFile {
    pub name: String,
    pub kind: FileKind,
    /// Includes the `0 FILE` line itself and any trailing `0 NOFILE`.
    pub lines: Vec<Line>,
}

impl ModelFile {
    /// Part lines with their line index and 1-based step.
    pub fn parts(&self) -> impl Iterator<Item = (usize, &PartLine, u32)> {
        let mut step = 1;
        self.lines
            .iter()
            .enumerate()
            .filter_map(move |(i, l)| match &l.kind {
                LineKind::Step | LineKind::RotStep => {
                    step += 1;
                    None
                }
                LineKind::Part(p) => Some((i, p, step)),
                _ => None,
            })
    }

    /// Number of step boundaries (`0 STEP` / `0 ROTSTEP`) plus one.
    pub fn step_count(&self) -> u32 {
        1 + self
            .lines
            .iter()
            .filter(|l| matches!(l.kind, LineKind::Step | LineKind::RotStep))
            .count() as u32
    }
}

/// A part of the flattened model, in the root model's frame (`flatten_model`).
#[derive(Clone, Debug, PartialEq)]
pub struct FlatPart {
    pub name: String,
    pub color: Color,
    pub pos: DVec3,
    pub rot: DMat3,
    /// Top-level step (1-based) in which the part, or the submodel holding it, appears.
    pub step: u32,
    /// Submodel names from the root down, joined by `/` ("" for root parts).
    pub path: String,
    /// The defining line: `(document, file, line)`, where document 0 is this one and higher numbers are
    /// external files loaded from `base_dir`.
    pub src: (usize, usize, usize),
    /// Index in the root file of the top-level line this part comes from.
    pub root_line: usize,
    /// Reference occurrences leading to this leaf, as (document, file, line).
    /// Unlike the display path this distinguishes multiple instances of a submodel.
    pub ancestors: Vec<(usize, usize, usize)>,
}

#[derive(Clone, Debug)]
pub struct Document {
    /// Lines before the first `0 FILE` of an MPD (comments only).
    pub preamble: Vec<Line>,
    pub files: Vec<ModelFile>,
    pub mpd: bool,
    bom: bool,
    latin1: bool,
    /// Terminator for new lines (the file's most common one).
    pub eol: Eol,
}

impl Document {
    /// An empty single-model document with the usual header.
    pub fn new(name: &str, title: &str) -> Document {
        let eol = Eol::CrLf;
        let lines = vec![
            Line::new(&format!("0 {title}"), eol),
            Line::new(&format!("0 Name: {name}"), eol),
            Line::new("0 Author: SuperBrick", eol),
        ];
        Document {
            preamble: Vec::new(),
            files: vec![ModelFile {
                name: name.to_string(),
                kind: FileKind::Model,
                lines,
            }],
            mpd: false,
            bom: false,
            latin1: false,
            eol,
        }
    }

    pub fn load(path: &Path) -> io::Result<Document> {
        let bytes = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "model.ldr".into());
        Ok(Document::parse(&bytes, &name))
    }

    pub fn parse(bytes: &[u8], default_name: &str) -> Document {
        let (bom, body) = match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
            Some(rest) => (true, rest),
            None => (false, bytes),
        };
        let (text, latin1) = decode(body);
        let raw = split_eol(&text);
        let mut counts = [0usize; 3];
        for (_, e) in &raw {
            match e {
                Eol::Lf => counts[0] += 1,
                Eol::CrLf => counts[1] += 1,
                Eol::Cr => counts[2] += 1,
                Eol::None => {}
            }
        }
        let eol = if counts[1] >= counts[0] && counts[1] >= counts[2] && counts[1] > 0 {
            Eol::CrLf
        } else if counts[2] > counts[0] {
            Eol::Cr
        } else {
            Eol::Lf
        };
        let lines: Vec<Line> = raw.iter().map(|(t, e)| Line::new(t, *e)).collect();
        let mpd = lines.iter().any(|l| matches!(l.kind, LineKind::File(_)));
        let mut doc = Document {
            preamble: Vec::new(),
            files: Vec::new(),
            mpd,
            bom,
            latin1,
            eol,
        };
        if !mpd {
            doc.files.push(ModelFile {
                name: default_name.to_string(),
                kind: FileKind::Model,
                lines,
            });
            return doc;
        }
        let mut cur: Option<ModelFile> = None;
        for l in lines {
            let start = match &l.kind {
                LineKind::File(n) => Some((n.clone(), FileKind::Model)),
                LineKind::Data(n) => Some((n.clone(), FileKind::Data)),
                _ => None,
            };
            if let Some((name, kind)) = start {
                if let Some(f) = cur.take() {
                    doc.files.push(f);
                }
                cur = Some(ModelFile {
                    name,
                    kind,
                    lines: vec![l],
                });
            } else if let Some(f) = cur.as_mut() {
                f.lines.push(l);
            } else {
                doc.preamble.push(l);
            }
        }
        doc.files.extend(cur);
        // Part lines before the first FILE form an implicit main model (ldraw.py would do the same).
        if doc
            .preamble
            .iter()
            .any(|l| matches!(l.kind, LineKind::Part(_)))
        {
            let lines = std::mem::take(&mut doc.preamble);
            doc.files.insert(
                0,
                ModelFile {
                    name: default_name.to_string(),
                    kind: FileKind::Model,
                    lines,
                },
            );
        }
        doc
    }

    /// Serialise. An unedited document gives back its input bytes exactly.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut s = String::new();
        for l in self
            .preamble
            .iter()
            .chain(self.files.iter().flat_map(|f| f.lines.iter()))
        {
            s.push_str(&l.out_text());
            s.push_str(l.eol.as_str());
        }
        let mut out = Vec::with_capacity(s.len() + 3);
        if self.bom {
            out.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
        }
        if self.latin1 && s.chars().all(|c| (c as u32) < 256) {
            out.extend(s.chars().map(|c| c as u8));
        } else {
            out.extend_from_slice(s.as_bytes());
        }
        out
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        std::fs::write(path, self.to_bytes())
    }

    /// Index of the main model (the first model file).
    pub fn main_index(&self) -> Option<usize> {
        self.files.iter().position(|f| f.kind == FileKind::Model)
    }

    pub fn main(&self) -> Option<&ModelFile> {
        self.main_index().map(|i| &self.files[i])
    }

    /// Model file by referenced name (case-insensitive, `\` = `/`).
    pub fn file_index(&self, name: &str) -> Option<usize> {
        let n = norm(name);
        self.files
            .iter()
            .position(|f| f.kind == FileKind::Model && norm(&f.name) == n)
    }

    /// Replace a part line; only that line is rewritten on save.
    pub fn set_part(&mut self, file: usize, line: usize, p: PartLine) {
        let l = &mut self.files[file].lines[line];
        l.text = p.to_text();
        l.kind = LineKind::Part(p);
        l.dirty = true;
    }

    /// Mark every part line edited (re-formats them all on save).
    pub fn touch_all_parts(&mut self) {
        for f in &mut self.files {
            for l in &mut f.lines {
                if matches!(l.kind, LineKind::Part(_)) {
                    l.dirty = true;
                }
            }
        }
    }

    /// Total number of part lines in all model files (submodel references included).
    pub fn line_part_count(&self) -> usize {
        self.files.iter().map(|f| f.parts().count()).sum()
    }

    /// Expand submodels recursively into parts in the root frame (`flatten_model`).
    /// With `base_dir`, `.ldr`/`.mpd` references found next to the model are loaded too.
    pub fn flatten(&self, base_dir: Option<&Path>) -> Vec<FlatPart> {
        match self.main_index() {
            Some(root) => self.flatten_file(root, base_dir),
            None => Vec::new(),
        }
    }

    /// Like [`Document::flatten`], with file `root` of this document as the root model (a submodel view).
    pub fn flatten_file(&self, root: usize, base_dir: Option<&Path>) -> Vec<FlatPart> {
        let mut ext: Vec<Document> = Vec::new();
        let mut keys: HashMap<String, (usize, usize)> = HashMap::new();
        let register = |keys: &mut HashMap<String, (usize, usize)>, di: usize, d: &Document| {
            for (fi, f) in d.files.iter().enumerate() {
                if f.kind == FileKind::Model {
                    keys.entry(norm(&f.name)).or_insert((di, fi));
                }
            }
        };
        register(&mut keys, 0, self);
        if let Some(dir) = base_dir {
            let refs = |d: &Document| -> Vec<String> {
                d.files
                    .iter()
                    .flat_map(|f| {
                        f.parts()
                            .map(|(_, p, _)| p.name.clone())
                            .collect::<Vec<_>>()
                    })
                    .collect()
            };
            let mut pending = refs(self);
            let mut tried = HashSet::new();
            while let Some(name) = pending.pop() {
                let n = norm(&name);
                if keys.contains_key(&n) || !tried.insert(n.clone()) {
                    continue;
                }
                for cand in [dir.join(&name), dir.join(name.replace('\\', "/"))] {
                    let ext_ok = cand.extension().and_then(|e| e.to_str()).is_some_and(|e| {
                        e.eq_ignore_ascii_case("ldr") || e.eq_ignore_ascii_case("mpd")
                    });
                    if !ext_ok || !cand.is_file() {
                        continue;
                    }
                    let Ok(d) = Document::load(&cand) else {
                        continue;
                    };
                    let di = ext.len() + 1;
                    if let Some(mi) = d.main_index() {
                        keys.insert(n.clone(), (di, mi));
                    }
                    register(&mut keys, di, &d);
                    pending.extend(refs(&d));
                    ext.push(d);
                    break;
                }
            }
        }
        let docs: Vec<&Document> = std::iter::once(self).chain(ext.iter()).collect();
        let mut out = Vec::new();
        if root < self.files.len() {
            let mut path = Vec::new();
            flatten_rec(
                &docs,
                &keys,
                &mut out,
                (0, root),
                DMat3::IDENTITY,
                DVec3::ZERO,
                Color::MAIN,
                None,
                &mut path,
                0,
                None,
                &mut Vec::new(),
            );
        }
        out
    }
}

#[allow(clippy::too_many_arguments)]
fn flatten_rec(
    docs: &[&Document],
    keys: &HashMap<String, (usize, usize)>,
    out: &mut Vec<FlatPart>,
    file: (usize, usize),
    m: DMat3,
    p: DVec3,
    color: Color,
    step: Option<u32>,
    path: &mut Vec<String>,
    depth: u32,
    root_line: Option<usize>,
    ancestors: &mut Vec<(usize, usize, usize)>,
) {
    if depth > 20 {
        return;
    }
    let mf = &docs[file.0].files[file.1];
    for (li, r, si) in mf.parts() {
        let st = step.unwrap_or(si);
        let c = if r.color == Color::MAIN {
            color
        } else {
            r.color
        };
        let m2 = m * r.rot;
        let p2 = m * r.pos + p;
        let rl = root_line.unwrap_or(li);
        if let Some(&sub) = keys.get(&norm(&r.name)) {
            path.push(r.name.clone());
            ancestors.push((file.0, file.1, li));
            flatten_rec(
                docs,
                keys,
                out,
                sub,
                m2,
                p2,
                c,
                Some(st),
                path,
                depth + 1,
                Some(rl),
                ancestors,
            );
            ancestors.pop();
            path.pop();
        } else {
            out.push(FlatPart {
                name: r.name.clone(),
                color: c,
                pos: p2,
                rot: m2,
                step: st,
                path: path.join("/"),
                src: (file.0, file.1, li),
                root_line: rl,
                ancestors: ancestors.clone(),
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MPD: &str = "0 FILE main.ldr\r\n0 Main\r\n1 16 0 0 0 1 0 0 0 1 0 0 0 1 sub.ldr\r\n0 STEP\r\n\
        1 4 0 -24 0 0 0 1 0 1 0 -1 0 0 3001.dat\r\n0 NOFILE\r\n0 FILE sub.ldr\r\n0 Sub\r\n\
        1 16 10 0 0 1 0 0 0 1 0 0 0 1 3003.dat\r\n1 2 0 -8 0 1 0 0 0 1 0 0 0 1 3024.dat\r\n0 NOFILE\r\n";

    #[test]
    fn mpd_round_trip_and_flatten() {
        let doc = Document::parse(MPD.as_bytes(), "x.mpd");
        assert!(doc.mpd);
        assert_eq!(doc.files.len(), 2);
        assert_eq!(doc.to_bytes(), MPD.as_bytes());
        assert_eq!(doc.eol, Eol::CrLf);
        let flat = doc.flatten(None);
        assert_eq!(flat.len(), 3);
        // sub.ldr placed in colour 16 at step 1; its 3003 inherits main colour 16, its 3024 stays green.
        assert_eq!(
            (
                flat[0].name.as_str(),
                flat[0].color,
                flat[0].step,
                flat[0].path.as_str()
            ),
            ("3003.dat", Color(16), 1, "sub.ldr")
        );
        assert_eq!(flat[1].color, Color(2));
        assert_eq!(
            (flat[2].name.as_str(), flat[2].step, flat[2].path.as_str()),
            ("3001.dat", 2, "")
        );
        assert_eq!(flat[0].pos, DVec3::new(10.0, 0.0, 0.0));
        assert_eq!(flat[0].root_line, 2);
    }

    #[test]
    fn edits_touch_only_their_line() {
        let src = "0 Title\n0 Name: a.ldr\n1 4 0 0 0 1 0 0 0 1 0 0 0 1 3001.dat\n1  1  20 0   0 1 0 0 0 1 0 0 0 1  3003.dat\n0 STEP\n";
        let mut doc = Document::parse(src.as_bytes(), "a.ldr");
        assert_eq!(doc.to_bytes(), src.as_bytes());
        let (li, p, _) = doc.files[0]
            .parts()
            .next()
            .map(|(i, p, s)| (i, p.clone(), s))
            .unwrap();
        let mut q = p.clone();
        q.pos.y = -24.0;
        q.color = Color::direct(0x123456);
        doc.set_part(0, li, q);
        let out = String::from_utf8(doc.to_bytes()).unwrap();
        assert_eq!(
            out,
            "0 Title\n0 Name: a.ldr\n1 0x2123456 0 -24 0 1 0 0 0 1 0 0 0 1 3001.dat\n1  1  20 0   0 1 0 0 0 1 0 0 0 1  3003.dat\n0 STEP\n"
        );
    }

    #[test]
    fn keeps_odd_bytes() {
        let src = b"0 Caf\xe9\r\n1 16 0 0 0 1 0 0 0 1 0 0 0 1 3001.dat";
        let doc = Document::parse(src, "a.ldr");
        assert_eq!(doc.to_bytes(), src.to_vec());
        let bom = b"\xEF\xBB\xBF0 T\n";
        assert_eq!(Document::parse(bom, "b.ldr").to_bytes(), bom.to_vec());
    }

    #[test]
    fn part_line_text_matches_ref_line() {
        let p = PartLine::parse("1 71 -10.5 -24 0.333333333 0 0 -1 0 1 0 1 0 0 s\\3001s01.dat")
            .unwrap();
        assert_eq!(
            p.to_text(),
            "1 71 -10.5 -24 0.333333 0 0 -1 0 1 0 1 0 0 s\\3001s01.dat"
        );
        assert!(PartLine::parse("1 71 a 0 0 1 0 0 0 1 0 0 0 1 x.dat").is_none());
    }
}
