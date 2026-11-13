//! Table layout and migrations. `meta.schema` holds the version; migrations only ever add.
use crate::{Result, sql};
use rusqlite::Connection;

pub const VERSION: i64 = 1;

const V1: &str = "
CREATE TABLE IF NOT EXISTS meta(key TEXT PRIMARY KEY, value TEXT NOT NULL);
CREATE TABLE IF NOT EXISTS blobs(hash TEXT PRIMARY KEY, codec INTEGER NOT NULL, size INTEGER NOT NULL, data BLOB NOT NULL);
CREATE TABLE IF NOT EXISTS trees(hash TEXT PRIMARY KEY, created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS tree_files(tree TEXT NOT NULL, idx INTEGER NOT NULL, name TEXT NOT NULL,
    text TEXT NOT NULL, ids TEXT NOT NULL, PRIMARY KEY(tree, idx));
CREATE TABLE IF NOT EXISTS branches(name TEXT PRIMARY KEY, head INTEGER, base INTEGER, created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS commits(id INTEGER PRIMARY KEY AUTOINCREMENT, branch TEXT NOT NULL, parent INTEGER,
    tree TEXT NOT NULL, kind TEXT NOT NULL, author TEXT NOT NULL, message TEXT NOT NULL,
    drafts TEXT NOT NULL DEFAULT '[]', time REAL NOT NULL);
CREATE INDEX IF NOT EXISTS commits_branch ON commits(branch, id);
CREATE TABLE IF NOT EXISTS working(branch TEXT PRIMARY KEY, tree TEXT NOT NULL, seq INTEGER NOT NULL,
    author TEXT NOT NULL, time REAL NOT NULL);
CREATE TABLE IF NOT EXISTS leases(branch TEXT PRIMARY KEY, holder TEXT NOT NULL, pid INTEGER NOT NULL, renewed REAL NOT NULL);
CREATE TABLE IF NOT EXISTS drafts(id INTEGER PRIMARY KEY, user TEXT NOT NULL, branch TEXT NOT NULL,
    state TEXT NOT NULL, json TEXT NOT NULL, created REAL NOT NULL, updated REAL NOT NULL);
CREATE INDEX IF NOT EXISTS drafts_user ON drafts(user, state);
CREATE TABLE IF NOT EXISTS observations(user TEXT NOT NULL, branch TEXT NOT NULL, part INTEGER NOT NULL,
    hash INTEGER NOT NULL, PRIMARY KEY(user, branch, part));
CREATE TABLE IF NOT EXISTS selections(id TEXT PRIMARY KEY, user TEXT NOT NULL, name TEXT, file INTEGER NOT NULL,
    parts TEXT NOT NULL, query TEXT, created REAL NOT NULL, pinned INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS candidates(id TEXT PRIMARY KEY, user TEXT NOT NULL, json TEXT NOT NULL,
    tree TEXT NOT NULL, created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS prefs(scope TEXT NOT NULL, key TEXT NOT NULL, value TEXT NOT NULL, PRIMARY KEY(scope, key));
CREATE TABLE IF NOT EXISTS assets(id TEXT PRIMARY KEY, kind TEXT NOT NULL, name TEXT NOT NULL, blob TEXT NOT NULL,
    mime TEXT NOT NULL, tree TEXT, meta TEXT NOT NULL DEFAULT '{}', created REAL NOT NULL);
CREATE TABLE IF NOT EXISTS changes(seq INTEGER PRIMARY KEY AUTOINCREMENT, origin TEXT NOT NULL, user TEXT NOT NULL,
    kind TEXT NOT NULL, ref TEXT NOT NULL, time REAL NOT NULL);
";

pub fn migrate(conn: &Connection) -> Result<()> {
    conn.execute_batch(V1).map_err(sql)?;
    let have: Option<String> = conn
        .query_row("SELECT value FROM meta WHERE key='schema'", [], |r| r.get(0))
        .ok();
    match have.and_then(|v| v.parse::<i64>().ok()) {
        None => {
            conn.execute(
                "INSERT OR REPLACE INTO meta(key,value) VALUES('schema',?1)",
                [VERSION.to_string()],
            )
            .map_err(sql)?;
            conn.execute(
                "INSERT OR IGNORE INTO meta(key,value) VALUES('app','superbrick')",
                [],
            )
            .map_err(sql)?;
        }
        Some(v) if v > VERSION => {
            return Err(format!(
                "this project was written by a newer SuperBrick (schema {v}, this build reads {VERSION})"
            ));
        }
        Some(_) => {}
    }
    Ok(())
}
