//! Adaptive render cost model.
//!
//! Every tool reply carries pictures, so a render has a budget: under a second typically, under five
//! seconds for any model. This module predicts what a render will cost from the number of triangle
//! draws, pixels and parts, picks the quality tier that fits the budget, and decides whether culling
//! pays for itself (culling costs CPU time per part; it only helps when it removes enough triangles).
//! The rates start from measurements of the warm `RenderService` (np.ldr, 647 parts: 6 ms for one
//! 1024x768 view; a 5000-part model: 19 ms) and are refined from every real render, so a slow GPU
//! degrades quality instead of blowing the budget.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};

use ldraw::{FlatPart, Library, Lod};

/// Typical budget for one tool's pictures (planning plus rendering).
pub const TARGET_MS: f64 = 900.0;
/// Never exceed this, whatever the model.
pub const WORST_MS: f64 = 4500.0;

#[derive(Clone, Copy, Debug)]
pub struct Rates {
    /// Milliseconds per million triangle draws (a triangle drawn once in one view).
    pub per_mtri: f64,
    /// Milliseconds per megapixel of output (fill, resolve, readback, PNG encode).
    pub per_mpx: f64,
    /// Microseconds per part for the CPU scene sync.
    pub sync_us: f64,
    /// Fixed cost per render call.
    pub base: f64,
    /// Microseconds per part for a frustum/sphere cull test.
    pub cull_us: f64,
}

impl Default for Rates {
    fn default() -> Self {
        // From the bench: 245k tris → 4.1 ms render, 2.66M tris → 13.5 ms; PNG ~2 ms per 0.8 MP.
        Rates { per_mtri: 4.2, per_mpx: 3.0, sync_us: 0.6, base: 2.0, cull_us: 0.08 }
    }
}

fn rates() -> &'static Mutex<Rates> {
    static R: OnceLock<Mutex<Rates>> = OnceLock::new();
    R.get_or_init(|| Mutex::new(Rates::default()))
}

pub fn current() -> Rates {
    *rates().lock().unwrap_or_else(|e| e.into_inner())
}

/// Predicted milliseconds for drawing `tris` triangles in `views` views at `px` output pixels.
pub fn predict(tris: f64, views: usize, px: f64, parts: usize) -> f64 {
    let r = current();
    r.base + r.per_mtri * tris * views as f64 / 1e6 + r.per_mpx * px / 1e6 + r.sync_us * parts as f64 / 1e3
}

/// Feed back a measured render so later predictions track this machine.
pub fn observe(tris: f64, views: usize, px: f64, parts: usize, ms: f64) {
    let mut r = rates().lock().unwrap_or_else(|e| e.into_inner());
    let fixed = r.base + r.per_mpx * px / 1e6 + r.sync_us * parts as f64 / 1e3;
    let draws = tris * views as f64 / 1e6;
    if draws < 0.05 || !ms.is_finite() {
        return;
    }
    let measured = ((ms - fixed) / draws).clamp(0.2, 400.0);
    // Exponential moving average; quick to learn a slow machine, slow to forget it.
    let a = if measured > r.per_mtri { 0.5 } else { 0.2 };
    r.per_mtri = r.per_mtri * (1.0 - a) + measured * a;
}

/// Is culling `parts` parts worth it if it removes `culled_tris` triangles from `views` views?
pub fn worth_culling(parts: usize, culled_tris: f64, views: usize) -> bool {
    let r = current();
    let saving = r.per_mtri * culled_tris * views as f64 / 1e6;
    let cost = r.cull_us * parts as f64 / 1e3;
    saving > cost * 1.5 && saving > 0.3
}

/// A render quality tier.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Quality {
    pub name: &'static str,
    pub lod: Lod,
    pub samples: u32,
    pub edges: bool,
    /// Output size factor.
    pub scale: f32,
}

pub const TIERS: [Quality; 5] = [
    Quality { name: "full", lod: Lod::Std, samples: 4, edges: true, scale: 1.0 },
    Quality { name: "no-msaa", lod: Lod::Std, samples: 1, edges: true, scale: 1.0 },
    Quality { name: "low-detail", lod: Lod::Low, samples: 1, edges: true, scale: 1.0 },
    Quality { name: "low-detail, no edges", lod: Lod::Low, samples: 1, edges: false, scale: 0.85 },
    Quality { name: "minimal", lod: Lod::Low, samples: 1, edges: false, scale: 0.65 },
];

/// The best tier whose predicted time fits `budget_ms`; the last one if none does.
/// `tris` is (standard detail, low detail).
pub fn choose(tris: (f64, f64), views: usize, size: (u32, u32), parts: usize, budget_ms: f64) -> (Quality, f64) {
    for q in TIERS {
        let t = if q.lod == Lod::Std { tris.0 } else { tris.1 };
        // Edges roughly add a third to the draw count; MSAA a quarter to the fill.
        let draws = t * if q.edges { 1.35 } else { 1.0 };
        let px = size.0 as f64 * size.1 as f64 * (q.scale as f64).powi(2) * if q.samples > 1 { 1.25 } else { 1.0 };
        let ms = predict(draws, views, px, parts);
        if ms <= budget_ms {
            return (q, ms);
        }
    }
    let q = TIERS[TIERS.len() - 1];
    (q, predict(tris.1, views, size.0 as f64 * size.1 as f64 * 0.42, parts))
}

/// Triangle count of a part at a detail level (cached per name).
pub fn part_tris(lib: &Library, name: &str, lod: Lod) -> f64 {
    static CACHE: OnceLock<Mutex<HashMap<(String, bool), f64>>> = OnceLock::new();
    let key = (name.to_ascii_lowercase(), lod == Lod::Low);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(&n) = cache.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return n;
    }
    let n = lib.mesh(name, lod).map(|m: Arc<ldraw::Mesh>| m.tris.len() as f64).unwrap_or(200.0);
    cache.lock().unwrap_or_else(|e| e.into_inner()).insert(key, n);
    n
}

/// (standard, low) triangle totals of a flattened model.
pub fn tris_of(lib: &Library, flat: &[FlatPart]) -> (f64, f64) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for f in flat {
        *counts.entry(f.name.as_str()).or_default() += 1;
    }
    let mut std = 0.0;
    let mut low = 0.0;
    for (name, n) in counts {
        std += part_tris(lib, name, Lod::Std) * n as f64;
        low += part_tris(lib, name, Lod::Low) * n as f64;
    }
    (std, low)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tiers_degrade_with_load() {
        let (q, _) = choose((250_000.0, 120_000.0), 1, (960, 720), 650, TARGET_MS);
        assert_eq!(q.name, "full");
        let (q, ms) = choose((4.0e9, 2.0e9), 4, (1280, 960), 400_000, TARGET_MS);
        assert_ne!(q.name, "full");
        assert!(ms > TARGET_MS);
    }

    #[test]
    fn culling_needs_a_real_saving() {
        assert!(!worth_culling(100, 1000.0, 1));
        assert!(worth_culling(5000, 2.0e6, 2));
    }
}
