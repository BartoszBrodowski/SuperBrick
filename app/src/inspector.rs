//! Selection inspector: what is selected, where it is, and what can be done with it.
//!
//! Layout: a summary (swatch, name, colour, size), a transform grid with one column per
//! axis, edit actions, a subbuild section that only offers what will succeed for the
//! current selection, and a collapsed, grouped list of every part in the open model.

use crate::app::{State, Tool};
use crate::panels::{colour_grid, paint_swatch};
use eframe::egui::{self, Align, Layout, RichText, Sense, Vec2};
use engine::{Cmd, Part};
use glam::{DMat3, DVec3};
use std::collections::{BTreeMap, BTreeSet};

/// Width of the left label column ("Position", "Move", …).
const LABEL_W: f32 = 64.0;
/// Width of the right column (units, step size, angle, trailing buttons).
const TRAIL_W: f32 = 84.0;
const ROW_H: f32 = 22.0;
const AXES: [(&str, egui::Color32); 3] = [
    ("X", egui::Color32::from_rgb(232, 96, 96)),
    ("Y", egui::Color32::from_rgb(112, 196, 96)),
    ("Z", egui::Color32::from_rgb(96, 148, 240)),
];
/// Move step choices: (menu text, short text, across X/Z, along Y), in LDU.
const MOVE_STEPS: [(&str, &str, f64, f64); 4] = [
    ("1 LDU", "1 LDU", 1.0, 1.0),
    ("½ stud · ½ plate", "½ stud", 10.0, 4.0),
    ("1 stud · 1 plate", "1 plate", 20.0, 8.0),
    ("1 stud · 1 brick", "1 brick", 20.0, 24.0),
];
const MIRROR_HELP: &str = "Verify every part's reflected geometry and counterpart. Unsupported/printed parts refuse the whole operation.";

pub fn show(st: &mut State, ui: &mut egui::Ui) {
    let snapshot = st.snapshot.clone();
    let ids = st.ids();
    let parts: Vec<&Part> = ids.iter().filter_map(|id| snapshot.model.parts.get(id)).collect();
    if parts.is_empty() {
        empty(st, ui);
    } else {
        summary(st, ui, &parts);
        section(ui, "Transform");
        transform(st, ui, &parts);
        section(ui, "Edit");
        edit(st, ui, &parts);
        section(ui, "Subbuild");
        subbuild(st, ui, &parts);
    }
    ui.add_space(6.0);
    ui.separator();
    parts_list(st, ui, &ids);
}

fn empty(st: &mut State, ui: &mut egui::Ui) {
    ui.add_space(2.0);
    ui.label(RichText::new("Nothing selected").size(15.0).strong());
    ui.label(RichText::new("Click a part in the view, or open Parts in this model below.").weak());
    if pending_copy(st) {
        section(ui, "Transform");
        mirror_row(st, ui, Cols::of(ui), true);
    }
}

// ---------------------------------------------------------------- summary

fn summary(st: &State, ui: &mut egui::Ui, parts: &[&Part]) {
    let single = parts.len() == 1;
    let (title, subtitle, colours, hover) = if single {
        let p = parts[0];
        let (title, what) = match subbuild_file(st, p) {
            Some(file) => (
                stem(&p.name).to_string(),
                format!("Subbuild · {}", plural(piece_count(st, ui, file), "piece")),
            ),
            None => (describe(st, &p.name), part_number(&p.name).to_string()),
        };
        let colour = colour_name(st, p.color, subbuild_file(st, p).is_some());
        let hover = format!("Part #{} · {} · colour {}", p.id, p.name, p.color);
        (title, format!("{what} · {colour}"), vec![p.color], hover)
    } else {
        let kinds: BTreeSet<&str> = parts.iter().map(|p| p.name.as_str()).collect();
        let mut colours: Vec<i32> = Vec::new();
        for p in parts {
            if !colours.contains(&p.color) {
                colours.push(p.color);
            }
        }
        let shown: Vec<String> = parts.iter().take(12).map(|p| format!("#{}", p.id)).collect();
        let more = if parts.len() > 12 { " …" } else { "" };
        (
            format!("{} parts", parts.len()),
            format!("{} · {}", plural(kinds.len(), "kind"), plural(colours.len(), "colour")),
            colours,
            format!("{}{more}", shown.join(", ")),
        )
    };
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let (rect, _) = ui.allocate_exact_size(Vec2::splat(34.0), Sense::hover());
        swatches(st, ui, rect, &colours);
        ui.vertical(|ui| {
            ui.spacing_mut().item_spacing.y = 1.0;
            ui.horizontal(|ui| {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    if single {
                        ui.label(RichText::new(format!("#{}", parts[0].id)).small().weak())
                            .on_hover_text(&hover);
                    }
                    ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                        ui.add(egui::Label::new(RichText::new(&title).size(15.0).strong()).truncate())
                            .on_hover_text(&hover);
                    });
                });
            });
            ui.add(egui::Label::new(RichText::new(&subtitle).weak()).truncate());
        });
    });
    ui.add_space(4.0);
    if let Some(size) = bounds(st, ui, parts) {
        row(ui, "Size", |ui| {
            ui.label(size_text(size)).on_hover_text(format!(
                "Bounding box, studs excluded: {:.0} × {:.0} × {:.0} LDU (X × Z × height)",
                size.x, size.z, size.y
            ));
        });
    }
    let lo = parts.iter().map(|p| p.step).min().unwrap_or(1);
    let hi = parts.iter().map(|p| p.step).max().unwrap_or(1);
    row(ui, "Step", |ui| {
        ui.label(if lo == hi { lo.to_string() } else { format!("{lo}–{hi}") })
            .on_hover_text("Construction step the selection is built in");
    });
}

