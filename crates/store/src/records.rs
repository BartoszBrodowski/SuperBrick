//! Per-user records kept beside the model: drafts, observations, selections, candidate sets,
//! preferences and generated assets.
use crate::{Result, Store, now, sql};
use engine::{Id, drafts::Draft};
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
pub struct Selection {
    pub id: String,
    pub user: String,
    pub name: Option<String>,
    pub file: usize,
    pub parts: Vec<Id>,
    /// The smart-select query that produced it, if any (re-runnable).
    pub query: Option<Value>,
    pub created: f64,
    pub pinned: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateSet {
    pub id: String,
    pub user: String,
    /// Whatever the producing op needs to commit a choice (part spec, poses, labels).
    pub json: Value,
    /// The working tree the candidates were computed against; stale once the tree moves on.
    pub tree: String,
    pub created: f64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Asset {
    pub id: String,
    pub kind: String,
    pub name: String,
    pub blob: String,
    pub mime: String,
    pub tree: Option<String>,
    pub meta: Value,
    pub created: f64,
}

impl Store {
    fn next_label(tx: &rusqlite::Transaction, key: &str, prefix: &str) -> rusqlite::Result<String> {
        let cur: Option<String> = tx
            .query_row("SELECT value FROM meta WHERE key=?1", [key], |r| r.get(0))
            .optional()?;
        let n: u64 = cur.and_then(|v| v.parse().ok()).unwrap_or(1);
        tx.execute(
            "INSERT OR REPLACE INTO meta(key,value) VALUES(?1,?2)",
            params![key, (n + 1).to_string()],
        )?;
        Ok(format!("{prefix}{n}"))
    }

    // ---- drafts -----------------------------------------------------------------------------
    /// Insert or update drafts (all users' drafts live in one table; the engine's in-memory list
    /// is rebuilt from it on load and on every change notification).
    pub fn save_drafts(&mut self, branch: &str, drafts: &[Draft]) -> Result<()> {
        if drafts.is_empty() {
            return Ok(());
        }
        let rows: Vec<_> = drafts
            .iter()
            .map(|d| (d.id, d.session.clone(), d.state.clone(), serde_json::to_string(d).unwrap()))
            .collect();
        let origin = self.origin().to_string();
        self.write(|tx| {
            for (id, user, state, json) in &rows {
                let old: Option<String> = tx
                    .query_row("SELECT json FROM drafts WHERE id=?1", [*id as i64], |r| r.get(0))
                    .optional()?;
                if old.as_deref() == Some(json.as_str()) {
                    continue;
                }
                tx.execute(
                    "INSERT INTO drafts(id,user,branch,state,json,created,updated) VALUES(?1,?2,?3,?4,?5,?6,?6) \
                     ON CONFLICT(id) DO UPDATE SET state=excluded.state, json=excluded.json, updated=excluded.updated",
                    params![*id as i64, user, branch, state, json, now()],
                )?;
                Self::log_change(tx, &origin, user, "draft", &id.to_string())?;
            }
            Ok(())
        })
    }
    /// Drafts of a branch. `live_only` skips accepted/rejected/withdrawn history.
    pub fn load_drafts(&self, branch: &str, live_only: bool) -> Result<Vec<Draft>> {
        let q = if live_only {
            "SELECT json FROM drafts WHERE branch=?1 AND state IN ('pending','stale','blocked') ORDER BY id"
        } else {
            "SELECT json FROM drafts WHERE branch=?1 ORDER BY id"
        };
        let mut st = self.connection().prepare_cached(q).map_err(sql)?;
        let rows: Vec<String> = st
            .query_map([branch], |r| r.get(0))
            .map_err(sql)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql)?;
        rows.iter()
            .map(|j| serde_json::from_str(j).map_err(|e| format!("bad draft record: {e}")))
            .collect()
    }
    pub fn draft(&self, id: u64) -> Result<Option<Draft>> {
        let j: Option<String> = self
            .connection()
            .query_row("SELECT json FROM drafts WHERE id=?1", [id as i64], |r| r.get(0))
            .optional()
            .map_err(sql)?;
        j.map(|j| serde_json::from_str(&j).map_err(|e| e.to_string()))
            .transpose()
    }

    // ---- observations -----------------------------------------------------------------------
    pub fn save_observations(&mut self, user: &str, branch: &str, seen: &BTreeMap<Id, u64>) -> Result<()> {
        let rows: Vec<_> = seen.iter().map(|(k, v)| (*k as i64, *v as i64)).collect();
        self.write(|tx| {
            let mut st = tx.prepare_cached(
                "INSERT OR REPLACE INTO observations(user,branch,part,hash) VALUES(?1,?2,?3,?4)",
            )?;
            for (p, h) in &rows {
                st.execute(params![user, branch, p, h])?;
            }
            Ok(())
        })
    }
    pub fn load_observations(&self, user: &str, branch: &str) -> Result<BTreeMap<Id, u64>> {
        let mut st = self
            .connection()
            .prepare_cached("SELECT part,hash FROM observations WHERE user=?1 AND branch=?2")
            .map_err(sql)?;
        st.query_map(params![user, branch], |r| {
            Ok((r.get::<_, i64>(0)? as Id, r.get::<_, i64>(1)? as u64))
        })
        .map_err(sql)?
        .collect::<rusqlite::Result<_>>()
        .map_err(sql)
    }

    // ---- selections -------------------------------------------------------------------------
    pub fn create_selection(
        &mut self,
        user: &str,
        name: Option<&str>,
        file: usize,
        parts: &[Id],
        query: Option<&Value>,
    ) -> Result<Selection> {
        let parts_json = serde_json::to_string(parts).unwrap();
        let query_json = query.map(|q| q.to_string());
        let origin = self.origin().to_string();
        let created = now();
        let id = self.write(|tx| {
            if let Some(n) = name {
                tx.execute(
                    "DELETE FROM selections WHERE user=?1 AND name=?2",
                    params![user, n],
                )?;
            }
            let id = Self::next_label(tx, "next_selection", "s")?;
            tx.execute(
                "INSERT INTO selections(id,user,name,file,parts,query,created,pinned) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![id, user, name, file as i64, parts_json, query_json, created, name.is_some() as i64],
            )?;
            Self::log_change(tx, &origin, user, "selection", &id)?;
            Ok(id)
        })?;
        Ok(Selection {
            id,
            user: user.into(),
            name: name.map(Into::into),
            file,
            parts: parts.to_vec(),
            query: query.cloned(),
            created,
            pinned: name.is_some(),
        })
    }
    /// A selection by id (`s12`) or by name, for this user first, then any user.
    pub fn selection(&self, user: &str, key: &str) -> Result<Selection> {
        let q = "SELECT id,user,name,file,parts,query,created,pinned FROM selections \
                 WHERE id=?1 OR name=?1 ORDER BY (user=?2) DESC, created DESC LIMIT 1";
        self.connection()
            .query_row(q, params![key, user], row_selection)
            .optional()
            .map_err(sql)?
            .ok_or_else(|| format!("no selection '{key}'"))
    }
    pub fn selections(&self, user: Option<&str>) -> Result<Vec<Selection>> {
        let mut st = self
            .connection()
            .prepare_cached(
                "SELECT id,user,name,file,parts,query,created,pinned FROM selections \
                 WHERE ?1 IS NULL OR user=?1 ORDER BY created",
            )
            .map_err(sql)?;
        st.query_map([user], row_selection)
            .map_err(sql)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql)
    }
    pub fn delete_selection(&mut self, user: &str, key: &str) -> Result<()> {
        let s = self.selection(user, key)?;
        self.write(|tx| {
            tx.execute("DELETE FROM selections WHERE id=?1", [&s.id])?;
            Ok(())
        })
    }
    /// Unnamed selections are scratch: keep the newest `keep` per user.
    pub fn prune_selections(&mut self, user: &str, keep: usize) -> Result<usize> {
        self.write(|tx| {
            Ok(tx.execute(
                "DELETE FROM selections WHERE user=?1 AND pinned=0 AND id NOT IN \
                 (SELECT id FROM selections WHERE user=?1 AND pinned=0 ORDER BY created DESC LIMIT ?2)",
                params![user, keep as i64],
            )?)
        })
    }

