//! The view planner: which angles, how many, and what to see through, so the agent gets every
//! picture it needs from the tool call itself instead of asking for previews.
//!
//! The planner measures instead of guessing. It frames the focus (the parts a reply is about) from
//! a sphere of candidate directions and renders all of them into one small id buffer, where each
//! pixel holds the part in front. A second, cheap pass draws only the focus parts, so for every
//! candidate and every focus part it knows the part's unoccluded size and how much of it really
//! shows. Views are then chosen greedily: the first maximises weighted visible focus (bigger and
//! less hidden is better, with a little context and a preference for three-quarter views from
//! above), and each further view must reveal focus the earlier ones missed and look from a clearly
//! different direction. Focus that no direction shows (inside a model) gets a cutaway view where
//! exactly the parts in front of it are drawn see-through. The number of candidates scales with
//! the model's cost (see [`crate::cost`]), and parts that cannot appear in any candidate view are
//! culled from the measuring passes.

use std::time::Instant;

use glam::{DVec3, Vec3};
use ldraw::FlatPart;
use render::{Camera, Panel, Rect, RenderService, SceneOptions};

use crate::cost;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Intent {
    /// After an edit: show what changed, one view unless more are needed.
    Edit,
    /// Problems: show every error, at least two views.
    Error,
    /// The agent asked to look: the requested number of informative views.
    Inspect,
    /// The whole model: views that show the most of it.
    Whole,
    /// One panel per item (placement candidates).
    Item,
}

impl Intent {
    /// (minimum, maximum) views before cutaways, given the caller's cap.
    pub fn range(self, cap: usize) -> (usize, usize) {
        let cap = cap.clamp(1, 6);
        match self {
            Intent::Edit => (1, cap),
            Intent::Error => (cap.min(2), cap),
            Intent::Inspect => (cap, cap),
            Intent::Whole => (cap.min(2).max(1), cap),
            Intent::Item => (1, 1),
        }
    }
}

pub struct Input<'a> {
    pub flat: &'a [FlatPart],
    pub boxes: &'a [Option<(DVec3, DVec3)>],
    /// Importance per flattened part; above zero means focus.
    pub weight: &'a [f32],
    /// Parts that never occlude or show (hidden, or drawn see-through as context).
    pub transparent: &'a [bool],
    /// The box every view frames.
    pub frame: (DVec3, DVec3),
    pub scene_radius: f32,
    pub intent: Intent,
    pub cap: usize,
    pub memory: Option<(f32, f32)>,
    pub budget_ms: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Main,
    /// Parts in front of hidden focus drawn see-through.
    Cutaway,
}

#[derive(Clone, Debug)]
pub struct View {
    pub yaw: f32,
    pub pitch: f32,
    pub kind: Kind,
    /// Flattened indices drawn see-through in this view.
    pub cutaway: Vec<usize>,
    /// Share of focus weight this view shows well.
    pub seen: f32,
}

#[derive(Clone, Debug, Default)]
pub struct Plan {
    pub views: Vec<View>,
    /// Focus parts (flattened indices) no chosen view shows, even with the cutaway.
    pub unseen: Vec<usize>,
    pub candidates: usize,
    pub culled: usize,
    pub ms: f64,
}

/// A part counts as shown in a view when this share of it is visible.
const SEEN: f32 = 0.3;

fn directions(n: usize) -> Vec<(f32, f32)> {
    let (yaws, pitches): (usize, &[f32]) = match n {
        0..=13 => (6, &[30.0, -28.0]),
        14..=25 => (8, &[55.0, 25.0, -28.0]),
        _ => (12, &[60.0, 32.0, 8.0, -30.0]),
    };
    let mut out = Vec::new();
    for (pi, &p) in pitches.iter().enumerate() {
        // Stagger alternate rings so the sphere is covered evenly.
        let offset = if pi % 2 == 1 { 180.0 / yaws as f32 } else { 0.0 } + 15.0;
        for i in 0..yaws {
            out.push(((offset + i as f32 * 360.0 / yaws as f32) % 360.0, p));
        }
    }
    out.push((35.0, 89.0));
    out
}

/// Unit view direction (from the target towards the eye) for separation tests.
pub fn direction(yaw: f32, pitch: f32) -> Vec3 {
    let (sy, cy) = yaw.to_radians().sin_cos();
    let (sp, cp) = pitch.to_radians().sin_cos();
    Vec3::new(cp * sy, sp, -cp * cy)
}

fn separation(a: (f32, f32), b: (f32, f32)) -> f32 {
    direction(a.0, a.1).dot(direction(b.0, b.1)).clamp(-1.0, 1.0).acos().to_degrees()
}

