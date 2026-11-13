//! wgpu renderer for flattened LDraw scenes: instanced part bodies, instanced studs with covered-stud
//! culling, edge and conditional lines, and offscreen frames (thumbnails, agent looks, benchmarks).

pub mod atlas;
pub mod gpu;
mod ground;
pub mod scene;
pub mod service;

pub use gpu::{
    COLOR_FORMAT, DEPTH_FORMAT, FLAG_DELETE, FLAG_GHOST, FLAG_HIDDEN, FLAG_SELECTED, FLAG_STALE,
    Gpu, Offscreen, OrbitTimes, PICK_FORMAT, Picker, Renderer, bench_orbit, save_png,
};
pub use gpu::{FLAG_ERROR, FLAG_FADE, FLAG_HIDDEN_GHOST, FLAG_PICK_HIDDEN};
pub use atlas::{SceneBuilder, SceneFrame};
pub use gpu::{GLOBALS_STRIDE, Pipelines};
pub use scene::{Camera, Scene, SceneOptions, SceneStats, StudMode};
pub use service::{
    IdImage, Panel, Rect, RenderOptions, RenderService, RenderTimings, RgbaImage, encode_png_fast,
};
