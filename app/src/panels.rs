use crate::app::{State, Tool};
use eframe::egui;
use engine::Cmd;
use glam::DVec3;
use serde_json::json;

pub fn parts(st: &mut State, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        let width = (ui.available_width() - 52.0 - ui.spacing().item_spacing.x).max(40.0);
        ui.add_sized(
            [width, 28.0],
            egui::TextEdit::singleline(&mut st.query).hint_text("Search parts…"),
        );
        colour_picker(st, ui);
    });
    if st.all_colors {
        st.filters.color = None;
    } else if st.filters.color.is_some() {
        st.filters.color = Some(st.color);
    }
    let recent_id = ui.id().with("show-recent");
    let used_id = ui.id().with("show-used");
    let mut show_recent = ui
        .data_mut(|d| d.get_persisted::<bool>(recent_id))
        .unwrap_or(true);
    let mut show_used = ui
        .data_mut(|d| d.get_persisted::<bool>(used_id))
        .unwrap_or(true);
    ui.horizontal(|ui| {
        ui.checkbox(&mut st.list_mode, "List view");
        ui.menu_button("Show", |ui| {
            ui.checkbox(&mut show_recent, "Recent pieces");
            ui.checkbox(&mut show_used, "Used pieces");
        });
    });
    ui.data_mut(|d| {
        d.insert_persisted(recent_id, show_recent);
        d.insert_persisted(used_id, show_used);
    });
    egui::CollapsingHeader::new("Colour & feature filters")
        .default_open(true)
        .show(ui, |ui| {
            ui.horizontal(|ui| {
                let mut filter = st.filters.color.is_some();
                if ui
                    .add_enabled(
                        !st.all_colors,
                        egui::Checkbox::new(&mut filter, "Made in colour"),
                    )
                    .changed()
                {
                    st.filters.color = filter.then_some(st.color);
                }
                if filter {
                    st.filters.color = Some(st.color);
                }
            });
            ui.horizontal_wrapped(|ui| {
                for tag in [
                    "SNOT", "hinge", "clip", "bar", "technic", "tile", "jumper", "print",
                ] {
                    let selected = st.filters.tags.iter().any(|t| t == tag);
                    if ui.selectable_label(selected, tag).clicked() {
                        if selected {
                            st.filters.tags.retain(|t| t != tag);
                        } else {
                            st.filters.tags.push(tag.into());
                        }
                    }
                }
            });
            ui.horizontal(|ui| {
                ui.checkbox(&mut st.filters.unknown_colors, "Include unknown colours");
            });
        });
    if show_recent && !st.recent.is_empty() {
        egui::CollapsingHeader::new("Recent pieces")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Recent:");
                    for id in &st.recent {
                        if ui
                            .small_button(part_name(st, id))
                            .on_hover_text(id)
                            .clicked()
                        {
                            st.query = id.clone();
                        }
                    }
                });
            });
    }
    if st.used_revision != st.snapshot.revision {
        let mut used = std::collections::BTreeMap::<String, usize>::new();
        for p in st.snapshot.model.parts.values() {
            if p.name.ends_with(".dat") {
                *used
                    .entry(p.name.trim_end_matches(".dat").to_string())
                    .or_default() += 1;
            }
        }
        st.used = used.into_iter().collect();
        st.used.sort_by_key(|(_, n)| std::cmp::Reverse(*n));
        st.used_revision = st.snapshot.revision;
    }
    if show_used && !st.used.is_empty() {
        egui::CollapsingHeader::new("Used pieces")
            .default_open(false)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("Used:");
                    for (id, n) in st.used.iter().take(8) {
                        if ui
                            .small_button(format!("{} ×{n}", part_name(st, id)))
                            .on_hover_text(id)
                            .clicked()
                        {
                            st.query = id.clone();
                        }
                    }
                });
            });
    }
    st.refresh_search();
    let Some(search) = st.search.clone() else {
        ui.spinner();
        ui.label("Loading local search…");
        return;
    };
    ui.label(format!(
        "{} matches · drag to build, or click for repeat placement",
        st.hits.len()
    ));
    let mut chosen = None;
    let mut similar_request = None;
    if !search.exact_matches(&st.query).is_empty() {
        ui.small("Exact part ID · shown first, regardless of filters");
    }
    let expanded = st.similar_to.as_deref().and_then(|id| {
        st.hits
            .iter()
            .position(|h| search.cat.parts[h.idx].id == id)
    });
    if st.list_mode {
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show_viewport(ui, |ui, viewport| {
                let heights = expanded_rows(
                    st.hits.len(),
                    30.0 + ui.spacing().item_spacing.y,
                    expanded,
                    similar_height(ui, st.similar_hits.len()),
                );
                let range = visible_rows(ui, viewport, &heights);
                for i in range {
                    let start = ui.cursor().min.y;
                    let p = &search.cat.parts[st.hits[i].idx];
                    let color = result_colour(p, st.all_colors, st.filters.color, st.color);
                    ui.horizontal(|ui| {
                        let response = ui
                            .add_sized(
                                [
                                    (ui.available_width() - 28.0 - ui.spacing().item_spacing.x)
                                        .max(1.0),
                                    30.0,
                                ],
                                egui::Button::new(egui::RichText::new(p.clean_desc()))
                                    .truncate()
                                    .sense(egui::Sense::click_and_drag()),
                            )
                            .on_hover_text(format!("{}\nColours: {:?}", p.clean_desc(), p.colors));
                        response.dnd_set_drag_payload(crate::view::LibraryDrag {
                            name: format!("{}.dat", p.id),
                            color,
                        });
                        if response.clicked() {
                            chosen = Some((p.id.clone(), p.clean_desc().to_string(), color));
                        }
                        similar_menu(ui, &p.id, &mut similar_request);
                    });
                    if expanded == Some(i) {
                        similar_inline(st, ui);
                    }
                    ui.add_space(
                        (heights[i + 1] - heights[i] - (ui.cursor().min.y - start)).max(0.0),
                    );
                }
            });
    } else {
        let width = (ui.available_width() - ui.spacing().scroll.allocated_width()).max(1.0);
        let (columns, cell) = thumbnail_layout(width, ui.spacing().item_spacing.x);
        let image_size = (cell
            - 2.0
                * ui.spacing()
                    .button_padding
                    .x
                    .max(ui.spacing().button_padding.y))
        .max(1.0);
        let row_height =
            cell + ui.spacing().item_spacing.y + 2.0 * ui.text_style_height(&egui::TextStyle::Body);
        let rows = st.hits.len().div_ceil(columns);
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .scroll_bar_visibility(egui::scroll_area::ScrollBarVisibility::AlwaysVisible)
            .show_viewport(ui, |ui, viewport| {
                let expanded_row = expanded.map(|i| i / columns);
                let heights = expanded_rows(
                    rows,
                    row_height + ui.spacing().item_spacing.y,
                    expanded_row,
                    similar_height(ui, st.similar_hits.len()),
                );
                let range = visible_rows(ui, viewport, &heights);
                for row in range {
                    let start = ui.cursor().min.y;
                    ui.horizontal(|ui| {
                        for h in st.hits.iter().skip(row * columns).take(columns) {
                            let p = &search.cat.parts[h.idx];
                            let color = result_colour(p, st.all_colors, st.filters.color, st.color);
                            let key = format!("{}:{}", p.id, color);
                            ui.vertical(|ui| {
                                ui.set_width(cell);
                                let response = if let Some(tex) = st.thumbs.get(&key) {
                                    ui.add_sized(
                                        [cell, cell],
                                        egui::Button::image(egui::Image::new((
                                            tex.id(),
                                            egui::vec2(image_size, image_size),
                                        )))
                                        .sense(egui::Sense::click_and_drag()),
                                    )
                                } else {
                                    if st.thumb_pending.insert(key.clone()) {
                                        let _ =
                                            st.thumb_tx.send((key, format!("{}.dat", p.id), color));
                                    }
                                    ui.add_sized(
                                        [cell, cell],
                                        egui::Button::new(p.clean_desc())
                                            .sense(egui::Sense::click_and_drag()),
                                    )
                                };
                                response.dnd_set_drag_payload(crate::view::LibraryDrag {
                                    name: format!("{}.dat", p.id),
                                    color,
                                });
                                if response.clicked() {
                                    chosen =
                                        Some((p.id.clone(), p.clean_desc().to_string(), color));
                                }
                                response.on_hover_text(format!(
                                    "{}\n{}\nColours: {:?}",
                                    p.id,
                                    p.clean_desc(),
                                    p.colors
                                ));
                                ui.horizontal(|ui| {
                                    ui.add_sized(
                                        [
                                            (cell - 28.0 - ui.spacing().item_spacing.x).max(1.0),
                                            2.0 * ui.text_style_height(&egui::TextStyle::Body),
                                        ],
                                        egui::Label::new(p.clean_desc()).truncate(),
                                    )
                                    .on_hover_text(format!(
                                        "{} · {}",
                                        p.id,
                                        p.clean_desc()
                                    ));
                                    similar_menu(ui, &p.id, &mut similar_request);
                                });
                            });
                        }
                    });
                    if expanded_row == Some(row) {
                        similar_inline(st, ui);
                    }
                    ui.add_space(
                        (heights[row + 1] - heights[row] - (ui.cursor().min.y - start)).max(0.0),
                    );
                }
            });
    }
    if let Some(id) = similar_request {
        if st.similar_to.as_ref() == Some(&id) {
            st.similar_to = None;
        } else {
            st.similar_hits = search.similar(&id, 8);
            st.similar_to = Some(id);
            ui.data_mut(|d| d.remove::<String>(egui::Id::new("similar-browse")));
        }
    }
    if let Some(piece) = chosen {
        choose_part(st, piece);
    }
}