/// Camera for one framed view.
pub fn camera(frame: (DVec3, DVec3), aspect: f32, yaw: f32, pitch: f32, scene_radius: f32) -> Camera {
    let mut cam = Camera::framing_subset(
        frame.0.as_vec3(),
        frame.1.as_vec3(),
        aspect,
        yaw.to_radians(),
        pitch.to_radians(),
        scene_radius,
    );
    if pitch.abs() > 85.0 {
        cam.ortho = true;
    }
    cam
}

/// A camera from `yaw`/`pitch` fitted to a cloud of points (part-box corners) rather than to
/// one bounding box: centred on the points' screen extent and backed off until they fill the
/// panel with a small margin. Much tighter than box framing for sprawling models.
pub fn camera_fit(points: &[DVec3], aspect: f32, yaw: f32, pitch: f32, scene_radius: f32) -> Camera {
    if points.is_empty() {
        return Camera::framing_subset(glam::Vec3::splat(-10.0), glam::Vec3::splat(10.0), aspect, yaw.to_radians(), pitch.to_radians(), scene_radius);
    }
    let (mut lo, mut hi) = (points[0], points[0]);
    for p in points {
        lo = lo.min(*p);
        hi = hi.max(*p);
    }
    let mut cam = camera((lo, hi), aspect, yaw, pitch, scene_radius);
    let (right, up, back) = cam.basis();
    let (right, up, back) = (right.as_dvec3(), up.as_dvec3(), back.as_dvec3());
    let ty = (cam.fov_y as f64 * 0.5).tan();
    let tx = ty * aspect.max(1e-3) as f64;
    let margin = 1.05;
    for _ in 0..3 {
        let t = cam.target.as_dvec3();
        if cam.ortho {
            let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
            for p in points {
                let d = *p - t;
                let (x, y) = (d.dot(right), d.dot(up));
                x0 = x0.min(x);
                x1 = x1.max(x);
                y0 = y0.min(y);
                y1 = y1.max(y);
            }
            let c = t + right * (x0 + x1) * 0.5 + up * (y0 + y1) * 0.5;
            cam.target = c.as_vec3();
            let half = ((x1 - x0) * 0.5 / tx).max((y1 - y0) * 0.5 / ty) * margin;
            cam.dist = half.max(1.0) as f32;
            break;
        }
        // Perspective: centre the projected extent, then back off so the widest point fits.
        let eye = t + back * cam.dist as f64;
        let (mut x0, mut x1, mut y0, mut y1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
        for p in points {
            let d = *p - eye;
            let z = (-d.dot(back)).max(1.0);
            let (x, y) = (d.dot(right) / z, d.dot(up) / z);
            x0 = x0.min(x);
            x1 = x1.max(x);
            y0 = y0.min(y);
            y1 = y1.max(y);
        }
        let shift = right * ((x0 + x1) * 0.5) * cam.dist as f64 + up * ((y0 + y1) * 0.5) * cam.dist as f64;
        let t = t + shift;
        let mut dist: f64 = 1.0;
        for p in points {
            let d = *p - t;
            let (x, y, z) = (d.dot(right).abs(), d.dot(up).abs(), d.dot(back));
            dist = dist.max(z + x * margin / tx).max(z + y * margin / ty);
        }
        cam.target = t.as_vec3();
        cam.dist = dist as f32;
    }
    cam.radius = cam.radius.max(scene_radius);
    cam
}

/// Plan the views for one picture.
pub fn plan(svc: &RenderService, lib: &ldraw::Library, input: &Input) -> Result<Plan, String> {
    let t0 = Instant::now();
    let focus: Vec<usize> = (0..input.flat.len()).filter(|&i| input.weight[i] > 0.0).collect();
    let (min, max) = input.intent.range(input.cap);
    if focus.is_empty() {
        let (y, p) = input.memory.unwrap_or((35.0, 28.0));
        return Ok(Plan {
            views: vec![View { yaw: y, pitch: p, kind: Kind::Main, cutaway: vec![], seen: 0.0 }],
            ..Plan::default()
        });
    }
    let total: f32 = focus.iter().map(|&i| input.weight[i]).sum();

    // Cull what no candidate view can contain: every candidate camera sits within `reach` of the
    // frame's centre and looks at it, so a part beyond that sphere is never in front of the focus.
    let centre = (input.frame.0 + input.frame.1) * 0.5;
    let half = ((input.frame.1 - input.frame.0).length() * 0.5).max(10.0);
    let probe = camera(input.frame, 4.0 / 3.0, 35.0, 30.0, input.scene_radius);
    let reach = (probe.dist as f64).max(half * 1.2) * 1.15 + 5.0;
    let mut sub: Vec<FlatPart> = Vec::new();
    let mut map: Vec<usize> = Vec::new();
    let mut culled = 0;
    for (i, f) in input.flat.iter().enumerate() {
        if input.transparent[i] && input.weight[i] <= 0.0 {
            continue;
        }
        if input.weight[i] <= 0.0 {
            if let Some((lo, hi)) = input.boxes[i] {
                let r = (hi - lo).length() * 0.5;
                if ((lo + hi) * 0.5 - centre).length() - r > reach {
                    culled += 1;
                    continue;
                }
            }
        }
        sub.push(f.clone());
        map.push(i);
    }
    let fidx: std::collections::HashMap<usize, usize> = focus.iter().enumerate().map(|(k, &i)| (i, k)).collect();

    // How many candidates the budget allows.
    let (tris, _) = cost::tris_of(lib, &sub);
    let tile = (96u32, 72u32);
    let per_view = cost::predict(tris, 1, (tile.0 * tile.1) as f64, 0) - cost::current().base;
    let affordable = ((input.budget_ms * 0.45 - cost::current().base) / per_view.max(0.01)).floor() as usize;
    let n = match input.intent {
        Intent::Item => affordable.clamp(13, 25),
        _ => affordable.clamp(13, 49),
    };
    let dirs = directions(n);
    let cols = (dirs.len() as f32).sqrt().ceil() as u32;
    let rows = (dirs.len() as u32).div_ceil(cols);
    let size = (cols * tile.0, rows * tile.1);
    let panels: Vec<Panel> = dirs
        .iter()
        .enumerate()
        .map(|(k, &(yaw, pitch))| {
            let rect = Rect::new((k as u32 % cols) * tile.0, (k as u32 / cols) * tile.1, tile.0, tile.1);
            Panel { camera: camera(input.frame, rect.aspect(), yaw, pitch, input.scene_radius), rect }
        })
        .collect();
    let opts = SceneOptions { edges: false, ..SceneOptions::default() };
    let full = svc.render_ids(lib, &sub, &[], &panels, size, &opts)?;
    let solo_flat: Vec<FlatPart> = focus.iter().map(|&i| input.flat[i].clone()).collect();
    let solo = svc.render_ids(lib, &solo_flat, &[], &panels, size, &opts)?;
    let measure_ms = t0.elapsed().as_secs_f64() * 1e3;
    cost::observe(tris + cost::tris_of(lib, &solo_flat).0, dirs.len(), size.0 as f64 * size.1 as f64 * 2.0, sub.len(), measure_ms);

    // Per candidate: visible and unoccluded pixels per focus part, and context pixels.
    let nf = focus.len();
    let mut vis = vec![vec![0f32; nf]; dirs.len()];
    let mut area = vec![vec![0f32; nf]; dirs.len()];
    let mut ctx = vec![0f32; dirs.len()];
    let mut solo_px = vec![vec![0u32; nf]; dirs.len()];
    for (c, p) in panels.iter().enumerate() {
        let shown = full.counts(p.rect, sub.len());
        let alone = solo.counts(p.rect, nf);
        let mut context = 0u32;
        let mut shown_focus = vec![0u32; nf];
        for (s, &px) in shown.iter().enumerate() {
            match fidx.get(&map[s]) {
                Some(&k) => shown_focus[k] = px,
                None => context += px,
            }
        }
        for k in 0..nf {
            solo_px[c][k] = alone[k];
            vis[c][k] = if alone[k] == 0 { 0.0 } else { (shown_focus[k] as f32 / alone[k] as f32).min(1.0) };
        }
        ctx[c] = (context as f32 / (tile.0 * tile.1) as f32).min(0.35) / 0.35;
    }
    for k in 0..nf {
        let best = (0..dirs.len()).map(|c| solo_px[c][k]).max().unwrap_or(0).max(1) as f32;
        for c in 0..dirs.len() {
            area[c][k] = solo_px[c][k] as f32 / best;
        }
    }
    let w: Vec<f32> = focus.iter().map(|&i| input.weight[i]).collect();
    let quality = |c: usize, k: usize| if vis[c][k] < 0.05 { 0.0 } else { 0.65 * vis[c][k] + 0.35 * area[c][k] * vis[c][k].sqrt() };
    let preference = |c: usize| {
        let (yaw, pitch) = dirs[c];
        let mut b = 0.0;
        if (15.0..=45.0).contains(&pitch) {
            b += 0.06;
        } else if pitch < -10.0 {
            b -= 0.04;
        } else if pitch > 80.0 {
            b -= 0.03;
        }
        if let Some((my, mp)) = input.memory {
            if separation((yaw, pitch), (my, mp)) < 40.0 {
                b += 0.04;
            }
        }
        b * total
    };

    // Greedy choice.
    let mut chosen: Vec<usize> = Vec::new();
    let mut best_q = vec![0f32; nf];
    loop {
        if chosen.len() >= max {
            break;
        }
        let mut pick: Option<(usize, f32)> = None;
        for c in 0..dirs.len() {
            if chosen.iter().any(|&o| separation(dirs[o], dirs[c]) < 50.0) {
                continue;
            }
            let gain: f32 = (0..nf).map(|k| w[k] * (quality(c, k) - best_q[k]).max(0.0)).sum();
            let score = if chosen.is_empty() {
                (0..nf).map(|k| w[k] * quality(c, k)).sum::<f32>() + 0.12 * total * ctx[c] + preference(c)
            } else {
                gain + 0.04 * total * ctx[c] + 0.5 * preference(c)
            };
            if pick.is_none_or(|(_, s)| score > s) {
                pick = Some((c, score));
            }
        }
        let Some((c, _)) = pick else { break };
        let gain: f32 = (0..nf).map(|k| w[k] * (quality(c, k) - best_q[k]).max(0.0)).sum();
        if chosen.len() >= min && gain < 0.12 * total {
            break;
        }
        for k in 0..nf {
            best_q[k] = best_q[k].max(quality(c, k));
        }
        chosen.push(c);
    }
    let mut views: Vec<View> = chosen
        .iter()
        .map(|&c| View {
            yaw: dirs[c].0,
            pitch: dirs[c].1,
            kind: Kind::Main,
            cutaway: vec![],
            seen: (0..nf).filter(|&k| vis[c][k] >= SEEN).map(|k| w[k]).sum::<f32>() / total,
        })
        .collect();

    // Focus no view shows well: one cutaway from where it is most exposed.
    let best_vis: Vec<f32> = (0..nf).map(|k| chosen.iter().map(|&c| vis[c][k]).fold(0.0, f32::max)).collect();
    let hidden: Vec<usize> = (0..nf).filter(|&k| best_vis[k] < SEEN).collect();
    let hidden_w: f32 = hidden.iter().map(|&k| w[k]).sum();
    let mut unseen: Vec<usize> = hidden.iter().map(|&k| focus[k]).collect();
    if !hidden.is_empty() && (hidden_w >= 0.08 * total || hidden.len() >= 1 && input.intent == Intent::Error) {
        let c = (0..dirs.len())
            .max_by(|&a, &b| {
                let ea: f32 = hidden.iter().map(|&k| w[k] * area[a][k]).sum::<f32>() + preference(a);
                let eb: f32 = hidden.iter().map(|&k| w[k] * area[b][k]).sum::<f32>() + preference(b);
                ea.total_cmp(&eb)
            })
            .unwrap_or(0);
        // Occluders: parts drawn where the hidden focus would show.
        let r = panels[c].rect;
        let hidden_ids: std::collections::HashSet<u32> = hidden.iter().map(|&k| k as u32 + 1).collect();
        let mut hits: std::collections::HashMap<usize, u32> = std::collections::HashMap::new();
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                if hidden_ids.contains(&solo.at(x, y)) {
                    let front = full.at(x, y);
                    if front > 0 {
                        let i = map[front as usize - 1];
                        if !fidx.contains_key(&i) {
                            *hits.entry(i).or_default() += 1;
                        }
                    }
                }
            }
        }
        let mut cut: Vec<usize> = hits.into_iter().filter(|&(_, n)| n >= 2).map(|(i, _)| i).collect();
        cut.sort_unstable();
        if !cut.is_empty() {
            let seen = hidden.iter().map(|&k| w[k]).sum::<f32>() / total;
            views.push(View { yaw: dirs[c].0, pitch: dirs[c].1, kind: Kind::Cutaway, cutaway: cut, seen });
            unseen.retain(|i| solo_px[c][fidx[i]] == 0);
        }
    }
    Ok(Plan { views, unseen, candidates: dirs.len(), culled, ms: t0.elapsed().as_secs_f64() * 1e3 })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_sets_cover_the_sphere() {
        for n in [13, 25, 49] {
            let d = directions(n);
            assert!(d.len() <= n, "{n}: {}", d.len());
            // Every direction on the upper half has a candidate within 45°.
            for yaw in (0..360).step_by(20) {
                for pitch in [0, 30, 60] {
                    let near = d.iter().map(|&c| separation(c, (yaw as f32, pitch as f32))).fold(180.0, f32::min);
                    assert!(near < 50.0, "n={n} yaw={yaw} pitch={pitch}: {near}");
                }
            }
        }
    }

    #[test]
    fn ranges_follow_intent() {
        assert_eq!(Intent::Edit.range(3), (1, 3));
        assert_eq!(Intent::Error.range(3), (2, 3));
        assert_eq!(Intent::Inspect.range(2), (2, 2));
        assert_eq!(Intent::Item.range(4), (1, 1));
    }
}
