//! Version-stamped, transactional agent proposals.
//!
//! A draft is one user's atomic batch of commands layered over the shared model. Its base is
//! the content hash of every user part it read, so only a real conflicting edit makes it stale
//! (undo of unrelated work does not). A draft depends only on earlier pending drafts of the
//! same user whose parts or files it overlaps, so rejecting one draft does not block unrelated
//! later work. Drafts serialise to JSON and replay deterministically from their allocated ids.
use super::{Cmd, Engine, Id, Model, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::{BTreeMap, BTreeSet, VecDeque};

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Touched {
    #[serde(default)]
    pub modified: BTreeSet<Id>,
    #[serde(default)]
    pub created: BTreeSet<Id>,
    #[serde(default)]
    pub removed: BTreeSet<Id>,
    /// Files whose part content changed (by name).
    #[serde(default)]
    pub files: BTreeSet<String>,
    /// Files this draft created.
    #[serde(default)]
    pub new_files: BTreeSet<String>,
    /// Submodels referenced by parts this draft created.
    #[serde(default)]
    pub uses: BTreeSet<String>,
}
impl Touched {
    /// Diff two models: what a batch did.
    pub fn between(before: &Model, after: &Model) -> Self {
        let mut t = Touched::default();
        let fname = |m: &Model, f: usize| m.doc.files.get(f).map(|f| f.name.clone());
        for (id, p) in &after.parts {
            match before.parts.get(id) {
                None => {
                    t.created.insert(*id);
                    t.files.extend(fname(after, p.file));
                    if after.doc.file_index(&p.name).is_some() {
                        t.uses.insert(p.name.to_lowercase());
                    }
                }
                Some(old) if old.content_hash() != p.content_hash() || old.file != p.file => {
                    t.modified.insert(*id);
                    t.files.extend(fname(before, old.file));
                    t.files.extend(fname(after, p.file));
                }
                _ => {}
            }
        }
        for (id, p) in &before.parts {
            if !after.parts.contains_key(id) {
                t.removed.insert(*id);
                t.files.extend(fname(before, p.file));
            }
        }
        let old: BTreeSet<_> = before.doc.files.iter().map(|f| f.name.clone()).collect();
        for f in &after.doc.files {
            if !old.contains(&f.name) {
                t.new_files.insert(f.name.clone());
                t.files.insert(f.name.clone());
            }
        }
        t
    }
    pub fn is_empty(&self) -> bool {
        self.modified.is_empty()
            && self.created.is_empty()
            && self.removed.is_empty()
            && self.new_files.is_empty()
    }
    fn outputs(&self) -> impl Iterator<Item = &Id> {
        self.created
            .iter()
            .chain(&self.modified)
            .chain(&self.removed)
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Draft {
    pub id: u64,
    /// The proposing user ("claude", "agent", …). Each user is in effect a branch.
    pub session: String,
    pub turn: u64,
    #[serde(default)]
    pub label: String,
    pub commands: Vec<Cmd>,
    /// Content hash of each pre-existing part the draft read or edited.
    pub base: BTreeMap<Id, u64>,
    pub dependencies: Vec<u64>,
    /// pending | accepted | rejected | withdrawn | stale | blocked
    pub state: String,
    #[serde(default)]
    pub reason: String,
    /// File names of the base model when proposed (structure check).
    pub files: Vec<String>,
    /// Ids allocated inside the commands, in order; replay hands them out again.
    #[serde(default)]
    pub allocated: Vec<Id>,
    #[serde(default)]
    pub touched: Touched,
    /// Every part id a command names (moving ids, endpoints, articulation pivots).
    #[serde(default)]
    pub refs: BTreeSet<Id>,
    /// A human's note on rejection or review; the agent sees it on its next call.
    #[serde(default)]
    pub note: String,
}
impl Draft {
    fn depends_on(&self, earlier: &Draft) -> bool {
        let out: BTreeSet<_> = earlier.touched.outputs().collect();
        self.refs
            .iter()
            .chain(&self.touched.modified)
            .chain(&self.touched.removed)
            .any(|id| out.contains(id))
            || self
                .touched
                .files
                .iter()
                .chain(&self.touched.uses)
                .any(|f| earlier.touched.new_files.contains(f))
    }
    pub fn is_live(&self) -> bool {
        self.state == "pending"
    }
}

#[derive(Default)]
pub struct Drafts {
    pub ops: Vec<Draft>,
    /// Per user: content hashes of parts it has observed.
    pub seen: BTreeMap<String, BTreeMap<Id, u64>>,
    pub next: u64,
}
impl Drafts {
    pub fn refresh(&mut self, model: &Model) {
        let files: Vec<_> = model.doc.files.iter().map(|f| f.name.clone()).collect();
        let mut bad = BTreeSet::new();
        for op in &mut self.ops {
            if op.state == "pending" && op.files != files {
                op.state = "stale".into();
                op.reason = "document model structure changed".into();
            }
            if op.state == "pending"
                && op.base.iter().any(|(id, hash)| {
                    model
                        .parts
                        .get(id)
                        .is_none_or(|p| p.content_hash() != *hash)
                })
            {
                op.state = "stale".into();
                op.reason = "a referenced user part changed or was deleted".into();
            }
            if op.state == "pending" && op.dependencies.iter().any(|d| bad.contains(d)) {
                op.state = "blocked".into();
                op.reason = "a dependency is unavailable".into();
            }
            if ["stale", "rejected", "blocked", "withdrawn"].contains(&op.state.as_str()) {
                bad.insert(op.id);
            }
        }
    }
    pub fn pending<'a>(&'a self, user: &'a str) -> impl Iterator<Item = &'a Draft> + 'a {
        self.ops
            .iter()
            .filter(move |o| o.session == user && o.state == "pending")
    }
    pub fn users(&self) -> BTreeSet<String> {
        self.ops.iter().map(|o| o.session.clone()).collect()
    }
}

/// One in-progress batch. Commands run on the user's effective model (base + its pending
/// drafts), so later commands can name ids that earlier commands created.
pub struct DraftTx<'a> {
    engine: &'a mut Engine,
    original: &'a Model,
    seen: BTreeMap<Id, u64>,
    base: BTreeMap<Id, u64>,
    refs: BTreeSet<Id>,
    commands: Vec<Cmd>,
    allocated: Vec<Id>,
    results: Vec<Value>,
}
impl DraftTx<'_> {
    pub fn model(&self) -> &Model {
        &self.engine.model
    }
    /// Read-only helpers (placement previews, validation) on the effective model.
    pub fn engine(&mut self) -> &mut Engine {
        self.engine
    }
    pub fn results(&self) -> &[Value] {
        &self.results
    }
    pub fn commands(&self) -> &[Cmd] {
        &self.commands
    }
    /// Check collisions once for the whole batch instead of after every command: commands
    /// apply unchecked until [`DraftTx::check_deferred`].
    pub fn defer_checks(&mut self) {
        self.engine.deferred = Some(Vec::new());
    }
    /// The batch's one collision check over every part its commands changed (and still exist).
    /// Ends deferral; refuses like a single command would (`last_failure` says what clashed).
    pub fn check_deferred(&mut self) -> Result<()> {
        let Some(mut ids) = self.engine.deferred.take() else { return Ok(()) };
        ids.sort_unstable();
        ids.dedup();
        ids.retain(|id| self.engine.model.parts.contains_key(id));
        if ids.is_empty() {
            return Ok(());
        }
        self.engine.validate_changes(&ids)
    }
    /// Run one command. On error nothing from this command remains.
    pub fn apply(&mut self, mut cmd: Cmd) -> Result<Value> {
        if matches!(cmd, Cmd::Select { .. } | Cmd::Hide { .. } | Cmd::Lock { .. }) {
            return Err("view commands (select/hide/lock) are not draft edits".into());
        }
        let refs = cmd.refs();
        for id in &refs {
            if let Some(p) = self.original.parts.get(id) {
                let hash = p.content_hash();
                if self.seen.get(id) != Some(&hash) {
                    return Err(format!(
                        "stale or unseen part #{id}; read the current model before editing it"
                    ));
                }
                self.base.insert(*id, hash);
            } else if !self.engine.model.parts.contains_key(id) {
                return Err(format!("unknown part #{id}"));
            }
        }
        if let Cmd::Add { parts, .. } = &mut cmd {
            for p in parts.iter_mut().filter(|p| p.id.is_none()) {
                p.id = Some(self.engine.allocate());
            }
        }
        let before = self.engine.model.clone();
        let rotation = self.engine.last_rotation.clone();
        self.engine.recorded = Some(Vec::new());
        let result = self.engine.apply(cmd.clone());
        let recorded = self.engine.recorded.take().unwrap_or_default();
        match result {
            Ok(value) => {
                self.allocated.extend(recorded);
                self.refs.extend(refs);
                self.commands.push(cmd);
                self.results.push(value.clone());
                Ok(value)
            }
            Err(e) => {
                self.engine.model = before;
                self.engine.last_rotation = rotation;
                Err(e)
            }
        }
    }
}