fn choose_part(st: &mut State, (id, desc, color): (String, String, i32)) {
    st.color = color;
    st.place = Some(format!("{id}.dat"));
    st.place_rot = glam::DMat3::IDENTITY;
    st.place_assembly.clear();
    st.tool = Tool::Place;
    st.message = format!("Place {desc} · Esc cancels · Alt allows unverified free placement");
    st.recent.retain(|p| p != &id);
    st.recent.insert(0, id);
    st.recent.truncate(8);
}

fn result_colour(
    p: &search::catalog::Part,
    all_colors: bool,
    filter: Option<i32>,
    color: i32,
) -> i32 {
    let selected = !all_colors && (filter.is_none() || p.colors.contains(&color));
    part_colour(p, selected.then_some(color))
}

fn similar_menu(ui: &mut egui::Ui, id: &str, request: &mut Option<String>) {
    ui.push_id(("similar", id), |ui| {
        ui.menu_button("▾", |ui| {
            ui.label(format!("Part {id}"));
            if ui.button("Show similar").clicked() {
                *request = Some(id.into());
                ui.close();
            }
        })
        .response
        .on_hover_text("More · show similar pieces");
    });
}

fn similar_inline(st: &mut State, ui: &mut egui::Ui) {
    let Some(anchor) = st.similar_to.clone() else {
        return;
    };
    let id = ui
        .data_mut(|d| d.get_temp::<String>(egui::Id::new("similar-browse")))
        .unwrap_or(anchor);
    let description = part_name(st, &id);
    let Some(search) = st.search.clone() else {
        return;
    };
    let mut open = true;
    let mut chosen = None;
    let mut request = None;
    let width = ui.available_width();
    let response = egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(format!("Similar · {description}"));
            if ui.small_button("×").clicked() {
                open = false;
            }
        });
        ui.small("Semantic neighbours across all colours · drag or click to place");
        let columns = ((ui.available_width() + 8.) / 118.).floor().clamp(1., 4.) as usize;
        let cell = ((ui.available_width() - (columns - 1) as f32 * 8.) / columns as f32)
            .min(110.)
            .max(1.);
        egui::Grid::new("similar-grid")
            .num_columns(columns)
            .spacing([8., 8.])
            .show(ui, |ui| {
                for (n, h) in st.similar_hits.iter().enumerate() {
                    let p = &search.cat.parts[h.idx];
                    // Similar browsing intentionally crosses colour restrictions.
                    let color = part_colour(
                        p,
                        (!st.all_colors && p.colors.contains(&st.color)).then_some(st.color),
                    );
                    let key = format!("{}:{color}", p.id);
                    ui.vertical(|ui| {
                        ui.set_width(cell);
                        let button = if let Some(tex) = st.thumbs.get(&key) {
                            egui::Button::image(egui::Image::new((
                                tex.id(),
                                egui::vec2((cell - 14.).max(1.), (cell - 14.).max(1.)),
                            )))
                        } else {
                            if st.thumb_pending.insert(key.clone()) {
                                let _ = st.thumb_tx.send((key, format!("{}.dat", p.id), color));
                            }
                            egui::Button::new(p.clean_desc())
                        };
                        let response =
                            ui.add_sized([cell, cell], button.sense(egui::Sense::click_and_drag()));
                        response.dnd_set_drag_payload(crate::view::LibraryDrag {
                            name: format!("{}.dat", p.id),
                            color,
                        });
                        if response.clicked() {
                            chosen = Some((p.id.clone(), p.clean_desc().into(), color));
                        }
                        response.on_hover_text(format!("{} · {}", p.id, p.clean_desc()));
                        ui.add_sized([cell, 36.], egui::Label::new(p.clean_desc()).truncate());
                        similar_menu(ui, &p.id, &mut request);
                    });
                    if (n + 1) % columns == 0 {
                        ui.end_row();
                    }
                }
            });
    });
    ui.data_mut(|d| {
        d.insert_temp(
            egui::Id::new(("similar-height", width.round() as i32)),
            response.response.rect.height() + ui.spacing().item_spacing.y,
        )
    });
    if !open {
        st.similar_to = None;
    }
    if let Some(id) = request {
        st.similar_hits = search.similar(&id, 8);
        ui.data_mut(|d| d.insert_temp(egui::Id::new("similar-browse"), id));
    }
    if let Some(piece) = chosen {
        choose_part(st, piece);
    }
}

