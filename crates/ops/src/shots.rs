//! Pictures for replies: what to show (roles, ghosts, labels), from where (view choice and
//! framing), and the drawing (warm GPU render plus CPU overlay).
//!
//! Views come from [`crate::planner`] (measured visibility over candidate directions, cutaways for
//! enclosed focus); quality and culling from [`crate::cost`]. A small focus in a big model gets a
//! "where is it" inset of the whole model in the corner of the first panel. Placement candidates
//! use the per-item mode: one framed panel per candidate, each planned on its own.
use crate::overlay::{self, Canvas, Rgb};
use engine::{Id, Model};
use glam::{DMat3, DVec3, Vec3, Vec4Swizzles};
use ldraw::{FlatPart, Library, Lod};
use render::{Camera, Panel, Rect, RenderOptions, RenderService, RgbaImage};
use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, OnceLock};
use std::time::Instant;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Role {
    /// Just added or changed: highlighted.
    New,
    /// Part of the selection: highlighted, the rest dimmed when asked.
    Selected,
    /// Has a problem: red.
    Error,
    /// In the way of something: orange.
    Blocker,
    /// A proposal not yet in the model (candidates, previews): blue, see-through.
    Ghost,
    /// Gone in this change: red, see-through.
    Removed,
    /// Context only: grey, see-through.
    Dim,
    /// Not drawn.
    Hidden,
}

/// Ops-only bit (the renderer ignores it): keep the part's true colour and outline what is
/// visible of it afterwards, so the agent sees exactly the colours it built.
const OUTLINE: u32 = 1 << 20;

impl Role {
    fn flag(self) -> u32 {
        match self {
            Role::New | Role::Selected => OUTLINE,
            Role::Error => render::FLAG_ERROR,
            Role::Removed => render::FLAG_DELETE,
            Role::Blocker => render::FLAG_STALE,
            Role::Ghost => render::FLAG_GHOST,
            Role::Dim => render::FLAG_FADE,
            Role::Hidden => render::FLAG_HIDDEN,
        }
    }
}

#[derive(Clone, Debug)]
pub enum Views {
    /// Let the shot choose up to this many views.
    Auto(usize),
    /// Exact (yaw, pitch) pairs in degrees.
    Angles(Vec<(f32, f32)>),
}

#[derive(Clone, Debug)]
pub struct Label {
    pub text: String,
    pub at: DVec3,
    pub color: Rgb,
}

#[derive(Clone, Debug)]
pub struct BoxMark {
    pub lo: DVec3,
    pub hi: DVec3,
    pub color: Rgb,
    pub text: String,
}

#[derive(Clone, Debug)]
pub struct Shot {
    pub file: usize,
    pub roles: BTreeMap<Id, Role>,
    /// Parts drawn that are not in the model (candidate ghosts, removed parts).
    pub extra: Vec<(FlatPart, Role)>,
    pub labels: Vec<Label>,
    /// Badge every focus part with its id (when there are not too many).
    pub id_badges: bool,
    pub boxes: Vec<BoxMark>,
    pub views: Views,
    /// Grey out parts without a role.
    pub dim_rest: bool,
    pub title: String,
    pub size: (u32, u32),
    /// Frame the whole model instead of the focus.
    pub whole: bool,
    pub legend: Vec<(Rgb, String)>,
    /// What the picture is for; decides how many views the planner takes.
    pub intent: crate::planner::Intent,
    /// One panel per item (placement candidates, problems) instead of shared views.
    pub items: Vec<Item>,
    /// With items: lead with a whole-model panel that marks where each item is.
    pub overview: bool,
    /// No captions or legend (thumbnails).
    pub bare: bool,
}

/// One panel of a per-item picture: its own ghost parts and/or model parts, and a big label.
#[derive(Clone, Debug, Default)]
pub struct Item {
    pub label: String,
    pub extra: Vec<(FlatPart, Role)>,
    /// Where the label's leader points (usually the candidate's centre).
    pub at: DVec3,
    /// Model parts this panel is about (framed and badged), with how to draw them.
    pub roles: Vec<(Id, Role)>,
    /// One line under the label (what the panel shows).
    pub note: String,
    pub boxes: Vec<BoxMark>,
    /// Label colour (blue when unset).
    pub color: Option<Rgb>,
}

impl Shot {
    pub fn new(file: usize, title: impl Into<String>) -> Shot {
        Shot {
            file,
            roles: BTreeMap::new(),
            extra: Vec::new(),
            labels: Vec::new(),
            id_badges: false,
            boxes: Vec::new(),
            views: Views::Auto(1),
            dim_rest: false,
            title: title.into(),
            size: (960, 720),
            whole: false,
            legend: Vec::new(),
            intent: crate::planner::Intent::Inspect,
            items: Vec::new(),
            overview: false,
            bare: false,
        }
    }
    pub fn role(mut self, ids: impl IntoIterator<Item = Id>, role: Role) -> Shot {
        for id in ids {
            self.roles.insert(id, role);
        }
        self
    }
}