/// What a batch produced. `effective` is the user's view after it (base + all its drafts).
pub struct Proposal<T> {
    pub draft: Option<u64>,
    pub touched: Touched,
    pub allocated: Vec<Id>,
    pub results: Vec<Value>,
    pub dependencies: Vec<u64>,
    pub effective: Model,
    pub before: Model,
    pub out: T,
}

impl Engine {
    pub(crate) fn apply_draft(&mut self, op: &Draft) -> Result<Value> {
        self.apply_draft_unchecked(op).map(|(v, _)| v)
    }

    /// Replay a draft without the legality check: it was checked when proposed, and staleness is
    /// caught by content hashes. Returns the parts it changed, for one check of a whole chain.
    pub(crate) fn apply_draft_unchecked(&mut self, op: &Draft) -> Result<(Value, Vec<Id>)> {
        let saved = self.deferred.replace(Vec::new());
        let out = self.apply_draft_inner(op);
        let changed = std::mem::replace(&mut self.deferred, saved).unwrap_or_default();
        out.map(|v| (v, changed))
    }

    fn apply_draft_inner(&mut self, op: &Draft) -> Result<Value> {
        let next = self.next_id;
        let rotation = self.last_rotation.clone();
        self.replay = Some(op.allocated.iter().copied().collect::<VecDeque<_>>());
        let mut result = Ok(Value::Null);
        for c in &op.commands {
            result = self.apply(c.clone());
            if result.is_err() {
                break;
            }
        }
        self.replay = None;
        self.last_rotation = rotation;
        self.next_id = if self.reserve.is_some() {
            next
        } else {
            self.next_id.max(next)
        };
        result
    }
    pub fn observe(&mut self, session: &str) {
        self.drafts.seen.insert(
            session.into(),
            self.model
                .parts
                .iter()
                .map(|(id, p)| (*id, p.content_hash()))
                .collect(),
        );
    }
    /// Mark only these parts observed (a focused read such as a region query).
    pub fn observe_ids(&mut self, session: &str, ids: impl IntoIterator<Item = Id>) {
        let seen = self.drafts.seen.entry(session.into()).or_default();
        for id in ids {
            if let Some(p) = self.model.parts.get(&id) {
                seen.insert(id, p.content_hash());
            }
        }
    }
    /// Single-command proposal (kept for the GUI tests and the legacy MCP).
    pub fn propose(&mut self, session: String, turn: u64, command: Cmd) -> Result<Value> {
        if matches!(
            command,
            Cmd::Select { .. } | Cmd::Hide { .. } | Cmd::Lock { .. }
        ) {
            return Err("agent view commands are not draft edits".into());
        }
        let p = self.propose_with(&session, turn, "", false, |tx| tx.apply(command))?;
        let id = p.draft.unwrap();
        Ok(json!({"draft":id,"state":"pending","result":p.out}))
    }
    /// The user's effective model: base plus its pending drafts (failed replays skipped).
    pub fn effective_model(&mut self, session: &str) -> Model {
        let original = self.model.clone();
        let ops: Vec<_> = self.drafts.pending(session).cloned().collect();
        for op in ops {
            let before = self.model.clone();
            if self.apply_draft(&op).is_err() {
                self.model = before;
            }
        }
        std::mem::replace(&mut self.model, original)
    }
    /// Run a batch atomically as one draft. With `dry_run` nothing is stored, but the
    /// effective result is still returned for rendering and checks.
    pub fn propose_with<T>(
        &mut self,
        session: &str,
        turn: u64,
        label: &str,
        dry_run: bool,
        f: impl FnOnce(&mut DraftTx) -> Result<T>,
    ) -> Result<Proposal<T>> {
        if self.read_only {
            return Err("this project is open read-only here".into());
        }
        self.last_failure = None;
        let original = self.model.clone();
        let previous: Vec<_> = self.drafts.pending(session).cloned().collect();
        for op in previous {
            let before = self.model.clone();
            if self.apply_draft(&op).is_err() {
                self.model = before;
            }
        }
        let before = self.model.clone();
        let seen = self.drafts.seen.get(session).cloned().unwrap_or_default();
        let mut tx = DraftTx {
            engine: self,
            original: &original,
            seen,
            base: BTreeMap::new(),
            refs: BTreeSet::new(),
            commands: Vec::new(),
            allocated: Vec::new(),
            results: Vec::new(),
        };
        let out = f(&mut tx);
        tx.engine.deferred = None;
        let DraftTx {
            base,
            refs,
            commands,
            allocated,
            results,
            ..
        } = tx;
        let effective = std::mem::replace(&mut self.model, original);
        let out = out?;
        if commands.is_empty() {
            return Err("the batch contains no edits".into());
        }
        let touched = Touched::between(&before, &effective);
        let mut draft = Draft {
            id: 0,
            session: session.into(),
            turn,
            label: label.into(),
            commands,
            base,
            dependencies: vec![],
            state: "pending".into(),
            reason: String::new(),
            files: self.model.doc.files.iter().map(|f| f.name.clone()).collect(),
            allocated: allocated.clone(),
            touched: touched.clone(),
            refs,
            note: String::new(),
        };
        draft.dependencies = self
            .drafts
            .pending(session)
            .filter(|e| draft.depends_on(e))
            .map(|e| e.id)
            .collect();
        let dependencies = draft.dependencies.clone();
        let id = if dry_run {
            None
        } else {
            self.drafts.next += 1;
            draft.id = self.drafts.next;
            self.drafts.ops.push(draft);
            Some(self.drafts.next)
        };
        Ok(Proposal {
            draft: id,
            touched,
            allocated,
            results,
            dependencies,
            effective,
            before,
            out,
        })
    }
    /// Re-insert drafts loaded from a project file (ids and states preserved).
    pub fn load_drafts(&mut self, ops: Vec<Draft>) {
        self.drafts.next = self
            .drafts
            .next
            .max(ops.iter().map(|o| o.id).max().unwrap_or(0));
        let known: BTreeSet<_> = self.drafts.ops.iter().map(|o| o.id).collect();
        self.drafts
            .ops
            .extend(ops.into_iter().filter(|o| !known.contains(&o.id)));
        self.drafts.ops.sort_by_key(|o| o.id);
        self.refresh_drafts();
    }
    pub fn accept(&mut self, session: &str, ids: Option<Vec<u64>>) -> Result<Value> {
        if self.read_only {
            return Err("this project is open read-only here".into());
        }
        self.drafts.refresh(&self.model);
        let mut wanted: BTreeSet<_> = ids
            .unwrap_or_else(|| self.drafts.pending(session).map(|o| o.id).collect())
            .into_iter()
            .collect();
        loop {
            let before = wanted.len();
            for op in &self.drafts.ops {
                if wanted.contains(&op.id) {
                    wanted.extend(&op.dependencies);
                }
            }
            if wanted.len() == before {
                break;
            }
        }
        let ops: Vec<_> = self
            .drafts
            .ops
            .iter()
            .filter(|o| wanted.contains(&o.id))
            .cloned()
            .collect();
        if ops.len() != wanted.len()
            || ops.iter().any(|o| {
                o.session != session || !["pending", "accepted"].contains(&o.state.as_str())
            })
        {
            return Err(
                "draft is stale, blocked, rejected, missing, or belongs to another session".into(),
            );
        }
        let old = self.model.clone();
        for op in ops.iter().filter(|o| o.state == "pending") {
            if let Err(err) = self.apply_draft(op) {
                self.model = old;
                if let Some(d) = self.drafts.ops.iter_mut().find(|d| d.id == op.id) {
                    d.state = "blocked".into();
                    d.reason = err.clone();
                }
                return Err(err);
            }
        }
        if ops.iter().any(|o| o.state == "pending") {
            for p in self.model.parts.values_mut() {
                if old
                    .parts
                    .get(&p.id)
                    .is_none_or(|o| o.content_hash() != p.content_hash())
                {
                    p.revision = self.revision + 1;
                }
            }
            let affected: BTreeMap<_, _> = self
                .model
                .parts
                .iter()
                .filter(|(id, p)| {
                    old.parts
                        .get(id)
                        .is_none_or(|o| o.content_hash() != p.content_hash())
                })
                .map(|(id, p)| (*id, p.content_hash()))
                .collect();
            self.undo.push(old);
            self.redo.clear();
            self.revision += 1;
            for op in &mut self.drafts.ops {
                if wanted.contains(&op.id) {
                    op.state = "accepted".into();
                }
            }
            // Acceptance is not a conflicting user edit to later work in this same draft.
            // Rebase its stamps, but never grant another session a fresh observation.
            let files: Vec<_> = self
                .model
                .doc
                .files
                .iter()
                .map(|f| f.name.clone())
                .collect();
            for op in self
                .drafts
                .ops
                .iter_mut()
                .filter(|o| o.session == session && o.state == "pending")
            {
                for (id, hash) in &mut op.base {
                    if let Some(new) = affected.get(id) {
                        *hash = *new;
                    }
                }
                op.files = files.clone();
            }
            self.drafts
                .seen
                .entry(session.into())
                .or_default()
                .extend(affected);
            self.refresh_drafts();
        }
        Ok(json!({"accepted":wanted}))
    }
    pub fn reject(&mut self, session: &str, ids: Vec<u64>) -> Result<()> {
        self.reject_with_note(session, ids, "")
    }
    /// Reject drafts, optionally telling the agent why.
    pub fn reject_with_note(&mut self, session: &str, ids: Vec<u64>, note: &str) -> Result<()> {
        self.set_state(session, &ids, "rejected", note)
    }
    /// An agent takes back its own drafts.
    pub fn withdraw(&mut self, session: &str, ids: Vec<u64>) -> Result<()> {
        self.set_state(session, &ids, "withdrawn", "")
    }
    fn set_state(&mut self, session: &str, ids: &[u64], state: &str, note: &str) -> Result<()> {
        for id in ids {
            let d = self
                .drafts
                .ops
                .iter()
                .find(|d| d.id == *id)
                .ok_or("unknown draft")?;
            if d.session != session || d.state == "accepted" {
                return Err(format!("cannot {} draft {id}", state.trim_end_matches('d')));
            }
        }
        for d in &mut self.drafts.ops {
            if ids.contains(&d.id) {
                d.state = state.into();
                if !note.is_empty() {
                    d.note = note.into();
                }
            }
        }
        self.drafts.refresh(&self.model);
        Ok(())
    }
}