/// One swatch, or up to four colours split into halves/quadrants.
fn swatches(st: &State, ui: &egui::Ui, rect: egui::Rect, colours: &[i32]) {
    let cells: Vec<egui::Rect> = match colours.len() {
        0 => return,
        1 => vec![rect],
        2 => {
            let (a, b) = rect.split_left_right_at_fraction(0.5);
            vec![a, b]
        }
        _ => {
            let (top, bottom) = rect.split_top_bottom_at_fraction(0.5);
            let (a, b) = top.split_left_right_at_fraction(0.5);
            let (c, d) = bottom.split_left_right_at_fraction(0.5);
            vec![a, b, c, d]
        }
    };
    let gap = if cells.len() > 1 { 1.0 } else { 0.0 };
    for (code, cell) in colours.iter().zip(cells) {
        let rgba = st.lib.colors.display_rgba(ldraw::Color(*code));
        paint_swatch(ui, cell.shrink(gap), [rgba[0], rgba[1], rgba[2]], rgba[3], false);
    }
}

// ---------------------------------------------------------------- transform

fn transform(st: &mut State, ui: &mut egui::Ui, parts: &[&Part]) {
    let ids: Vec<u64> = parts.iter().map(|p| p.id).collect();
    let locked = parts.iter().any(|p| p.locked);
    let anchor = parts[0];
    let cols = Cols::of(ui);
    if locked {
        ui.label(RichText::new("🔒 Locked — untick Locked below to move, rotate or mirror.").weak());
    }
    axis_header(ui, cols);
    ui.add_enabled_ui(!locked, |ui| {
        position_row(st, ui, anchor, &ids, cols, parts.len() > 1);
        move_row(st, ui, &ids, cols);
        rotate_row(st, ui, &ids, anchor.pos, cols);
        if cols.trail.is_none() {
            amount_row(st, ui, cols);
        }
    });
    mirror_row(st, ui, cols, !locked);
}

/// "LDU | X | Y | Z": the corner names the unit of every number below it.
fn axis_header(ui: &mut egui::Ui, cols: Cols) {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(LABEL_W, 14.0), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(egui::vec2(LABEL_W, 14.0));
            ui.label(RichText::new("LDU").small().weak()).on_hover_text(
                "Positions and moves are in LDraw units: 1 stud = 20, 1 plate = 8, 1 brick = 24",
            );
        });
        for (name, colour) in AXES {
            ui.add_sized([cols.cell, 14.0], egui::Label::new(RichText::new(name).strong().color(colour)))
                .on_hover_text(match name {
                    "Y" => "Vertical. Y points down in LDraw: smaller Y is higher.",
                    _ => "Horizontal",
                });
        }
    });
}

/// Editable absolute position of the first selected part; a commit moves the whole
/// selection by the difference. Dragging previews in the field and applies on release.
fn position_row(st: &mut State, ui: &mut egui::Ui, anchor: &Part, ids: &[u64], cols: Cols, many: bool) {
    let editing_key = egui::Id::new(("inspector-position-editing", anchor.id));
    if !ui.data(|d| d.get_temp::<bool>(editing_key)).unwrap_or(false) {
        st.inspect_delta = anchor.pos.to_array();
    }
    let mut editing = false;
    let mut commit = false;
    row(ui, "Position", |ui| {
        for axis in 0..3 {
            let r = ui
                .add_sized(
                    [cols.cell, ROW_H],
                    egui::DragValue::new(&mut st.inspect_delta[axis])
                        .speed(1.0)
                        .max_decimals(3)
                        .update_while_editing(false),
                )
                .on_hover_text("Type a value or drag; applied on Enter or release");
            editing |= r.has_focus() || r.dragged();
            commit |= r.drag_stopped() || (r.changed() && !r.dragged());
        }
    })
    .on_hover_text(if many {
        format!("Origin of #{}, the first selected part. Changing it moves the whole selection.", anchor.id)
    } else {
        "Part origin (for bricks: centre of the top face)".to_string()
    });
    ui.data_mut(|d| d.insert_temp(editing_key, editing));
    if commit {
        let delta = DVec3::from_array(st.inspect_delta) - anchor.pos;
        if delta.length() > 1e-9 {
            st.command(Cmd::Move {
                ids: ids.to_vec(),
                delta,
                free: false,
            });
        }
    }
}