    // ---- candidate sets ---------------------------------------------------------------------
    pub fn create_candidates(&mut self, user: &str, json: &Value, tree: &str) -> Result<String> {
        let body = json.to_string();
        self.write(|tx| {
            let id = Self::next_label(tx, "next_candidates", "c")?;
            tx.execute(
                "INSERT INTO candidates(id,user,json,tree,created) VALUES(?1,?2,?3,?4,?5)",
                params![id, user, body, tree, now()],
            )?;
            tx.execute(
                "DELETE FROM candidates WHERE user=?1 AND id NOT IN \
                 (SELECT id FROM candidates WHERE user=?1 ORDER BY created DESC LIMIT 32)",
                [user],
            )?;
            Ok(id)
        })
    }
    pub fn candidates(&self, id: &str) -> Result<CandidateSet> {
        self.connection()
            .query_row(
                "SELECT id,user,json,tree,created FROM candidates WHERE id=?1",
                [id],
                |r| {
                    let j: String = r.get(2)?;
                    Ok(CandidateSet {
                        id: r.get(0)?,
                        user: r.get(1)?,
                        json: serde_json::from_str(&j).unwrap_or(Value::Null),
                        tree: r.get(3)?,
                        created: r.get(4)?,
                    })
                },
            )
            .optional()
            .map_err(sql)?
            .ok_or_else(|| format!("no candidate set '{id}' (they expire after 32 newer sets)"))
    }

