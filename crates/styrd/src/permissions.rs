//! Permission broker: a blocking hook waits here for the phone.
//!
//! Exactly one `PermissionResolved` is recorded per request, whichever way it ends:
//! - the phone answers → `Selected { … }` by `Phone(device)`;
//! - nobody answers in time → `TimedOut` by `Daemon` (the agent falls back to its
//!   own prompt in the terminal);
//! - the hook process goes away (agent killed it, the user answered in the terminal,
//!   the session ended) → `Cancelled` by `Terminal`, recorded when the wait is dropped.

use crate::daemon::Daemon;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use styr_core::event::{
    EventKind, EventSource, OptionKind, PermissionOption, PermissionOutcome, Resolver,
};
use styr_core::ids::{DeviceId, NativeSessionRef, RequestId, SessionId};
use styr_core::ipc::SessionUpdate;
use tokio::sync::oneshot;

#[derive(Debug)]
pub struct Answer {
    pub option: String,
    pub kind: OptionKind,
    pub device: DeviceId,
}

struct Pending {
    session: SessionId,
    options: Vec<PermissionOption>,
    tx: oneshot::Sender<Answer>,
}

#[derive(Default)]
pub struct Broker {
    pending: Mutex<HashMap<RequestId, Pending>>,
}

#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ResolveError {
    #[error("that request is no longer waiting for an answer")]
    NotPending,
    #[error("'{0}' is not an option of that request")]
    BadOption(String),
}

impl Broker {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<RequestId, Pending>> {
        self.pending.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Answer a pending request from the phone.
    pub fn resolve(
        &self,
        session: &SessionId,
        request: &RequestId,
        option: &str,
        device: DeviceId,
    ) -> Result<(), ResolveError> {
        let mut map = self.lock();
        let p = map
            .get(request)
            .filter(|p| &p.session == session)
            .ok_or(ResolveError::NotPending)?;
        let kind = p
            .options
            .iter()
            .find(|o| o.id == option)
            .map(|o| o.kind)
            .ok_or_else(|| ResolveError::BadOption(option.to_owned()))?;
        let p = map.remove(request).expect("present");
        p.tx.send(Answer {
            option: option.to_owned(),
            kind,
            device,
        })
        .map_err(|_| ResolveError::NotPending)
    }

    pub fn is_pending(&self, request: &RequestId) -> bool {
        self.lock().contains_key(request)
    }
}

/// Records the resolution exactly once, including when the wait is cancelled.
struct Recorder {
    daemon: Arc<Daemon>,
    native: NativeSessionRef,
    request: RequestId,
    done: bool,
}

impl Recorder {
    fn record(&mut self, outcome: PermissionOutcome, by: Resolver) {
        if std::mem::replace(&mut self.done, true) {
            return;
        }
        self.daemon.broker.lock().remove(&self.request);
        // The adapter may already have closed it (e.g. PostToolUse arrived first).
        let still_open = self
            .daemon
            .session_view(&self.native)
            .map(|v| v.open_requests.contains(&self.request))
            .unwrap_or(true);
        if !still_open {
            return;
        }
        let ev = EventKind::PermissionResolved {
            request: self.request.clone(),
            outcome,
            by,
        };
        if let Err(e) = self.daemon.ingest(
            &SessionUpdate::for_native(self.native.clone()),
            EventSource::Daemon,
            vec![ev],
        ) {
            tracing::warn!("recording permission resolution failed: {e:#}");
        }
    }
}

impl Drop for Recorder {
    fn drop(&mut self) {
        // The hook went away before an answer: the user dealt with it in the terminal.
        self.record(PermissionOutcome::Cancelled, Resolver::Terminal);
    }
}

impl Daemon {
    /// Block until the phone answers, the timeout passes, or the caller drops this future.
    pub async fn wait_for_permission(
        self: &Arc<Self>,
        session: SessionId,
        native: NativeSessionRef,
        request: RequestId,
        options: Vec<PermissionOption>,
        timeout: Duration,
    ) -> PermissionOutcome {
        let (tx, rx) = oneshot::channel();
        self.broker.lock().insert(
            request.clone(),
            Pending {
                session,
                options,
                tx,
            },
        );
        let mut rec = Recorder {
            daemon: self.clone(),
            native,
            request,
            done: false,
        };
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(a)) => {
                let outcome = PermissionOutcome::Selected {
                    option: a.option,
                    kind: a.kind,
                };
                rec.record(outcome.clone(), Resolver::Phone(a.device));
                outcome
            }
            Ok(Err(_)) => {
                rec.record(PermissionOutcome::Cancelled, Resolver::Daemon);
                PermissionOutcome::Cancelled
            }
            Err(_) => {
                rec.record(PermissionOutcome::TimedOut, Resolver::Daemon);
                PermissionOutcome::TimedOut
            }
        }
    }
}