fn similar_height(ui: &egui::Ui, count: usize) -> f32 {
    if let Some(h) = ui.data_mut(|d| {
        d.get_temp::<f32>(egui::Id::new((
            "similar-height",
            ui.available_width().round() as i32,
        )))
    }) {
        return h;
    }
    let columns = ((ui.available_width() - 16. + 8.) / 118.)
        .floor()
        .clamp(1., 4.) as usize;
    88. + count.div_ceil(columns) as f32 * 184.
}

fn expanded_rows(count: usize, height: f32, expanded: Option<usize>, extra: f32) -> Vec<f32> {
    let mut offsets = vec![0.];
    for row in 0..count {
        offsets.push(
            offsets.last().unwrap() + height + if expanded == Some(row) { extra } else { 0. },
        );
    }
    offsets
}

fn visible_rows(
    ui: &mut egui::Ui,
    viewport: egui::Rect,
    offsets: &[f32],
) -> std::ops::Range<usize> {
    let count = offsets.len().saturating_sub(1);
    ui.set_min_height(*offsets.last().unwrap_or(&0.));
    let first = offsets
        .partition_point(|y| *y < viewport.min.y)
        .saturating_sub(1)
        .min(count);
    let end = offsets.partition_point(|y| *y <= viewport.max.y).min(count);
    ui.add_space(offsets[first]);
    first..end
}

fn part_name(st: &State, id: &str) -> String {
    st.search
        .as_ref()
        .and_then(|s| s.cat.parts.iter().find(|p| p.id == id))
        .map(|p| p.clean_desc().to_string())
        .unwrap_or_else(|| id.to_string())
}