const MOVE_STEP_KEY: &str = "inspector-move-step";

fn move_step(ui: &egui::Ui) -> usize {
    ui.data_mut(|d| *d.get_persisted_mut_or(egui::Id::new(MOVE_STEP_KEY), 2usize))
        .min(MOVE_STEPS.len() - 1)
}

/// Move-step picker; `width` includes the drop-down arrow.
fn move_step_combo(ui: &mut egui::Ui, width: f32) {
    let mut step = move_step(ui);
    let (_, short, across, up) = MOVE_STEPS[step];
    egui::ComboBox::from_id_salt(MOVE_STEP_KEY)
        .width(width)
        .selected_text(short)
        .show_ui(ui, |ui| {
            for (i, (text, ..)) in MOVE_STEPS.iter().enumerate() {
                ui.selectable_value(&mut step, i, *text);
            }
        })
        .response
        .on_hover_text(format!("Move step: {across} LDU across (X, Z), {up} LDU up or down (Y)"));
    ui.data_mut(|d| d.insert_persisted(egui::Id::new(MOVE_STEP_KEY), step));
}

fn angle_field(st: &mut State, ui: &mut egui::Ui, width: f32) {
    ui.add_sized(
        [width, ROW_H],
        egui::DragValue::new(&mut st.angle).range(1.0..=180.0).speed(1.0).suffix("°"),
    )
    .on_hover_text("Rotation step. Turns are about the first selected part's origin.");
}

fn move_row(st: &mut State, ui: &mut egui::Ui, ids: &[u64], cols: Cols) {
    let (_, _, across, up) = MOVE_STEPS[move_step(ui)];
    let mut delta = DVec3::ZERO;
    row(ui, "Move", |ui| {
        for axis in 0..3 {
            let name = AXES[axis].0;
            let (minus, plus, tips) = if axis == 1 {
                ("⏶", "⏷", [format!("Up {up} LDU"), format!("Down {up} LDU")])
            } else {
                ("−", "+", [format!("−{across} LDU along {name}"), format!("+{across} LDU along {name}")])
            };
            let (a, b) = button_pair(ui, cols.cell, minus, plus);
            let amount = if axis == 1 { up } else { across };
            if a.on_hover_text(&tips[0]).clicked() {
                delta[axis] -= amount;
            }
            if b.on_hover_text(&tips[1]).clicked() {
                delta[axis] += amount;
            }
        }
        if let Some(w) = cols.trail {
            move_step_combo(ui, w);
        }
    });
    if delta != DVec3::ZERO {
        st.command(Cmd::Move {
            ids: ids.to_vec(),
            delta,
            free: false,
        });
    }
}

fn rotate_row(st: &mut State, ui: &mut egui::Ui, ids: &[u64], pivot: DVec3, cols: Cols) {
    let mut turn = None;
    row(ui, "Rotate", |ui| {
        for axis in 0..3 {
            let name = AXES[axis].0;
            let (a, b) = button_pair(ui, cols.cell, "↺", "↻");
            if a.on_hover_text(format!("Rotate −{}° about {name}", st.angle)).clicked() {
                turn = Some((axis, -st.angle));
            }
            if b.on_hover_text(format!("Rotate +{}° about {name}", st.angle)).clicked() {
                turn = Some((axis, st.angle));
            }
        }
        if let Some(w) = cols.trail {
            angle_field(st, ui, w);
        }
    });
    if let Some((axis, angle)) = turn {
        st.command(Cmd::Rotate {
            ids: ids.to_vec(),
            axis: DMat3::IDENTITY.col(axis),
            angle,
            pivot,
            free: false,
        });
    }
}

/// Narrow panels: the move step and rotation step get their own row under Move/Rotate.
fn amount_row(st: &mut State, ui: &mut egui::Ui, cols: Cols) {
    row(ui, "Step size", |ui| {
        move_step_combo(ui, 2.0 * cols.cell + cols.gap);
        angle_field(st, ui, cols.cell);
    });
}

fn mirror_row(st: &mut State, ui: &mut egui::Ui, cols: Cols, enabled: bool) {
    let pending = pending_copy(st);
    let label = if pending { "Mirror copy" } else { "Mirror" };
    let tip = if pending {
        "Mirror the copy you are placing"
    } else {
        "Mirror the selection in place"
    };
    let mut axis_clicked = None;
    row(ui, label, |ui| {
        ui.add_enabled_ui(enabled && st.mirror_job.is_none(), |ui| {
            for (axis, (name, _)) in AXES.iter().enumerate() {
                if ui
                    .add_sized([cols.cell, ROW_H], egui::Button::new(*name))
                    .on_hover_text(format!("{tip} across {name}. {MIRROR_HELP}"))
                    .clicked()
                {
                    axis_clicked = Some(axis);
                }
            }
        });
        if st.mirror_job.is_some() {
            ui.spinner();
        }
    });
    if let Some(axis) = axis_clicked {
        st.mirror_selection(axis);
    }
}

