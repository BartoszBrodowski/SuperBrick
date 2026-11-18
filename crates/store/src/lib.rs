//! SuperBrick project files (`.sb`).
//!
//! A project is one SQLite database in WAL mode. It holds the LDraw files (one blob per file plus
//! a parallel blob of stable part ids), branches and commits, every user's drafts, saved
//! selections and candidate sets, preferences and generated assets. Several processes (the GUI
//! and any number of agents) open the same file; every write is a short `BEGIN IMMEDIATE`
//! transaction with retry, and the `changes` table is the feed their watchers poll.
//!
//! Rules the callers follow:
//! - one process (the lease holder, normally the GUI) writes a branch's working tree;
//! - agents only write drafts, observations, selections, candidates and assets;
//! - all expensive work (serialising, compressing) happens before a transaction begins.
pub mod checkpoint;
mod records;
mod schema;

pub use records::*;

use engine::{Id, IdMap, Model};
use ldraw::Document;
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub type Result<T> = std::result::Result<T, String>;

/// The default branch every project starts with.
pub const MAIN: &str = "main";
/// How many part ids a process reserves at a time.
pub const ID_BLOCK: u64 = 4096;
/// A lease not renewed for this long may be taken over.
pub const LEASE_STALE_SECS: f64 = 10.0;

pub fn now() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs_f64())
        .unwrap_or(0.0)
}

/// Error inside a write transaction: SQL errors are retried when the database is busy.
#[derive(Debug)]
pub enum TxError {
    Sql(rusqlite::Error),
    Msg(String),
}
impl From<rusqlite::Error> for TxError {
    fn from(e: rusqlite::Error) -> Self {
        TxError::Sql(e)
    }
}
impl From<String> for TxError {
    fn from(e: String) -> Self {
        TxError::Msg(e)
    }
}
impl From<&str> for TxError {
    fn from(e: &str) -> Self {
        TxError::Msg(e.into())
    }
}
fn busy(e: &rusqlite::Error) -> bool {
    matches!(
        e,
        rusqlite::Error::SqliteFailure(f, _)
            if matches!(f.code, rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked)
    )
}
pub(crate) fn sql(e: rusqlite::Error) -> String {
    e.to_string()
}