/// A rendered picture and what is in it.
#[derive(Clone, Debug)]
pub struct Picture {
    pub png: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub caption: String,
    /// The views used, (yaw, pitch) degrees.
    pub views: Vec<(f32, f32)>,
    pub ms: f64,
    /// Things the agent should know about the picture (enclosed parts, cutaways, quality drops).
    pub notes: Vec<String>,
}

/// The process's warm renderer (created on first use; GPU setup is the slow part).
pub fn service() -> Result<Arc<RenderService>, String> {
    static SERVICE: OnceLock<Result<Arc<RenderService>, String>> = OnceLock::new();
    SERVICE.get_or_init(|| RenderService::new().map(Arc::new)).clone()
}

/// World box of one placed part (studs included).
pub fn part_box(lib: &Library, name: &str, pos: DVec3, rot: DMat3) -> Option<(DVec3, DVec3)> {
    let mesh = lib.mesh(name, Lod::Low).ok()?;
    let (lo, hi) = mesh.bbox(false);
    if !lo.is_finite() || !hi.is_finite() {
        return None;
    }
    let (lo, hi) = (lo.as_dvec3(), hi.as_dvec3());
    let mut a = DVec3::splat(f64::INFINITY);
    let mut b = DVec3::splat(f64::NEG_INFINITY);
    for i in 0..8 {
        let c = DVec3::new(
            if i & 1 == 0 { lo.x } else { hi.x },
            if i & 2 == 0 { lo.y } else { hi.y },
            if i & 4 == 0 { lo.z } else { hi.z },
        );
        let w = rot * c + pos;
        a = a.min(w);
        b = b.max(w);
    }
    Some((a, b))
}

pub fn flat_box(lib: &Library, f: &FlatPart) -> Option<(DVec3, DVec3)> {
    part_box(lib, &f.name, f.pos, f.rot)
}

fn union(boxes: impl IntoIterator<Item = (DVec3, DVec3)>) -> Option<(DVec3, DVec3)> {
    boxes.into_iter().reduce(|a, b| (a.0.min(b.0), a.1.max(b.1)))
}

/// Flattened parts of `file` with the part id each one belongs to (its top-level line).
pub fn flat_with_ids(model: &Model, file: usize) -> Vec<(FlatPart, Option<Id>)> {
    let by_line: HashMap<usize, Id> = model
        .parts
        .values()
        .filter(|p| p.file == file)
        .map(|p| (p.line, p.id))
        .collect();
    model
        .flat(file)
        .into_iter()
        .map(|f| {
            let id = by_line.get(&f.root_line).copied();
            (f, id)
        })
        .collect()
}

/// Image size and panel rectangles for `n` panels, scaled from the one-panel `base` size.
pub fn layout(n: usize, base: (u32, u32)) -> ((u32, u32), Vec<Rect>) {
    let (w, h) = (base.0 as f32, base.1 as f32);
    let (size, cols, rows) = match n {
        0 | 1 => (base, 1, 1),
        2 => (((w * 4.0 / 3.0) as u32, (h * 2.0 / 3.0) as u32), 2, 1),
        3 | 4 => (((w * 4.0 / 3.0) as u32, (h * 4.0 / 3.0) as u32), 2, 2),
        _ => (((w * 1.5) as u32, h as u32), 3, 2),
    };
    let rects = Rect::grid(size, cols, rows, 4).into_iter().take(n.max(1)).collect();
    (size, rects)
}

fn weight(role: Role) -> f32 {
    match role {
        Role::Error => 3.0,
        Role::New | Role::Ghost => 2.0,
        Role::Blocker | Role::Removed => 1.5,
        Role::Selected => 1.0,
        Role::Dim | Role::Hidden => 0.0,
    }
}

/// The scene of a shot: parts, flags and what matters.
struct Built {
    flat: Vec<FlatPart>,
    flags: Vec<u32>,
    boxes: Vec<Option<(DVec3, DVec3)>>,
    weight: Vec<f32>,
    transparent: Vec<bool>,
    badge: Vec<(DVec3, Id)>,
    ids: Vec<Option<Id>>,
    model_box: (DVec3, DVec3),
    focus: Option<(DVec3, DVec3)>,
}

