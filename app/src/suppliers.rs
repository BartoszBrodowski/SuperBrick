//! Explicit export-and-quote handoffs; no hidden uploads, scraping or purchases.
use crate::app::State;
use engine::inventory::Inventory;
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::mpsc,
};

#[derive(Default)]
pub struct Panel {
    requested: Option<u64>,
    loaded: Option<u64>,
    rx: Option<mpsc::Receiver<(u64, Inventory)>>,
    inventory: Inventory,
    query: String,
    names: HashMap<String, usize>,
    allow_unresolved: bool,
    status: String,
    last_export: Option<PathBuf>,
}
#[derive(Clone, Copy)]
struct Supplier {
    name: &'static str,
    url: &'static str,
    info: &'static str,
    csv: bool,
    /// LEGO Pick a Brick `elementId,quantity` format (needs element-ID mapping).
    pab: bool,
}
const SUPPLIERS: [Supplier; 5] = [
    Supplier {
        name: "BrickLink",
        url: "https://www.bricklink.com/v2/wanted/upload.page",
        csv: false,
        pab: false,
        info: "Upload the exported .bsx (BrickLink part and colour IDs) to a Wanted List, then use Buy All to compare sellers. Login required. No API credentials are stored by CAD.",
    },
    Supplier {
        name: "BuyMeBricks",
        url: "https://buymebricks.com/upload",
        csv: false,
        pab: false,
        info: "Upload the exported .ldr for the supplier's current prices and availability. Review unavailable parts separately; the available subtotal is not necessarily the full-build total.",
    },
    Supplier {
        name: "Wobrick",
        url: "https://wobrick.com/toolkit/",
        csv: true,
        pab: false,
        info: "Upload the exported CSV as Rebrickable format. This tool's documented fields use LDraw part/colour IDs. Review matching, missing items and shipping on the site.",
    },
    Supplier {
        name: "JoyGoBricks",
        url: "https://joygobricks.com/pages/faqs",
        csv: true,
        pab: false,
        info: "Request a quote using the site's contact instructions and attach the exported CSV. Their FAQ describes a parts-list quote with an unavailable-parts report, usually within 24 hours—not an instant pricing API.",
    },
    Supplier {
        name: "LEGO Pick a Brick",
        url: "https://www.lego.com/pick-and-build/pick-a-brick",
        csv: true,
        pab: true,
        info: "Saves an elementId,quantity CSV (≤200 lines per file, extra files are numbered). Use the page's bulk 'upload a list' option, then review the bag. Parts with no known LEGO element ID are listed and left out; Pick a Brick only stocks a subset of elements.",
    },
];

