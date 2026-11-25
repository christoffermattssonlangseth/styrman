//! SQLite persistence: sessions and their sequenced event history.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use styr_core::event::{Event, EventKind, EventSource, TimestampMs};
use styr_core::ids::{AgentKind, NativeSessionRef, SessionId};
use styr_core::ipc::SessionUpdate;

const SCHEMA_VERSION: i64 = 2;

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sessions (
    id              TEXT PRIMARY KEY,
    agent           TEXT NOT NULL,
    native_id       TEXT NOT NULL,
    cwd             TEXT,
    transcript_path TEXT,
    model           TEXT,
    title           TEXT,
    shared          INTEGER NOT NULL DEFAULT 0,
    created_ms      INTEGER NOT NULL,
    updated_ms      INTEGER NOT NULL,
    last_seq        INTEGER NOT NULL DEFAULT 0,
    UNIQUE (agent, native_id)
);
CREATE TABLE IF NOT EXISTS events (
    session_id TEXT    NOT NULL REFERENCES sessions(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    at_ms      INTEGER NOT NULL,
    source     TEXT    NOT NULL,
    kind       TEXT    NOT NULL,
    body       TEXT    NOT NULL,
    PRIMARY KEY (session_id, seq)
);
"#;

/// v2: paired phones. Only a SHA-256 of each device secret is stored.
const SCHEMA_V2: &str = r#"
CREATE TABLE IF NOT EXISTS devices (
    id           TEXT PRIMARY KEY,
    name         TEXT NOT NULL,
    secret_hash  BLOB NOT NULL,
    created_ms   INTEGER NOT NULL,
    last_seen_ms INTEGER,
    revoked      INTEGER NOT NULL DEFAULT 0
);
"#;

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct DeviceRecord {
    pub id: String,
    pub name: String,
    #[serde(skip)]
    pub secret_hash: Vec<u8>,
    pub created_ms: TimestampMs,
    pub last_seen_ms: Option<TimestampMs>,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct SessionRecord {
    pub id: SessionId,
    pub native: NativeSessionRef,
    pub cwd: Option<PathBuf>,
    pub transcript_path: Option<PathBuf>,
    pub model: Option<String>,
    pub title: Option<String>,
    pub shared: bool,
    pub created_ms: TimestampMs,
    pub updated_ms: TimestampMs,
    pub last_seq: u64,
}

pub struct Store {
    conn: Connection,
}

fn enum_str<T: serde::Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

fn path_str(p: &Option<PathBuf>) -> Option<String> {
    p.as_ref().map(|p| p.to_string_lossy().into_owned())
}

impl Store {
    pub fn open(path: &Path) -> Result<Self> {
        let conn = Connection::open(path).with_context(|| format!("opening {}", path.display()))?;
        Self::init(conn)
    }

    pub fn open_in_memory() -> Result<Self> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(conn: Connection) -> Result<Self> {
        conn.pragma_update(None, "journal_mode", "WAL")?;
        conn.pragma_update(None, "synchronous", "NORMAL")?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        conn.busy_timeout(std::time::Duration::from_secs(5))?;
        let version: i64 = conn.pragma_query_value(None, "user_version", |r| r.get(0))?;
        if version > SCHEMA_VERSION {
            bail!("database schema v{version} is newer than this styrd (v{SCHEMA_VERSION})");
        }
        conn.execute_batch(SCHEMA)?;
        conn.execute_batch(SCHEMA_V2)?;
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)?;
        Ok(Self { conn })
    }

    fn row_to_session(r: &rusqlite::Row<'_>) -> rusqlite::Result<SessionRecord> {
        let agent: String = r.get("agent")?;
        let agent: AgentKind = agent.parse().map_err(|e: String| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, e.into())
        })?;
        Ok(SessionRecord {
            id: SessionId(r.get("id")?),
            native: NativeSessionRef::new(agent, r.get::<_, String>("native_id")?),
            cwd: r.get::<_, Option<String>>("cwd")?.map(PathBuf::from),
            transcript_path: r
                .get::<_, Option<String>>("transcript_path")?
                .map(PathBuf::from),
            model: r.get("model")?,
            title: r.get("title")?,
            shared: r.get::<_, i64>("shared")? != 0,
            created_ms: r.get("created_ms")?,
            updated_ms: r.get("updated_ms")?,
            last_seq: r.get::<_, i64>("last_seq")? as u64,
        })
    }

    pub fn session(&self, id: &SessionId) -> Result<Option<SessionRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM sessions WHERE id = ?1",
                [id.as_str()],
                Self::row_to_session,
            )
            .optional()?)
    }

    pub fn session_by_native(&self, native: &NativeSessionRef) -> Result<Option<SessionRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM sessions WHERE agent = ?1 AND native_id = ?2",
                params![native.agent.as_str(), native.native_id],
                Self::row_to_session,
            )
            .optional()?)
    }

    pub fn sessions(&self) -> Result<Vec<SessionRecord>> {
        let mut st = self
            .conn
            .prepare("SELECT * FROM sessions ORDER BY updated_ms DESC")?;
        let rows = st.query_map([], Self::row_to_session)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    /// Find or create the session for `update.native`, then apply its non-empty fields.
    /// Returns the record and whether it was created.
    pub fn upsert_session(
        &mut self,
        update: &SessionUpdate,
        now: TimestampMs,
    ) -> Result<(SessionRecord, bool)> {
        let native = update
            .native
            .as_ref()
            .context("session update without a native id")?;
        let tx = self.conn.transaction()?;
        let existing: Option<String> = tx
            .query_row(
                "SELECT id FROM sessions WHERE agent = ?1 AND native_id = ?2",
                params![native.agent.as_str(), native.native_id],
                |r| r.get(0),
            )
            .optional()?;
        let created = existing.is_none();
        let id = match existing {
            Some(id) => {
                tx.execute(
                    "UPDATE sessions SET
                        cwd = COALESCE(?2, cwd),
                        transcript_path = COALESCE(?3, transcript_path),
                        model = COALESCE(?4, model),
                        title = COALESCE(?5, title),
                        updated_ms = ?6
                     WHERE id = ?1",
                    params![
                        id,
                        path_str(&update.cwd),
                        path_str(&update.transcript_path),
                        update.model,
                        update.title,
                        now
                    ],
                )?;
                id
            }
            None => {
                let id = SessionId::generate().0;
                tx.execute(
                    "INSERT INTO sessions (id, agent, native_id, cwd, transcript_path, model, title, created_ms, updated_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8)",
                    params![
                        id,
                        native.agent.as_str(),
                        native.native_id,
                        path_str(&update.cwd),
                        path_str(&update.transcript_path),
                        update.model,
                        update.title,
                        now
                    ],
                )?;
                id
            }
        };
        tx.commit()?;
        let rec = self
            .session(&SessionId(id))?
            .context("session vanished after upsert")?;
        Ok((rec, created))
    }

    pub fn set_title_if_empty(&mut self, id: &SessionId, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1 AND (title IS NULL OR title = '')",
            params![id.as_str(), title],
        )?;
        Ok(())
    }

    pub fn set_shared(&mut self, id: &SessionId, shared: bool, now: TimestampMs) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET shared = ?2, updated_ms = ?3 WHERE id = ?1",
            params![id.as_str(), shared as i64, now],
        )?;
        Ok(())
    }

    /// Append one event, assigning the next per-session sequence number atomically.
    pub fn append(
        &mut self,
        session: &SessionId,
        source: EventSource,
        kind: EventKind,
        at_ms: TimestampMs,
    ) -> Result<Event> {
        let tx = self.conn.transaction()?;
        let seq: i64 = tx
            .query_row(
                "UPDATE sessions SET last_seq = last_seq + 1, updated_ms = ?2 WHERE id = ?1 RETURNING last_seq",
                params![session.as_str(), at_ms],
                |r| r.get(0),
            )
            .optional()?
            .with_context(|| format!("unknown session {session}"))?;
        let event = Event {
            session: session.clone(),
            seq: seq as u64,
            at_ms,
            source,
            kind,
        };
        tx.execute(
            "INSERT INTO events (session_id, seq, at_ms, source, kind, body) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                session.as_str(),
                seq,
                at_ms,
                enum_str(&source),
                event.kind.name(),
                serde_json::to_string(&event.kind)?
            ],
        )?;
        tx.commit()?;
        Ok(event)
    }

    pub fn events_since(
        &self,
        session: &SessionId,
        since_seq: u64,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let mut st = self.conn.prepare(
            "SELECT seq, at_ms, source, body FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let rows = st.query_map(params![session.as_str(), since_seq as i64, limit], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            let (seq, at_ms, source, body) = row?;
            out.push(Event {
                session: session.clone(),
                seq: seq as u64,
                at_ms,
                source: serde_json::from_value(serde_json::Value::String(source))?,
                kind: serde_json::from_str(&body)?,
            });
        }
        Ok(out)
    }

    /// All event kinds of a session, oldest first (for rebuilding status on start).
    pub fn event_kinds(&self, session: &SessionId) -> Result<Vec<EventKind>> {
        let mut st = self
            .conn
            .prepare("SELECT body FROM events WHERE session_id = ?1 ORDER BY seq")?;
        let rows = st.query_map([session.as_str()], |r| r.get::<_, String>(0))?;
        rows.map(|b| Ok(serde_json::from_str(&b?)?)).collect()
    }
}