fn build(lib: &Library, model: &Model, shot: &Shot, extra: &[(FlatPart, Role)]) -> Built {
    let parts = flat_with_ids(model, shot.file);
    let mut b = Built {
        flat: Vec::with_capacity(parts.len() + extra.len()),
        flags: Vec::new(),
        boxes: Vec::new(),
        weight: Vec::new(),
        transparent: Vec::new(),
        badge: Vec::new(),
        ids: Vec::new(),
        model_box: (DVec3::splat(-20.0), DVec3::splat(20.0)),
        focus: None,
    };
    let mut focus_boxes = Vec::new();
    let mut seen_badge = std::collections::BTreeSet::new();
    let mut push = |b: &mut Built, f: FlatPart, role: Option<Role>, id: Option<Id>| {
        let bx = flat_box(lib, &f);
        let flag = match role {
            Some(r) => r.flag(),
            None if shot.dim_rest => render::FLAG_FADE,
            None => 0,
        };
        let w = role.map(weight).unwrap_or(0.0);
        if w > 0.0 {
            if let Some(bx) = bx {
                focus_boxes.push(bx);
                if let Some(i) = id {
                    if seen_badge.insert(i) {
                        b.badge.push(((bx.0 + bx.1) * 0.5, i));
                    }
                }
            }
        }
        b.transparent.push(flag & (render::FLAG_HIDDEN | render::FLAG_HIDDEN_GHOST) != 0);
        b.flags.push(flag);
        b.boxes.push(bx);
        b.weight.push(w);
        b.ids.push(id);
        b.flat.push(f);
    };
    for (f, id) in parts {
        let role = id.and_then(|i| shot.roles.get(&i).copied());
        push(&mut b, f, role, id);
    }
    for (f, r) in extra {
        push(&mut b, f.clone(), Some(*r), None);
    }
    for m in &shot.boxes {
        focus_boxes.push((m.lo, m.hi));
    }
    for l in &shot.labels {
        focus_boxes.push((l.at, l.at));
    }
    b.model_box = union(b.boxes.iter().flatten().copied()).unwrap_or(b.model_box);
    b.focus = if shot.whole { None } else { union(focus_boxes) };
    if b.focus.is_none() || shot.intent == crate::planner::Intent::Whole {
        // The whole model is the subject: every visible part counts the same.
        for i in 0..b.flat.len() {
            if b.weight[i] == 0.0 && !b.transparent[i] {
                b.weight[i] = 1.0;
            }
        }
    }
    b
}

/// Box to frame around a focus: two studs of context at least, a third of its size again.
fn padded(focus: Option<(DVec3, DVec3)>, model: (DVec3, DVec3)) -> (DVec3, DVec3) {
    match focus {
        Some(f) => {
            let pad = ((f.1 - f.0) * 0.35).max(DVec3::splat(30.0));
            (f.0 - pad, f.1 + pad)
        }
        None => {
            let pad = DVec3::splat(4.0);
            (model.0 - pad, model.1 + pad)
        }
    }
}

/// Corners of every visible part's box.
fn box_corners(b: &Built) -> Vec<DVec3> {
    let mut out = Vec::with_capacity(b.boxes.len() * 8);
    for (i, bx) in b.boxes.iter().enumerate() {
        let Some((lo, hi)) = bx else { continue };
        if b.flags[i] & render::FLAG_HIDDEN != 0 {
            continue;
        }
        for k in 0..8 {
            out.push(DVec3::new(
                if k & 1 == 0 { lo.x } else { hi.x },
                if k & 2 == 0 { lo.y } else { hi.y },
                if k & 4 == 0 { lo.z } else { hi.z },
            ));
        }
    }
    out
}

/// Copy `src` into `dst` at (x, y).
fn blit(dst: &mut RgbaImage, src: &RgbaImage, x: u32, y: u32) {
    for row in 0..src.height.min(dst.height.saturating_sub(y)) {
        let n = (src.width.min(dst.width.saturating_sub(x)) * 4) as usize;
        let s = (row * src.width * 4) as usize;
        let d = (((y + row) * dst.width + x) * 4) as usize;
        dst.pixels[d..d + n].copy_from_slice(&src.pixels[s..s + n]);
    }
}

/// Is a box inside a camera's frustum (conservative: false only when all corners are outside one plane)?
fn in_frustum(m: &glam::Mat4, b: (DVec3, DVec3)) -> bool {
    let mut out = [true; 6];
    for i in 0..8 {
        let p = glam::Vec3::new(
            if i & 1 == 0 { b.0.x } else { b.1.x } as f32,
            if i & 2 == 0 { b.0.y } else { b.1.y } as f32,
            if i & 4 == 0 { b.0.z } else { b.1.z } as f32,
        );
        let c = *m * p.extend(1.0);
        let tests = [c.x < -c.w, c.x > c.w, c.y < -c.w, c.y > c.w, c.z < 0.0, c.z > c.w];
        for (o, t) in out.iter_mut().zip(tests) {
            *o &= t;
        }
    }
    !out.iter().any(|&o| o)
}