fn colour_picker(st: &mut State, ui: &mut egui::Ui) {
    let menu = ui.menu_button("       ▾", |ui| {
        ui.label("Part colour");
        if ui.selectable_label(st.all_colors, "∅  No colour · show all colours")
            .on_hover_text("No manufactured-colour restriction. Each part uses an available colour where known, otherwise neutral grey. Printed/fixed colours are preserved.")
            .clicked() {
            st.all_colors = true;
            st.filters.color = None;
            ui.close();
        }
        ui.separator();
        egui::ScrollArea::vertical()
            .max_height(300.0)
            .show(ui, |ui| {
                egui::Grid::new("part-colour-swatches")
                    .spacing([3.0, 3.0])
                    .show(ui, |ui| {
                        let mut n = 0;
                        for (id, c) in &st.lib.colors.by_code {
                            if *id == 16 || *id == 24 {
                                continue;
                            }
                            let (rect, response) = ui
                                .allocate_exact_size(egui::vec2(25.0, 25.0), egui::Sense::click());
                            paint_swatch(ui, rect.shrink(2.0), c.rgb, c.alpha, !st.all_colors && *id == st.color);
                            if response
                                .on_hover_text(format!("{} · {id}", c.name))
                                .clicked()
                            {
                                if st.all_colors {
                                    st.filters.color = Some(*id);
                                }
                                st.all_colors = false;
                                st.color = *id;
                                ui.close();
                            }
                            n += 1;
                            if n % 10 == 0 {
                                ui.end_row();
                            }
                        }
                    });
            });
    });
    let rgba = st.lib.colors.display_rgba(ldraw::Color(st.color));
    let swatch = egui::Rect::from_min_size(
        menu.response.rect.min + egui::vec2(5.0, 4.0),
        egui::vec2(20.0, 14.0),
    );
    if st.all_colors {
        ui.painter()
            .rect_filled(swatch, 2.0, ui.visuals().faint_bg_color);
        ui.painter().text(
            swatch.center(),
            egui::Align2::CENTER_CENTER,
            "∅",
            egui::FontId::proportional(18.0),
            ui.visuals().text_color(),
        );
    } else {
        paint_swatch(ui, swatch, [rgba[0], rgba[1], rgba[2]], rgba[3], false);
    }
    menu.response.on_hover_text(if st.all_colors {
        "No colour · all manufactured colours and unknown-colour parts".into()
    } else {
        st.lib
            .colors
            .by_code
            .get(&st.color)
            .map(|c| format!("{} · choose part colour", c.name))
            .unwrap_or_else(|| "Choose part colour".into())
    });
}

// LDraw shapes generally have no single factory/default colour. In unrestricted
// mode pick a stable known available colour, retaining file-defined decoration.
fn part_colour(part: &search::catalog::Part, selected: Option<i32>) -> i32 {
    selected.unwrap_or_else(|| {
        [71, 7, 0, 15, 4, 1]
            .into_iter()
            .find(|c| part.colors.contains(c))
            .or_else(|| part.colors.iter().copied().find(|c| *c != 16 && *c != 24))
            .unwrap_or(71)
    })
}

fn thumbnail_layout(width: f32, gap: f32) -> (usize, f32) {
    let columns = ((width + gap) / (96.0 + gap)).floor().max(1.0) as usize;
    (
        columns,
        ((width - gap * (columns - 1) as f32) / columns as f32).max(1.0),
    )
}

#[cfg(test)]
mod layout_tests {
    use super::*;

    #[test]
    fn no_colour_uses_each_parts_available_colour_not_the_previous_filter() {
        let part = search::catalog::Part {
            colors: vec![14, 1],
            ..Default::default()
        };
        assert_eq!(part_colour(&part, None), 1);
        assert_eq!(part_colour(&part, Some(4)), 4);
        let rare = search::catalog::Part {
            colors: vec![226],
            ..Default::default()
        };
        assert_eq!(part_colour(&rare, None), 226);
        assert_eq!(part_colour(&search::catalog::Part::default(), None), 71);
    }

    #[test]
    fn thumbnails_fill_width_and_add_columns_only_when_they_fit() {
        for width in [60.0, 96.0, 200.0, 310.0, 350.0, 410.0, 600.0] {
            let (columns, cell) = thumbnail_layout(width, 8.0);
            assert!((columns as f32 * cell + (columns - 1) as f32 * 8.0 - width).abs() < 0.001);
            if columns > 1 {
                assert!(cell >= 96.0);
            }
        }
        assert_eq!(thumbnail_layout(350.0, 8.0).0, 3);
        assert_eq!(thumbnail_layout(410.0, 8.0).0, 4);
    }
}