// ---------------------------------------------------------------- edit

fn edit(st: &mut State, ui: &mut egui::Ui, parts: &[&Part]) {
    let ids: Vec<u64> = parts.iter().map(|p| p.id).collect();
    let locked = parts.iter().any(|p| p.locked);
    let gap = ui.spacing().item_spacing.x;
    let w = ((ui.available_width() - 2.0 * gap) / 3.0).floor();
    ui.horizontal(|ui| {
        let paint = ui
            .add_enabled(!locked, egui::Button::new("🎨 Paint…").min_size([w, ROW_H].into()))
            .on_hover_text("Choose a colour for the selection")
            .on_disabled_hover_text("Unlock to paint");
        let uniform = parts.iter().all(|p| p.color == parts[0].color).then_some(parts[0].color);
        egui::Popup::menu(&paint).show(|ui| {
            ui.label(RichText::new("Paint selection").strong());
            if let Some(color) = colour_grid(st, ui, "inspector-paint-swatches", uniform) {
                st.command(Cmd::Paint { ids: ids.clone(), color });
                ui.close();
            }
        });
        if ui
            .add(egui::Button::new("🗐 Copy").min_size([w, ROW_H].into()))
            .on_hover_text("Copy the selection and place a duplicate · Ctrl/Cmd+C")
            .clicked()
        {
            st.copy_to_place();
        }
        if ui
            .add_enabled(!locked, egui::Button::new("🗑 Delete").min_size([w, ROW_H].into()))
            .on_hover_text("Delete the selection · Delete/Backspace")
            .on_disabled_hover_text("Unlock to delete")
            .clicked()
        {
            st.command(Cmd::Remove { ids: ids.clone() });
        }
    });
    ui.add_space(2.0);
    ui.horizontal(|ui| {
        let n = parts.len();
        let hidden = parts.iter().filter(|p| p.hidden).count();
        let mut all_hidden = hidden == n;
        if ui
            .add(egui::Checkbox::new(&mut all_hidden, "Hidden").indeterminate(hidden > 0 && hidden < n))
            .on_hover_text("Hide in the view; hidden parts stay in the model and the exports")
            .changed()
        {
            st.command(Cmd::Hide { ids: ids.clone(), value: all_hidden });
        }
        ui.add_space(12.0);
        let locked_n = parts.iter().filter(|p| p.locked).count();
        let mut all_locked = locked_n == n;
        if ui
            .add(egui::Checkbox::new(&mut all_locked, "Locked").indeterminate(locked_n > 0 && locked_n < n))
            .on_hover_text("Locked parts can be selected but not moved, painted or deleted")
            .changed()
        {
            st.command(Cmd::Lock { ids: ids.clone(), value: all_locked });
        }
    });
}

// ---------------------------------------------------------------- subbuild

fn subbuild(st: &mut State, ui: &mut egui::Ui, parts: &[&Part]) {
    let instance = match parts {
        [p] => subbuild_file(st, p).map(|file| (*p, file)),
        _ => None,
    };
    match instance {
        Some((_, file)) => instance_section(st, ui, file),
        None => make_section(st, ui, parts),
    }
}

/// A selected subbuild instance: rename it, open it for editing.
fn instance_section(st: &mut State, ui: &mut egui::Ui, file: usize) {
    let current = st.snapshot.model.doc.files[file].name.clone();
    let key = egui::Id::new(("inspector-rename", file, current.as_str()));
    let mut name = ui.data_mut(|d| d.get_temp::<String>(key)).unwrap_or_else(|| stem(&current).to_string());
    let problem = rename_problem(st, file, &current, &name);
    let cols = Cols::of(ui);
    // The field spans the axis columns and the button sits in the right column, so
    // they line up with the transform grid above at any panel width.
    // A narrow panel's cell can be thinner than the button's text; the field gives way.
    let (total, column) = match cols.trail {
        Some(trail) => (cols.span() + cols.gap + trail, trail),
        None => (cols.span(), cols.cell),
    };
    let text = egui::WidgetText::from("Rename")
        .into_galley(ui, Some(egui::TextWrapMode::Extend), f32::INFINITY, egui::TextStyle::Button)
        .size()
        .x;
    let button_w = column.max(text + 2.0 * ui.spacing().button_padding.x);
    let field_w = total - cols.gap - button_w;
    let mut rename = false;
    row(ui, "Name", |ui| {
        let r = ui.add_sized(
            [field_w, ROW_H],
            egui::TextEdit::singleline(&mut name).hint_text("Subbuild name"),
        );
        let enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
        let button = ui
            .add_enabled(problem.is_none(), egui::Button::new("Rename").min_size([button_w, ROW_H].into()))
            .on_hover_text(format!("Rename {current} and every copy of it"));
        let button = match problem {
            Some(Some(why)) => button.on_disabled_hover_text(why),
            _ => button,
        };
        rename = button.clicked() || (enter && problem.is_none());
    });
    if let Some(Some(why)) = problem {
        row(ui, "", |ui| {
            ui.label(RichText::new(why).small().weak());
        });
    }
    if rename {
        st.command(Cmd::RenameSubbuild {
            file,
            name: name.trim().to_string(),
        });
        ui.data_mut(|d| d.remove::<String>(key));
    } else {
        ui.data_mut(|d| d.insert_temp(key, name));
    }
    let copies = instance_count(st, ui, file);
    row(ui, "", |ui| {
        if ui
            .add(egui::Button::new("✏ Edit subbuild").min_size([field_w, ROW_H].into()))
            .on_hover_text(format!("Open {current} in its own view. Changes apply to every copy."))
            .clicked()
        {
            st.views[st.active].open_submodel = Some(file);
        }
        if copies > 1 {
            ui.label(RichText::new(plural(copies, "copy")).weak())
                .on_hover_text(format!("{current} is used {copies} times; editing it changes all of them"));
        }
    });
}