#[derive(Clone, Debug, Serialize, PartialEq)]
pub struct Working {
    pub branch: String,
    pub tree: String,
    pub seq: i64,
    pub author: String,
    pub time: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Commit {
    pub id: i64,
    pub branch: String,
    pub parent: Option<i64>,
    pub tree: String,
    pub kind: String,
    pub author: String,
    pub message: String,
    pub drafts: Vec<u64>,
    pub time: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Branch {
    pub name: String,
    pub head: Option<i64>,
    pub base: Option<i64>,
    pub created: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Lease {
    pub branch: String,
    pub holder: String,
    pub pid: u32,
    pub renewed: f64,
}
#[derive(Clone, Debug, Serialize)]
pub struct Change {
    pub seq: i64,
    pub origin: String,
    pub user: String,
    pub kind: String,
    pub reference: String,
    pub time: f64,
}

/// A tree serialised and compressed outside any transaction, ready to insert.
pub struct PreparedTree {
    pub hash: String,
    files: Vec<(i64, String, (String, Vec<u8>, i64), (String, Vec<u8>, i64))>,
    next_id: u64,
}

pub struct Store {
    conn: Connection,
    path: PathBuf,
    origin: String,
    /// Blob hashes this connection knows exist, to skip recompressing unchanged files.
    known: std::collections::HashSet<String>,
}

impl Store {
    /// Create a new project holding one empty `main.ldr`.
    pub fn create(path: &Path, title: &str) -> Result<Store> {
        let doc = Document::new("main.ldr", title);
        let model = Model::from_document(doc, None);
        Self::create_with(path, &model, "human", "new project")
    }
    /// Create a new project from an existing LDraw document (`sb model.ldr` → save as `.sb`).
    pub fn import(path: &Path, doc: Document, author: &str) -> Result<Store> {
        let model = Model::from_document(doc, None);
        Self::create_with(path, &model, author, "imported")
    }
    /// Create a new project from a model, keeping its part ids.
    pub fn create_with(path: &Path, model: &Model, author: &str, message: &str) -> Result<Store> {
        if path.exists() {
            return Err(format!("{} already exists", path.display()));
        }
        let mut s = Self::connect(path)?;
        let next = model.parts.keys().next_back().copied().unwrap_or(0) + 1;
        s.write(|tx| {
            tx.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('next_part_id',?1)",
                [next.to_string()],
            )?;
            Ok(())
        })?;
        s.publish(MAIN, model, None, author)?;
        s.commit(MAIN, "import", author, message, &[])?;
        Ok(s)
    }
    /// Open an existing project.
    pub fn open(path: &Path) -> Result<Store> {
        if !path.exists() {
            return Err(format!("{} does not exist", path.display()));
        }
        Self::connect(path)
    }
    fn connect(path: &Path) -> Result<Store> {
        let conn = Connection::open(path).map_err(sql)?;
        conn.busy_timeout(Duration::from_millis(2000)).map_err(sql)?;
        let mode: String = conn
            .query_row("PRAGMA journal_mode=WAL", [], |r| r.get(0))
            .map_err(sql)?;
        if !mode.eq_ignore_ascii_case("wal") && std::env::var_os("SB_ALLOW_NO_WAL").is_none() {
            return Err(format!(
                "{} cannot use write-ahead logging (journal mode '{mode}'); network and \
                 cloud-synced folders are not supported. Move the project to a local folder \
                 or set SB_ALLOW_NO_WAL=1 to open it anyway.",
                path.display()
            ));
        }
        conn.execute_batch("PRAGMA synchronous=NORMAL; PRAGMA foreign_keys=OFF;")
            .map_err(sql)?;
        schema::migrate(&conn)?;
        Ok(Store {
            conn,
            path: path.to_path_buf(),
            origin: format!("{}:{}", std::process::id(), now()),
            known: Default::default(),
        })
    }
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Tag written into `changes.origin`, so a watcher can skip its own writes.
    pub fn origin(&self) -> &str {
        &self.origin
    }
    pub fn set_origin(&mut self, origin: impl Into<String>) {
        self.origin = origin.into();
    }
    pub fn connection(&self) -> &Connection {
        &self.conn
    }

    /// Run `f` in a `BEGIN IMMEDIATE` transaction, retrying with jitter while the database is
    /// busy. `f` may run more than once, so it must only touch the database.
    pub fn write<T>(
        &mut self,
        mut f: impl FnMut(&rusqlite::Transaction) -> std::result::Result<T, TxError>,
    ) -> Result<T> {
        let delays = [50u64, 150, 400, 1000];
        let mut attempt = 0;
        loop {
            let result = (|| {
                let tx = self
                    .conn
                    .transaction_with_behavior(TransactionBehavior::Immediate)?;
                let value = f(&tx)?;
                tx.commit()?;
                Ok::<T, TxError>(value)
            })();
            match result {
                Ok(v) => return Ok(v),
                Err(TxError::Sql(e)) if busy(&e) && attempt < delays.len() => {
                    let jitter = (now().fract() * 50.0) as u64;
                    std::thread::sleep(Duration::from_millis(delays[attempt] + jitter));
                    attempt += 1;
                }
                Err(TxError::Sql(e)) => return Err(sql(e)),
                Err(TxError::Msg(m)) => return Err(m),
            }
        }
    }
    /// Changes whenever another connection commits; cheap enough to poll every 200 ms.
    pub fn data_version(&self) -> Result<i64> {
        self.conn
            .query_row("PRAGMA data_version", [], |r| r.get(0))
            .map_err(sql)
    }
    pub(crate) fn log_change(
        tx: &rusqlite::Transaction,
        origin: &str,
        user: &str,
        kind: &str,
        reference: &str,
    ) -> rusqlite::Result<i64> {
        tx.execute(
            "INSERT INTO changes(origin,user,kind,ref,time) VALUES(?1,?2,?3,?4,?5)",
            params![origin, user, kind, reference, now()],
        )?;
        Ok(tx.last_insert_rowid())
    }
    pub fn changes_since(&self, seq: i64) -> Result<Vec<Change>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT seq,origin,user,kind,ref,time FROM changes WHERE seq>?1 ORDER BY seq",
            )
            .map_err(sql)?;
        st.query_map([seq], |r| {
            Ok(Change {
                seq: r.get(0)?,
                origin: r.get(1)?,
                user: r.get(2)?,
                kind: r.get(3)?,
                reference: r.get(4)?,
                time: r.get(5)?,
            })
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql)
    }
    pub fn last_change(&self) -> Result<i64> {
        self.conn
            .query_row("SELECT COALESCE(MAX(seq),0) FROM changes", [], |r| r.get(0))
            .map_err(sql)
    }

    // ---- meta and ids ------------------------------------------------------------------------
    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        self.conn
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()
            .map_err(sql)
    }
    fn bump(tx: &rusqlite::Transaction, key: &str, n: u64, default: u64) -> rusqlite::Result<u64> {
        let cur: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?;
        let start = cur.and_then(|v| v.parse().ok()).unwrap_or(default);
        tx.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
            params![key, (start + n).to_string()],
        )?;
        Ok(start)
    }
    /// Reserve `n` part ids for this process: `[start, end)`.
    pub fn reserve_ids(&mut self, n: u64) -> Result<(Id, Id)> {
        let start = self.write(|tx| Ok(Self::bump(tx, "next_part_id", n, 1)?))?;
        Ok((start, start + n))
    }
    /// A reserver for [`engine::Engine::set_id_reserver`]: it opens its own connection, so it can
    /// be moved into the engine's thread.
    pub fn id_reserver(path: PathBuf) -> Box<dyn FnMut() -> Option<(Id, Id)> + Send> {
        Self::id_reserver_sized(path, ID_BLOCK)
    }
    /// Like [`Store::id_reserver`] with a chosen block size (small for one-shot CLI calls).
    pub fn id_reserver_sized(path: PathBuf, block: u64) -> Box<dyn FnMut() -> Option<(Id, Id)> + Send> {
        let mut store: Option<Store> = None;
        Box::new(move || {
            if store.is_none() {
                store = Store::open(&path).ok();
            }
            store.as_mut()?.reserve_ids(block.max(1)).ok()
        })
    }
    /// Give back the unused tail `[next, end)` of an id block — only if nobody reserved after
    /// it (the counter still equals `end`). Returns whether the ids were returned.
    pub fn release_ids(&mut self, next: Id, end: Id) -> Result<bool> {
        self.write(|tx| {
            let n = tx.execute(
                "UPDATE meta SET value=?1 WHERE key='next_part_id' AND value=?2",
                params![next.to_string(), end.to_string()],
            )?;
            Ok(n == 1)
        })
    }
    /// Reserve `n` consecutive draft ids, so drafts from different processes never collide.
    pub fn reserve_draft_ids(&mut self, n: u64) -> Result<u64> {
        self.write(|tx| Ok(Self::bump(tx, "next_draft_id", n, 1)?))
    }

    // ---- blobs ------------------------------------------------------------------------------
    /// Hash and (unless `raw`) compress bytes; done outside transactions.
    pub fn prepare_blob(bytes: &[u8], raw: bool) -> (String, Vec<u8>, i64) {
        let hash = blake3::hash(bytes).to_hex().to_string();
        if raw {
            (hash, bytes.to_vec(), 0)
        } else {
            (hash, miniz_oxide::deflate::compress_to_vec(bytes, 6), 1)
        }
    }
    pub(crate) fn insert_blob(
        tx: &rusqlite::Transaction,
        blob: &(String, Vec<u8>, i64),
        size: usize,
    ) -> rusqlite::Result<()> {
        tx.execute(
            "INSERT OR IGNORE INTO blobs(hash,codec,size,data) VALUES(?1,?2,?3,?4)",
            params![blob.0, blob.2, size as i64, blob.1],
        )?;
        Ok(())
    }
    pub fn blob(&self, hash: &str) -> Result<Vec<u8>> {
        let (codec, data): (i64, Vec<u8>) = self
            .conn
            .query_row(
                "SELECT codec,data FROM blobs WHERE hash=?1",
                [hash],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(|e| format!("missing blob {hash}: {e}"))?;
        match codec {
            0 => Ok(data),
            1 => miniz_oxide::inflate::decompress_to_vec(&data)
                .map_err(|e| format!("corrupt blob {hash}: {e:?}")),
            c => Err(format!("unknown blob codec {c}")),
        }
    }

    // ---- trees ------------------------------------------------------------------------------
    /// Serialise a model into per-file text and id blobs (no database access).
    pub fn prepare_tree(&self, model: &Model) -> PreparedTree {
        let map = model.id_map();
        let mut files = Vec::new();
        let mut hasher = blake3::Hasher::new();
        let eol = |l: &ldraw::Line| l.eol.as_str();
        let pre: String = model
            .doc
            .preamble
            .iter()
            .map(|l| format!("{}{}", l.out_text(), eol(l)))
            .collect();
        let mut push = |idx: i64, name: &str, text: String, ids: Vec<u8>| {
            let t = self.prepare_known(text.as_bytes());
            let i = self.prepare_known(&ids);
            hasher.update(format!("{idx}\0{name}\0{}\0{}\n", t.0, i.0).as_bytes());
            files.push((idx, name.to_string(), t, i));
        };
        if !pre.is_empty() {
            push(-1, "", pre, Vec::new());
        }
        for (k, f) in model.doc.files.iter().enumerate() {
            let text: String = f
                .lines
                .iter()
                .map(|l| format!("{}{}", l.out_text(), eol(l)))
                .collect();
            let mut ids = Vec::new();
            for (id, flags) in map.get(k).map(|v| v.as_slice()).unwrap_or(&[]) {
                ids.extend_from_slice(&id.to_le_bytes());
                ids.push(*flags);
            }
            push(k as i64, &f.name, text, ids);
        }
        PreparedTree {
            hash: hasher.finalize().to_hex().to_string(),
            files,
            next_id: model.parts.keys().next_back().copied().unwrap_or(0) + 1,
        }
    }
    /// Like [`prepare_blob`](Self::prepare_blob) but skips compression for blobs this connection
    /// has already stored (unchanged files of a large model).
    fn prepare_known(&self, bytes: &[u8]) -> (String, Vec<u8>, i64) {
        let hash = blake3::hash(bytes).to_hex().to_string();
        if self.known.contains(&hash) {
            (hash, Vec::new(), -1)
        } else {
            let mut b = Self::prepare_blob(bytes, false);
            b.0 = hash;
            b
        }
    }
    pub(crate) fn insert_tree(tx: &rusqlite::Transaction, t: &PreparedTree) -> rusqlite::Result<()> {
        let exists: Option<i64> = tx
            .query_row("SELECT 1 FROM trees WHERE hash=?1", [&t.hash], |r| r.get(0))
            .optional()?;
        if exists.is_some() {
            return Ok(());
        }
        tx.execute(
            "INSERT INTO trees(hash,created) VALUES(?1,?2)",
            params![t.hash, now()],
        )?;
        for (idx, name, text, ids) in &t.files {
            for b in [text, ids] {
                if b.2 >= 0 {
                    Self::insert_blob(tx, b, 0)?;
                }
            }
            tx.execute(
                "INSERT INTO tree_files(tree,idx,name,text,ids) VALUES(?1,?2,?3,?4,?5)",
                params![t.hash, idx, name, text.0, ids.0],
            )?;
        }
        // Ids above the reserved range (imported or human-made) must never be handed out again.
        let cur: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key='next_part_id'", [], |r| r.get(0))
            .optional()?;
        let cur: u64 = cur.and_then(|v| v.parse().ok()).unwrap_or(1);
        if t.next_id > cur {
            tx.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('next_part_id',?1)",
                [t.next_id.to_string()],
            )?;
        }
        Ok(())
    }
    fn remember(&mut self, t: &PreparedTree) {
        for (_, _, a, b) in &t.files {
            self.known.insert(a.0.clone());
            self.known.insert(b.0.clone());
        }
    }
    pub fn put_tree(&mut self, model: &Model) -> Result<String> {
        let t = self.prepare_tree(model);
        self.write(|tx| Ok(Self::insert_tree(tx, &t)?))?;
        self.remember(&t);
        Ok(t.hash)
    }
    /// Rebuild a model, with its stable ids, from a stored tree.
    pub fn load_tree(&self, hash: &str) -> Result<Model> {
        let rows: Vec<(i64, String, String, String)> = {
            let mut st = self
                .conn
                .prepare_cached("SELECT idx,name,text,ids FROM tree_files WHERE tree=?1 ORDER BY idx")
                .map_err(sql)?;
            st.query_map([hash], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(sql)?
                .collect::<rusqlite::Result<_>>()
                .map_err(sql)?
        };
        if rows.iter().all(|r| r.0 < 0) {
            return Err(format!("tree {hash} has no files"));
        }
        let mut bytes = Vec::new();
        let mut map: IdMap = Vec::new();
        let mut first = None;
        for (idx, name, text, ids) in &rows {
            bytes.extend(self.blob(text)?);
            if *idx < 0 {
                continue;
            }
            first.get_or_insert(name.clone());
            let raw = self.blob(ids)?;
            map.push(
                raw.chunks_exact(9)
                    .map(|c| (u64::from_le_bytes(c[..8].try_into().unwrap()), c[8]))
                    .collect(),
            );
        }
        let doc = Document::parse(&bytes, first.as_deref().unwrap_or("main.ldr"));
        if doc.files.len() != map.len() {
            return Err(format!(
                "tree {hash}: {} files stored but {} parsed",
                map.len(),
                doc.files.len()
            ));
        }
        Ok(Model::from_document_with_ids(doc, None, &map))
    }

    // ---- working trees ----------------------------------------------------------------------
    pub fn working(&self, branch: &str) -> Result<Working> {
        self.conn
            .query_row(
                "SELECT branch,tree,seq,author,time FROM working WHERE branch=?1",
                [branch],
                |r| {
                    Ok(Working {
                        branch: r.get(0)?,
                        tree: r.get(1)?,
                        seq: r.get(2)?,
                        author: r.get(3)?,
                        time: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(sql)?
            .ok_or_else(|| format!("no branch '{branch}'"))
    }
    /// The branch's current model and the working row it came from.
    pub fn load(&self, branch: &str) -> Result<(Working, Model)> {
        let w = self.working(branch)?;
        let m = self.load_tree(&w.tree)?;
        Ok((w, m))
    }
    /// Write a branch's working tree. With `expected` set this is a compare-and-swap on the
    /// branch's sequence number and fails with "conflict" if someone else published first.
    pub fn publish(
        &mut self,
        branch: &str,
        model: &Model,
        expected: Option<i64>,
        author: &str,
    ) -> Result<i64> {
        let t = self.prepare_tree(model);
        let origin = self.origin.clone();
        let seq = self.write(|tx| {
            let cur: Option<(i64, String)> = tx
                .query_row(
                    "SELECT seq,tree FROM working WHERE branch=?1",
                    [branch],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            if let (Some(want), Some((have, _))) = (expected, &cur) {
                if want != *have {
                    return Err(TxError::Msg(format!(
                        "conflict: {branch} is at {have}, expected {want}"
                    )));
                }
            }
            if let Some((have, tree)) = &cur {
                if *tree == t.hash {
                    return Ok(*have);
                }
            }
            Self::insert_tree(tx, &t)?;
            let seq = cur.map(|c| c.0).unwrap_or(0) + 1;
            tx.execute(
                "INSERT OR REPLACE INTO working(branch,tree,seq,author,time) VALUES(?1,?2,?3,?4,?5)",
                params![branch, t.hash, seq, author, now()],
            )?;
            tx.execute(
                "INSERT OR IGNORE INTO branches(name,head,base,created) VALUES(?1,NULL,NULL,?2)",
                params![branch, now()],
            )?;
            Self::log_change(tx, &origin, author, "working", branch)?;
            Ok(seq)
        })?;
        self.remember(&t);
        Ok(seq)
    }

    // ---- commits and branches ---------------------------------------------------------------
    /// Record the branch's current working tree as a commit. A commit identical to its parent's
    /// tree is skipped and the parent returned (so repeated saves cost nothing).
    pub fn commit(
        &mut self,
        branch: &str,
        kind: &str,
        author: &str,
        message: &str,
        drafts: &[u64],
    ) -> Result<i64> {
        let tree = self.working(branch)?.tree;
        self.commit_tree(branch, &tree, kind, author, message, drafts)
    }
    pub fn commit_tree(
        &mut self,
        branch: &str,
        tree: &str,
        kind: &str,
        author: &str,
        message: &str,
        drafts: &[u64],
    ) -> Result<i64> {
        let origin = self.origin.clone();
        let drafts_json = serde_json::to_string(drafts).unwrap();
        self.write(|tx| {
            let head: Option<i64> = tx
                .query_row("SELECT head FROM branches WHERE name=?1", [branch], |r| r.get(0))
                .optional()?
                .flatten();
            if let Some(h) = head {
                let parent_tree: String =
                    tx.query_row("SELECT tree FROM commits WHERE id=?1", [h], |r| r.get(0))?;
                if parent_tree == tree && drafts.is_empty() {
                    return Ok(h);
                }
            }
            tx.execute(
                "INSERT INTO commits(branch,parent,tree,kind,author,message,drafts,time) \
                 VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![branch, head, tree, kind, author, message, drafts_json, now()],
            )?;
            let id = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO branches(name,head,base,created) VALUES(?1,?2,NULL,?3) \
                 ON CONFLICT(name) DO UPDATE SET head=excluded.head",
                params![branch, id, now()],
            )?;
            Self::log_change(tx, &origin, author, "commit", &id.to_string())?;
            Ok(id)
        })
    }
    pub fn log(&self, branch: &str, limit: usize) -> Result<Vec<Commit>> {
        let mut st = self
            .conn
            .prepare_cached(
                "SELECT id,branch,parent,tree,kind,author,message,drafts,time FROM commits \
                 WHERE branch=?1 ORDER BY id DESC LIMIT ?2",
            )
            .map_err(sql)?;
        st.query_map(params![branch, limit as i64], row_commit)
            .map_err(sql)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql)
    }
    pub fn commit_by_id(&self, id: i64) -> Result<Commit> {
        self.conn
            .query_row(
                "SELECT id,branch,parent,tree,kind,author,message,drafts,time FROM commits WHERE id=?1",
                [id],
                row_commit,
            )
            .map_err(|e| format!("no commit {id}: {e}"))
    }
    pub fn branches(&self) -> Result<Vec<Branch>> {
        let mut st = self
            .conn
            .prepare_cached("SELECT name,head,base,created FROM branches ORDER BY created")
            .map_err(sql)?;
        st.query_map([], |r| {
            Ok(Branch {
                name: r.get(0)?,
                head: r.get(1)?,
                base: r.get(2)?,
                created: r.get(3)?,
            })
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql)
    }
    /// A named variant: a new branch whose working tree starts at `from` (a commit) or at the
    /// current working tree of `main`.
    pub fn create_branch(&mut self, name: &str, from: Option<i64>, author: &str) -> Result<()> {
        if name.is_empty() || name.contains(char::is_whitespace) {
            return Err("branch names must be non-empty and contain no spaces".into());
        }
        if self.working(name).is_ok() {
            return Err(format!("branch '{name}' already exists"));
        }
        let (tree, base) = match from {
            Some(c) => (self.commit_by_id(c)?.tree, Some(c)),
            None => (self.working(MAIN)?.tree, self.branches()?.iter().find(|b| b.name == MAIN).and_then(|b| b.head)),
        };
        let origin = self.origin.clone();
        self.write(|tx| {
            tx.execute(
                "INSERT INTO working(branch,tree,seq,author,time) VALUES(?1,?2,1,?3,?4)",
                params![name, tree, author, now()],
            )?;
            tx.execute(
                "INSERT INTO branches(name,head,base,created) VALUES(?1,NULL,?2,?3)",
                params![name, base, now()],
            )?;
            Self::log_change(tx, &origin, author, "branch", name)?;
            Ok(())
        })?;
        self.commit(name, "branch", author, &format!("branched from {}", from.map(|c| format!("commit {c}")).unwrap_or_else(|| MAIN.into())), &[])?;
        Ok(())
    }
    /// Put an old commit's tree back as the branch's working tree (recorded as a commit).
    pub fn restore(&mut self, branch: &str, commit: i64, author: &str) -> Result<i64> {
        let c = self.commit_by_id(commit)?;
        let model = self.load_tree(&c.tree)?;
        self.publish(branch, &model, None, author)?;
        self.commit(branch, "restore", author, &format!("restored commit {commit}"), &[])
    }

    // ---- leases -----------------------------------------------------------------------------
    pub fn lease(&self, branch: &str) -> Result<Option<Lease>> {
        self.conn
            .query_row(
                "SELECT branch,holder,pid,renewed FROM leases WHERE branch=?1",
                [branch],
                |r| {
                    Ok(Lease {
                        branch: r.get(0)?,
                        holder: r.get(1)?,
                        pid: r.get(2)?,
                        renewed: r.get(3)?,
                    })
                },
            )
            .optional()
            .map_err(sql)
    }
    /// Take (or renew) the right to write `branch`'s working tree. Fails over a live holder;
    /// takes over a lease that is stale or whose process is gone.
    pub fn acquire_lease(&mut self, branch: &str, holder: &str) -> Result<bool> {
        let pid = std::process::id();
        let origin = self.origin.clone();
        self.write(|tx| {
            let cur: Option<(String, u32, f64)> = tx
                .query_row(
                    "SELECT holder,pid,renewed FROM leases WHERE branch=?1",
                    [branch],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .optional()?;
            if let Some((h, p, renewed)) = &cur {
                let mine = h == holder;
                let alive = now() - renewed < LEASE_STALE_SECS && pid_alive(*p);
                if !mine && alive {
                    return Ok(false);
                }
            }
            tx.execute(
                "INSERT OR REPLACE INTO leases(branch,holder,pid,renewed) VALUES(?1,?2,?3,?4)",
                params![branch, holder, pid, now()],
            )?;
            if cur.as_ref().is_none_or(|c| c.0 != holder) {
                Self::log_change(tx, &origin, holder, "lease", branch)?;
            }
            Ok(true)
        })
    }
    pub fn release_lease(&mut self, branch: &str, holder: &str) -> Result<()> {
        self.write(|tx| {
            tx.execute(
                "DELETE FROM leases WHERE branch=?1 AND holder=?2",
                params![branch, holder],
            )?;
            Ok(())
        })
    }
    /// Whether a live process other than `holder` owns the branch.
    pub fn leased_by_other(&self, branch: &str, holder: &str) -> Result<bool> {
        Ok(self.lease(branch)?.is_some_and(|l| {
            l.holder != holder && now() - l.renewed < LEASE_STALE_SECS && pid_alive(l.pid)
        }))
    }

    // ---- export and maintenance --------------------------------------------------------------
    /// The branch's working tree as LDraw bytes (an MPD when it has submodels).
    pub fn export_ldr(&self, branch: &str) -> Result<Vec<u8>> {
        Ok(engine::export_bytes(&self.load(branch)?.1))
    }
    /// Drop checkpoint commits the retention policy no longer keeps, then unreferenced trees and
    /// blobs. Returns how many rows went.
    pub fn maintain(&mut self, branch: &str) -> Result<usize> {
        let commits = self.log(branch, usize::MAX >> 1)?;
        let rows: Vec<_> = commits
            .iter()
            .map(|c| (c.id, c.time, c.kind.as_str()))
            .collect();
        let doomed = checkpoint::thin(&rows, now());
        let origin = self.origin.clone();
        self.write(|tx| {
            let mut n = 0;
            for id in &doomed {
                let parent: Option<i64> =
                    tx.query_row("SELECT parent FROM commits WHERE id=?1", [id], |r| r.get(0))?;
                tx.execute("UPDATE commits SET parent=?1 WHERE parent=?2", params![parent, id])?;
                tx.execute("UPDATE branches SET head=?1 WHERE head=?2", params![parent, id])?;
                n += tx.execute("DELETE FROM commits WHERE id=?1", [id])?;
            }
            n += tx.execute(
                "DELETE FROM tree_files WHERE tree NOT IN (SELECT tree FROM commits UNION SELECT tree FROM working)",
                [],
            )?;
            n += tx.execute(
                "DELETE FROM trees WHERE hash NOT IN (SELECT tree FROM commits UNION SELECT tree FROM working)",
                [],
            )?;
            n += tx.execute(
                "DELETE FROM blobs WHERE hash NOT IN (SELECT text FROM tree_files UNION SELECT ids FROM tree_files UNION SELECT blob FROM assets)",
                [],
            )?;
            if n > 0 {
                Self::log_change(tx, &origin, "", "maintain", branch)?;
            }
            Ok(n)
        })
        .inspect(|_| self.known.clear())
    }
}

fn row_commit(r: &rusqlite::Row) -> rusqlite::Result<Commit> {
    let drafts: String = r.get(7)?;
    Ok(Commit {
        id: r.get(0)?,
        branch: r.get(1)?,
        parent: r.get(2)?,
        tree: r.get(3)?,
        kind: r.get(4)?,
        author: r.get(5)?,
        message: r.get(6)?,
        drafts: serde_json::from_str(&drafts).unwrap_or_default(),
        time: r.get(8)?,
    })
}

/// Best effort: whether a process with this pid still exists on this machine.
pub fn pid_alive(pid: u32) -> bool {
    ldraw::platform::pid_alive(pid)
}