    // ---- preferences ------------------------------------------------------------------------
    /// `scope` is `project` or `user:<name>`.
    pub fn pref(&self, scope: &str, key: &str) -> Result<Option<Value>> {
        let v: Option<String> = self
            .connection()
            .query_row(
                "SELECT value FROM prefs WHERE scope=?1 AND key=?2",
                params![scope, key],
                |r| r.get(0),
            )
            .optional()
            .map_err(sql)?;
        Ok(v.and_then(|v| serde_json::from_str(&v).ok()))
    }
    pub fn set_pref(&mut self, scope: &str, key: &str, value: &Value) -> Result<()> {
        let v = value.to_string();
        let origin = self.origin().to_string();
        self.write(|tx| {
            let old: Option<String> = tx
                .query_row(
                    "SELECT value FROM prefs WHERE scope=?1 AND key=?2",
                    params![scope, key],
                    |r| r.get(0),
                )
                .optional()?;
            if old.as_deref() == Some(v.as_str()) {
                return Ok(());
            }
            tx.execute(
                "INSERT OR REPLACE INTO prefs(scope,key,value) VALUES(?1,?2,?3)",
                params![scope, key, v],
            )?;
            Self::log_change(tx, &origin, scope, "pref", key)?;
            Ok(())
        })
    }
    pub fn prefs(&self, scope: &str) -> Result<BTreeMap<String, Value>> {
        let mut st = self
            .connection()
            .prepare_cached("SELECT key,value FROM prefs WHERE scope=?1")
            .map_err(sql)?;
        st.query_map([scope], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
            .map_err(sql)?
            .map(|r| {
                r.map(|(k, v)| (k, serde_json::from_str(&v).unwrap_or(Value::String(v))))
            })
            .collect::<rusqlite::Result<_>>()
            .map_err(sql)
    }

    // ---- assets -----------------------------------------------------------------------------
    /// Store a generated file (render, instruction page, PDF). Already-compressed formats (PNG,
    /// PDF) are stored raw. Returns the asset id.
    pub fn put_asset(
        &mut self,
        kind: &str,
        name: &str,
        mime: &str,
        bytes: &[u8],
        tree: Option<&str>,
        meta: &Value,
    ) -> Result<String> {
        let raw = matches!(mime, "image/png" | "image/jpeg" | "application/pdf" | "application/zip");
        let blob = Self::prepare_blob(bytes, raw);
        let meta = meta.to_string();
        let origin = self.origin().to_string();
        self.write(|tx| {
            Self::insert_blob(tx, &blob, bytes.len())?;
            let id = Self::next_label(tx, "next_asset", "a")?;
            tx.execute(
                "INSERT INTO assets(id,kind,name,blob,mime,tree,meta,created) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",
                params![id, kind, name, blob.0, mime, tree, meta, now()],
            )?;
            Self::log_change(tx, &origin, "", "asset", &id)?;
            Ok(id)
        })
    }
    pub fn assets(&self, kind: Option<&str>) -> Result<Vec<Asset>> {
        let mut st = self
            .connection()
            .prepare_cached(
                "SELECT id,kind,name,blob,mime,tree,meta,created FROM assets \
                 WHERE ?1 IS NULL OR kind=?1 ORDER BY created",
            )
            .map_err(sql)?;
        st.query_map([kind], row_asset)
            .map_err(sql)?
            .collect::<rusqlite::Result<_>>()
            .map_err(sql)
    }
    pub fn asset(&self, id: &str) -> Result<(Asset, Vec<u8>)> {
        let a = self
            .connection()
            .query_row(
                "SELECT id,kind,name,blob,mime,tree,meta,created FROM assets WHERE id=?1 OR name=?1 ORDER BY created DESC LIMIT 1",
                [id],
                row_asset,
            )
            .optional()
            .map_err(sql)?
            .ok_or_else(|| format!("no asset '{id}'"))?;
        let bytes = self.blob(&a.blob)?;
        Ok((a, bytes))
    }
    pub fn delete_assets(&mut self, kind: &str) -> Result<usize> {
        self.write(|tx| Ok(tx.execute("DELETE FROM assets WHERE kind=?1", [kind])?))
    }
}

fn row_selection(r: &rusqlite::Row) -> rusqlite::Result<Selection> {
    let parts: String = r.get(4)?;
    let query: Option<String> = r.get(5)?;
    Ok(Selection {
        id: r.get(0)?,
        user: r.get(1)?,
        name: r.get(2)?,
        file: r.get::<_, i64>(3)? as usize,
        parts: serde_json::from_str(&parts).unwrap_or_default(),
        query: query.and_then(|q| serde_json::from_str(&q).ok()),
        created: r.get(6)?,
        pinned: r.get::<_, i64>(7)? != 0,
    })
}
fn row_asset(r: &rusqlite::Row) -> rusqlite::Result<Asset> {
    let meta: String = r.get(6)?;
    Ok(Asset {
        id: r.get(0)?,
        kind: r.get(1)?,
        name: r.get(2)?,
        blob: r.get(3)?,
        mime: r.get(4)?,
        tree: r.get(5)?,
        meta: serde_json::from_str(&meta).unwrap_or(Value::Null),
        created: r.get(7)?,
    })
}