/// `None` = ready; `Some(None)` = nothing to do (unchanged); `Some(Some(why))` = refused.
fn rename_problem(st: &State, file: usize, current: &str, name: &str) -> Option<Option<&'static str>> {
    let name = name.trim();
    if let Some(why) = name_problem(name) {
        return Some(Some(why));
    }
    let full = with_ldr(name);
    if full == current {
        return Some(None);
    }
    if st.snapshot.model.doc.file_index(&full).is_some_and(|other| other != file) {
        return Some(Some("Another subbuild already has this name"));
    }
    None
}

/// Plain parts (or several instances): group them into a new subbuild.
fn make_section(st: &mut State, ui: &mut egui::Ui, parts: &[&Part]) {
    let doc = &st.snapshot.model.doc;
    if is_default_name(&st.sub_name) && doc.file_index(&with_ldr(st.sub_name.trim())).is_some() {
        st.sub_name = (2..)
            .map(|n| format!("subbuild-{n}"))
            .find(|n| doc.file_index(&with_ldr(n)).is_none())
            .unwrap_or_default();
    }
    let problem = if parts.iter().any(|p| p.file != parts[0].file) {
        Some("The selection spans several models; select parts from one model")
    } else if parts.iter().any(|p| p.step != parts[0].step) {
        Some("The selection spans several build steps; a subbuild must come from one step")
    } else if let Some(why) = name_problem(st.sub_name.trim()) {
        Some(why)
    } else if doc.file_index(&with_ldr(st.sub_name.trim())).is_some() {
        Some("A subbuild with this name already exists")
    } else {
        None
    };
    let width = ui.available_width() - LABEL_W - ui.spacing().item_spacing.x;
    let mut enter = false;
    row(ui, "Name", |ui| {
        let r = ui.add_sized(
            [width, ROW_H],
            egui::TextEdit::singleline(&mut st.sub_name).hint_text("New subbuild name"),
        );
        enter = r.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
    });
    let mut make = false;
    row(ui, "", |ui| {
        let button = ui
            .add_enabled(problem.is_none(), egui::Button::new("Make subbuild").min_size([width, ROW_H].into()))
            .on_hover_text(
                "Move the selected parts into a new subbuild (a reusable submodel) and put one copy of it in their place",
            );
        let button = match problem {
            Some(why) => button.on_disabled_hover_text(why),
            None => button,
        };
        make = button.clicked() || (enter && problem.is_none());
    });
    if let Some(why) = problem {
        row(ui, "", |ui| {
            ui.add(egui::Label::new(RichText::new(why).small().weak()).wrap());
        });
    }
    if make {
        let ids: Vec<u64> = parts.iter().map(|p| p.id).collect();
        let name = st.sub_name.trim().to_string();
        match st.handle.command(Cmd::MakeSubbuild { ids, name }) {
            Ok(v) => {
                if let Some(file) = v["file"].as_u64() {
                    st.views[st.active].open_submodel = Some(file as usize);
                }
                st.message = format!("Made subbuild {}", v["name"].as_str().unwrap_or_default());
            }
            Err(e) => st.message = e,
        }
    }
}

fn name_problem(name: &str) -> Option<&'static str> {
    let name = name.trim();
    if name.is_empty() {
        Some("Enter a name")
    } else if name.contains(['/', '\\', '\n', '\r']) || name == "." || name == ".." {
        Some("Names can't contain / or \\")
    } else {
        None
    }
}

fn is_default_name(name: &str) -> bool {
    let name = name.trim();
    name == "subbuild"
        || name
            .strip_prefix("subbuild-")
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
}

// ---------------------------------------------------------------- parts list

