//! Conservative native validator over the current voxel relation port.
//! Unknown connectors are UNVERIFIED, never silently claimed to be verified.
use ldraw::{FlatPart, Library};
use physics::{Body, Shape};
use rayon::prelude::*;
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

#[derive(Clone, Debug, Serialize)]
pub struct Row {
    pub uid: usize,
    pub status: Vec<String>,
    pub collides: Vec<usize>,
    pub links: Vec<usize>,
}
#[derive(Clone, Debug, Serialize)]
pub struct Report {
    pub colour_warnings: Vec<crate::availability::ColourWarning>,
    pub reviewed: Vec<PairReview>,
    pub pieces: usize,
    pub verdict: String,
    pub rows: Vec<Row>,
    pub groups: Vec<Vec<usize>>,
    pub errors: usize,
    pub link_types: BTreeMap<String, usize>,
    pub limitations: Vec<String>,
    /// Pieces in groups that neither rest on the ground nor touch anything outside their group.
    #[serde(default)]
    pub floating: usize,
    /// Every nearby pair the narrow phase looked at (flat indices), for clearance and
    /// sturdiness analysis. Not serialised: it is large and only used in-process.
    #[serde(skip)]
    pub pairs: Vec<Pair>,
}
/// One narrow-phase result between flat parts `a` and `b`.
#[derive(Clone, Debug, Default)]
pub struct Pair {
    pub a: usize,
    pub b: usize,
    pub overlap: f64,
    pub contact: bool,
    pub studs: usize,
    /// Intersection of the two world boxes (studs included); empty when they only touch.
    pub lo: [f64; 3],
    pub hi: [f64; 3],
}
impl Pair {
    /// Smallest axis-aligned move of `a` that would clear the box intersection, in LDU.
    /// An estimate (boxes, not meshes) used to label clearance failures.
    pub fn separation(&self, centre_a: [f64; 3], centre_b: [f64; 3]) -> ([f64; 3], f64) {
        let mut best = ([0.0; 3], f64::INFINITY);
        for k in 0..3 {
            let depth = (self.hi[k] - self.lo[k]).max(0.0);
            if depth < best.1 {
                let mut v = [0.0; 3];
                v[k] = if centre_a[k] >= centre_b[k] { depth } else { -depth };
                best = (v, depth);
            }
        }
        best
    }
}
#[derive(Clone, Debug, Serialize, serde::Deserialize)]
pub struct PairReview {
    pub a: usize,
    pub b: usize,
    pub collision: bool,
    #[serde(default)]
    pub warning: bool,
    pub overlap: f64,
    pub types: Vec<String>,
    #[serde(default)]
    pub reasons: Vec<String>,
    #[serde(default)]
    pub points: Vec<[f64; 3]>,
    #[serde(default)]
    pub raw_overlap: f64,
}
impl Report {
    /// Full pairwise narrow phase, including conflicts missed by the native port.
    pub fn review_collisions(&mut self, model: &crate::Model, file: usize) -> crate::Result<()> {
        let pairs: Vec<_> = self
            .rows
            .iter()
            .flat_map(|r| r.collides.iter().map(move |&b| (r.uid - 1, b)))
            .collect();
        let response = crate::connectors::request(
            model,
            file,
            serde_json::json!({"action":"check_pairs","pairs":pairs,"all_nearby":true}),
        )?;
        let checked: Vec<PairReview> =
            serde_json::from_value(response["pairs"].clone()).map_err(|e| e.to_string())?;
        if pairs
            .iter()
            .any(|&(a, b)| !checked.iter().any(|p| p.a == a && p.b == b))
            || checked.iter().any(|r| {
                r.a >= self.rows.len()
                    || r.b >= self.rows.len()
                    || r.a == r.b
                    || !r.overlap.is_finite()
                    || r.points.iter().flatten().any(|v| !v.is_finite())
            })
        {
            return Err("Incomplete connector collision review".into());
        }
        for p in &checked {
            if p.warning && !self.rows[p.a].status.iter().any(|s| s == "CONTACT_REVIEW") {
                self.rows[p.a].status.push("CONTACT_REVIEW".into());
            }
            if !p.collision {
                self.rows[p.a].collides.retain(|&b| b != p.b);
            } else if !self.rows[p.a].collides.contains(&p.b) {
                self.rows[p.a].collides.push(p.b);
            }
        }
        for r in &mut self.rows {
            if r.collides.is_empty() {
                r.status.retain(|s| s != "COLLISION");
            } else if !r.status.iter().any(|s| s == "COLLISION") {
                r.status.push("COLLISION".into());
            }
        }
        self.errors = self.rows.iter().filter(|r| !r.collides.is_empty()).count();
        self.verdict = if self.errors > 0 {
            "ILLEGAL"
        } else {
            "UNVERIFIED"
        }
        .into();
        self.reviewed = checked;
        self.limitations.push("All nearby piece pairs checked with full connector keep-outs and seating conflicts, not just native collision candidates. This is still not full stability, access or instruction-sequence certification.".into());
        Ok(())
    }
}
#[derive(Default)]
pub struct Validator {
    shapes: HashMap<String, Option<Arc<Shape>>>,
    pairs: HashMap<String, Option<physics::Relation>>,
}
impl Validator {
    pub fn shape(&mut self, lib: &Library, name: &str) -> Option<Arc<Shape>> {
        self.shapes
            .entry(name.into())
            .or_insert_with(|| Shape::build(lib, name).ok().map(Arc::new))
            .clone()
    }
    pub fn check(&mut self, lib: &Library, parts: &[FlatPart]) -> Report {
        self.check_focus(lib, parts, None)
    }
    /// Like [`check`](Self::check), but when `focus` is given only pairs with at least one
    /// focused member are evaluated. Edit validation uses this so an add or move costs time
    /// proportional to its neighbourhood instead of the whole model. Groups and FLOATING are
    /// only meaningful for full checks.
    pub fn check_focus(
        &mut self,
        lib: &Library,
        parts: &[FlatPart],
        focus: Option<&[bool]>,
    ) -> Report {
        let missing: Vec<_> = parts
            .iter()
            .map(|p| p.name.clone())
            .filter(|n| !self.shapes.contains_key(n))
            .collect();
        let built: Vec<_> = missing
            .par_iter()
            .map(|n| (n.clone(), Shape::build(lib, n).ok().map(Arc::new)))
            .collect();
        self.shapes.extend(built);
        let bodies: Vec<_> = parts
            .iter()
            .map(|p| {
                self.shapes[&p.name]
                    .clone()
                    .map(|s| Body::new(s, p.rot, p.pos))
            })
            .collect();
        // Sweep-and-prune. The broad phase is O(n log n + nearby pairs), not all n² pairs.
        let mut order: Vec<_> = (0..parts.len()).filter(|&i| bodies[i].is_some()).collect();
        let focused = |i: usize| focus.is_none_or(|f| f.get(i).copied().unwrap_or(false));
        order.sort_by(|&a, &b| {
            bodies[a]
                .as_ref()
                .unwrap()
                .flo
                .x
                .total_cmp(&bodies[b].as_ref().unwrap().flo.x)
        });
        let mut jobs = Vec::new();
        for (k, &i) in order.iter().enumerate() {
            let a = bodies[i].as_ref().unwrap();
            for &j in &order[k + 1..] {
                let b = bodies[j].as_ref().unwrap();
                if b.flo.x > a.fhi.x + 1.0 {
                    break;
                }
                if a.flo.y > b.fhi.y + 1.0
                    || b.flo.y > a.fhi.y + 1.0
                    || a.flo.z > b.fhi.z + 1.0
                    || b.flo.z > a.fhi.z + 1.0
                    || !(focused(i) || focused(j))
                {
                    continue;
                }
                let (i, j) = if (parts[i].step, i) > (parts[j].step, j) {
                    (i, j)
                } else {
                    (j, i)
                };
                let key = format!(
                    "{}:{:?}:{:?}|{}:{:?}:{:?}",
                    parts[i].name,
                    parts[i].pos,
                    parts[i].rot,
                    parts[j].name,
                    parts[j].pos,
                    parts[j].rot
                );
                jobs.push((i, j, key));
            }
        }
        let computed: Vec<_> = jobs
            .par_iter()
            .filter(|(_, _, k)| !self.pairs.contains_key(k))
            .map(|(i, j, k)| {
                (
                    k.clone(),
                    physics::relation(
                        bodies[*i].as_ref().unwrap(),
                        bodies[*j].as_ref().unwrap(),
                        true,
                    ),
                )
            })
            .collect();
        self.pairs.extend(computed);
        let mut rows: Vec<_> = (0..parts.len())
            .map(|i| Row {
                uid: i + 1,
                status: vec![],
                collides: vec![],
                links: vec![],
            })
            .collect();
        let mut parent: Vec<_> = (0..parts.len()).collect();
        fn root(p: &mut [usize], mut i: usize) -> usize {
            while p[i] != i {
                p[i] = p[p[i]];
                i = p[i];
            }
            i
        }
        let mut stud_count = 0;
        let mut pairs = Vec::with_capacity(jobs.len());
        for (i, j, key) in jobs {
            if let Some(r) = &self.pairs[&key] {
                let (a, b) = (bodies[i].as_ref().unwrap(), bodies[j].as_ref().unwrap());
                let lo = a.flo.max(b.flo);
                let hi = a.fhi.min(b.fhi);
                pairs.push(Pair {
                    a: i,
                    b: j,
                    overlap: r.overlap,
                    contact: r.contact,
                    studs: r.studs_ab + r.studs_ba,
                    lo: lo.to_array(),
                    hi: hi.to_array(),
                });
                if r.collides() {
                    rows[i].collides.push(j);
                }
                if r.studs_ab + r.studs_ba > 0 {
                    rows[i].links.push(j);
                    rows[j].links.push(i);
                    let a = root(&mut parent, i);
                    let b = root(&mut parent, j);
                    parent[a] = b;
                    stud_count += r.studs_ab + r.studs_ba;
                }
            }
        }
        let mut errors = 0;
        for (i, row) in rows.iter_mut().enumerate() {
            if !row.collides.is_empty() {
                row.status.push("COLLISION".into());
                errors += 1;
            }
            // Until shadow connector completeness is ported, absence of a geometric link cannot
            // prove FLOATING, and geometric stud matches cannot establish full-model parity.
            row.status.push(
                if bodies[i].is_none() {
                    "UNCHECKED"
                } else {
                    "UNVERIFIED"
                }
                .into(),
            );
        }
        let mut groups: BTreeMap<usize, Vec<usize>> = BTreeMap::new();
        for i in 0..parts.len() {
            groups.entry(root(&mut parent, i)).or_default().push(i + 1);
        }
        let mut groups: Vec<_> = groups.into_values().collect();
        groups.sort_by_key(|g| (std::cmp::Reverse(g.len()), g[0]));
        // FLOATING, conservatively: a stud-link group other than the largest that has no piece
        // on the ground (largest world y, LDraw y points down) and no geometric contact with any
        // piece outside the group. Technic or clip joints that the stud graph misses still count
        // as contact, so they never produce a false FLOATING.
        let mut floating = 0;
        if focus.is_none() && groups.len() > 1 {
            let ground = bodies
                .iter()
                .flatten()
                .map(|b| b.fhi.y)
                .fold(f64::NEG_INFINITY, f64::max);
            let mut group_of = vec![0usize; parts.len()];
            for (g, members) in groups.iter().enumerate() {
                for &uid in members {
                    group_of[uid - 1] = g;
                }
            }
            let mut touches_other = vec![false; groups.len()];
            for p in &pairs {
                if (p.contact || p.overlap > 0.0) && group_of[p.a] != group_of[p.b] {
                    touches_other[group_of[p.a]] = true;
                    touches_other[group_of[p.b]] = true;
                }
            }
            for (g, members) in groups.iter().enumerate().skip(1) {
                let grounded = members.iter().any(|&uid| {
                    bodies[uid - 1]
                        .as_ref()
                        .is_none_or(|b| b.fhi.y >= ground - 0.5)
                });
                if grounded || touches_other[g] {
                    continue;
                }
                for &uid in members {
                    rows[uid - 1].status.push("FLOATING".into());
                    floating += 1;
                }
            }
        }
        // Bound stale-pose memory in long editing sessions.
        if self.pairs.len() > 200000 {
            self.pairs.clear();
        }
        Report { colour_warnings:crate::availability::check(parts),reviewed:vec![],pieces:parts.len(),verdict:if errors>0 {"ILLEGAL"}else if parts.is_empty(){"VERIFIED"}else{"UNVERIFIED"}.into(),rows,groups,errors,
            link_types:BTreeMap::from([("stud".into(),stud_count)]),
            limitations:vec!["Native voxel port: shadow connectors, compliance, gears and precise tilted narrow phase are not yet implemented.".into()],
            floating, pairs }
    }
}
