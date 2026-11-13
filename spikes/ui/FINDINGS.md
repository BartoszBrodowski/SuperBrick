# S5 findings: egui_dock shell with two wgpu viewports

Run: `cargo run --release -- /tmp/cad-synth/tortoise7.mpd --bench 240` (Apple M2, Metal, 2940×1782 window, 4× MSAA viewports of 1649×779 each).

| phase | frame interval p50 / p95 / max | frames over 25 ms | `ui()` CPU p50 / p95 |
|---|---|---|---|
| both viewports idle, chat streaming ~60 tok/s | 16.7 / 17.5 / 45.8 ms | 2 of 120 (the first frames after load) | 0.12 / 1.20 ms |
| both viewports orbiting, chat streaming | 16.6 / 18.2 / 19.4 ms | 0 | 2.34 / 2.84 ms |

- Build tab: 11,060 parts (seven tortoises), 4.4M triangles after stud culling, loaded on a background thread in about 150 ms, then 0.5 ms to encode a redraw.
- Subbuild tab: 286 parts (skilltest helmet), 0.2M triangles.
- Both hold vsync (60 fps) while orbiting, and the chat keeps streaming. Viewports redraw only when their camera or contents change, so an idle model costs nothing.

## Settled

- **Stack:** egui 0.36, egui_dock 0.21, eframe 0.36 and wgpu 30 share one device with `crates/render`, so the viewport needs no copy. egui_dock 0.21 needs egui 0.36. Moving to egui 0.37 later means waiting for egui_dock, or switching to egui_tiles 0.18.
- **Viewport pattern:** each viewport renders offscreen (MSAA colour → resolve texture + depth) and registers the resolve texture with `egui_wgpu::Renderer`. When the size changes it is re-registered through `update_egui_texture_from_wgpu_texture`.
- **sRGB trap:** render through an `Rgba8UnormSrgb` view, but give egui a second `Rgba8Unorm` view of the same texture (`view_formats`). Sampling the sRGB view makes the model far too dark, because egui blends gamma-space colours on a non-sRGB surface.
- **Dock:** Parts, Build, AI and Subbuild N tabs, splits and closeable panes; the View menu brings closed panels back.
- **Markdown:** egui_commonmark 0.25 renders streamed markdown (lists, tables, code), re-parsed every frame, with no visible cost at chat lengths.
- **Screenshots:** `ViewportCommand::Screenshot` works, but the app must keep requesting repaints until the `Event::Screenshot` arrives.

## Not covered here

- Picking, gizmos and drag-to-place are M1/M3 work.
- The parts list here is a plain substring filter, so stickers sort first. That is the problem S6 search solves.
- The chat is a mock stream; the real one is the ACP client from S1.
