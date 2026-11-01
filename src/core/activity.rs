use crate::core::{audit::CallDetails, config::ActivityConfig, error::Result};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use rand::{rngs::OsRng, RngCore};
use rusqlite::{params, params_from_iter, types::Value as SqlValue, Connection};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{self, OpenOptions},
    path::Path,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Mutex,
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

#[derive(Debug, Clone, Default)]
pub struct Caller {
    pub key: String,
    pub job_owner_key: String,
    pub grouping: String,
    pub transport: String,
    pub session_id: Option<String>,
    pub client_name: Option<String>,
    pub client_version: Option<String>,
    pub oauth_client_id: Option<String>,
    pub oauth_client_name: Option<String>,
    pub user_agent: Option<String>,
}

impl Caller {
    pub fn from_key(key: &str, process_id: &str) -> Self {
        let (grouping, transport, session_id, stable_key) =
            if let Some(id) = key.strip_prefix("session:") {
                ("mcp_session", "http", Some(id.to_string()), key.to_string())
            } else if key.starts_with("bearer:")
                || key.starts_with("static-bearer:")
                || key.starts_with("oauth-grant:")
            {
                ("credential_group", "http", None, key.to_string())
            } else if key == "http:anonymous" {
                ("anonymous_group", "http", None, key.to_string())
            } else {
                (
                    "stdio_process",
                    "stdio",
                    None,
                    format!("stdio:{process_id}"),
                )
            };
        Self {
            job_owner_key: stable_key.clone(),
            key: stable_key,
            grouping: grouping.to_string(),
            transport: transport.to_string(),
            session_id,
            ..Default::default()
        }
    }
}

pub struct Event {
    pub caller: Caller,
    pub started_ms: i64,
    pub duration_ms: f64,
    pub kind: &'static str,
    pub name: String,
    pub status: &'static str,
    pub target: Option<String>,
    pub argument_keys: Vec<String>,
    pub rpc_error_code: Option<i64>,
    pub details: Option<CallDetails>,
}

#[derive(Debug, Deserialize, Default)]
pub struct Query {
    pub hours: Option<i64>,
    pub limit: Option<usize>,
    pub before: Option<i64>,
    pub session: Option<String>,
    pub client: Option<String>,
    pub tool: Option<String>,
    pub status: Option<String>,
    pub search: Option<String>,
}

pub struct ActivityStore {
    conn: Mutex<Connection>,
    config: ActivityConfig,
    writes: AtomicUsize,
}