/// Render parts with flags into panels, culling what no panel can see when that pays.
fn draw(
    svc: &RenderService,
    lib: &Library,
    b: &Built,
    flags: &[u32],
    panels: &[Panel],
    size: (u32, u32),
    opts: &RenderOptions,
    tris: f64,
) -> Result<(RgbaImage, usize), String> {
    let parts = b.flat.len();
    let mut keep: Vec<bool> = vec![true; parts];
    let mut culled = 0;
    // Culling only pays when it can remove a good share of a big scene.
    if parts > 300 && crate::cost::worth_culling(parts, tris * 0.3, panels.len()) {
        let ms: Vec<glam::Mat4> = panels.iter().map(|p| p.camera.view_proj(p.rect.aspect())).collect();
        for i in 0..parts {
            if flags[i] & render::FLAG_HIDDEN != 0 {
                keep[i] = false;
                continue;
            }
            if let Some(bx) = b.boxes[i] {
                if !ms.iter().any(|m| in_frustum(m, bx)) {
                    keep[i] = false;
                    culled += 1;
                }
            }
        }
    }
    let mut flat = Vec::with_capacity(parts - culled);
    let mut fl = Vec::new();
    for i in 0..parts {
        if keep[i] {
            if flags[i] != 0 {
                fl.push((flat.len() as u32, flags[i]));
            }
            flat.push(b.flat[i].clone());
        }
    }
    let mut img = svc.render_panels_with(lib, &flat, &fl, panels, size, opts)?;
    let outlined: Vec<bool> = {
        let mut o = vec![false; flat.len()];
        for &(i, f) in &fl {
            o[i as usize] = f & OUTLINE != 0;
        }
        o
    };
    if outlined.iter().any(|&o| o) {
        if let Ok(ids) = svc.render_ids(lib, &flat, &fl, panels, size, &opts.scene) {
            let mask: Vec<bool> = ids.ids.iter().map(|&id| id != 0 && outlined.get(id as usize - 1).copied().unwrap_or(false)).collect();
            crate::overlay::Canvas::new(&mut img).outline(&mask, crate::overlay::ORANGE, 2);
        }
    }
    Ok((img, culled))
}

