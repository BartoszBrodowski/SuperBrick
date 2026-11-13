use eframe::egui::{self, Color32, Pos2, Rect, Stroke};
use std::collections::BTreeSet;
#[derive(Default)]
pub struct Lasso {
    points: Vec<Pos2>,
}
impl Lasso {
    pub fn update(
        &mut self,
        ui: &egui::Ui,
        response: &egui::Response,
        cam: &render::Camera,
        rect: Rect,
        loaded: &crate::view::Loaded,
        snapshot: &engine::Snapshot,
        graph: Option<&engine::joints::JointGraph>,
    ) -> Option<engine::Cmd> {
        if response.drag_started_by(egui::PointerButton::Primary) {
            self.points.clear();
            if let Some(p) = ui.input(|i| i.pointer.press_origin()) {
                self.points.push(p);
            }
        }
        if response.dragged_by(egui::PointerButton::Primary) {
            if let Some(p) = ui.input(|i| i.pointer.interact_pos()) {
                if self.points.last().is_none_or(|q| q.distance(p) > 2.) {
                    self.points.push(p);
                }
            }
        }
        if self.points.len() > 1 {
            let mut points = self.points.clone();
            points.push(points[0]);
            ui.painter_at(rect).add(egui::Shape::line(
                points,
                Stroke::new(2., Color32::LIGHT_BLUE),
            ));
        }
        if !response.drag_stopped_by(egui::PointerButton::Primary) {
            return None;
        }
        let tolerance = ui.data_mut(|d| {
            d.get_persisted::<f32>(egui::Id::new("lasso-tolerance"))
                .unwrap_or(8.)
        });
        let stop = ui.data_mut(|d| {
            d.get_persisted::<bool>(egui::Id::new("lasso-joints"))
                .unwrap_or(true)
        });
        let mut ids = BTreeSet::new();
        for (p, id) in loaded.flat.iter().zip(&loaded.ids) {
            let Some(id) = id else {
                continue;
            };
            if snapshot
                .model
                .parts
                .get(id)
                .is_none_or(|p| p.hidden || p.locked)
            {
                continue;
            }
            if crate::mechanical::project(cam, rect, p.pos)
                .is_some_and(|p| contains(&self.points, p, tolerance))
            {
                ids.insert(*id);
            }
        }
        self.points.clear();
        if let Some(g) = graph {
            ids = expand(&ids, g, stop);
        }
        ids.retain(|id| {
            snapshot
                .model
                .parts
                .get(id)
                .is_some_and(|p| !p.hidden && !p.locked)
        });
        if ui.input(|i| i.modifiers.shift) {
            ids.extend(snapshot.model.selection.iter());
        }
        Some(engine::Cmd::Select {
            ids: ids.into_iter().collect(),
        })
    }
}
pub fn expand(
    seeds: &BTreeSet<u64>,
    graph: &engine::joints::JointGraph,
    stop: bool,
) -> BTreeSet<u64> {
    let mut ids = seeds.clone();
    loop {
        let old = ids.len();
        for j in &graph.joints {
            if stop && !matches!(j.motion, engine::joints::Motion::Fixed) {
                continue;
            }
            if ids.contains(&j.a) {
                ids.insert(j.b);
            }
            if ids.contains(&j.b) {
                ids.insert(j.a);
            }
        }
        if old == ids.len() {
            return ids;
        }
    }
}
fn contains(poly: &[Pos2], p: Pos2, tolerance: f32) -> bool {
    if poly.len() < 3 {
        return false;
    }
    let mut inside = false;
    for i in 0..poly.len() {
        let a = poly[i];
        let b = poly[(i + 1) % poly.len()];
        let d = b - a;
        let t = ((p - a).dot(d) / d.length_sq().max(0.0001)).clamp(0., 1.);
        if (p - (a + d * t)).length() <= tolerance {
            return true;
        }
        if (a.y > p.y) != (b.y > p.y) && p.x < (b.x - a.x) * (p.y - a.y) / (b.y - a.y) + a.x {
            inside = !inside;
        }
    }
    inside
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn tolerance() {
        let p = [
            Pos2::new(0., 0.),
            Pos2::new(100., 0.),
            Pos2::new(100., 100.),
            Pos2::new(0., 100.),
        ];
        assert!(contains(&p, Pos2::new(50., 50.), 0.));
        assert!(!contains(&p, Pos2::new(105., 50.), 0.));
        assert!(contains(&p, Pos2::new(105., 50.), 8.));
    }
    #[test]
    fn smart_selection_stops_at_joints_by_default() {
        use engine::joints::*;
        let graph = JointGraph {
            parts: [1, 2, 3, 4].into_iter().collect(),
            joints: vec![
                Joint {
                    id: 1,
                    a: 1,
                    b: 2,
                    point: glam::DVec3::ZERO,
                    motion: Motion::Fixed,
                    label: "stud".into(),
                },
                Joint {
                    id: 2,
                    a: 2,
                    b: 3,
                    point: glam::DVec3::ZERO,
                    motion: Motion::Ball {
                        axes: [glam::DVec3::X, glam::DVec3::Y, glam::DVec3::Z],
                    },
                    label: "ball".into(),
                },
                Joint {
                    id: 3,
                    a: 3,
                    b: 4,
                    point: glam::DVec3::ZERO,
                    motion: Motion::Fixed,
                    label: "stud".into(),
                },
            ],
        };
        assert_eq!(
            expand(&[1].into_iter().collect(), &graph, true),
            [1, 2].into_iter().collect()
        );
        assert_eq!(
            expand(&[1].into_iter().collect(), &graph, false),
            [1, 2, 3, 4].into_iter().collect()
        );
    }
}