fn parts_list(st: &mut State, ui: &mut egui::Ui, selected: &[u64]) {
    let snapshot = st.snapshot.clone();
    let file = st.views[st.active].file;
    let mut groups: BTreeMap<(String, i32), Vec<&Part>> = BTreeMap::new();
    for p in snapshot.model.parts.values().filter(|p| p.file == file) {
        groups.entry((p.name.to_lowercase(), p.color)).or_default().push(p);
    }
    let total: usize = groups.values().map(Vec::len).sum();
    let title = format!("Parts in this model · {total}");
    let mut select = None;
    egui::CollapsingHeader::new(RichText::new(title).strong())
        .id_salt("inspector-parts")
        .default_open(false)
        .show(ui, |ui| {
            if groups.is_empty() {
                ui.label(RichText::new("No parts yet.").weak());
                return;
            }
            let mut rows: Vec<(String, i32, Vec<&Part>)> = groups
                .into_values()
                .map(|ps| (describe(st, &ps[0].name), ps[0].color, ps))
                .collect();
            rows.sort_by(|a, b| {
                a.0.to_lowercase()
                    .cmp(&b.0.to_lowercase())
                    .then_with(|| b.2.len().cmp(&a.2.len()))
                    .then(a.1.cmp(&b.1))
            });
            ui.label(
                RichText::new(format!(
                    "{} · click to select, Ctrl/Cmd-click to add",
                    plural(rows.len(), "group")
                ))
                .small()
                .weak(),
            );
            egui::ScrollArea::vertical()
                .id_salt("inspector-parts-scroll")
                .max_height(300.0)
                .auto_shrink([false, true])
                .show(ui, |ui| {
                    ui.spacing_mut().item_spacing.y = 0.0;
                    for (desc, color, ps) in &rows {
                        if let Some(ids) = part_row(st, ui, desc, *color, ps, selected) {
                            select = Some(ids);
                        }
                    }
                });
        });
    if let Some(ids) = select {
        st.command(Cmd::Select { ids });
    }
}

/// One group row; returns the new selection when clicked.
fn part_row(st: &State, ui: &mut egui::Ui, desc: &str, color: i32, ps: &[&Part], selected: &[u64]) -> Option<Vec<u64>> {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(ui.available_width(), ROW_H), Sense::click());
    let all = ps.iter().all(|p| selected.contains(&p.id));
    let any = ps.iter().any(|p| selected.contains(&p.id));
    let visuals = ui.visuals();
    if all {
        ui.painter().rect_filled(rect, 3.0, visuals.selection.bg_fill);
    } else if response.hovered() {
        ui.painter().rect_filled(rect, 3.0, visuals.widgets.hovered.weak_bg_fill);
    }
    let text_color = if all {
        visuals.selection.stroke.color
    } else {
        visuals.text_color()
    };
    let weak = if all { text_color } else { visuals.weak_text_color() };
    let swatch = egui::Rect::from_center_size(rect.left_center() + egui::vec2(12.0, 0.0), Vec2::splat(13.0));
    let rgba = st.lib.colors.display_rgba(ldraw::Color(color));
    paint_swatch(ui, swatch, [rgba[0], rgba[1], rgba[2]], rgba[3], false);
    let inner = egui::Rect::from_min_max(rect.min + egui::vec2(26.0, 0.0), rect.max - egui::vec2(6.0, 0.0));
    let mut child = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(inner)
            .layout(Layout::right_to_left(Align::Center)),
    );
    child.label(RichText::new(format!("×{}", ps.len())).strong().color(text_color));
    let hidden = ps.iter().filter(|p| p.hidden).count();
    let locked = ps.iter().filter(|p| p.locked).count();
    if locked > 0 {
        child.label(RichText::new("🔒").small().color(weak));
    }
    if hidden > 0 {
        child.label(RichText::new("hidden").small().color(weak));
    }
    if any && !all {
        child.label(RichText::new("partly selected").small().color(weak));
    }
    child.with_layout(Layout::left_to_right(Align::Center), |ui| {
        ui.add(egui::Label::new(RichText::new(desc).color(text_color)).truncate().selectable(false));
    });
    let colour = colour_name(st, color, subbuild_name(st, &ps[0].name));
    let ids: Vec<String> = ps.iter().take(8).map(|p| format!("#{}", p.id)).collect();
    let more = if ps.len() > 8 { " …" } else { "" };
    let response = response.on_hover_text(format!(
        "{} · {colour}\n{}{more}{}{}",
        part_number(&ps[0].name),
        ids.join(", "),
        if hidden > 0 { format!("\n{hidden} hidden") } else { String::new() },
        if locked > 0 { format!("\n{locked} locked") } else { String::new() },
    ));
    if !response.clicked() {
        return None;
    }
    let group: Vec<u64> = ps.iter().map(|p| p.id).collect();
    if ui.input(|i| i.modifiers.command || i.modifiers.shift) {
        let mut ids: BTreeSet<u64> = selected.iter().copied().collect();
        if all {
            for id in &group {
                ids.remove(id);
            }
        } else {
            ids.extend(&group);
        }
        Some(ids.into_iter().collect())
    } else {
        Some(group)
    }
}

