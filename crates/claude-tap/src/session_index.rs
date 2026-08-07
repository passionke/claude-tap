//! SQLite session index. Author: kejiqing

use chrono::{SecondsFormat, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::{Path, PathBuf};

pub const SESSIONS_DB_FILENAME: &str = "claude_tap_sessions.sqlite3";
pub const SESSIONS_SUBDIR: &str = "sessions";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    pub claw_session_id: String,
    pub storage_slug: String,
    pub jsonl_relpath: String,
    pub created_at: String,
    pub updated_at: String,
    pub first_calendar_date: Option<String>,
    pub last_calendar_date: Option<String>,
    pub last_turn: i64,
}

pub fn jsonl_relpath_for_slug(storage_slug: &str) -> String {
    format!("{SESSIONS_SUBDIR}/{storage_slug}/trace.jsonl")
}

fn utc_now_iso() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Micros, true)
}

fn utc_date() -> String {
    Utc::now().date_naive().format("%Y-%m-%d").to_string()
}

pub struct SessionIndex {
    output_dir: PathBuf,
    conn: Mutex<Connection>,
}

impl SessionIndex {
    pub fn open(output_dir: impl AsRef<Path>) -> anyhow::Result<Self> {
        let output_dir = output_dir.as_ref().to_path_buf();
        std::fs::create_dir_all(&output_dir)?;
        let db_path = output_dir.join(SESSIONS_DB_FILENAME);
        let conn = Connection::open(&db_path)?;
        conn.execute_batch("PRAGMA journal_mode=WAL;")?;
        conn.execute_batch(
            r#"
            CREATE TABLE IF NOT EXISTS sessions (
              claw_session_id TEXT PRIMARY KEY,
              storage_slug TEXT UNIQUE NOT NULL,
              jsonl_relpath TEXT NOT NULL,
              created_at TEXT NOT NULL,
              updated_at TEXT NOT NULL,
              first_calendar_date TEXT,
              last_calendar_date TEXT,
              last_turn INTEGER NOT NULL DEFAULT 0
            );
            CREATE INDEX IF NOT EXISTS idx_sessions_updated
              ON sessions(updated_at DESC);
            "#,
        )?;
        Ok(Self {
            output_dir,
            conn: Mutex::new(conn),
        })
    }

    pub fn output_dir(&self) -> &Path {
        &self.output_dir
    }

    pub fn get_last_turn(&self, claw_session_id: &str) -> anyhow::Result<i64> {
        let conn = self.conn.lock();
        let v: Option<i64> = conn
            .query_row(
                "SELECT last_turn FROM sessions WHERE claw_session_id = ?1",
                params![claw_session_id],
                |row| row.get(0),
            )
            .optional()?;
        Ok(v.unwrap_or(0))
    }

    pub fn upsert_session_row(
        &self,
        claw_session_id: &str,
        storage_slug: &str,
        jsonl_relpath: &str,
    ) -> anyhow::Result<()> {
        let now = utc_now_iso();
        let cal = utc_date();
        let conn = self.conn.lock();
        conn.execute(
            r#"
            INSERT INTO sessions (
              claw_session_id, storage_slug, jsonl_relpath,
              created_at, updated_at, first_calendar_date, last_calendar_date, last_turn
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 0)
            ON CONFLICT(claw_session_id) DO UPDATE SET
              updated_at = excluded.updated_at
            "#,
            params![
                claw_session_id,
                storage_slug,
                jsonl_relpath,
                now,
                now,
                cal,
                cal
            ],
        )?;
        Ok(())
    }

    pub fn record_write(&self, claw_session_id: &str, turn: i64) -> anyhow::Result<()> {
        let now = utc_now_iso();
        let cal = utc_date();
        let conn = self.conn.lock();
        let prev: i64 = conn
            .query_row(
                "SELECT last_turn FROM sessions WHERE claw_session_id = ?1",
                params![claw_session_id],
                |row| row.get(0),
            )
            .optional()?
            .unwrap_or(0);
        let new_last = prev.max(turn);
        conn.execute(
            r#"
            UPDATE sessions SET
              updated_at = ?1,
              last_turn = ?2,
              last_calendar_date = ?3,
              first_calendar_date = COALESCE(first_calendar_date, ?4)
            WHERE claw_session_id = ?5
            "#,
            params![now, new_last, cal, cal, claw_session_id],
        )?;
        Ok(())
    }