/// Render a shot.
pub fn render(lib: &Library, model: &Model, shot: &Shot, memory: Option<(f32, f32)>) -> Result<Picture, String> {
    if !shot.items.is_empty() {
        return render_items(lib, model, shot, memory);
    }
    let t0 = Instant::now();
    let svc = service()?;
    let b = build(lib, model, shot, &shot.extra);
    let frame = padded(b.focus, b.model_box);
    let scene_radius = ((b.model_box.1 - b.model_box.0).length() * 0.5).max(40.0) as f32;
    // Whole-model pictures fit the part-box corners, not the (loose) model box.
    let cloud = (b.focus.is_none() || shot.intent == crate::planner::Intent::Whole).then(|| box_corners(&b));
    let mut notes = Vec::new();
    let (views, plan_ms) = match &shot.views {
        Views::Angles(a) => (
            a.iter()
                .take(6)
                .map(|&(yaw, pitch)| crate::planner::View { yaw, pitch, kind: crate::planner::Kind::Main, cutaway: vec![], seen: 0.0 })
                .collect::<Vec<_>>(),
            0.0,
        ),
        Views::Auto(n) => {
            let input = crate::planner::Input {
                flat: &b.flat,
                boxes: &b.boxes,
                weight: &b.weight,
                transparent: &b.transparent,
                frame,
                scene_radius,
                intent: shot.intent,
                cap: *n,
                memory,
                budget_ms: crate::cost::TARGET_MS * 0.4,
            };
            let plan = crate::planner::plan(&svc, lib, &input)?;
            if !plan.unseen.is_empty() && shot.intent != crate::planner::Intent::Whole {
                notes.push(format!("{} focus part(s) are enclosed and not visible from any angle", plan.unseen.len()));
            }
            (plan.views, plan.ms)
        }
    };
    let (size0, rects0) = layout(views.len(), shot.size);
    // Quality for what is left of the budget.
    let tris = crate::cost::tris_of(lib, &b.flat);
    let left = (crate::cost::TARGET_MS - plan_ms).max(crate::cost::TARGET_MS * 0.4);
    let (q, _) = crate::cost::choose(tris, views.len(), size0, b.flat.len(), left);
    if q.name != "full" {
        notes.push(format!("rendered at {} quality to stay fast on this model", q.name));
    }
    let size = ((size0.0 as f32 * q.scale) as u32, (size0.1 as f32 * q.scale) as u32);
    let rects: Vec<Rect> = if q.scale == 1.0 {
        rects0
    } else {
        layout(views.len(), ((shot.size.0 as f32 * q.scale) as u32, (shot.size.1 as f32 * q.scale) as u32)).1
    };
    let opts = RenderOptions {
        scene: render::SceneOptions { lod: q.lod, edges: q.edges, ..Default::default() },
        samples: q.samples,
        ..RenderOptions::default()
    };
    let tri_count = if q.lod == Lod::Std { tris.0 } else { tris.1 };
    let cams: Vec<(Camera, Rect)> = views
        .iter()
        .zip(&rects)
        .map(|(v, &rect)| {
            let cam = match &cloud {
                Some(p) => crate::planner::camera_fit(p, rect.aspect(), v.yaw, v.pitch, scene_radius),
                None => crate::planner::camera(frame, rect.aspect(), v.yaw, v.pitch, scene_radius),
            };
            (cam, rect)
        })
        .collect();
    let t_draw = Instant::now();
    // Main views share one pass; each cutaway is its own pass with its occluders see-through.
    let main: Vec<Panel> = cams
        .iter()
        .zip(&views)
        .filter(|(_, v)| v.cutaway.is_empty())
        .map(|((camera, rect), _)| Panel { camera: *camera, rect: *rect })
        .collect();
    let (mut img, culled) = draw(&svc, lib, &b, &b.flags, &main, size, &opts, tri_count)?;
    let mut passes = 1;
    for ((cam, rect), v) in cams.iter().zip(&views) {
        if v.cutaway.is_empty() {
            continue;
        }
        let mut flags = b.flags.clone();
        for &i in &v.cutaway {
            flags[i] = render::FLAG_HIDDEN_GHOST;
        }
        let local = Panel { camera: *cam, rect: Rect::full((rect.w, rect.h)) };
        let (sub, _) = draw(&svc, lib, &b, &flags, &[local], (rect.w, rect.h), &opts, tri_count)?;
        blit(&mut img, &sub, rect.x, rect.y);
        passes += 1;
    }
    // Where is it: a small focus in a big model gets the whole model in the first panel's corner.
    let mut inset: Option<(Camera, Rect)> = None;
    if let (Some(f), Some(first)) = (b.focus, rects.first()) {
        let small = (f.1 - f.0).length() < 0.3 * (b.model_box.1 - b.model_box.0).length();
        if small && b.flat.len() > 12 {
            let r = Rect::new(0, 0, (first.w as f32 / 3.4) as u32, (first.h as f32 / 3.4) as u32);
            let v = &views[0];
            let cam = crate::planner::camera(padded(None, b.model_box), r.aspect(), v.yaw, v.pitch, scene_radius);
            let (sub, _) = draw(&svc, lib, &b, &b.flags, &[Panel { camera: cam, rect: r }], (r.w, r.h), &opts, tri_count)?;
            let at = Rect::new(first.x + first.w - r.w - 6, first.y + first.h - r.h - 6, r.w, r.h);
            blit(&mut img, &sub, at.x, at.y);
            inset = Some((cam, at));
            passes += 1;
        }
    }
    let draw_ms = t_draw.elapsed().as_secs_f64() * 1e3;
    crate::cost::observe(tri_count * if q.edges { 1.35 } else { 1.0 }, views.len(), (size.0 * size.1) as f64, b.flat.len() - culled, draw_ms / passes as f64 * 1.0);
    // Overlay.
    {
        let mut c = Canvas::new(&mut img);
        let many = views.len() > 1;
        for (k, ((cam, rect), v)) in cams.iter().zip(&views).enumerate() {
            let what = match v.kind {
                crate::planner::Kind::Cutaway => format!("  ·  cutaway: {} parts see-through", v.cutaway.len()),
                crate::planner::Kind::Main => String::new(),
            };
            let cap = if many {
                format!("view {}  ·  yaw {:.0}° pitch {:.0}°{what}", k + 1, v.yaw, v.pitch)
            } else {
                format!("{}  ·  yaw {:.0}° pitch {:.0}°{what}", shot.title, v.yaw, v.pitch)
            };
            if !shot.bare {
                c.caption(rect.x as i32, rect.y as i32, &cap);
            }
            for m in &shot.boxes {
                draw_box(&mut c, cam, *rect, m);
            }
            if shot.id_badges && b.badge.len() <= 40 {
                for (at, id) in &b.badge {
                    if let Some((x, y)) = project(cam, *rect, *at) {
                        if inside(*rect, x, y) {
                            c.label(x, y, &format!("#{id}"), 12.0, overlay::WHITE, [60, 64, 72]);
                        }
                    }
                }
            }
            for l in &shot.labels {
                if let Some((x, y)) = project(cam, *rect, l.at) {
                    if inside(*rect, x, y) {
                        c.label(x, y, &l.text, 15.0, overlay::WHITE, l.color);
                    }
                }
            }
            if many && k == 0 && !shot.title.is_empty() {
                c.chip(rect.x as f32 + 4.0, rect.y as f32 + 26.0, &shot.title, 13.0, overlay::WHITE, [40, 44, 52]);
            }
        }
        if let Some((cam, r)) = inset {
            c.rect(r.x as i32 - 1, r.y as i32 - 1, r.w as i32 + 2, r.h as i32 + 2, [90, 96, 108], 2);
            let fb = BoxMark { lo: frame.0, hi: frame.1, color: overlay::ORANGE, text: String::new() };
            draw_box(&mut c, &cam, r, &fb);
            c.chip(r.x as f32 + 3.0, r.y as f32 + 3.0, "where", 11.0, overlay::WHITE, [90, 96, 108]);
        }
        if !shot.legend.is_empty() {
            let h = c.height() as i32;
            c.legend(0, h, &shot.legend);
        }
    }
    let png = render::encode_png_fast(&img);
    Ok(Picture {
        png,
        width: img.width,
        height: img.height,
        caption: shot.title.clone(),
        views: views.iter().map(|v| (v.yaw, v.pitch)).collect(),
        ms: t0.elapsed().as_secs_f64() * 1e3,
        notes,
    })
}