fn paint_swatch(ui: &egui::Ui, rect: egui::Rect, rgb: [u8; 3], alpha: u8, selected: bool) {
    let painter = ui.painter();
    painter.rect_filled(rect, 2.0, egui::Color32::WHITE);
    if alpha < 255 {
        let half = rect.size() * 0.5;
        for offset in [egui::Vec2::ZERO, half] {
            painter.rect_filled(
                egui::Rect::from_min_size(rect.min + offset, half),
                0.0,
                egui::Color32::GRAY,
            );
        }
    }
    painter.rect_filled(
        rect,
        2.0,
        egui::Color32::from_rgba_unmultiplied(rgb[0], rgb[1], rgb[2], alpha),
    );
    painter.rect_stroke(
        rect,
        2.0,
        egui::Stroke::new(1.0, egui::Color32::from_gray(100)),
        egui::StrokeKind::Inside,
    );
    if selected {
        painter.rect_stroke(
            rect.expand(2.0),
            3.0,
            egui::Stroke::new(2.0, ui.visuals().selection.bg_fill),
            egui::StrokeKind::Outside,
        );
    }
}
pub fn inspector(st: &mut State, ui: &mut egui::Ui) {
    egui::ScrollArea::vertical().show(ui, |ui| {
        let ids = st.ids();
        ui.heading("Selection");
        ui.label(format!("{} source parts", ids.len()));
        if ids.len() == 1 {
            if let Some(p) = st.snapshot.model.parts.get(&ids[0]) {
                ui.label(format!(
                    "#{} · {} · colour {} · step {}",
                    p.id, p.name, p.color, p.step
                ));
                ui.monospace(format!(
                    "LDU: {:.3}, {:.3}, {:.3}",
                    p.pos.x, p.pos.y, p.pos.z
                ));
                ui.label(format!("revision {}", p.revision));
                if let Some(file) = st.snapshot.model.doc.file_index(&p.name) {
                    if ui.button("Open subbuild panel").clicked() {
                        st.views[st.active].open_submodel = Some(file);
                    }
                }
            }
        }
        ui.separator();
        ui.label("Translation (LDraw units)");
        ui.horizontal(|ui| {
            for (axis, v) in ["X", "Y", "Z"].iter().zip(st.inspect_delta.iter_mut()) {
                ui.label(*axis);
                ui.add(egui::DragValue::new(v).speed(1.0));
            }
        });
        if ui.button("Move").clicked() {
            st.command(Cmd::Move {
                ids: ids.clone(),
                delta: DVec3::from_array(st.inspect_delta),
                free: false,
            });
        }
        ui.horizontal(|ui| {
            ui.add(egui::DragValue::new(&mut st.angle).suffix("°"));
            for (name, axis) in [("X", DVec3::X), ("Y", DVec3::Y), ("Z", DVec3::Z)] {
                if ui.button(format!("Rotate {name}")).clicked() {
                    let pivot = ids
                        .first()
                        .and_then(|id| st.snapshot.model.parts.get(id))
                        .map(|p| p.pos)
                        .unwrap_or_default();
                    st.command(Cmd::Rotate {
                        ids: ids.clone(),
                        axis,
                        angle: st.angle,
                        pivot,
                        free: false,
                    });
                }
            }
        });
        ui.horizontal(|ui| {
            if ui.button("Paint").clicked() {
                st.command(Cmd::Paint {
                    ids: ids.clone(),
                    color: st.color,
                });
            }
            if ui.button("Delete").clicked() {
                st.command(Cmd::Remove { ids: ids.clone() });
            }
        if ui.button("Copy · Ctrl/Cmd+C").clicked() {
                st.copy_to_place();
            }
        });
        ui.horizontal(|ui| {
            for (label, c) in [
                (
                    "Hide",
                    Cmd::Hide {
                        ids: ids.clone(),
                        value: true,
                    },
                ),
                (
                    "Show",
                    Cmd::Hide {
                        ids: ids.clone(),
                        value: false,
                    },
                ),
                (
                    "Lock",
                    Cmd::Lock {
                        ids: ids.clone(),
                        value: true,
                    },
                ),
                (
                    "Unlock",
                    Cmd::Lock {
                        ids: ids.clone(),
                        value: false,
                    },
                ),
            ] {
                if ui.button(label).clicked() {
                    st.command(c);
                }
            }
        });
        ui.separator();
        ui.heading("Subbuild");
        ui.text_edit_singleline(&mut st.sub_name);
        if ui.button("Selection → editable subbuild").clicked() {
            match st.handle.command(Cmd::MakeSubbuild {
                ids: ids.clone(),
                name: st.sub_name.clone(),
            }) {
                Ok(v) => {
                    if let Some(file) = v["file"].as_u64() {
                        st.views[st.active].open_submodel = Some(file as usize);
                    }
                }
                Err(e) => st.message = e,
            }
        }
        ui.horizontal(|ui| {
            let pending=st.tool==Tool::Place && st.assembly_clipboard.first().is_some_and(|p|st.place.as_deref()==Some(p.name.as_str()));
            ui.label(if pending {"Mirror pending copy:"}else{"Mirror selected assembly:"});
            for (axis,label) in [(0,"X"),(1,"Y"),(2,"Z")] {
                if ui.add_enabled(st.mirror_job.is_none(),egui::Button::new(label)).on_hover_text("Verify every part's reflected geometry and counterpart. Unsupported/printed parts refuse the whole operation.").clicked(){st.mirror_selection(axis);}
            }
        });
        ui.separator();
        ui.heading("Parts in this model");
        let file = st.views[st.active].file;
        let parts: Vec<_> = st
            .snapshot
            .model
            .parts
            .values()
            .filter(|p| p.file == file)
            .map(|p| (p.id, p.name.clone(), p.hidden, p.locked))
            .collect();
        for (id, name, hidden, locked) in parts.iter().take(500) {
            if ui
                .selectable_label(
                    ids.contains(id),
                    format!(
                        "#{id} {name}{}{}",
                        if *hidden { " [hidden]" } else { "" },
                        if *locked { " [locked]" } else { "" }
                    ),
                )
                .clicked()
            {
                st.command(Cmd::Select { ids: vec![*id] });
            }
        }
        if parts.len() > 500 {
            ui.label("First 500 shown; use MCP list_parts for the full model.");
        }
        if let Some(report) = st.report.clone() {
            ui.separator();
            ui.heading(format!("Validation: {}", report.verdict));
            for l in &report.limitations {
                ui.colored_label(egui::Color32::from_rgb(150, 110, 0), l);
            }
            let current=st.report_revision==st.snapshot.revision;
            if !current {ui.colored_label(egui::Color32::YELLOW,"Build changed — run Check again before locating issues.");}
            if ui.button("Clear X-ray highlight").clicked(){st.issue=None;}
            for row in report
                .rows
                .iter()
                .filter(|r| !r.collides.is_empty())
                .take(100)
            {
                let index=row.uid.saturating_sub(1);
                let name=st.report_flat.get(index).map(|p|p.name.as_str()).unwrap_or("unknown piece");
                if ui.add_enabled(current,egui::Button::new(format!("Locate #{} · {} · {:?}",row.uid,name,row.status))).clicked(){
                    let mut indices=vec![index];indices.extend(&row.collides);st.issue_serial+=1;
                    st.issue=Some((st.report_file,indices,st.report_revision,st.issue_serial));
                    if let Some(p)=st.report_flat.get(index){if let Some(id)=st.snapshot.model.id_at(st.report_file,p.root_line){st.command(Cmd::Select{ids:vec![id]});}}
                }
                ui.small(format!("Overlaps piece(s): {} · red = issue, amber = conflicting neighbour",row.collides.iter().map(|i|(i+1).to_string()).collect::<Vec<_>>().join(", ")));
                if let(Some(a),Some(b))=(st.report_flat.get(index),row.collides.first().and_then(|i|st.report_flat.get(*i))){
                    let d=a.pos-b.pos;let direction=if d.length_squared()>0.001{d.normalize()}else{DVec3::X};
                    ui.small(format!("Possible adjustment: move the red piece away from the amber one (direction {:.1}, {:.1}, {:.1}), or reconnect it with C. Re-check afterward; this is not a verified repair.",direction.x,direction.y,direction.z));
                }
            }
        }
    });
}
pub fn checker(st: &mut State, ui: &mut egui::Ui) {
    ui.heading("Build checker");
    if ui.button("Check again").clicked() {
        st.check();
    }
    let Some(report) = st.report.clone() else {
        ui.label("Run Check to inspect your build.");
        return;
    };
    let current = st.report_revision == st.snapshot.revision;
    ui.strong(format!(
        "{} · {} flagged pieces",
        report.verdict, report.errors
    ));
    if !current {
        ui.colored_label(egui::Color32::YELLOW, "Build changed: run Check again.");
    }
    ui.label("Red/amber identifies a blocking pair. Blue means the geometry/connector review cleared a native flag. Unresolved contact still blocks instruction generation.");
    let mut locate = None;
    ui.horizontal(|ui| {
        if ui.button("Clear X-ray").clicked() {
            st.issue = None;
        }
        if ui
            .add_enabled(current, egui::Button::new("Weakest point"))
            .on_hover_text("Find the connection holding least: the fewest studs whose loss splits the main assembly")
            .clicked()
        {
            let edges = engine::strength::edges(&report);
            let group = report.groups.iter().map(|g| g.iter().map(|u| u.saturating_sub(1)).collect::<Vec<_>>()).max_by_key(|g| g.len()).unwrap_or_default();
            if group.len() > 3000 {
                st.message = format!("The main assembly has {} pieces; select a smaller region and use `sb … verify` with ids", group.len());
            } else if let Some(c) = engine::strength::weakest_cut(report.rows.len(), &edges, &group) {
                let small = group.iter().filter(|&&r| c.side[r]).count().min(group.iter().filter(|&&r| !c.side[r]).count());
                let mut idx: Vec<usize> = c.edges.iter().flat_map(|&(a, b, _)| [a, b]).collect();
                idx.sort_unstable();
                idx.dedup();
                st.message = format!(
                    "Weakest point: {} connection(s) worth {:.0} stud(s) hold {} piece(s) to the rest",
                    c.edges.len(),
                    c.value,
                    small
                );
                locate = Some(idx);
            } else {
                st.message = "The main assembly has only one piece".into();
            }
        }
    });
    egui::ScrollArea::vertical().show(ui,|ui|{
        if !report.colour_warnings.is_empty() {
            ui.separator();
            ui.colored_label(egui::Color32::YELLOW, format!("{} part-colour availability warnings (not geometry errors)", report.colour_warnings.len()));
            ui.small("Catalogue observations are incomplete, not supplier stock or proof a colour was never produced.");
            for warning in &report.colour_warnings {
                ui.colored_label(egui::Color32::YELLOW, &warning.message);
                ui.small(format!("{} piece(s), including #{}",warning.pieces.len(),warning.pieces[0]));
                if ui.add_enabled(current,egui::Button::new(format!("Select {} · colour {}",warning.part,warning.color))).clicked() {
                    let ids:Vec<_>=warning.pieces.iter().filter_map(|i|st.report_flat.get(i-1)).filter_map(|p|st.snapshot.model.id_at(st.report_file,p.root_line)).collect();
                    st.command(Cmd::Select{ids});
                }
            }
        }
        for row in &report.rows {for &b in &row.collides {
            let a=row.uid-1;let names=|i:usize|st.report_flat.get(i).map(|p|p.name.as_str()).unwrap_or("unknown");
            ui.separator();
            if ui.add_enabled(current,egui::Button::new(format!("Locate issue: #{} {} ↔ #{} {}",a+1,names(a),b+1,names(b)))).clicked(){locate=Some(vec![a,b]);}
            if let Some(p)=report.reviewed.iter().find(|p|(p.a==a && p.b==b)||(p.a==b && p.b==a)) {
                for reason in &p.reasons {ui.colored_label(egui::Color32::LIGHT_RED,reason);}
                ui.small(format!("Solid overlap: {:.2} LDU³. Matched connectors: {}.",p.overlap,if p.types.is_empty(){"none".into()}else{p.types.join(", ")}));
                for point in &p.points {ui.small(format!("Conflict point: x {:.2}, y {:.2}, z {:.2} LDU (red cross).",point[0],point[1],point[2]));}
                if p.reasons.iter().any(|s|s.contains("not pressed home")||s.contains("gap")) {
                    ui.label("The connector is aligned but the parts are not fully seated. Try reconnecting these endpoints with C to close the gap; recheck afterwards. No automatic edit has been made.");
                } else {ui.small("Try reseating/reconnecting the pair or moving it apart, then recheck. The geometric test may still be approximate; this is not a verified repair.");}
            } else {ui.small("Native geometric flag; full diagnosis unavailable. Recheck for connector-aware details.");}
        }}
        for pair in &report.reviewed {if !pair.collision {
            ui.separator();ui.colored_label(if pair.warning {egui::Color32::YELLOW}else{egui::Color32::LIGHT_BLUE},format!("{}: #{} ↔ #{} · {} · solid overlap {:.2} LDU³",if pair.warning {"Contact review — does not block draft instructions"}else{"Cleared native warning"},pair.a+1,pair.b+1,pair.types.join(", "),pair.overlap));
            if pair.warning {for reason in &pair.reasons {ui.label(reason);}}
            if ui.add_enabled(current,egui::Button::new("Show reviewed connection")).clicked(){locate=Some(vec![pair.a,pair.b]);}
        }}
        if let Some(indices)=locate {st.issue_serial+=1;st.issue=Some((st.report_file,indices,st.report_revision,st.issue_serial));}
        ui.separator();for note in &report.limitations {ui.small(note);}
    });
}
pub fn drafts(st: &mut State, ui: &mut egui::Ui) {
    ui.heading("Agents");
    ui.small("Agents edit through proposals; your own edits stay solid. Nothing an agent does lands until you accept it.");
    if st.snapshot.drafts.is_empty() {
        if st.link.is_some() {
            ui.label("No proposals yet. Agents connect with `sb mcp --user NAME` or run `sb PROJECT.sb --user NAME TOOL '{...}'`.");
        } else {
            ui.label("Save this model as a SuperBrick project (.sb) to share it with agents: they work on the project file, and their proposals appear here.");
        }
        return;
    }
    ui.horizontal(|ui| {
        ui.label("Note for rejections:");
        ui.add(egui::TextEdit::singleline(&mut st.reject_note).hint_text("why (the agent reads this)").desired_width(220.0));
    });
    let users: std::collections::BTreeSet<_> = st.snapshot.drafts.iter().map(|d| d.session.clone()).collect();
    let live = |s: &str| matches!(s, "pending" | "stale" | "blocked");
    for user in users {
        let ops: Vec<_> = st.snapshot.drafts.iter().filter(|d| d.session == user).cloned().collect();
        let pending = ops.iter().filter(|d| d.state == "pending").count();
        let header = if pending > 0 { format!("{user} — {pending} pending") } else { user.clone() };
        egui::CollapsingHeader::new(egui::RichText::new(header).strong())
            .id_salt(("agent", &user))
            .default_open(true)
            .show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.add_enabled(pending > 0, egui::Button::new("Accept all")).clicked() {
                        let s = user.clone();
                        let r = st.handle.call(move |e| e.accept(&s, None));
                        st.result(r);
                    }
                    let checked: Vec<u64> = ops.iter().filter(|d| d.state == "pending" && st.draft_selection.contains(&d.id)).map(|d| d.id).collect();
                    if ui.add_enabled(!checked.is_empty(), egui::Button::new(format!("Accept checked ({})", checked.len()))).clicked() {
                        let s = user.clone();
                        let r = st.handle.call(move |e| e.accept(&s, Some(checked)));
                        st.result(r);
                    }
                    let open: Vec<u64> = ops.iter().filter(|d| live(&d.state)).map(|d| d.id).collect();
                    if ui.add_enabled(!open.is_empty(), egui::Button::new("Reject all")).clicked() {
                        let s = user.clone();
                        let note = st.reject_note.clone();
                        let r = st.handle.call(move |e| e.reject_with_note(&s, open, &note));
                        st.result(r);
                    }
                });
                let mut shown = 0;
                for op in ops.iter().rev() {
                    // Live proposals always; a few recent decisions for context.
                    if !live(&op.state) {
                        shown += 1;
                        if shown > 5 {
                            continue;
                        }
                    }
                    ui.horizontal(|ui| {
                        let mut checked = st.draft_selection.contains(&op.id);
                        if ui.add_enabled(op.state == "pending", egui::Checkbox::without_text(&mut checked)).changed() {
                            if checked {
                                st.draft_selection.insert(op.id);
                            } else {
                                st.draft_selection.remove(&op.id);
                            }
                        }
                        let color = match op.state.as_str() {
                            "pending" => egui::Color32::from_rgb(30, 100, 210),
                            "stale" => egui::Color32::from_rgb(170, 120, 0),
                            "blocked" => egui::Color32::RED,
                            "accepted" => egui::Color32::from_rgb(40, 140, 60),
                            _ => egui::Color32::GRAY,
                        };
                        let title = if op.label.is_empty() { format!("{} op(s)", op.commands.len()) } else { op.label.clone() };
                        let t = &op.touched;
                        let mut what = Vec::new();
                        if !t.created.is_empty() { what.push(format!("+{}", t.created.len())); }
                        if !t.modified.is_empty() { what.push(format!("~{}", t.modified.len())); }
                        if !t.removed.is_empty() { what.push(format!("−{}", t.removed.len())); }
                        let link = ui
                            .add(egui::Label::new(egui::RichText::new(format!("#{} {} · {}", op.id, title, what.join(" "))).color(color)).sense(egui::Sense::click()))
                            .on_hover_text(format!(
                                "{} · turn {}\n{} added · {} changed · {} removed\n{}{}\nClick to select the parts it touches.",
                                op.state,
                                op.turn,
                                t.created.len(),
                                t.modified.len(),
                                t.removed.len(),
                                op.commands.iter().take(6).map(|c| format!("{c:?}").chars().take(160).collect::<String>()).collect::<Vec<_>>().join("\n"),
                                if op.reason.is_empty() { String::new() } else { format!("\n{}", op.reason) },
                            ));
                        if link.clicked() {
                            let ids: Vec<u64> = t.modified.iter().chain(&t.removed).copied().filter(|id| st.snapshot.model.parts.contains_key(id)).collect();
                            if !ids.is_empty() {
                                st.command(engine::Cmd::Select { ids });
                            }
                        }
                        if op.state == "pending" {
                            if ui.small_button("Accept").clicked() {
                                let s = user.clone();
                                let id = op.id;
                                let r = st.handle.call(move |e| e.accept(&s, Some(vec![id])));
                                st.result(r);
                            }
                        }
                        if live(&op.state) && ui.small_button("Reject").on_hover_text("Reject with the note above").clicked() {
                            let s = user.clone();
                            let id = op.id;
                            let note = st.reject_note.clone();
                            let r = st.handle.call(move |e| e.reject_with_note(&s, vec![id], &note));
                            st.result(r);
                        }
                    });
                    if !op.reason.is_empty() && live(&op.state) {
                        ui.small(&op.reason);
                    }
                    if !op.note.is_empty() && !live(&op.state) {
                        ui.small(format!("note: {}", op.note));
                    }
                }
            });
    }
}
pub fn history(st: &mut State, ui: &mut egui::Ui) {
    ui.heading("History");
    let Some(link) = st.link.as_mut() else {
        ui.label("Version history is kept in SuperBrick projects. Use Save as… and choose a .sb file.");
        return;
    };
    ui.horizontal(|ui| {
        if ui.button("Save version").on_hover_text("Write a named version now (⌘S)").clicked() {
            link.publish(&st.snapshot);
            link.commit("save", "saved");
        }
        if ui.button("Refresh").clicked() {
            link.refresh_log();
        }
    });
    ui.horizontal(|ui| {
        ui.add(egui::TextEdit::singleline(&mut st.branch_name).hint_text("branch name").desired_width(140.0));
        if ui.add_enabled(!st.branch_name.trim().is_empty(), egui::Button::new("Branch from here")).clicked() {
            link.branch(st.branch_name.trim());
            st.branch_name.clear();
        }
    });
    ui.separator();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs_f64()).unwrap_or(0.0);
    let ago = |t: f64| {
        let s = (now - t).max(0.0);
        if s < 90.0 { format!("{:.0}s ago", s) } else if s < 5400.0 { format!("{:.0}m ago", s / 60.0) } else if s < 172800.0 { format!("{:.0}h ago", s / 3600.0) } else { format!("{:.0}d ago", s / 86400.0) }
    };
    let mut restore = None;
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        for c in &link.log {
            ui.horizontal(|ui| {
                ui.monospace(format!("{:>4}", c.id));
                ui.label(egui::RichText::new(&c.kind).strong());
                ui.label(format!("{} · {}", c.author, ago(c.time)));
                if ui.small_button("Restore").on_hover_text("Make this version current (the current state is kept in history)").clicked() {
                    restore = Some(c.id);
                }
            });
            if !c.message.is_empty() {
                ui.small(&c.message);
            }
        }
        if link.log.is_empty() {
            ui.label("No versions yet.");
        }
    });
    if let Some(id) = restore {
        link.publish(&st.snapshot);
        link.restore(id);
        st.message = format!("Restoring version {id}…");
    }
}
pub fn palette(st: &mut State, ctx: &egui::Context) {
    if !st.palette {
        return;
    }
    let mut open = true;
    egui::Window::new("Shared command palette").open(&mut open).default_width(560.0).show(ctx,|ui|{
        ui.label("Every engine command is available here and through MCP. Angles are degrees; positions are LDraw units.");
        egui::ComboBox::from_id_salt("command").selected_text(&st.command_name).show_ui(ui,|ui|{
            for def in engine::registry(){if ui.selectable_value(&mut st.command_name,def.name.into(),def.name).changed(){
                let ids=st.ids();st.command_json=match def.name{
                    "move"=>json!({"ids":ids,"delta":[20,0,0]}),"nudge"=>json!({"ids":ids,"direction":[1,0,0]}),"rotate"=>json!({"ids":ids,"axis":[0,-1,0],"angle":90,"pivot":[0,0,0]}),
                    "paint"=>json!({"ids":ids,"color":st.color}),"copy"=>json!({"ids":ids,"delta":[20,0,0],"count":1}),
                    "mirror"=>json!({"ids":ids,"axis":0,"pivot":[0,0,0]}),
                    "make_subbuild"|"group"=>json!({"ids":ids,"name":"subbuild"}),"hide"|"lock"=>json!({"ids":ids,"value":true}),
                    "add"=>json!({"file":st.views[st.active].file,"parts":[{"name":"3003.dat","color":st.color,"pos":[0,0,0]}]}),
                    _=>json!({"ids":ids}),
                }.to_string();
            }}
        });
        ui.add(egui::TextEdit::multiline(&mut st.command_json).code_editor().desired_rows(8).desired_width(f32::INFINITY));
        if ui.button("Run command").clicked(){
            let r=serde_json::from_str::<serde_json::Value>(&st.command_json).map_err(|e|e.to_string()).and_then(|mut v|{v["command"]=json!(st.command_name);serde_json::from_value(v).map_err(|e|e.to_string())});
            match r{Ok(c)=>st.command(c),Err(e)=>st.message=e}
        }
    });
    st.palette = open;
}