impl ActivityStore {
    pub fn open(path: &Path, config: &ActivityConfig) -> Result<Self> {
        let dir = path.parent().ok_or_else(|| {
            crate::core::error::Error::Config("activity database_path has no parent".into())
        })?;
        #[cfg(unix)]
        let new_dir = !dir.exists();
        fs::create_dir_all(dir)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            // Never chmod an existing arbitrary parent (e.g. /tmp).
            if new_dir {
                fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
            }
            if fs::metadata(dir)?.permissions().mode() & 0o077 != 0 {
                return Err(crate::core::error::Error::Config(format!(
                    "activity database directory {} must be private (mode 0700)",
                    dir.display()
                )));
            }
        }
        let mut options = OpenOptions::new();
        options.write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        let conn = Connection::open(path)?;
        conn.busy_timeout(Duration::from_secs(3))?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA foreign_keys=ON;
             CREATE TABLE IF NOT EXISTS sessions (
                session_key TEXT PRIMARY KEY,
                grouping TEXT NOT NULL,
                transport TEXT NOT NULL,
                session_id TEXT,
                client_name TEXT,
                client_version TEXT,
                oauth_client_id TEXT,
                oauth_client_name TEXT,
                user_agent TEXT,
                first_seen_ms INTEGER NOT NULL,
                last_seen_ms INTEGER NOT NULL,
                ended_ms INTEGER,
                total_calls INTEGER NOT NULL DEFAULT 0,
                error_calls INTEGER NOT NULL DEFAULT 0
             );
             CREATE TABLE IF NOT EXISTS calls (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                session_key TEXT NOT NULL REFERENCES sessions(session_key),
                started_ms INTEGER NOT NULL,
                duration_ms REAL NOT NULL,
                kind TEXT NOT NULL,
                name TEXT NOT NULL,
                status TEXT NOT NULL,
                target TEXT,
                argument_keys TEXT NOT NULL,
                rpc_error_code INTEGER
             );
             CREATE INDEX IF NOT EXISTS idx_calls_time ON calls(started_ms DESC);
             CREATE INDEX IF NOT EXISTS idx_calls_session ON calls(session_key, started_ms DESC);
             CREATE INDEX IF NOT EXISTS idx_calls_name_time ON calls(name, started_ms DESC);
             CREATE INDEX IF NOT EXISTS idx_calls_status_time ON calls(status, started_ms DESC);
             CREATE TABLE IF NOT EXISTS call_details (
               call_id INTEGER PRIMARY KEY REFERENCES calls(id) ON DELETE CASCADE,
               command_text TEXT,
               cwd TEXT,
               request_json TEXT NOT NULL,
               response_json TEXT NOT NULL,
               request_truncated INTEGER NOT NULL DEFAULT 0,
               response_truncated INTEGER NOT NULL DEFAULT 0
             );
             CREATE INDEX IF NOT EXISTS idx_sessions_last_seen ON sessions(last_seen_ms DESC);
             CREATE TABLE IF NOT EXISTS dashboard_sessions (
               token_hash TEXT PRIMARY KEY,
               auth_binding TEXT NOT NULL,
               created_ms INTEGER NOT NULL,
               expires_ms INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_dashboard_expires ON dashboard_sessions(expires_ms);",
        )?;
        let store = Self {
            conn: Mutex::new(conn),
            config: config.clone(),
            writes: AtomicUsize::new(0),
        };
        store.cleanup()?;
        Ok(store)
    }

    /// Browser-only session. Store hashes rather than bearer cookie values.
    pub fn issue_dashboard_session(&self, password: &str) -> Result<String> {
        let mut bytes = [0u8; 32];
        OsRng.fill_bytes(&mut bytes);
        let token = URL_SAFE_NO_PAD.encode(bytes);
        let now = now_ms();
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        tx.execute(
            "DELETE FROM dashboard_sessions WHERE expires_ms <= ?1",
            params![now],
        )?;
        tx.execute(
            "INSERT INTO dashboard_sessions(token_hash,auth_binding,created_ms,expires_ms)
             VALUES (?1,?2,?3,?4)",
            params![
                session_hash(&token),
                session_auth_binding(&token, password),
                now,
                now + 12 * 3_600_000
            ],
        )?;
        tx.execute(
            "DELETE FROM dashboard_sessions WHERE token_hash NOT IN
                (SELECT token_hash FROM dashboard_sessions ORDER BY created_ms DESC LIMIT 128)",
            [],
        )?;
        tx.commit()?;
        Ok(token)
    }

    pub fn dashboard_session_valid(&self, token: &str, password: &str) -> Result<bool> {
        if token.len() != 43
            || !token
                .bytes()
                .all(|ch| ch.is_ascii_alphanumeric() || ch == b'-' || ch == b'_')
        {
            return Ok(false);
        }
        let conn = self.conn.lock().unwrap();
        let saved = conn.query_row(
            "SELECT auth_binding FROM dashboard_sessions
             WHERE token_hash=?1 AND expires_ms > ?2",
            params![session_hash(token), now_ms()],
            |row| row.get::<_, String>(0),
        );
        match saved {
            Ok(saved) => Ok(constant_time_eq(
                saved.as_bytes(),
                session_auth_binding(token, password).as_bytes(),
            )),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    pub fn revoke_dashboard_session(&self, token: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "DELETE FROM dashboard_sessions WHERE token_hash=?1",
            params![session_hash(token)],
        )?;
        Ok(())
    }

    pub fn record(&self, event: Event) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let caller = event.caller;
        let truncate =
            |value: Option<String>, n| value.map(|v| v.chars().take(n).collect::<String>());
        let name = truncate(caller.client_name, 128);
        let version = truncate(caller.client_version, 64);
        let user_agent = truncate(caller.user_agent, 256);
        let oauth_name = truncate(caller.oauth_client_name, 128);
        let oauth_id = truncate(caller.oauth_client_id, 256);
        tx.execute(
            "INSERT INTO sessions
              (session_key, grouping, transport, session_id, client_name, client_version,
               oauth_client_id, oauth_client_name, user_agent, first_seen_ms, last_seen_ms)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?10)
             ON CONFLICT(session_key) DO UPDATE SET
               last_seen_ms=MAX(sessions.last_seen_ms,excluded.last_seen_ms),
               first_seen_ms=MIN(sessions.first_seen_ms,excluded.first_seen_ms),
               client_name=COALESCE(excluded.client_name, sessions.client_name),
               client_version=COALESCE(excluded.client_version, sessions.client_version),
               oauth_client_id=COALESCE(excluded.oauth_client_id, sessions.oauth_client_id),
               oauth_client_name=COALESCE(excluded.oauth_client_name, sessions.oauth_client_name),
               user_agent=COALESCE(sessions.user_agent, excluded.user_agent)",
            params![
                caller.key,
                caller.grouping,
                caller.transport,
                caller.session_id,
                name,
                version,
                oauth_id,
                oauth_name,
                user_agent,
                event.started_ms
            ],
        )?;
        tx.execute(
            "INSERT INTO calls (session_key, started_ms, duration_ms, kind, name, status,
                target, argument_keys, rpc_error_code)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![
                caller.key,
                event.started_ms,
                event.duration_ms.max(0.0),
                event.kind,
                event.name.chars().take(128).collect::<String>(),
                event.status,
                event
                    .target
                    .map(|t| t.chars().take(128).collect::<String>()),
                serde_json::to_string(&event.argument_keys)?,
                event.rpc_error_code
            ],
        )?;
        if let Some(details) = event.details {
            tx.execute(
                "INSERT INTO call_details
                    (call_id,command_text,cwd,request_json,response_json,
                     request_truncated,response_truncated)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)",
                params![
                    tx.last_insert_rowid(),
                    details.command,
                    details.cwd,
                    details.request_json,
                    details.response_json,
                    details.request_truncated,
                    details.response_truncated
                ],
            )?;
        }
        tx.execute(
            "UPDATE sessions SET total_calls=total_calls+1,
                error_calls=error_calls+?2 WHERE session_key=?1",
            params![caller.key, i64::from(event.status == "error")],
        )?;
        tx.commit()?;
        drop(conn);
        if self.writes.fetch_add(1, Ordering::Relaxed) % 512 == 511 {
            self.cleanup()?;
        }
        Ok(())
    }

    pub fn end_session(&self, key: &str) -> Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE sessions SET ended_ms=?2 WHERE session_key=?1",
            params![key, now_ms()],
        )?;
        Ok(())
    }

    fn cleanup(&self) -> Result<()> {
        let mut conn = self.conn.lock().unwrap();
        let tx = conn.transaction()?;
        let cutoff = now_ms() - (self.config.retention_days as i64) * 86_400_000;
        tx.execute("DELETE FROM calls WHERE started_ms < ?1", params![cutoff])?;
        tx.execute(
            "DELETE FROM dashboard_sessions WHERE expires_ms <= ?1",
            params![now_ms()],
        )?;
        tx.execute(
            "DELETE FROM calls WHERE id <= COALESCE(
                (SELECT id FROM calls ORDER BY id DESC LIMIT 1 OFFSET ?1), -1)",
            params![self.config.max_calls as i64],
        )?;
        tx.execute(
            "DELETE FROM sessions WHERE last_seen_ms < ?1 AND
            NOT EXISTS (SELECT 1 FROM calls WHERE calls.session_key=sessions.session_key)",
            params![cutoff],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn call_details(&self, call_id: i64) -> Result<Option<Value>> {
        if call_id <= 0 {
            return Ok(None);
        }
        let conn = self.conn.lock().unwrap();
        match conn.query_row(
            "SELECT c.id, d.command_text, d.cwd, d.request_json, d.response_json,
                    d.request_truncated, d.response_truncated
             FROM calls c LEFT JOIN call_details d ON d.call_id = c.id
             WHERE c.id=?1",
            params![call_id],
            |row| {
                let arguments: Option<String> = row.get(3)?;
                let result: Option<String> = row.get(4)?;
                Ok(json!({
                    "id": row.get::<_,i64>(0)?,
                    "available": arguments.is_some(),
                    "command": row.get::<_,Option<String>>(1)?,
                    "cwd": row.get::<_,Option<String>>(2)?,
                    "arguments": arguments.and_then(|value|serde_json::from_str::<Value>(&value).ok()),
                    "result": result.and_then(|value|serde_json::from_str::<Value>(&value).ok()),
                    "request_truncated": row.get::<_,Option<bool>>(5)?.unwrap_or(false),
                    "response_truncated": row.get::<_,Option<bool>>(6)?.unwrap_or(false)
                }))
            }
        ) {
            Ok(row) => Ok(Some(row)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(err) => Err(err.into())
        }
    }

    pub fn query(&self, query: Query) -> Result<Value> {
        let hours = query.hours.unwrap_or(24).clamp(1, 24 * 30);
        let limit = query.limit.unwrap_or(60).clamp(1, 200);
        let since = now_ms() - hours * 3_600_000;
        let mut where_parts = vec!["c.started_ms >= ?".to_string()];
        let mut args = vec![SqlValue::Integer(since)];
        let clean = |v: Option<String>| {
            v.map(|s| s.chars().take(128).collect::<String>())
                .filter(|s| !s.is_empty())
        };
        if let Some(session) = clean(query.session) {
            where_parts.push("c.session_key = ?".into());
            args.push(SqlValue::Text(session));
        }
        if let Some(client) = clean(query.client) {
            where_parts.push(format!("{CLIENT_SQL} = ?"));
            args.push(SqlValue::Text(client));
        }
        if let Some(tool) = clean(query.tool) {
            where_parts.push("c.name = ?".into());
            args.push(SqlValue::Text(tool));
        }
        if let Some(status) = clean(query.status) {
            if status == "ok" || status == "error" {
                where_parts.push("c.status = ?".into());
                args.push(SqlValue::Text(status));
            }
        }
        if let Some(search) = clean(query.search) {
            let value = format!(
                "%{}%",
                search
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_")
            );
            where_parts.push(format!("(c.name LIKE ? ESCAPE '\\' OR c.target LIKE ? ESCAPE '\\' OR {CLIENT_SQL} LIKE ? ESCAPE '\\' OR EXISTS (SELECT 1 FROM call_details d WHERE d.call_id = c.id AND (d.command_text LIKE ? ESCAPE '\\' OR d.request_json LIKE ? ESCAPE '\\')))"));
            for _ in 0..5 {
                args.push(SqlValue::Text(value.clone()));
            }
        }
        let where_sql = where_parts.join(" AND ");
        let conn = self.conn.lock().unwrap();

        let (count, errors, distinct_sessions, distinct_clients, avg_ms): (
            i64,
            i64,
            i64,
            i64,
            f64,
        ) = conn.query_row(
            &format!(
                "SELECT COUNT(*), COALESCE(SUM(c.status='error'),0),
                    COUNT(DISTINCT c.session_key), COUNT(DISTINCT {CLIENT_SQL}),
                    COALESCE(AVG(c.duration_ms),0)
                 FROM calls c JOIN sessions s ON c.session_key=s.session_key
                 WHERE {where_sql}"
            ),
            params_from_iter(args.iter()),
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            },
        )?;

        let p95_ms: Option<f64> = if count > 0 {
            let mut percentile_args = args.clone();
            percentile_args.push(SqlValue::Integer(
                ((count as f64 * 0.95).ceil() as i64 - 1).max(0),
            ));
            conn.query_row(
                &format!(
                    "SELECT c.duration_ms FROM calls c JOIN sessions s
                    ON c.session_key=s.session_key WHERE {where_sql}
                    ORDER BY c.duration_ms ASC LIMIT 1 OFFSET ?"
                ),
                params_from_iter(percentile_args.iter()),
                |row| row.get(0),
            )
            .ok()
        } else {
            None
        };

        let mut top_tools = Vec::new();
        let mut stmt = conn.prepare(&format!(
            "SELECT c.name, c.kind, COUNT(*), COALESCE(SUM(c.status='error'),0)
             FROM calls c JOIN sessions s ON c.session_key=s.session_key WHERE {where_sql}
             AND c.kind='tool'
             GROUP BY c.name,c.kind ORDER BY COUNT(*) DESC LIMIT 10"
        ))?;
        let rows = stmt.query_map(params_from_iter(args.iter()), |row| {
            Ok(json!({
                "name": row.get::<_,String>(0)?, "kind": row.get::<_,String>(1)?,
                "count": row.get::<_,i64>(2)?, "errors": row.get::<_,i64>(3)?
            }))
        })?;
        for row in rows {
            top_tools.push(row?);
        }

        let mut trend = Vec::new();
        let mut stmt = conn.prepare(&format!(
            "SELECT CAST(c.started_ms / 3600000 AS INTEGER), COUNT(*),
                COALESCE(SUM(c.status='error'),0)
             FROM calls c JOIN sessions s ON c.session_key=s.session_key WHERE {where_sql}
             GROUP BY 1 ORDER BY 1"
        ))?;
        for row in stmt.query_map(params_from_iter(args.iter()), |row| {
            Ok(json!({
                "hour": row.get::<_,i64>(0)?, "count": row.get::<_,i64>(1)?,
                "errors": row.get::<_,i64>(2)?
            }))
        })? {
            trend.push(row?);
        }

        let mut sessions = Vec::new();
        let mut stmt = conn.prepare(&format!(
            "SELECT s.session_key,s.grouping,s.transport,s.session_id,
                s.client_name,s.client_version,s.oauth_client_name,s.user_agent,
                s.first_seen_ms,s.last_seen_ms,s.ended_ms,COUNT(*),
                COALESCE(SUM(c.status='error'),0),MAX(c.started_ms),{CLIENT_SQL}
             FROM calls c JOIN sessions s ON c.session_key=s.session_key WHERE {where_sql}
             GROUP BY s.session_key ORDER BY MAX(c.started_ms) DESC LIMIT 100"
        ))?;
        for row in stmt.query_map(params_from_iter(args.iter()), |r| {
            Ok(json!({
                "key": r.get::<_,String>(0)?, "grouping": r.get::<_,String>(1)?,
                "transport": r.get::<_,String>(2)?, "session_id": r.get::<_,Option<String>>(3)?,
                "client_name": r.get::<_,Option<String>>(4)?,
                "client_version": r.get::<_,Option<String>>(5)?,
                "oauth_client_name": r.get::<_,Option<String>>(6)?,
                "user_agent": r.get::<_,Option<String>>(7)?,
                "first_seen_ms": r.get::<_,i64>(8)?,
                "last_seen_ms": r.get::<_,i64>(9)?,
                "ended_ms": r.get::<_,Option<i64>>(10)?,
                "count": r.get::<_,i64>(11)?, "errors": r.get::<_,i64>(12)?,
                "last_call_ms": r.get::<_,i64>(13)?, "client": r.get::<_,String>(14)?
            }))
        })? {
            sessions.push(row?);
        }

        let mut call_args = args.clone();
        let mut call_where = where_sql;
        if let Some(before) = query.before.filter(|id| *id > 0) {
            call_where.push_str(" AND c.id < ?");
            call_args.push(SqlValue::Integer(before));
        }
        call_args.push(SqlValue::Integer(limit as i64 + 1));
        let mut calls = Vec::new();
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id,c.started_ms,c.duration_ms,c.kind,c.name,c.status,
                c.target,c.argument_keys,c.rpc_error_code,s.session_key,s.grouping,
                s.transport,{CLIENT_SQL},s.client_version,
                SUBSTR(d.command_text,1,240),d.cwd,(d.call_id IS NOT NULL)
             FROM calls c JOIN sessions s ON c.session_key=s.session_key
             LEFT JOIN call_details d ON d.call_id=c.id
             WHERE {call_where} ORDER BY c.id DESC LIMIT ?"
        ))?;
        for row in stmt.query_map(params_from_iter(call_args.iter()), |r| {
            let keys: String = r.get(7)?;
            Ok(json!({
                "id": r.get::<_,i64>(0)?, "started_ms": r.get::<_,i64>(1)?,
                "duration_ms": r.get::<_,f64>(2)?, "kind": r.get::<_,String>(3)?,
                "name": r.get::<_,String>(4)?, "status": r.get::<_,String>(5)?,
                "target": r.get::<_,Option<String>>(6)?,
                "argument_keys": serde_json::from_str::<Value>(&keys).unwrap_or(json!([])),
                "rpc_error_code": r.get::<_,Option<i64>>(8)?,
                "session_key": r.get::<_,String>(9)?,
                "grouping": r.get::<_,String>(10)?, "transport": r.get::<_,String>(11)?,
                "client": r.get::<_,String>(12)?, "client_version": r.get::<_,Option<String>>(13)?,
                "command_preview": r.get::<_,Option<String>>(14)?,
                "cwd": r.get::<_,Option<String>>(15)?,
                "has_details": r.get::<_,bool>(16)?
            }))
        })? {
            calls.push(row?);
        }
        let has_more = calls.len() > limit;
        if has_more {
            calls.pop();
        }
        let next_before = if has_more {
            calls.last().and_then(|c| c["id"].as_i64())
        } else {
            None
        };
        Ok(json!({
            "window_hours": hours,
            "summary": {"count": count, "errors":errors, "sessions":distinct_sessions,
                "clients":distinct_clients, "avg_ms":avg_ms, "p95_ms":p95_ms},
            "top_tools":top_tools, "trend":trend, "sessions":sessions, "calls":calls,
            "next_before":next_before
        }))
    }
}