/// One framed panel per item, each planned on its own (placement candidates, problems),
/// optionally after a whole-model overview that marks where each item is.
fn render_items(lib: &Library, model: &Model, shot: &Shot, memory: Option<(f32, f32)>) -> Result<Picture, String> {
    struct Over {
        cam: Camera,
        rect: Rect,
        view: crate::planner::View,
        item: Option<usize>,
        badges: Vec<(DVec3, Id)>,
    }
    let t0 = Instant::now();
    let svc = service()?;
    let cap = if shot.overview { 5 } else { 6 };
    let items = &shot.items[..shot.items.len().min(cap)];
    let (size, rects) = layout(items.len() + shot.overview as usize, shot.size);
    let mut img = RgbaImage { width: size.0, height: size.1, pixels: vec![255; (size.0 * size.1 * 4) as usize] };
    let mut memory = memory;
    let mut views = Vec::new();
    let mut overs: Vec<Over> = Vec::new();
    let fallback = crate::planner::View { yaw: 35.0, pitch: 28.0, kind: crate::planner::Kind::Main, cutaway: vec![], seen: 0.0 };
    let budget = crate::cost::TARGET_MS * 0.3 / rects.len().max(1) as f64;
    let mut rects_left = rects.iter();
    if shot.overview {
        let rect = *rects_left.next().ok_or("no panel")?;
        let mut whole = shot.clone();
        whole.items.clear();
        for it in items {
            for (id, r) in &it.roles {
                whole.roles.insert(*id, *r);
            }
        }
        let mut extra = shot.extra.clone();
        extra.extend(items.iter().flat_map(|i| i.extra.iter().cloned()));
        let b = build(lib, model, &whole, &extra);
        let scene_radius = ((b.model_box.1 - b.model_box.0).length() * 0.5).max(40.0) as f32;
        let input = crate::planner::Input {
            flat: &b.flat,
            boxes: &b.boxes,
            weight: &b.weight,
            transparent: &b.transparent,
            frame: padded(None, b.model_box),
            scene_radius,
            intent: crate::planner::Intent::Error,
            cap: 1,
            memory,
            budget_ms: budget,
        };
        let plan = crate::planner::plan(&svc, lib, &input)?;
        let mut v = plan.views.first().cloned().unwrap_or(fallback.clone());
        v.cutaway.clear();
        memory = Some((v.yaw, v.pitch));
        let local = Rect::full((rect.w, rect.h));
        let cam = crate::planner::camera_fit(&box_corners(&b), local.aspect(), v.yaw, v.pitch, scene_radius);
        let tris = crate::cost::tris_of(lib, &b.flat).0;
        let (sub, _) = draw(&svc, lib, &b, &b.flags, &[Panel { camera: cam, rect: local }], (rect.w, rect.h), &RenderOptions::default(), tris)?;
        blit(&mut img, &sub, rect.x, rect.y);
        views.push((v.yaw, v.pitch));
        overs.push(Over { cam, rect, view: v, item: None, badges: Vec::new() });
    }
    for (k, (item, rect)) in items.iter().zip(rects_left).enumerate() {
        let mut one = shot.clone();
        one.items.clear();
        for (id, r) in &item.roles {
            one.roles.insert(*id, *r);
        }
        let mut extra = shot.extra.clone();
        extra.extend(item.extra.iter().cloned());
        let b = build(lib, model, &one, &extra);
        // Frame only this item's parts.
        let n_model = b.flat.len() - item.extra.len();
        let mine: BTreeMap<Id, Role> = item.roles.iter().copied().collect();
        let mut w = vec![0.0; b.flat.len()];
        let mut own_boxes = Vec::new();
        for i in 0..b.flat.len() {
            let role = b.ids[i].and_then(|id| mine.get(&id).copied());
            if i >= n_model || role.is_some() {
                w[i] = role.map_or(2.0, |r| weight(r).max(1.0));
                if let Some(bx) = b.boxes[i] {
                    own_boxes.push(bx);
                }
            }
        }
        own_boxes.extend(item.boxes.iter().map(|m| (m.lo, m.hi)));
        let item_box = union(own_boxes).unwrap_or((item.at, item.at));
        let frame = padded(Some(item_box), b.model_box);
        let scene_radius = ((b.model_box.1 - b.model_box.0).length() * 0.5).max(40.0) as f32;
        let input = crate::planner::Input {
            flat: &b.flat,
            boxes: &b.boxes,
            weight: &w,
            transparent: &b.transparent,
            frame,
            scene_radius,
            intent: crate::planner::Intent::Item,
            cap: 1,
            memory,
            budget_ms: budget,
        };
        let plan = crate::planner::plan(&svc, lib, &input)?;
        let v = plan.views.first().cloned().unwrap_or(fallback.clone());
        memory = Some((v.yaw, v.pitch));
        let local = Rect::full((rect.w, rect.h));
        let cam = crate::planner::camera(frame, local.aspect(), v.yaw, v.pitch, scene_radius);
        let mut flags = b.flags.clone();
        for &i in &v.cutaway {
            flags[i] = render::FLAG_HIDDEN_GHOST;
        }
        let tris = crate::cost::tris_of(lib, &b.flat).0;
        let (sub, _) = draw(&svc, lib, &b, &flags, &[Panel { camera: cam, rect: local }], (rect.w, rect.h), &RenderOptions::default(), tris)?;
        blit(&mut img, &sub, rect.x, rect.y);
        views.push((v.yaw, v.pitch));
        let badges = if shot.id_badges { b.badge.iter().filter(|(_, id)| mine.contains_key(id)).copied().collect() } else { Vec::new() };
        overs.push(Over { cam, rect: *rect, view: v, item: Some(k), badges });
    }
    {
        let mut c = Canvas::new(&mut img);
        for o in &overs {
            let (rx, ry) = (o.rect.x as f32, o.rect.y as f32);
            let local = Rect::full((o.rect.w, o.rect.h));
            let at = |p: DVec3| project(&o.cam, local, p).filter(|&(x, y)| inside(local, x, y)).map(|(x, y)| (rx + x, ry + y));
            match o.item {
                None => {
                    for it in items {
                        if let Some((x, y)) = at(it.at) {
                            c.label(x, y, &it.label, 15.0, overlay::WHITE, it.color.unwrap_or(overlay::BLUE));
                        }
                    }
                    c.chip(rx + 6.0, ry + 6.0, if shot.title.is_empty() { "overview" } else { &shot.title }, 15.0, overlay::WHITE, [40, 44, 52]);
                }
                Some(k) => {
                    let it = &items[k];
                    for m in &it.boxes {
                        let shifted = Rect::new(o.rect.x, o.rect.y, o.rect.w, o.rect.h);
                        draw_box(&mut c, &o.cam, shifted, m);
                    }
                    for (p, id) in &o.badges {
                        if let Some((x, y)) = at(*p) {
                            c.label(x, y, &format!("#{id}"), 12.0, overlay::WHITE, [60, 64, 72]);
                        }
                    }
                    let colour = it.color.unwrap_or(overlay::BLUE);
                    if let Some((x, y)) = at(it.at) {
                        c.label(x, y, &it.label, 16.0, overlay::WHITE, colour);
                    }
                    c.chip(rx + 6.0, ry + 6.0, &it.label, 22.0, overlay::WHITE, colour);
                    if !it.note.is_empty() {
                        let note = fit(&it.note, o.rect.w.saturating_sub(16), 13.0);
                        c.chip(rx + 6.0, ry + 40.0, &note, 13.0, overlay::WHITE, [40, 44, 52]);
                    }
                }
            }
            let cap = format!("yaw {:.0}° pitch {:.0}°{}", o.view.yaw, o.view.pitch, if o.view.cutaway.is_empty() { "" } else { " · cutaway" });
            c.caption(o.rect.x as i32, (o.rect.y + o.rect.h) as i32 - 22, &cap);
            c.rect(o.rect.x as i32, o.rect.y as i32, o.rect.w as i32, o.rect.h as i32, [200, 204, 210], 1);
        }
    }
    Ok(Picture {
        png: render::encode_png_fast(&img),
        width: img.width,
        height: img.height,
        caption: shot.title.clone(),
        views,
        ms: t0.elapsed().as_secs_f64() * 1e3,
        notes: Vec::new(),
    })
}

