//! Smart checkpoints: when to record an automatic history commit, and which old ones to thin.
//!
//! Pure policy, no I/O. The caller feeds every accepted edit (the set of parts it touched and
//! how) into a [`Burst`] and asks [`Burst::due`] whether to write a checkpoint now.
use std::collections::BTreeSet;

/// Move on to a new region after this long → checkpoint.
pub const MOVE_ON_SECS: f64 = 60.0;
/// Idle this long with unrecorded edits → checkpoint.
pub const IDLE_SECS: f64 = 120.0;
/// Significance that forces a checkpoint even while editing the same parts.
pub const THRESHOLD: f64 = 40.0;

/// What one edit did, as seen by the store thread.
#[derive(Clone, Debug, Default)]
pub struct Edit {
    pub added: BTreeSet<u64>,
    pub removed: BTreeSet<u64>,
    pub changed: BTreeSet<u64>,
    /// Submodel files created or deleted.
    pub files: usize,
    pub time: f64,
}
impl Edit {
    fn parts(&self) -> impl Iterator<Item = &u64> {
        self.added.iter().chain(&self.removed).chain(&self.changed)
    }
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.changed.is_empty() && self.files == 0
    }
}

/// Edits since the last checkpoint. Repeated edits to the same parts only extend the burst, so
/// rotate → rotate → rotate on one part costs one checkpoint, not three.
#[derive(Clone, Debug, Default)]
pub struct Burst {
    pub parts: BTreeSet<u64>,
    added: BTreeSet<u64>,
    removed: BTreeSet<u64>,
    changed: BTreeSet<u64>,
    files: usize,
    started: Option<f64>,
    last: f64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Due {
    /// Write a checkpoint of the state *before* this edit, then start a new burst with it.
    Before(&'static str),
    /// Write a checkpoint of the current state.
    Now(&'static str),
    No,
}

impl Burst {
    pub fn is_empty(&self) -> bool {
        self.started.is_none()
    }
    /// Weighted size of the burst: distinct parts, with large batches and new or deleted
    /// submodels counting heavily.
    pub fn significance(&self) -> f64 {
        let n = (self.added.len() + self.removed.len()) as f64 * 1.0 + self.changed.len() as f64 * 0.5;
        let batch = if self.parts.len() >= 24 { 20.0 } else { 0.0 };
        n + batch + self.files as f64 * 25.0
    }
    /// Decide what to do about an incoming edit; call [`push`](Self::push) afterwards.
    pub fn check(&self, edit: &Edit) -> Due {
        let Some(started) = self.started else {
            return Due::No;
        };
        let outside = edit.parts().any(|p| !self.parts.contains(p)) || edit.files > 0;
        if outside && edit.time - started >= MOVE_ON_SECS {
            return Due::Before("moved on");
        }
        Due::No
    }
    pub fn push(&mut self, edit: &Edit) {
        if edit.is_empty() {
            return;
        }
        self.started.get_or_insert(edit.time);
        self.last = edit.time;
        for p in &edit.added {
            self.removed.remove(p);
            self.added.insert(*p);
        }
        for p in &edit.removed {
            // Added then removed inside one burst leaves no trace.
            if !self.added.remove(p) {
                self.removed.insert(*p);
            }
            self.changed.remove(p);
        }
        for p in &edit.changed {
            if !self.added.contains(p) {
                self.changed.insert(*p);
            }
        }
        self.parts.extend(edit.parts().copied());
        self.files += edit.files;
    }
    /// After pushing: whether the burst itself is due (size threshold or idleness at `now`).
    pub fn due(&self, now: f64) -> Due {
        if self.started.is_none() {
            return Due::No;
        }
        if self.significance() >= THRESHOLD {
            return Due::Now("large edit");
        }
        if now - self.last >= IDLE_SECS {
            return Due::Now("idle");
        }
        Due::No
    }
    pub fn clear(&mut self) {
        *self = Burst::default();
    }
    /// A one-line description for the commit message.
    pub fn describe(&self) -> String {
        let mut bits = Vec::new();
        if !self.added.is_empty() {
            bits.push(format!("+{}", self.added.len()));
        }
        if !self.removed.is_empty() {
            bits.push(format!("-{}", self.removed.len()));
        }
        if !self.changed.is_empty() {
            bits.push(format!("~{}", self.changed.len()));
        }
        if self.files > 0 {
            bits.push(format!("{} submodel(s)", self.files));
        }
        if bits.is_empty() {
            "no changes".into()
        } else {
            bits.join(" ")
        }
    }
}

/// Checkpoint commits to delete under the retention policy: keep every checkpoint from the last
/// hour, the newest per hour for a day, the newest per day after that. Commits of any other kind
/// (save, accept, import, branch, restore) are never thinned. `commits` is `(id, time, kind)`.
pub fn thin(commits: &[(i64, f64, &str)], now: f64) -> Vec<i64> {
    let mut keep_bucket = std::collections::HashSet::new();
    let mut sorted: Vec<_> = commits.iter().filter(|c| c.2 == "checkpoint").collect();
    sorted.sort_by(|a, b| b.1.total_cmp(&a.1));
    let mut doomed = Vec::new();
    for (id, time, _) in sorted {
        let age = now - time;
        let bucket = if age < 3600.0 {
            continue;
        } else if age < 86400.0 {
            (1, (time / 3600.0).floor() as i64)
        } else {
            (2, (time / 86400.0).floor() as i64)
        };
        if !keep_bucket.insert(bucket) {
            doomed.push(*id);
        }
    }
    doomed
}

#[cfg(test)]
mod tests {
    use super::*;
    fn edit(changed: &[u64], t: f64) -> Edit {
        Edit {
            changed: changed.iter().copied().collect(),
            time: t,
            ..Default::default()
        }
    }

    #[test]
    fn repeated_edits_to_one_part_make_one_burst() {
        let mut b = Burst::default();
        for k in 0..3 {
            let e = edit(&[7], 10.0 * k as f64);
            assert_eq!(b.check(&e), Due::No);
            b.push(&e);
            assert_eq!(b.due(e.time), Due::No);
        }
        assert_eq!(b.parts.len(), 1);
        assert_eq!(b.due(20.0 + IDLE_SECS), Due::Now("idle"));
    }

    #[test]
    fn moving_to_new_parts_after_a_minute_checkpoints_first() {
        let mut b = Burst::default();
        b.push(&edit(&[1], 0.0));
        assert_eq!(b.check(&edit(&[2], 30.0)), Due::No);
        assert_eq!(b.check(&edit(&[2], 61.0)), Due::Before("moved on"));
        assert_eq!(b.check(&edit(&[1], 61.0)), Due::No);
    }

    #[test]
    fn big_batches_and_submodels_are_significant() {
        let mut b = Burst::default();
        b.push(&Edit {
            added: (0..30).collect(),
            time: 0.0,
            ..Default::default()
        });
        assert_eq!(b.due(1.0), Due::Now("large edit"));
        let mut s = Burst::default();
        s.push(&Edit {
            files: 2,
            time: 0.0,
            ..Default::default()
        });
        assert_eq!(s.due(0.0), Due::Now("large edit"));
    }

    #[test]
    fn add_then_remove_cancels() {
        let mut b = Burst::default();
        b.push(&Edit {
            added: [5].into(),
            time: 0.0,
            ..Default::default()
        });
        b.push(&Edit {
            removed: [5].into(),
            time: 1.0,
            ..Default::default()
        });
        assert_eq!(b.describe(), "no changes");
    }

    #[test]
    fn thinning_keeps_recent_and_one_per_bucket() {
        let now = 10.0 * 86400.0;
        let c = vec![
            (1, now - 60.0, "checkpoint"),
            (2, now - 120.0, "checkpoint"),
            (3, now - 2.0 * 3600.0 - 10.0, "checkpoint"),
            (4, now - 2.0 * 3600.0 - 20.0, "checkpoint"),
            (5, now - 3.0 * 86400.0 - 100.0, "checkpoint"),
            (6, now - 3.0 * 86400.0 - 160.0, "checkpoint"),
            (7, now - 3.0 * 86400.0 - 120.0, "save"),
        ];
        let d = thin(&c, now);
        assert_eq!(d, vec![4, 6]);
    }
}
