//! The checker: `app/src/cad_validate.py` (SuperBrick's own, on the bench's `physics.relation`)
//! run inside the bundled connector service.
//!
//! It knows every connector family the bench knows (studs, side studs, pins, axles, clips, bars,
//! hinges, balls, gears, compliance) and reports per piece OK / COLLISION / CONFLICT / FLOATING /
//! LOOSE / UNVERIFIED / UNCHECKED with an overall verdict VERIFIED / UNVERIFIED / ILLEGAL.
//! Pair relations stay cached in the service between calls, keyed by the two parts' names and
//! poses, so after an edit only the changed parts and their neighbours are re-tested.
//!
//! Numbers: `uid` is the bench's piece number, flat index + 1 (file order). `id` is the SuperBrick
//! part id of the top-level line the piece comes from (a submodel's pieces share its id).
use crate::{Id, Model, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;

pub const STATUSES: [&str; 7] = [
    "OK",
    "COLLISION",
    "CONFLICT",
    "FLOATING",
    "LOOSE",
    "UNVERIFIED",
    "UNCHECKED",
];
/// Statuses that make a model ILLEGAL.
pub const ERRORS: [&str; 3] = ["COLLISION", "CONFLICT", "FLOATING"];

/// A collision or connector conflict of a row's piece with an earlier piece.
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Hit {
    pub with: usize,
    #[serde(default)]
    pub with_id: Option<Id>,
    /// "collision" or "conflict".
    pub kind: String,
    /// Solid overlap in LDU³ (collisions).
    #[serde(default)]
    pub overlap: f64,
    /// Why the connectors cannot mate (conflicts).
    #[serde(default)]
    pub reason: String,
    #[serde(default, rename = "where")]
    pub at: Option<[f64; 3]>,
    /// Intersection of the two pieces' world boxes (studs included).
    #[serde(default, rename = "box")]
    pub bounds: Option<[[f64; 3]; 2]>,
    /// Box centres of this row's piece and of `with`.
    #[serde(default)]
    pub centres: Option<[[f64; 3]; 2]>,
}
impl Hit {
    /// Smallest axis-aligned move of the row's piece that would clear the box intersection, in
    /// LDU. An estimate (boxes, not meshes) used to label clearance failures.
    pub fn separation(&self) -> ([f64; 3], f64) {
        let (Some([lo, hi]), Some([a, b])) = (self.bounds, self.centres) else {
            return ([0.0; 3], 0.0);
        };
        let mut best = ([0.0; 3], f64::INFINITY);
        for k in 0..3 {
            let depth = (hi[k] - lo[k]).max(0.0);
            if depth < best.1 {
                let mut v = [0.0; 3];
                v[k] = if a[k] >= b[k] { depth } else { -depth };
                best = (v, depth);
            }
        }
        best
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RowLink {
    pub with: usize,
    /// {link type: count}: stud, side-stud, pin-hole, axle-axlehole, clip-bar, bar, hinge, ball, gen…
    #[serde(default)]
    pub types: BTreeMap<String, usize>,
    /// Remaining freedom: rigid, slide, rotate, ball, …
    #[serde(default)]
    pub dof: Value,
    #[serde(default)]
    pub axis: Option<[f64; 3]>,
    /// The user's note when the link was asserted rather than found.
    #[serde(default)]
    pub asserted: String,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Row {
    pub uid: usize,
    #[serde(default)]
    pub id: Option<Id>,
    pub step: usize,
    pub label: String,
    pub status: Vec<String>,
    /// The bench's own words (bench piece numbers).
    pub detail: Vec<String>,
    /// `detail` with SuperBrick part ids.
    #[serde(default)]
    pub notes: Vec<String>,
    #[serde(default)]
    pub source: String,
    #[serde(default)]
    pub links: Vec<RowLink>,
    #[serde(default)]
    pub hits: Vec<Hit>,
}
impl Row {
    pub fn has(&self, status: &str) -> bool {
        self.status.iter().any(|s| s == status)
    }
    pub fn is_error(&self) -> bool {
        self.status.iter().any(|s| ERRORS.contains(&s.as_str()))
    }
    pub fn is_ok(&self) -> bool {
        self.status.iter().all(|s| s == "OK")
    }
    /// Flat index of the piece.
    pub fn index(&self) -> usize {
        self.uid - 1
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Report {
    pub pieces: usize,
    #[serde(default)]
    pub steps: usize,
    /// VERIFIED, UNVERIFIED or ILLEGAL.
    pub verdict: String,
    /// In build order (step, then file order), not uid order: use [`Report::row`].
    pub rows: Vec<Row>,
    pub errors: usize,
    #[serde(default)]
    pub warnings: usize,
    #[serde(default)]
    pub unverified: usize,
    /// Connected groups of uids, largest first.
    pub groups: Vec<Vec<usize>>,
    /// (earlier uid, later uid, connection count).
    #[serde(default)]
    pub links: Vec<(usize, usize, usize)>,
    pub link_types: BTreeMap<String, usize>,
    #[serde(default)]
    pub gears: Value,
    #[serde(default)]
    pub sources: BTreeMap<String, usize>,
    /// The bench's text report (SuperBrick part ids).
    #[serde(default)]
    pub text: String,
    #[serde(skip_deserializing)]
    pub colour_warnings: Vec<crate::availability::ColourWarning>,
    #[serde(skip)]
    by_uid: Vec<usize>,
}
impl Report {
    fn index(&mut self) {
        self.by_uid = vec![usize::MAX; self.pieces + 1];
        for (k, r) in self.rows.iter().enumerate() {
            if r.uid < self.by_uid.len() {
                self.by_uid[r.uid] = k;
            }
        }
    }
    pub fn from_value(v: Value) -> Result<Report> {
        let mut r: Report = serde_json::from_value(v).map_err(|e| format!("checker reply: {e}"))?;
        r.index();
        Ok(r)
    }
    /// Row of bench piece `uid` (flat index + 1).
    pub fn row(&self, uid: usize) -> Option<&Row> {
        self.by_uid
            .get(uid)
            .and_then(|&k| self.rows.get(k))
            .filter(|r| r.uid == uid)
    }
    /// Row of flat part `i`.
    pub fn flat_row(&self, i: usize) -> Option<&Row> {
        self.row(i + 1)
    }
    pub fn count(&self, status: &str) -> usize {
        self.rows.iter().filter(|r| r.has(status)).count()
    }
    /// The bench's words for the verdict: "LEGAL (verified)", "UNVERIFIED" or "ILLEGAL".
    pub fn verdict_words(&self) -> &'static str {
        match self.verdict.as_str() {
            "VERIFIED" => "LEGAL (verified)",
            "ILLEGAL" => "ILLEGAL",
            _ => "UNVERIFIED",
        }
    }
    /// Every collision/conflict as (later flat index, earlier flat index, hit).
    pub fn hits(&self) -> impl Iterator<Item = (usize, usize, &Hit)> {
        self.rows
            .iter()
            .flat_map(|r| r.hits.iter().map(move |h| (r.uid - 1, h.with - 1, h)))
    }
    /// Connection graph: (flat a, flat b, connection count).
    pub fn edges(&self) -> impl Iterator<Item = (usize, usize, usize)> + '_ {
        self.links.iter().map(|&(a, b, n)| (a - 1, b - 1, n))
    }
}

/// `0 !BENCH ASSERT i j note` lines anywhere in the document: user-asserted links between
/// pieces i and j (bench numbers).
fn asserts(model: &Model) -> Vec<Value> {
    let doc = &model.doc;
    doc.preamble
        .iter()
        .chain(doc.files.iter().flat_map(|f| f.lines.iter()))
        .filter_map(|l| {
            let t: Vec<&str> = l.text.trim().splitn(6, char::is_whitespace).collect();
            (t.len() >= 5 && t[0] == "0" && t[1].eq_ignore_ascii_case("!BENCH") && t[2].eq_ignore_ascii_case("ASSERT"))
                .then(|| {
                    let a: usize = t[3].parse().ok()?;
                    let b: usize = t[4].parse().ok()?;
                    let note = t.get(5).map(|s| s.trim()).filter(|s| !s.is_empty()).unwrap_or("asserted");
                    Some(json!({"a":a,"b":b,"note":note}))
                })
                .flatten()
        })
        .collect()
}

/// Check file `file` of `model` (with whatever drafts the model already includes).
pub fn check(model: &Model, file: usize) -> Result<Report> {
    let name = model.doc.files.get(file).map(|f| f.name.clone()).unwrap_or_default();
    let reply = crate::connectors::request(
        model,
        file,
        json!({"action":"validate","name":name,"asserts":asserts(model)}),
    )?;
    let mut report = Report::from_value(reply)?;
    report.colour_warnings = crate::availability::check(&model.flat(file));
    Ok(report)
}

/// Which candidate placements the checker accepts in file `file` of `model`: each candidate is a
/// set of new parts, checked against every part except `exclude` (the parts being moved). A
/// candidate fails when it would collide or conflict (the checker's COLLISION / CONFLICT).
/// Answers `None` for an accepted candidate, else the reason.
pub fn check_candidates(
    model: &Model,
    file: usize,
    exclude: &[Id],
    candidates: &[Vec<crate::PartSpec>],
) -> Result<Vec<Option<String>>> {
    if candidates.is_empty() {
        return Ok(vec![]);
    }
    let cands: Vec<Value> = candidates
        .iter()
        .map(|c| {
            json!(c
                .iter()
                .map(|p| json!({"name":p.name,"color":p.color,"pos":p.pos,"rot":p.rot}))
                .collect::<Vec<_>>())
        })
        .collect();
    let reply = crate::connectors::request(
        model,
        file,
        json!({"action":"check_candidates","exclude":exclude,"candidates":cands}),
    )?;
    Ok(reply
        .as_array()
        .ok_or("checker reply: candidates")?
        .iter()
        .map(|r| {
            (!r["ok"].as_bool().unwrap_or(false))
                .then(|| r["reason"].as_str().unwrap_or("rejected").to_string())
        })
        .collect())
}