/// Project a world point into a panel; `None` behind the camera.
pub fn project(cam: &Camera, rect: Rect, p: DVec3) -> Option<(f32, f32)> {
    let m = cam.view_proj(rect.aspect());
    let c = m * p.as_vec3().extend(1.0);
    if c.w <= 1e-6 {
        return None;
    }
    let ndc = c.xyz() / c.w;
    Some((
        rect.x as f32 + (ndc.x * 0.5 + 0.5) * rect.w as f32,
        rect.y as f32 + (0.5 - ndc.y * 0.5) * rect.h as f32,
    ))
}

fn inside(r: Rect, x: f32, y: f32) -> bool {
    x >= r.x as f32 && y >= r.y as f32 && x < (r.x + r.w) as f32 && y < (r.y + r.h) as f32
}

fn draw_box(c: &mut Canvas, cam: &Camera, rect: Rect, b: &BoxMark) {
    let corner = |i: usize| {
        DVec3::new(
            if i & 1 == 0 { b.lo.x } else { b.hi.x },
            if i & 2 == 0 { b.lo.y } else { b.hi.y },
            if i & 4 == 0 { b.lo.z } else { b.hi.z },
        )
    };
    let pts: Vec<Option<(f32, f32)>> = (0..8).map(|i| project(cam, rect, corner(i))).collect();
    for (a, z) in [(0, 1), (2, 3), (4, 5), (6, 7), (0, 2), (1, 3), (4, 6), (5, 7), (0, 4), (1, 5), (2, 6), (3, 7)] {
        if let (Some(p), Some(q)) = (pts[a], pts[z]) {
            c.line(p.0, p.1, q.0, q.1, b.color, 2.0);
        }
    }
    if !b.text.is_empty() {
        if let Some((x, y)) = project(cam, rect, b.hi) {
            c.label(x + 6.0, y - 6.0, &b.text, 13.0, overlay::WHITE, b.color);
        }
    }
}