    fn map_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRow> {
        Ok(SessionRow {
            claw_session_id: row.get(0)?,
            storage_slug: row.get(1)?,
            jsonl_relpath: row.get(2)?,
            created_at: row.get(3)?,
            updated_at: row.get(4)?,
            first_calendar_date: row.get(5)?,
            last_calendar_date: row.get(6)?,
            last_turn: row.get(7)?,
        })
    }

    pub fn get_session(&self, claw_session_id: &str) -> anyhow::Result<Option<SessionRow>> {
        let conn = self.conn.lock();
        let row = conn
            .query_row(
                "SELECT claw_session_id, storage_slug, jsonl_relpath, created_at, updated_at, first_calendar_date, last_calendar_date, last_turn FROM sessions WHERE claw_session_id = ?1",
                params![claw_session_id],
                Self::map_row,
            )
            .optional()?;
        Ok(row)
    }

    pub fn list_sessions(&self, limit: i64, offset: i64) -> anyhow::Result<(Vec<SessionRow>, i64)> {
        let conn = self.conn.lock();
        let total: i64 = conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?;
        let mut stmt = conn.prepare(
            r#"
            SELECT claw_session_id, storage_slug, jsonl_relpath, created_at, updated_at,
                   first_calendar_date, last_calendar_date, last_turn
            FROM sessions
            ORDER BY updated_at DESC
            LIMIT ?1 OFFSET ?2
            "#,
        )?;
        let rows = stmt
            .query_map(params![limit, offset], Self::map_row)?
            .collect::<Result<Vec<_>, _>>()?;
        Ok((rows, total))
    }

    pub fn session_count(&self) -> anyhow::Result<i64> {
        let conn = self.conn.lock();
        Ok(conn.query_row("SELECT COUNT(*) FROM sessions", [], |r| r.get(0))?)
    }

    pub fn delete_oldest_sessions(&self, count: i64) -> anyhow::Result<i64> {
        if count <= 0 {
            return Ok(0);
        }
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT claw_session_id, jsonl_relpath FROM sessions ORDER BY updated_at ASC LIMIT ?1",
        )?;
        let victims: Vec<(String, String)> = stmt
            .query_map(params![count], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        let mut deleted = 0i64;
        for (cid, relpath) in victims {
            conn.execute(
                "DELETE FROM sessions WHERE claw_session_id = ?1",
                params![cid],
            )?;
            deleted += 1;
            let abs_path = self.output_dir.join(&relpath);
            if let Some(parent) = abs_path.parent() {
                if parent.file_name().and_then(|s| s.to_str()) != Some(SESSIONS_SUBDIR) {
                    let _ = std::fs::remove_dir_all(parent);
                } else if abs_path.exists() {
                    let _ = std::fs::remove_file(&abs_path);
                }
            }
            let html = abs_path.with_extension("html");
            if html.exists() {
                let _ = std::fs::remove_file(html);
            }
        }
        Ok(deleted)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn upsert_list_and_turns() {
        let dir = tempdir().unwrap();
        let idx = SessionIndex::open(dir.path()).unwrap();
        idx.upsert_session_row("s1", "slug1", "sessions/slug1/trace.jsonl")
            .unwrap();
        assert_eq!(idx.get_last_turn("s1").unwrap(), 0);
        idx.record_write("s1", 2).unwrap();
        assert_eq!(idx.get_last_turn("s1").unwrap(), 2);
        idx.record_write("s1", 1).unwrap();
        assert_eq!(idx.get_last_turn("s1").unwrap(), 2);
        let (rows, total) = idx.list_sessions(10, 0).unwrap();
        assert_eq!(total, 1);
        assert_eq!(rows[0].claw_session_id, "s1");
        assert_eq!(rows[0].storage_slug, "slug1");
    }
}