// ---------------------------------------------------------------- layout helpers

fn section(ui: &mut egui::Ui, title: &str) {
    ui.add_space(6.0);
    ui.separator();
    ui.label(RichText::new(title).strong());
    ui.add_space(1.0);
}

/// A row with a fixed-width, left-aligned label column.
fn row(ui: &mut egui::Ui, label: &str, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    ui.horizontal(|ui| {
        ui.allocate_ui_with_layout(egui::vec2(LABEL_W, ROW_H), Layout::left_to_right(Align::Center), |ui| {
            ui.set_min_size(egui::vec2(LABEL_W, ROW_H));
            ui.label(RichText::new(label).weak());
        });
        add(ui);
    })
    .response
}

/// Column widths: a label column, three axis columns and, on wide panels, a right column
/// for the move step and angle (narrow panels put those on a row of their own instead).
#[derive(Clone, Copy)]
struct Cols {
    cell: f32,
    trail: Option<f32>,
    gap: f32,
}

impl Cols {
    fn of(ui: &egui::Ui) -> Self {
        let gap = ui.spacing().item_spacing.x;
        let avail = ui.available_width();
        let wide = ((avail - LABEL_W - TRAIL_W - 4.0 * gap) / 3.0).floor();
        if wide >= 56.0 {
            Cols { cell: wide, trail: Some(TRAIL_W), gap }
        } else {
            Cols {
                cell: ((avail - LABEL_W - 3.0 * gap) / 3.0).floor().max(36.0),
                trail: None,
                gap,
            }
        }
    }

    /// Width spanning the three axis columns.
    fn span(&self) -> f32 {
        3.0 * self.cell + 2.0 * self.gap
    }
}

/// Two equal buttons filling one axis column.
fn button_pair(ui: &mut egui::Ui, cell: f32, a: &str, b: &str) -> (egui::Response, egui::Response) {
    ui.allocate_ui_with_layout(egui::vec2(cell, ROW_H), Layout::left_to_right(Align::Center), |ui| {
        ui.spacing_mut().item_spacing.x = 2.0;
        let w = ((cell - 2.0) / 2.0).floor();
        let a = ui.add_sized([w, ROW_H], egui::Button::new(a));
        let b = ui.add_sized([cell - 2.0 - w, ROW_H], egui::Button::new(b));
        (a, b)
    })
    .inner
}

// ---------------------------------------------------------------- model facts

fn pending_copy(st: &State) -> bool {
    st.tool == Tool::Place
        && st
            .assembly_clipboard
            .first()
            .is_some_and(|p| st.place.as_deref() == Some(p.name.as_str()))
}

/// The model file a part line refers to, when the part is a subbuild instance.
fn subbuild_file(st: &State, p: &Part) -> Option<usize> {
    let doc = &st.snapshot.model.doc;
    doc.file_index(&p.name).filter(|f| Some(*f) != doc.main_index())
}

fn subbuild_name(st: &State, name: &str) -> bool {
    st.snapshot.model.doc.file_index(name).is_some()
}

/// Human name: the library description ("Brick 2 x 4"), or the subbuild's name.
fn describe(st: &State, name: &str) -> String {
    if subbuild_name(st, name) {
        return format!("{} (subbuild)", stem(name));
    }
    let raw = st.lib.description(name);
    let tidy = raw.trim_start_matches(['~', '=', '_', '|']).split_whitespace().collect::<Vec<_>>().join(" ");
    if tidy.is_empty() { name.to_string() } else { tidy }
}

fn part_number(name: &str) -> &str {
    let lower = name.to_ascii_lowercase();
    if lower.ends_with(".dat") || lower.ends_with(".ldr") {
        &name[..name.len() - 4]
    } else {
        name
    }
}

fn stem(name: &str) -> &str {
    if name.to_ascii_lowercase().ends_with(".ldr") { &name[..name.len() - 4] } else { name }
}

fn with_ldr(name: &str) -> String {
    if name.to_ascii_lowercase().ends_with(".ldr") { name.to_string() } else { format!("{name}.ldr") }
}

fn colour_name(st: &State, code: i32, instance: bool) -> String {
    if code == 16 {
        return if instance { "Own colours" } else { "Main colour" }.into();
    }
    let name = st.lib.colors.name(code);
    let mut chars = name.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => format!("Colour {code}"),
    }
}

fn plural(n: usize, word: &str) -> String {
    match (n, word) {
        (1, _) => format!("1 {word}"),
        (_, "copy") => format!("{n} copies"),
        _ => format!("{n} {word}s"),
    }
}