/// A grid of part thumbnails with captions (search results, palettes).
pub fn thumbnail_grid(lib: &Library, items: &[(String, i32, String)], cell: u32, cols: u32) -> Result<Picture, String> {
    let t0 = Instant::now();
    let svc = service()?;
    let cols = cols.max(1).min(items.len().max(1) as u32);
    let rows = (items.len() as u32).div_ceil(cols).max(1);
    let cap_h = 34;
    let (w, h) = (cols * cell, rows * (cell + cap_h));
    let mut img = RgbaImage { width: w, height: h, pixels: vec![255; (w * h * 4) as usize] };
    for (i, (name, color, caption)) in items.iter().enumerate() {
        let (cx, cy) = ((i as u32 % cols) * cell, (i as u32 / cols) * (cell + cap_h));
        if let Ok(t) = svc.thumbnail(lib, name, ldraw::Color(*color), cell) {
            for y in 0..t.height.min(cell) {
                let src = (y * t.width * 4) as usize;
                let dst = (((cy + y) * w + cx) * 4) as usize;
                let n = (t.width.min(cell) * 4) as usize;
                img.pixels[dst..dst + n].copy_from_slice(&t.pixels[src..src + n]);
            }
        }
        let mut c = Canvas::new(&mut img);
        c.rect(cx as i32, cy as i32, cell as i32, (cell + cap_h) as i32, [225, 228, 232], 1);
        let mut lines = caption.lines();
        if let Some(l) = lines.next() {
            c.text(cx as f32 + 5.0, (cy + cell) as f32 + 1.0, &fit(l, cell, 13.0), 13.0, overlay::BLACK);
        }
        if let Some(l) = lines.next() {
            c.text(cx as f32 + 5.0, (cy + cell) as f32 + 17.0, &fit(l, cell, 11.0), 11.0, overlay::GREY);
        }
    }
    Ok(Picture {
        png: render::encode_png_fast(&img),
        width: w,
        height: h,
        caption: format!("{} parts", items.len()),
        views: vec![(35.0, 28.0)],
        ms: t0.elapsed().as_secs_f64() * 1e3,
        notes: Vec::new(),
    })
}

fn fit(s: &str, width: u32, px: f32) -> String {
    let mut out = String::new();
    for ch in s.chars() {
        let (w, _) = Canvas::measure(&format!("{out}{ch}…"), px);
        if w > width as f32 - 10.0 {
            out.push('…');
            return out;
        }
        out.push(ch);
    }
    out
}

/// Box of a set of model parts (by id).
pub fn ids_box(lib: &Library, model: &Model, ids: &[Id]) -> Option<(DVec3, DVec3)> {
    union(ids.iter().filter_map(|id| model.parts.get(id)).filter_map(|p| part_box(lib, &p.name, p.pos, p.rot)))
}

pub fn vec3(v: Vec3) -> DVec3 {
    v.as_dvec3()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layouts_fill_the_image() {
        for n in 1..=6 {
            let (size, rects) = layout(n, (960, 720));
            assert_eq!(rects.len(), n);
            for r in &rects {
                assert!(r.x + r.w <= size.0 && r.y + r.h <= size.1, "{n}: {r:?} in {size:?}");
            }
        }
    }
}
