//! SuperBrick logo and wordmark for the window icon, toolbar and start page.
//! The artwork lives in `app/assets/` (master: `logo.svg`, `wordmark.svg`).

const ICON: &[u8] = include_bytes!("../assets/icon-512.png");
const TOOLBAR: &[u8] = include_bytes!("../assets/toolbar-logo.png");
const WORDMARK: &[u8] = include_bytes!("../assets/wordmark.png");
const WORDMARK_LIGHT: &[u8] = include_bytes!("../assets/wordmark-light-bg.png");

/// The application icon for the window and Dock.
pub fn icon() -> Option<egui::IconData> {
    let img = crate::home::decode(ICON).ok()?;
    let [w, h] = img.size;
    let rgba = img.pixels.iter().flat_map(|c| c.to_srgba_unmultiplied()).collect();
    Some(egui::IconData { rgba, width: w as u32, height: h as u32 })
}

fn texture(ctx: &egui::Context, name: &'static str, bytes: &'static [u8]) -> Option<egui::TextureHandle> {
    let id = egui::Id::new(("superbrick-brand", name));
    if let Some(t) = ctx.data(|d| d.get_temp::<egui::TextureHandle>(id)) {
        return Some(t);
    }
    let img = crate::home::decode(bytes).ok()?;
    let t = ctx.load_texture(name, img, egui::TextureOptions::LINEAR);
    ctx.data_mut(|d| d.insert_temp(id, t.clone()));
    Some(t)
}

fn image(ui: &mut egui::Ui, name: &'static str, bytes: &'static [u8], height: f32, sense: egui::Sense) -> egui::Response {
    match texture(ui.ctx(), name, bytes) {
        Some(t) => {
            let [w, h] = t.size();
            let size = egui::vec2(height * w as f32 / h as f32, height);
            ui.add(egui::Image::new((t.id(), size)).sense(sense))
        }
        None => ui.add(egui::Label::new(egui::RichText::new(ldraw::APP_NAME).strong()).sense(sense)),
    }
}

/// The full "SuperBrick" wordmark (mark plus name), `height` points tall.
pub fn wordmark(ui: &mut egui::Ui, height: f32) -> egui::Response {
    if ui.visuals().dark_mode {
        image(ui, "wordmark", WORDMARK, height, egui::Sense::hover())
    } else {
        image(ui, "wordmark-light", WORDMARK_LIGHT, height, egui::Sense::hover())
    }
}

/// The compact toolbar logo (drawn for a dark toolbar).
/// The toolbar logo; clickable (it takes the window back to the start page).
pub fn toolbar(ui: &mut egui::Ui) -> egui::Response {
    let r = if ui.visuals().dark_mode {
        image(ui, "toolbar", TOOLBAR, 22.0, egui::Sense::click())
    } else {
        image(ui, "wordmark-light", WORDMARK_LIGHT, 22.0, egui::Sense::click())
    };
    r.on_hover_cursor(egui::CursorIcon::PointingHand)
}