impl Store {
    pub fn insert_device(
        &mut self,
        id: &str,
        name: &str,
        secret_hash: &[u8],
        now: TimestampMs,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT INTO devices (id, name, secret_hash, created_ms) VALUES (?1, ?2, ?3, ?4)",
            params![id, name, secret_hash, now],
        )?;
        Ok(())
    }

    fn row_to_device(r: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRecord> {
        Ok(DeviceRecord {
            id: r.get("id")?,
            name: r.get("name")?,
            secret_hash: r.get("secret_hash")?,
            created_ms: r.get("created_ms")?,
            last_seen_ms: r.get("last_seen_ms")?,
            revoked: r.get::<_, i64>("revoked")? != 0,
        })
    }

    pub fn device(&self, id: &str) -> Result<Option<DeviceRecord>> {
        Ok(self
            .conn
            .query_row(
                "SELECT * FROM devices WHERE id = ?1",
                [id],
                Self::row_to_device,
            )
            .optional()?)
    }

    pub fn devices(&self) -> Result<Vec<DeviceRecord>> {
        let mut st = self
            .conn
            .prepare("SELECT * FROM devices ORDER BY created_ms")?;
        let rows = st.query_map([], Self::row_to_device)?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn touch_device(&mut self, id: &str, now: TimestampMs) -> Result<()> {
        self.conn.execute(
            "UPDATE devices SET last_seen_ms = ?2 WHERE id = ?1",
            params![id, now],
        )?;
        Ok(())
    }

    /// Returns whether a device was revoked.
    pub fn revoke_device(&mut self, id: &str) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE devices SET revoked = 1 WHERE id = ?1 AND revoked = 0",
            [id],
        )? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use styr_core::event::TurnEndReason;

    fn upd(native: &str) -> SessionUpdate {
        SessionUpdate {
            native: Some(NativeSessionRef::new(AgentKind::Claude, native)),
            cwd: Some("/repo".into()),
            ..Default::default()
        }
    }

    #[test]
    fn upsert_is_idempotent_and_merges_fields() {
        let mut s = Store::open_in_memory().unwrap();
        let (a, created) = s.upsert_session(&upd("x"), 1).unwrap();
        assert!(created);
        let mut u = SessionUpdate::for_native(NativeSessionRef::new(AgentKind::Claude, "x"));
        u.model = Some("m".into());
        let (b, created) = s.upsert_session(&u, 2).unwrap();
        assert!(!created);
        assert_eq!(a.id, b.id);
        assert_eq!(
            b.cwd.as_deref(),
            Some(Path::new("/repo")),
            "None must not clear a stored field"
        );
        assert_eq!(b.model.as_deref(), Some("m"));
        // Same native id under another agent is a different session.
        let (c, _) = s
            .upsert_session(
                &SessionUpdate::for_native(NativeSessionRef::new(AgentKind::Codex, "x")),
                3,
            )
            .unwrap();
        assert_ne!(a.id, c.id);
    }

    #[test]
    fn append_assigns_sequential_seqs_and_reads_back() {
        let mut s = Store::open_in_memory().unwrap();
        let (rec, _) = s.upsert_session(&upd("x"), 1).unwrap();
        for i in 0..3 {
            let e = s
                .append(
                    &rec.id,
                    EventSource::Hook,
                    EventKind::TurnEnded {
                        turn: None,
                        reason: TurnEndReason::EndTurn,
                        last_message: Some(i.to_string()),
                    },
                    10 + i,
                )
                .unwrap();
            assert_eq!(e.seq, i as u64 + 1);
        }
        let evs = s.events_since(&rec.id, 1, 10).unwrap();
        assert_eq!(evs.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![2, 3]);
        assert_eq!(s.session(&rec.id).unwrap().unwrap().last_seq, 3);
        assert!(
            s.append(
                &SessionId::from("nope"),
                EventSource::Hook,
                EventKind::SessionEnded { reason: None },
                1
            )
            .is_err()
        );
    }
}
