//! SQLite persistence: sessions and their sequenced event history.

use anyhow::{Context, Result, bail};
use rusqlite::{Connection, OptionalExtension, params};
use std::path::{Path, PathBuf};
use styr_core::event::{Event, EventKind, EventSource, TimestampMs};
use styr_core::ids::{AgentKind, NativeSessionRef, SessionId};
use styr_core::ipc::SessionUpdate;

const SCHEMA_VERSION: i64 = 3;

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
    #[serde(skip)]
    pub push_token: Option<String>,
    /// "sandbox" or "production".
    #[serde(skip)]
    pub push_env: Option<String>,
    /// The phone's current Live Activity push token (updates and ends it).
    #[serde(skip)]
    pub activity_token: Option<String>,
    /// The phone's push-to-start token (starts a Live Activity while the app is closed).
    #[serde(skip)]
    pub start_token: Option<String>,
    /// Notifications at all (set from the phone's settings).
    #[serde(skip)]
    pub notify: bool,
    /// Also when a turn finishes or a session is shared (approvals and errors always).
    #[serde(skip)]
    pub notify_finished: bool,
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
    pub agent_pid: Option<u32>,
    pub launch_id: Option<String>,
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
        // v3: APNs device tokens.
        let has_push: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('devices') WHERE name = 'push_token'")?
            .exists([])?;
        if !has_push {
            conn.execute_batch(
                "ALTER TABLE devices ADD COLUMN push_token TEXT;
                 ALTER TABLE devices ADD COLUMN push_env TEXT;",
            )?;
        }
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS meta (key TEXT PRIMARY KEY, value TEXT NOT NULL);",
        )?;
        // v3: the agent's process id, to notice sessions that died without SessionEnd.
        let has_pid: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'agent_pid'")?
            .exists([])?;
        if !has_pid {
            conn.execute_batch("ALTER TABLE sessions ADD COLUMN agent_pid INTEGER;")?;
        }
        // v3: the `styr <agent>` wrapper a session runs in (survives daemon restarts).
        let has_launch: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('sessions') WHERE name = 'launch_id'")?
            .exists([])?;
        if !has_launch {
            conn.execute_batch("ALTER TABLE sessions ADD COLUMN launch_id TEXT;")?;
        }
        // Live Activity tokens (update + push-to-start).
        let has_live: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('devices') WHERE name = 'activity_token'")?
            .exists([])?;
        if !has_live {
            conn.execute_batch(
                "ALTER TABLE devices ADD COLUMN activity_token TEXT;
                 ALTER TABLE devices ADD COLUMN start_token TEXT;",
            )?;
        }
        // Which notifications each phone wants.
        let has_notify: bool = conn
            .prepare("SELECT 1 FROM pragma_table_info('devices') WHERE name = 'notify'")?
            .exists([])?;
        if !has_notify {
            conn.execute_batch(
                "ALTER TABLE devices ADD COLUMN notify INTEGER NOT NULL DEFAULT 1;
                 ALTER TABLE devices ADD COLUMN notify_finished INTEGER NOT NULL DEFAULT 1;",
            )?;
        }
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
            agent_pid: r.get::<_, Option<i64>>("agent_pid")?.map(|p| p as u32),
            launch_id: r.get("launch_id")?,
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
                        updated_ms = ?6,
                        agent_pid = COALESCE(?7, agent_pid),
                        launch_id = COALESCE(?8, launch_id)
                     WHERE id = ?1",
                    params![
                        id,
                        path_str(&update.cwd),
                        path_str(&update.transcript_path),
                        update.model,
                        update.title,
                        now,
                        update.agent_pid.map(|p| p as i64),
                        update.launch_id
                    ],
                )?;
                id
            }
            None => {
                let id = SessionId::generate().0;
                tx.execute(
                    "INSERT INTO sessions (id, agent, native_id, cwd, transcript_path, model, title, created_ms, updated_ms, agent_pid, launch_id)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?8, ?9, ?10)",
                    params![
                        id,
                        native.agent.as_str(),
                        native.native_id,
                        path_str(&update.cwd),
                        path_str(&update.transcript_path),
                        update.model,
                        update.title,
                        now,
                        update.agent_pid.map(|p| p as i64),
                        update.launch_id
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

    pub fn meta(&self, key: &str) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row("SELECT value FROM meta WHERE key = ?1", [key], |r| r.get(0))
            .optional()?)
    }

    pub fn set_meta(&mut self, key: &str, value: &str) -> Result<()> {
        self.conn.execute(
            "INSERT INTO meta (key, value) VALUES (?1, ?2) ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![key, value],
        )?;
        Ok(())
    }

    pub fn delete_meta(&mut self, key: &str) -> Result<()> {
        self.conn
            .execute("DELETE FROM meta WHERE key = ?1", [key])?;
        Ok(())
    }

    /// Delete a session and (by cascade) its history. Returns whether it existed.
    pub fn delete_session(&mut self, id: &SessionId) -> Result<bool> {
        Ok(self
            .conn
            .execute("DELETE FROM sessions WHERE id = ?1", [id.as_str()])?
            > 0)
    }

    pub fn set_title_if_empty(&mut self, id: &SessionId, title: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET title = ?2 WHERE id = ?1 AND (title IS NULL OR title = '')",
            params![id.as_str(), title],
        )?;
        Ok(())
    }

    /// Give a placeholder session (see `Daemon::share_launch`) its real native id.
    pub fn rename_native(&mut self, id: &SessionId, native_id: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sessions SET native_id = ?2 WHERE id = ?1",
            params![id.as_str(), native_id],
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

    /// Up to `limit` events after `since_seq`. A row that no longer decodes (written
    /// by a newer or broken build) is skipped and logged, never an error: one bad
    /// row must not break every backfill.
    pub fn events_since(
        &self,
        session: &SessionId,
        since_seq: u64,
        limit: u32,
    ) -> Result<Vec<Event>> {
        let mut st = self.conn.prepare(
            "SELECT seq, at_ms, source, body FROM events WHERE session_id = ?1 AND seq > ?2 ORDER BY seq LIMIT ?3",
        )?;
        let mut out = Vec::new();
        let mut after = since_seq;
        loop {
            let want = limit - out.len() as u32;
            let rows = st.query_map(params![session.as_str(), after as i64, want], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                ))
            })?;
            let mut fetched = 0;
            for row in rows {
                let (seq, at_ms, source, body) = row?;
                fetched += 1;
                after = seq as u64;
                let decoded = serde_json::from_value(serde_json::Value::String(source))
                    .and_then(|source| Ok((source, serde_json::from_str(&body)?)));
                match decoded {
                    Ok((source, kind)) => out.push(Event {
                        session: session.clone(),
                        seq: seq as u64,
                        at_ms,
                        source,
                        kind,
                    }),
                    Err(e) => {
                        tracing::warn!(%session, seq, "skipping an event that doesn't decode: {e}")
                    }
                }
            }
            // Short of `limit` only because rows were skipped: read on.
            if fetched < want || out.len() as u32 >= limit {
                break;
            }
        }
        Ok(out)
    }

    /// The device that sent the session's latest prompt (`origin.device`), if any.
    pub fn last_prompt_device(&self, session: &SessionId) -> Result<Option<String>> {
        Ok(self
            .conn
            .query_row(
                "SELECT json_extract(body, '$.origin.device') FROM events
                 WHERE session_id = ?1 AND kind = 'user_message' ORDER BY seq DESC LIMIT 1",
                [session.as_str()],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// All event kinds of a session, oldest first (for rebuilding status on start).
    /// Rows that don't decode are skipped and logged.
    pub fn event_kinds(&self, session: &SessionId) -> Result<Vec<EventKind>> {
        let mut st = self
            .conn
            .prepare("SELECT body FROM events WHERE session_id = ?1 ORDER BY seq")?;
        let rows = st.query_map([session.as_str()], |r| r.get::<_, String>(0))?;
        let mut out = Vec::new();
        for body in rows {
            match serde_json::from_str(&body?) {
                Ok(kind) => out.push(kind),
                // One bad row must not keep the daemon from starting.
                Err(e) => tracing::warn!(%session, "skipping an event that doesn't decode: {e}"),
            }
        }
        Ok(out)
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
            push_token: r.get("push_token")?,
            push_env: r.get("push_env")?,
            activity_token: r.get("activity_token")?,
            start_token: r.get("start_token")?,
            notify: r.get::<_, i64>("notify")? != 0,
            notify_finished: r.get::<_, i64>("notify_finished")? != 0,
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

    pub fn set_push_token(
        &mut self,
        id: &str,
        token: Option<&str>,
        env: Option<&str>,
    ) -> Result<()> {
        self.conn.execute(
            "UPDATE devices SET push_token = ?2, push_env = ?3 WHERE id = ?1",
            params![id, token, env],
        )?;
        Ok(())
    }

    /// Which notifications a device wants.
    pub fn set_notify(&mut self, id: &str, on: bool, finished: bool) -> Result<()> {
        self.conn.execute(
            "UPDATE devices SET notify = ?2, notify_finished = ?3 WHERE id = ?1",
            params![id, on, finished],
        )?;
        Ok(())
    }

    /// Set (or clear) a device's Live Activity token; `start` is the push-to-start token.
    pub fn set_activity_token(&mut self, id: &str, start: bool, token: Option<&str>) -> Result<()> {
        let sql = if start {
            "UPDATE devices SET start_token = ?2 WHERE id = ?1"
        } else {
            "UPDATE devices SET activity_token = ?2 WHERE id = ?1"
        };
        self.conn.execute(sql, params![id, token])?;
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
    fn a_phone_keeps_its_notification_settings() {
        let mut s = Store::open_in_memory().unwrap();
        s.insert_device("d", "Phone", b"h", 1).unwrap();
        let d = s.device("d").unwrap().unwrap();
        assert!(
            d.notify && d.notify_finished,
            "everything, until the phone says otherwise"
        );
        s.set_notify("d", true, false).unwrap();
        let d = s.device("d").unwrap().unwrap();
        assert!(d.notify && !d.notify_finished);
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
    fn an_undecodable_row_is_skipped() {
        let mut s = Store::open_in_memory().unwrap();
        let (rec, _) = s.upsert_session(&upd("x"), 1).unwrap();
        for i in 0..4 {
            s.append(
                &rec.id,
                EventSource::Hook,
                EventKind::SessionEnded { reason: None },
                10 + i,
            )
            .unwrap();
        }
        s.conn
            .execute(
                "UPDATE events SET body = '{\"type\":\"from_the_future\"}' WHERE seq = 2",
                [],
            )
            .unwrap();
        assert_eq!(s.event_kinds(&rec.id).unwrap().len(), 3);
        // A page that lost a row to skipping still fills up from later rows.
        let evs = s.events_since(&rec.id, 0, 2).unwrap();
        assert_eq!(evs.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![1, 3]);
        let evs = s.events_since(&rec.id, 3, 10).unwrap();
        assert_eq!(evs.iter().map(|e| e.seq).collect::<Vec<_>>(), vec![4]);
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