const CLIENT_SQL: &str =
    "COALESCE(NULLIF(s.client_name,''),NULLIF(s.oauth_client_name,''),NULLIF(s.user_agent,''),'Unknown')";

fn session_hash(token: &str) -> String {
    format!("{:x}", Sha256::digest(token.as_bytes()))
}

fn session_auth_binding(token: &str, password: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(b"target-ops/dashboard-session/v1");
    hasher.update(token.as_bytes());
    hasher.update(password.as_bytes());
    format!("{:x}", hasher.finalize())
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter()
        .zip(b.iter())
        .fold(0u8, |acc, (&x, &y)| acc | (x ^ y))
        == 0
}

pub fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn store() -> (tempfile::TempDir, ActivityStore) {
        let dir = tempdir().unwrap();
        let store = ActivityStore::open(
            &dir.path().join("activity/calls.sqlite3"),
            &ActivityConfig::default(),
        )
        .unwrap();
        (dir, store)
    }

    #[test]
    fn browser_session_survives_restart_and_rejects_password_change() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("private/activity.sqlite");
        let store = ActivityStore::open(&path, &ActivityConfig::default()).unwrap();
        let token = store
            .issue_dashboard_session("test-only-passphrase")
            .unwrap();
        assert!(store
            .dashboard_session_valid(&token, "test-only-passphrase")
            .unwrap());
        assert!(!store.dashboard_session_valid(&token, "invalid").unwrap());
        assert!(!store
            .dashboard_session_valid("not-a-token", "test-only-passphrase")
            .unwrap());
        drop(store);
        let reopened = ActivityStore::open(&path, &ActivityConfig::default()).unwrap();
        assert!(reopened
            .dashboard_session_valid(&token, "test-only-passphrase")
            .unwrap());
        assert!(!reopened
            .dashboard_session_valid(&token, "changed-password")
            .unwrap());
        let bytes = fs::read(&path).unwrap();
        assert!(!bytes
            .windows(token.len())
            .any(|slice| slice == token.as_bytes()));
        reopened.revoke_dashboard_session(&token).unwrap();
        assert!(!reopened
            .dashboard_session_valid(&token, "test-only-passphrase")
            .unwrap());
    }

    #[test]
    fn attribution_filtering_and_persistence() {
        let (dir, store) = store();
        let now = now_ms();
        let mut caller = Caller::from_key("session:abc", "test");
        caller.client_name = Some("Claude Desktop".into());
        caller.client_version = Some("1.2".into());
        store
            .record(Event {
                caller: caller.clone(),
                started_ms: now,
                duration_ms: 42.0,
                kind: "tool",
                name: "exec".into(),
                status: "ok",
                target: Some("ssh:mac".into()),
                argument_keys: vec!["command".into(), "target".into()],
                rpc_error_code: None,
                details: None,
            })
            .unwrap();
        store
            .record(Event {
                caller,
                started_ms: now,
                duration_ms: 25.0,
                kind: "tool",
                name: "file_read".into(),
                status: "error",
                target: None,
                argument_keys: vec!["path".into()],
                rpc_error_code: None,
                details: None,
            })
            .unwrap();
        let data = store
            .query(Query {
                client: Some("Claude Desktop".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(data["summary"]["count"], 2);
        assert_eq!(data["summary"]["errors"], 1);
        assert_eq!(data["summary"]["sessions"], 1);
        assert_eq!(data["calls"][0]["name"], "file_read");
        assert_eq!(data["sessions"][0]["client_version"], "1.2");
        assert_eq!(
            data["calls"][1]["argument_keys"],
            json!(["command", "target"])
        );
        assert_eq!(
            store
                .query(Query {
                    status: Some("error".into()),
                    ..Default::default()
                })
                .unwrap()["summary"]["count"],
            1
        );
        assert_eq!(data["summary"]["p95_ms"], json!(42.0));
        assert_eq!(
            store
                .query(Query {
                    search: Some("ssh:mac".into()),
                    ..Default::default()
                })
                .unwrap()["summary"]["count"],
            1
        );
        assert_eq!(
            store
                .query(Query {
                    search: Some("%".into()),
                    ..Default::default()
                })
                .unwrap()["summary"]["count"],
            0
        );
        let first = store
            .query(Query {
                limit: Some(1),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(first["calls"].as_array().unwrap().len(), 1);
        let next = first["next_before"].as_i64().unwrap();
        assert_eq!(
            store
                .query(Query {
                    limit: Some(1),
                    before: Some(next),
                    ..Default::default()
                })
                .unwrap()["calls"][0]["name"],
            "exec"
        );
        drop(store);
        let reopened = ActivityStore::open(
            &dir.path().join("activity/calls.sqlite3"),
            &ActivityConfig::default(),
        )
        .unwrap();
        assert_eq!(
            reopened.query(Query::default()).unwrap()["summary"]["count"],
            2
        );
    }

    #[test]
    fn completion_order_cannot_regress_session_timestamps() {
        let (_dir, store) = store();
        let now = now_ms();
        for started in [now, now - 5_000] {
            store
                .record(Event {
                    caller: Caller::from_key("session:out-of-order", "test"),
                    started_ms: started,
                    duration_ms: 0.25,
                    kind: "tool",
                    name: "ping".into(),
                    status: "ok",
                    target: None,
                    argument_keys: vec![],
                    rpc_error_code: None,
                    details: None,
                })
                .unwrap();
        }
        let data = store.query(Query::default()).unwrap();
        assert_eq!(data["sessions"][0]["first_seen_ms"], now - 5_000);
        assert_eq!(data["sessions"][0]["last_seen_ms"], now);
    }

    #[cfg(unix)]
    #[test]
    fn existing_public_directory_is_not_chmodded() {
        use std::os::unix::fs::PermissionsExt;
        let tmp = tempdir().unwrap();
        let public_dir = tmp.path().join("existing");
        fs::create_dir_all(&public_dir).unwrap();
        fs::set_permissions(&public_dir, fs::Permissions::from_mode(0o755)).unwrap();
        let result = ActivityStore::open(
            &public_dir.join("calls.sqlite3"),
            &ActivityConfig::default(),
        );
        assert!(result.is_err());
        assert_eq!(
            fs::metadata(&public_dir).unwrap().permissions().mode() & 0o777,
            0o755
        );
    }

    #[test]
    fn credential_group_survives_http_client_recreation() {
        for key in ["bearer:hash", "static-bearer:hash", "oauth-grant:principal"] {
            let caller = Caller::from_key(key, "http");
            assert_eq!(caller.key, key);
            assert_eq!(caller.job_owner_key, key);
            assert_eq!(caller.transport, "http");
            assert_eq!(caller.grouping, "credential_group");
            assert_eq!(caller.session_id, None);
        }
    }

    #[test]
    fn retention_count_and_grouping() {
        let (dir, _) = store();
        let cfg = ActivityConfig {
            max_calls: 2,
            ..ActivityConfig::default()
        };
        let store = ActivityStore::open(&dir.path().join("activity/calls.sqlite3"), &cfg).unwrap();
        for i in 0..3 {
            let caller = Caller::from_key(&format!("bearer:{i}"), "test");
            assert_eq!(caller.grouping, "credential_group");
            store
                .record(Event {
                    caller,
                    started_ms: now_ms(),
                    duration_ms: 0.0,
                    kind: "protocol",
                    name: "ping".into(),
                    status: "ok",
                    target: None,
                    argument_keys: vec![],
                    rpc_error_code: None,
                    details: None,
                })
                .unwrap();
        }
        store.cleanup().unwrap();
        assert_eq!(
            store.query(Query::default()).unwrap()["summary"]["count"],
            2
        );
    }
}