/// Memoised per model revision, so per-frame drawing stays cheap.
fn cached<T: Clone + Send + Sync + 'static>(st: &State, ui: &egui::Ui, key: impl std::hash::Hash + std::fmt::Debug, make: impl FnOnce() -> T) -> T {
    let id = egui::Id::new(("inspector-cache", st.snapshot.revision, key));
    if let Some(v) = ui.data(|d| d.get_temp::<T>(id)) {
        return v;
    }
    let v = make();
    ui.data_mut(|d| d.insert_temp(id, v.clone()));
    v
}

fn piece_count(st: &State, ui: &egui::Ui, file: usize) -> usize {
    cached(st, ui, ("pieces", file), || st.snapshot.model.flat(file).len())
}

fn instance_count(st: &State, ui: &egui::Ui, file: usize) -> usize {
    cached(st, ui, ("instances", file), || {
        let doc = &st.snapshot.model.doc;
        st.snapshot.model.parts.values().filter(|p| doc.file_index(&p.name) == Some(file)).count()
    })
}

/// Size of the selection's bounding box in LDU (studs excluded), or `None` for huge selections.
fn bounds(st: &State, ui: &egui::Ui, parts: &[&Part]) -> Option<DVec3> {
    if parts.len() > 2000 {
        return None;
    }
    let ids: Vec<u64> = parts.iter().map(|p| p.id).collect();
    cached(st, ui, ("bounds", ids), || {
        let mut lo = DVec3::splat(f64::INFINITY);
        let mut hi = DVec3::splat(f64::NEG_INFINITY);
        let mut add = |name: &str, pos: DVec3, rot: DMat3| {
            let Ok(mesh) = st.lib.mesh(name, ldraw::Lod::Low) else { return };
            let (a, b) = mesh.bbox(true);
            let (a, b) = (a.as_dvec3(), b.as_dvec3());
            if !(a.is_finite() && b.is_finite()) {
                return;
            }
            for corner in 0..8 {
                let c = DVec3::new(
                    if corner & 1 == 0 { a.x } else { b.x },
                    if corner & 2 == 0 { a.y } else { b.y },
                    if corner & 4 == 0 { a.z } else { b.z },
                );
                let w = pos + rot * c;
                lo = lo.min(w);
                hi = hi.max(w);
            }
        };
        for p in parts {
            match subbuild_file(st, p) {
                Some(file) => {
                    for fp in st.snapshot.model.flat(file) {
                        add(&fp.name, p.pos + p.rot * fp.pos, p.rot * fp.rot);
                    }
                }
                None => add(&p.name, p.pos, p.rot),
            }
        }
        (lo.x <= hi.x).then(|| hi - lo)
    })
}

/// "2 × 4 studs · 1 brick tall"; height in bricks or plates when whole, else LDU.
fn size_text(size: DVec3) -> String {
    let num = |v: f64| {
        let r = (v * 10.0).round() / 10.0;
        if (r - r.round()).abs() < 1e-9 { format!("{}", r.round() as i64) } else { format!("{r:.1}") }
    };
    let whole = |v: f64| (v - v.round()).abs() < 0.05 && v.round() > 0.0;
    let h = size.y;
    let height = if whole(h / 24.0) {
        let n = (h / 24.0).round() as usize;
        plural(n, "brick")
    } else if whole(h / 8.0) {
        let n = (h / 8.0).round() as usize;
        plural(n, "plate")
    } else if whole(h / 4.0) {
        format!("{} plates", num(h / 8.0))
    } else {
        format!("{} LDU", num(h))
    };
    format!("{} × {} studs · {height} tall", num(size.x / 20.0), num(size.z / 20.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sizes_read_in_studs_and_bricks_or_plates() {
        assert_eq!(size_text(DVec3::new(40.0, 24.0, 80.0)), "2 × 4 studs · 1 brick tall");
        assert_eq!(size_text(DVec3::new(20.0, 8.0, 20.0)), "1 × 1 studs · 1 plate tall");
        assert_eq!(size_text(DVec3::new(20.0, 16.0, 60.0)), "1 × 3 studs · 2 plates tall");
        assert_eq!(size_text(DVec3::new(30.0, 13.0, 20.0)), "1.5 × 1 studs · 13 LDU tall");
        assert_eq!(size_text(DVec3::new(40.0, 68.0, 40.0)), "2 × 2 studs · 8.5 plates tall");
    }

    #[test]
    fn default_subbuild_names_are_recognised_but_typed_ones_are_kept() {
        assert!(is_default_name("subbuild"));
        assert!(is_default_name("subbuild-12"));
        assert!(!is_default_name("subbuild-"));
        assert!(!is_default_name("wing"));
        assert_eq!(with_ldr("Ear"), "Ear.ldr");
        assert_eq!(with_ldr("ear.LDR"), "ear.LDR");
        assert_eq!(stem("Ear.ldr"), "Ear");
        assert_eq!(part_number("3001.dat"), "3001");
        assert_eq!(name_problem(" "), Some("Enter a name"));
        assert_eq!(name_problem("a/b"), Some("Names can't contain / or \\"));
    }
}