pub fn ui(st: &mut State, ui: &mut egui::Ui) {
    let mut panel = std::mem::take(&mut st.suppliers);
    panel.draw(st, ui);
    st.suppliers = panel;
}
impl Panel {
    fn draw(&mut self, st: &mut State, ui: &mut egui::Ui) {
        let revision = st.snapshot.revision;
        if self.requested != Some(revision) {
            let snapshot = st.snapshot.clone();
            let lib = st.lib.clone();
            let ctx = ui.ctx().clone();
            let (tx, rx) = mpsc::channel();
            std::thread::spawn(move || {
                let mut inventory = Inventory::build(&snapshot.model);
                for lot in &inventory.lots {
                    if lot.part.ends_with(".dat") && !lib.exists(&lot.part) {
                        inventory.warnings.push(format!("{} is absent from the installed LDraw library; verify its identity with the supplier.",lot.part));
                    }
                }
                let _ = tx.send((snapshot.revision, inventory));
                ctx.request_repaint();
            });
            self.requested = Some(revision);
            self.rx = Some(rx);
        }
        if let Some((rev, inventory)) = self.rx.as_ref().and_then(|rx| rx.try_recv().ok()) {
            if rev == revision {
                self.inventory = inventory;
                self.loaded = Some(rev);
                self.allow_unresolved = false;
            }
            self.rx = None;
        }
        if self.names.is_empty() {
            if let Some(search) = &st.search {
                self.names = search
                    .cat
                    .parts
                    .iter()
                    .enumerate()
                    .map(|(i, p)| (format!("{}.dat", p.id), i))
                    .collect();
            }
        }
        ui.heading("Build parts & suppliers");
        ui.small("Whole main build · all subbuild instances · hidden pieces included · pending agent drafts excluded");
        let ready = self.loaded == Some(revision);
        if !ready {
            ui.spinner();
            ui.label("Updating inventory…");
        }
        ui.label(format!(
            "{} pieces · {} part/colour lots · {} distinct parts",
            self.inventory.pieces,
            self.inventory.lots.len(),
            self.inventory.unique_parts()
        ));
        if !self.inventory.warnings.is_empty() {
            ui.collapsing(
                format!(
                    "⚠ {} unresolved inventory issues",
                    self.inventory.warnings.len()
                ),
                |ui| {
                    for warning in &self.inventory.warnings {
                        ui.colored_label(egui::Color32::YELLOW, warning);
                    }
                },
            );
            ui.checkbox(
                &mut self.allow_unresolved,
                "Export unresolved items for manual review",
            );
        }
        egui::CollapsingHeader::new("Suppliers — check the full list").default_open(true).show(ui,|ui|{
            ui.small("Buttons save an inventory file and open the supplier. You upload/send it yourself. Nothing is purchased or transmitted automatically.");
            for supplier in SUPPLIERS {
                let label=if supplier.pab {"Export element CSV & open Pick a Brick"}else if supplier.name=="JoyGoBricks" {"Export CSV & request quote"}else if supplier.csv {"Export CSV & check prices"}else {"Export LDraw & check prices"};
                if ui.add_enabled(ready && self.inventory.pieces>0 && (self.inventory.warnings.is_empty()||self.allow_unresolved),egui::Button::new(format!("{} · {label}",supplier.name))).on_hover_text(supplier.info).clicked() {
                    self.export(st,Some(supplier),ui);
                }
            }
            ui.separator();ui.small("No live totals are claimed inside CAD yet: supplier APIs need supported access/authentication. BrickLink price guides are not a guaranteed purchasable basket. Compare missing quantities, shipping, taxes, currency and LEGO vs compatible brands before ordering.");
        });
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    ready && self.inventory.pieces > 0,
                    egui::Button::new("Export parts CSV…"),
                )
                .clicked()
            {
                self.export(st, None, ui);
            }
            if ui
                .add_enabled(ready, egui::Button::new("Copy parts CSV"))
                .clicked()
            {
                ui.ctx().copy_text(self.inventory.upload_csv());
                self.status = "Copied inventory CSV (LDraw colour IDs).".into();
            }
        });
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        if let Some(path) = &self.last_export {
            ui.small(path.display().to_string());
        }
        ui.separator();
        ui.add(
            egui::TextEdit::singleline(&mut self.query)
                .hint_text("Filter names, part IDs or colours…")
                .desired_width(f32::INFINITY),
        );
        let query = self.query.to_lowercase();
        let rows = self
            .inventory
            .lots
            .iter()
            .filter(|lot| {
                let desc = self
                    .names
                    .get(&lot.part)
                    .and_then(|i| st.search.as_ref()?.cat.parts.get(*i))
                    .map(|p| p.clean_desc())
                    .unwrap_or(&lot.part);
                let color = st
                    .lib
                    .colors
                    .by_code
                    .get(&lot.color)
                    .map(|c| c.name.as_str())
                    .unwrap_or("Unresolved");
                format!("{} {} {} {}", lot.part, desc, color, lot.color)
                    .to_lowercase()
                    .contains(&query)
            })
            .collect::<Vec<_>>();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_rows(ui, 48., rows.len(), |ui, range| {
                for n in range {
                    let lot = rows[n];
                    ui.horizontal(|ui| {
                        ui.label(format!("{} ×", lot.quantity));
                        let desc = self
                            .names
                            .get(&lot.part)
                            .and_then(|i| st.search.as_ref()?.cat.parts.get(*i))
                            .map(|p| p.clean_desc())
                            .unwrap_or(&lot.part);
                        ui.vertical(|ui| {
                            ui.add(egui::Label::new(desc).truncate());
                            ui.horizontal(|ui| {
                                if let Some(c) = st.lib.colors.by_code.get(&lot.color) {
                                    let (rect, _) = ui.allocate_exact_size(
                                        egui::vec2(12., 12.),
                                        egui::Sense::hover(),
                                    );
                                    ui.painter().rect_filled(
                                        rect,
                                        2.,
                                        egui::Color32::from_rgb(c.rgb[0], c.rgb[1], c.rgb[2]),
                                    );
                                    ui.small(&c.name);
                                }
                                ui.small(format!(
                                    "{} · LDraw colour {}",
                                    lot.part.trim_end_matches(".dat"),
                                    lot.color
                                ));
                            });
                        });
                    });
                }
            });
    }
    fn export(&mut self, st: &State, supplier: Option<Supplier>, ui: &egui::Ui) {
        let csv = supplier.is_none_or(|s| s.csv);
        let bricklink = supplier.is_some_and(|s| s.name == "BrickLink");
        let ext = if csv { "csv" } else if bricklink { "bsx" } else { "ldr" };
        let filename = format!(
            "build-parts-{}.{}",
            supplier
                .map(|s| s.name.to_lowercase())
                .unwrap_or("inventory".into()),
            ext
        );
        let mut dialog = rfd::FileDialog::new();
        // Start next to the project, not wherever the last save dialog happened to be.
        if let Some(dir) = st.snapshot.model.path.as_deref().and_then(Path::parent) {
            dialog = dialog.set_directory(dir);
        }
        let Some(path) = dialog
            .set_file_name(filename)
            .add_filter("Parts inventory", &[ext])
            .save_file()
        else {
            return;
        };
        if self.loaded != Some(st.handle.snapshot().revision) {
            self.status="Build changed while choosing the export file. Inventory will refresh; please export again.".into();
            return;
        }
        if supplier.is_some_and(|s| s.pab) {
            self.export_pab(&path, st, supplier.unwrap(), ui);
            return;
        }
        let mut skipped = Vec::new();
        let data = if csv {
            self.inventory.upload_csv()
        } else if bricklink {
            // BrickLink rejects the whole upload on one item it doesn't know, and its own .ldr
            // importer turns some current LDraw numbers into retired ones (3062 → 3062b), so
            // write BrickStore XML with BrickLink's own part and colour IDs.
            let (xml, s) = self
                .inventory
                .bricklink_bsx(|p| st.lib.bricklink_number(p), ldraw::bricklink::colour);
            skipped = s;
            xml
        } else {
            self.inventory.upload_ldr_as(|p| st.lib.canonical_part(p))
        };
        match write_export(&path, st.snapshot.model.path.as_deref(), data.as_bytes()) {
            Ok(()) => {
                self.last_export = Some(path.clone());
                self.status="Saved complete inventory. Review supplier mapping and missing items before ordering.".into();
                if let Some(s) = supplier {
                    ui.ctx().open_url(egui::OpenUrl::new_tab(s.url));
                    self.status = format!(
                        "Saved inventory for {} to {}. Upload/send the saved file using the page just opened; no list was sent automatically.",
                        s.name,
                        path.display()
                    );
                }
                if !skipped.is_empty() {
                    self.status += &format!(
                        " Left out (colour has no BrickLink ID): {}.",
                        skipped.join(", ")
                    );
                }
            }
            Err(e) => self.status = format!("Export failed: {e}"),
        }
    }
}
impl Panel {
    fn export_pab(&mut self, path: &Path, st: &State, s: Supplier, ui: &egui::Ui) {
        let Some(sets) = search::sets::Sets::get() else {
            self.status = "Element-ID table (sets.bin) is not installed; cannot build a Pick a Brick list.".into();
            return;
        };
        let pab = self.inventory.pick_a_brick(sets);
        if pab.files.is_empty() {
            self.status = format!("No LEGO element IDs found for this build ({} lots unmapped).", pab.missing.len());
            return;
        }
        let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("build-parts");
        let mut saved = Vec::new();
        for (i, data) in pab.files.iter().enumerate() {
            let target = if i == 0 { path.to_owned() } else { path.with_file_name(format!("{stem}-{}.csv", i + 1)) };
            if let Err(e) = write_export(&target, st.snapshot.model.path.as_deref(), data.as_bytes()) {
                self.status = format!("Export failed: {e}");
                return;
            }
            saved.push(target);
        }
        self.last_export = saved.first().cloned();
        ui.ctx().open_url(egui::OpenUrl::new_tab(s.url));
        self.status = format!(
            "Saved {} Pick a Brick file(s); upload each on the page just opened. {} lot(s) have no element ID and are NOT in the list{}",
            saved.len(),
            pab.missing.len(),
            if pab.missing.is_empty() { ".".into() } else { format!(": {}", pab.missing.iter().take(5).cloned().collect::<Vec<_>>().join("; ")) }
        );
    }
}
fn write_export(path: &Path, source: Option<&Path>, bytes: &[u8]) -> Result<(), String> {
    if source.is_some_and(|s| same_path(path, s)) {
        return Err("Choose a different filename: the build itself must not be overwritten by a purchasing inventory.".into());
    }
    use std::io::Write;
    let parent = path.parent().unwrap_or(Path::new("."));
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let temp = parent.join(format!(".cad-inventory-{}-{nonce}.tmp", std::process::id()));
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temp)
        .map_err(|e| e.to_string())?;
    let result = (|| {
        f.write_all(bytes)?;
        f.sync_all()?;
        std::fs::rename(&temp, path)
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temp);
    }
    result.map_err(|e: std::io::Error| e.to_string())
}
fn same_path(a: &Path, b: &Path) -> bool {
    let absolute = |p: &Path| {
        p.canonicalize().unwrap_or_else(|_| {
            if p.is_absolute() {
                p.to_owned()
            } else {
                std::env::current_dir().unwrap_or_default().join(p)
            }
        })
    };
    absolute(a) == absolute(b)
}
#[cfg(test)]
mod tests {
    #[test]
    fn export_refuses_to_replace_the_source_model() {
        let dir = std::env::temp_dir().join(format!("cad-supplier-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("model.ldr");
        std::fs::write(&path, "original").unwrap();
        assert!(super::write_export(&path, Some(&path), b"inventory").is_err());
        assert_eq!(std::fs::read(&path).unwrap(), b"original");
        let output = dir.join("inventory.csv");
        super::write_export(&output, Some(&path), b"inventory").unwrap();
        assert_eq!(std::fs::read(&output).unwrap(), b"inventory");
        std::fs::remove_file(path).unwrap();
        std::fs::remove_file(output).unwrap();
        std::fs::remove_dir(dir).unwrap();
    }
}
