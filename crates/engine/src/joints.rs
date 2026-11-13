//! Kinematic topology, independent of rendering and connector recognition.
//!
//! Only confirmed connector links belong here. Spatial contact is NOT a rigid link.
//! Connector recognition is supplied by the full connector adapter.
use crate::{Cmd, Id, Result};
use glam::DVec3;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum Motion {
    Fixed,
    Revolute { axis: DVec3 },
    Ball { axes: [DVec3; 3] },
    Slide { axis: DVec3 },
    Cylindrical { axis: DVec3 },
}
impl Motion {
    pub fn rotation_axes(&self) -> Vec<DVec3> {
        match self {
            Self::Revolute { axis } | Self::Cylindrical { axis } => vec![*axis],
            Self::Ball { axes } => axes.to_vec(),
            _ => vec![],
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Joint {
    pub id: u64,
    pub a: Id,
    pub b: Id,
    pub point: DVec3,
    pub motion: Motion,
    pub label: String,
}
impl Joint {
    /// Does this bearing admit rotation about the proposed world-space line?
    /// Anti-parallel axes are the same line; parallel but offset axes are not.
    fn admits_rotation(&self, pivot: DVec3, axis: DVec3) -> bool {
        let on_line = (self.point - pivot).cross(axis).length() < 1e-3;
        match &self.motion {
            Motion::Revolute { axis: other } | Motion::Cylindrical { axis: other } => {
                on_line && axis.dot(*other).abs() > 1. - 1e-6
            }
            Motion::Ball { .. } => on_line,
            Motion::Fixed | Motion::Slide { .. } => false,
        }
    }
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct JointGraph {
    pub parts: BTreeSet<Id>,
    pub joints: Vec<Joint>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Articulation {
    pub pivot_joint: u64,
    pub pivot: DVec3,
    pub axes: Vec<DVec3>,
    /// A clicked rigid chunk plus everything held upstream of the pivot.
    pub moving: BTreeSet<Id>,
    /// Explicitly preview and confirm these loop-opening detachments in the UI.
    pub detach: Vec<u64>,
}
impl Articulation {
    /// Rotate is already the shared transactional UI/MCP/undo command. This does
    /// not authorize a loop detachment: the caller must obtain confirmation first.
    pub fn rotation(&self, axis: usize, degrees: f64, confirmed_detach: &[u64]) -> Result<Cmd> {
        if !degrees.is_finite() {
            return Err("angle must be finite".into());
        }
        let direction = self
            .axes
            .get(axis)
            .ok_or("joint does not allow that rotation axis")?;
        let confirmed: BTreeSet<_> = confirmed_detach.iter().copied().collect();
        if self.detach.iter().any(|id| !confirmed.contains(id)) {
            return Err("confirm the opposite loop connection(s) before rotating".into());
        }
        Ok(Cmd::Rotate {
            ids: self.moving.iter().copied().collect(),
            axis: *direction,
            angle: degrees,
            pivot: self.pivot,
            free: false,
        })
    }
}

impl JointGraph {
    pub fn validate(&self) -> Result<()> {
        let mut ids = BTreeSet::new();
        for j in &self.joints {
            if !ids.insert(j.id)
                || !self.parts.contains(&j.a)
                || !self.parts.contains(&j.b)
                || j.a == j.b
            {
                return Err("invalid joint identity or endpoint".into());
            }
            if !j.point.is_finite() {
                return Err("joint pivot must be finite".into());
            }
            let axes = match &j.motion {
                Motion::Slide { axis } => vec![*axis],
                other => other.rotation_axes(),
            };
            if axes
                .iter()
                .any(|a| !a.is_finite() || (a.length() - 1.0).abs() > 1e-6)
            {
                return Err("joint axes must be finite unit vectors".into());
            }
            if matches!(j.motion, Motion::Ball { .. })
                && axes
                    .iter()
                    .enumerate()
                    .any(|(n, a)| axes[..n].iter().any(|b| a.dot(*b).abs() > 1e-6))
            {
                return Err("ball-joint axes must be orthogonal".into());
            }
        }
        Ok(())
    }
    pub fn rigid_chunk(&self, start: Id) -> Result<BTreeSet<Id>> {
        self.validate()?;
        if !self.parts.contains(&start) {
            return Err("unknown piece".into());
        }
        Ok(self.reachable(start, &BTreeSet::new(), true))
    }
    /// All legitimate boundary pivots. The UI can cycle them rather than silently
    /// assuming every Technic connection rotates or always choosing world Y.
    pub fn pivots(&self, clicked: Id) -> Result<Vec<u64>> {
        let chunk = self.rigid_chunk(clicked)?;
        let component = self.reachable(clicked, &BTreeSet::new(), false);
        Ok(self
            .joints
            .iter()
            .filter(|j| {
                (chunk.contains(&j.a) != chunk.contains(&j.b))
                    || (component.contains(&j.a)
                        && !j
                            .label
                            .split(" + ")
                            .all(|s| matches!(s, "stud" | "side-stud" | "tube")))
            })
            .filter(|j| !j.motion.rotation_axes().is_empty())
            .map(|j| j.id)
            .collect())
    }
    pub fn articulate(&self, clicked: Id, pivot: u64) -> Result<Articulation> {
        self.validate()?;
        let chunk = self.rigid_chunk(clicked)?;
        let joint = self
            .joints
            .iter()
            .find(|j| j.id == pivot)
            .ok_or("unknown pivot joint")?;
        let mut axes = joint.motion.rotation_axes();
        if axes.is_empty() {
            return Err("this joint does not allow rotation".into());
        }
        let fixed = match (chunk.contains(&joint.a), chunk.contains(&joint.b)) {
            (true, false) => joint.b,
            (false, true) => joint.a,
            (false, false) => {
                let cut = BTreeSet::from([pivot]);
                let side = self.reachable(clicked, &cut, false);
                match (side.contains(&joint.a), side.contains(&joint.b)) {
                    (true, false) => joint.b,
                    (false, true) => joint.a,
                    (true, true) => {
                        let a = self
                            .path(clicked, joint.a, &cut)
                            .map(|p| p.len())
                            .unwrap_or(usize::MAX);
                        let b = self
                            .path(clicked, joint.b, &cut)
                            .map(|p| p.len())
                            .unwrap_or(usize::MAX);
                        if a <= b { joint.b } else { joint.a }
                    }
                    _ => return Err("chosen joint is not connected to the clicked assembly".into()),
                }
            }
            _ => return Err("choose a joint outside the clicked rigid chunk".into()),
        };
        let mut cut = BTreeSet::from([pivot]);
        let mut detach = vec![];
        // Coaxial bearings constrain the same motion, not an interfering loop.
        // Remove them only from the partition traversal: preserve their actual
        // connections and let the full backend check every bearing on commit.
        // A genuinely incompatible second path closes a kinematic loop. Cut its movable connection
        // closest to the stationary endpoint, never a rigid stud/axle constraint.
        // Repeat for independent loops; sorting/path traversal is deterministic.
        while let Some(path) = self.path(clicked, fixed, &cut) {
            let other = path
                .iter()
                .rev()
                .filter_map(|id| self.joints.iter().find(|j| j.id == *id))
                .find(|j| !matches!(j.motion, Motion::Fixed))
                .ok_or("loop is rigidly constrained; no opposite joint can detach")?;
            cut.insert(other.id);
            let compatible: Vec<_> = axes
                .iter()
                .copied()
                .filter(|axis| other.admits_rotation(joint.point, *axis))
                .collect();
            if compatible.is_empty() {
                detach.push(other.id);
            } else {
                axes = compatible;
            }
        }
        let moving = self.reachable(clicked, &cut, false);
        if !chunk.is_subset(&moving) || moving.contains(&fixed) {
            return Err("invalid articulation partition".into());
        }
        Ok(Articulation {
            pivot_joint: pivot,
            pivot: joint.point,
            axes,
            moving,
            detach,
        })
    }
    fn adjacency(&self, cut: &BTreeSet<u64>, rigid: bool) -> BTreeMap<Id, Vec<(Id, u64)>> {
        let mut out: BTreeMap<Id, Vec<(Id, u64)>> = BTreeMap::new();
        let mut joints: Vec<_> = self.joints.iter().collect();
        joints.sort_by_key(|j| j.id);
        for j in joints {
            if cut.contains(&j.id) || (rigid && !matches!(j.motion, Motion::Fixed)) {
                continue;
            }
            out.entry(j.a).or_default().push((j.b, j.id));
            out.entry(j.b).or_default().push((j.a, j.id));
        }
        out
    }
    fn reachable(&self, start: Id, cut: &BTreeSet<u64>, rigid: bool) -> BTreeSet<Id> {
        let adjacent = self.adjacency(cut, rigid);
        let mut seen = BTreeSet::from([start]);
        let mut queue = VecDeque::from([start]);
        while let Some(a) = queue.pop_front() {
            for &(b, _) in adjacent.get(&a).into_iter().flatten() {
                if seen.insert(b) {
                    queue.push_back(b);
                }
            }
        }
        seen
    }
    fn path(&self, start: Id, target: Id, cut: &BTreeSet<u64>) -> Option<Vec<u64>> {
        let adjacent = self.adjacency(cut, false);
        let mut parents = BTreeMap::from([(start, (start, 0))]);
        let mut queue = VecDeque::from([start]);
        while let Some(a) = queue.pop_front() {
            for &(b, j) in adjacent.get(&a).into_iter().flatten() {
                if parents.contains_key(&b) {
                    continue;
                }
                parents.insert(b, (a, j));
                if b == target {
                    let mut path = vec![];
                    let mut p = target;
                    while p != start {
                        let (previous, joint) = parents[&p];
                        path.push(joint);
                        p = previous;
                    }
                    path.reverse();
                    return Some(path);
                }
                queue.push_back(b);
            }
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn j(id: u64, a: Id, b: Id, motion: Motion) -> Joint {
        Joint {
            id,
            a,
            b,
            motion,
            point: DVec3::new(10., 20., 30.),
            label: format!("joint {id}"),
        }
    }
    fn graph(joints: Vec<Joint>) -> JointGraph {
        JointGraph {
            parts: joints.iter().flat_map(|j| [j.a, j.b]).collect(),
            joints,
        }
    }
    fn hinge() -> Motion {
        Motion::Revolute { axis: DVec3::Z }
    }
    #[test]
    fn head_selection_can_choose_remote_neck_across_incidental_stud_pivot() {
        let mut stud = j(1, 1, 2, hinge());
        stud.label = "stud".into();
        let mut neck = j(
            2,
            2,
            3,
            Motion::Ball {
                axes: [DVec3::X, DVec3::Y, DVec3::Z],
            },
        );
        neck.label = "ball".into();
        let g = graph(vec![stud, neck]);
        assert!(g.pivots(1).unwrap().contains(&2));
        let plan = g.articulate(1, 2).unwrap();
        assert_eq!(plan.moving, BTreeSet::from([1, 2]));
        assert!(plan.detach.is_empty());
    }
    #[test]
    fn picks_rigid_chunk_and_carries_upstream_articulated_children() {
        let g = graph(vec![
            j(1, 1, 2, Motion::Fixed),
            j(2, 2, 3, hinge()),
            j(3, 3, 4, Motion::Fixed),
            j(
                4,
                4,
                5,
                Motion::Ball {
                    axes: [DVec3::X, DVec3::Y, DVec3::Z],
                },
            ),
            j(5, 5, 6, Motion::Fixed),
        ]);
        assert_eq!(g.rigid_chunk(4).unwrap(), BTreeSet::from([3, 4]));
        assert_eq!(g.pivots(3).unwrap(), vec![2, 4]);
        let plan = g.articulate(4, 2).unwrap();
        assert_eq!(plan.moving, BTreeSet::from([3, 4, 5, 6]));
        assert!(plan.detach.is_empty());
        assert_eq!(plan.axes, vec![DVec3::Z]);
        assert!(matches!(
            plan.rotation(0, 15., &[]).unwrap(),
            Cmd::Rotate { free: false, .. }
        ));
        assert!(plan.rotation(1, 15., &[]).is_err());
    }
    #[test]
    fn balls_expose_three_axes_but_sliding_and_fixed_connections_do_not_rotate() {
        let g = graph(vec![
            j(
                1,
                1,
                2,
                Motion::Ball {
                    axes: [DVec3::X, DVec3::Y, DVec3::Z],
                },
            ),
            j(2, 2, 3, Motion::Slide { axis: DVec3::X }),
        ]);
        assert_eq!(g.articulate(1, 1).unwrap().axes.len(), 3);
        assert!(g.articulate(3, 2).is_err());
    }
    #[test]
    fn loop_cuts_opposite_joint_and_requires_explicit_confirmation() {
        let mut g = graph(vec![
            j(1, 1, 2, hinge()),
            j(2, 2, 3, Motion::Fixed),
            j(3, 3, 4, hinge()),
            j(4, 4, 5, Motion::Fixed),
            j(5, 5, 1, hinge()),
        ]);
        // These hinges form a real loop, not multiple bearings on one axis.
        g.joints[2].point += DVec3::X * 10.;
        g.joints[4].point += DVec3::X * 20.;
        let plan = g.articulate(2, 1).unwrap();
        assert_eq!(plan.detach, vec![5]);
        assert_eq!(plan.moving, BTreeSet::from([2, 3, 4, 5]));
        assert!(plan.rotation(0, 15., &[]).is_err());
        assert!(plan.rotation(0, 15., &[5]).is_ok());
    }
    #[test]
    fn coaxial_hinges_between_rigid_chunks_are_one_pivot_not_detachments() {
        let mut g = graph(vec![
            j(1, 1, 2, hinge()),
            j(2, 1, 3, Motion::Fixed),
            j(3, 2, 4, Motion::Fixed),
            j(4, 3, 4, Motion::Revolute { axis: DVec3::NEG_Z }),
        ]);
        g.joints[3].point += DVec3::Z * 40.;
        for pivot in [1, 4] {
            let plan = g.articulate(4, pivot).unwrap();
            assert_eq!(plan.moving, BTreeSet::from([2, 4]));
            assert!(
                plan.detach.is_empty(),
                "coaxial bearings must stay connected"
            );
            assert!(plan.rotation(0, 15., &[]).is_ok());
        }
        // Parallel is not sufficient: offset axes cannot rotate as one bearing.
        g.joints[3].point += DVec3::X * 5.;
        let plan = g.articulate(4, 1).unwrap();
        assert_eq!(plan.detach, vec![4]);
        assert!(plan.rotation(0, 15., &[]).is_err());
    }
    #[test]
    fn fixed_braces_cannot_be_silently_broken_or_called_a_hinge() {
        let g = graph(vec![
            j(1, 1, 2, hinge()),
            j(2, 2, 3, Motion::Fixed),
            j(3, 3, 1, Motion::Fixed),
        ]);
        assert!(g.articulate(2, 1).is_err());
        let mut bad = graph(vec![j(1, 1, 2, hinge())]);
        bad.joints[0].point = DVec3::NAN;
        assert!(bad.validate().is_err());
    }
}
