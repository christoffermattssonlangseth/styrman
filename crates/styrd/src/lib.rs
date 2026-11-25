//! `styrd`, the Styrman daemon. A library so integration tests can run it in-process.

pub mod api;
pub mod auth;
pub mod daemon;
pub mod ipc;
pub mod launcher;
pub mod peers;
pub mod permissions;
pub mod prompts;
pub mod push;
pub mod store;
pub mod terminals;
mod usage;

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::future::Future;
use std::sync::Arc;
use styr_core::paths::Paths;

pub use daemon::Daemon;

/// Session history and keys are private: data dir 0700, its files 0600.
fn restrict_permissions(paths: &Paths) {
    use std::os::unix::fs::PermissionsExt;
    let _ = std::fs::set_permissions(&paths.data_dir, std::fs::Permissions::from_mode(0o700));
    if let Ok(entries) = std::fs::read_dir(&paths.data_dir) {
        for e in entries.flatten() {
            let p = e.path();
            if p.is_file() {
                let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o600));
            }
        }
    }
}

/// Holds the single-instance lock for as long as it lives.
pub struct InstanceLock(#[allow(dead_code)] File);

pub fn lock_instance(paths: &Paths) -> Result<InstanceLock> {
    std::fs::create_dir_all(&paths.data_dir)?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.data_dir.join("styrd.lock"))?;
    // A just-stopped instance (or a child process mid-exec) can hold the lock for a
    // moment; retry briefly before declaring another daemon alive.
    for _ in 0..20 {
        match file.try_lock() {
            Ok(()) => return Ok(InstanceLock(file)),
            Err(std::fs::TryLockError::WouldBlock) => {
                std::thread::sleep(std::time::Duration::from_millis(100))
            }
            Err(std::fs::TryLockError::Error(e)) => return Err(e).context("locking instance"),
        }
    }
    match file.try_lock() {
        Ok(()) => Ok(InstanceLock(file)),
        Err(std::fs::TryLockError::WouldBlock) => {
            bail!(
                "another styrd is already running for {}",
                paths.data_dir.display()
            )
        }
        Err(std::fs::TryLockError::Error(e)) => Err(e).context("locking instance"),
    }
}

/// How long the phone can answer a permission: `STYR_PERMISSION_TIMEOUT` seconds
/// (default 540). The terminal prompt is shown in parallel either way.
pub fn permission_timeout() -> std::time::Duration {
    let secs = std::env::var("STYR_PERMISSION_TIMEOUT")
        .ok()
        .and_then(|s| s.parse::<u64>().ok())
        .filter(|s| (1..=540).contains(s)) // stays below the plugin's 600 s hook timeout
        .unwrap_or(540);
    std::time::Duration::from_secs(secs)
}

/// Every adapter this build knows about.
pub fn default_adapters() -> Vec<Arc<dyn styr_core::adapter::AgentAdapter>> {
    vec![
        Arc::new(styr_adapter_claude::ClaudeAdapter {
            config: styr_adapter_claude::ClaudeConfig {
                permission_timeout: permission_timeout(),
            },
        }),
        Arc::new(styr_adapter_agy::AntigravityAdapter),
        Arc::new(styr_adapter_codex::CodexAdapter),
        Arc::new(styr_adapter_pty::PtyAdapter),
    ]
}

/// Run the daemon until `shutdown` resolves, with the phone API configured from the environment.
pub async fn run(paths: Paths, shutdown: impl Future<Output = ()>) -> Result<()> {
    run_with(paths, api::ApiConfig::from_env(), shutdown).await
}

pub async fn run_with(
    paths: Paths,
    api_cfg: api::ApiConfig,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    let _lock = lock_instance(&paths)?;
    let store = store::Store::open(&paths.db)?;
    restrict_permissions(&paths);
    let daemon: Arc<Daemon> = Daemon::open(store, default_adapters())?;
    let listener = ipc::bind(&paths.socket)?;
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    match daemon.reload_push(Some(&paths.data_dir)) {
        Ok(true) => tracing::info!("push notifications on"),
        Ok(false) => tracing::info!("push notifications off (run `styr push setup`)"),
        Err(e) => tracing::warn!("push notifications off: {e:#}"),
    }
    let notifier = tokio::spawn(push::run_notifier(daemon.clone(), stop_rx.clone()));
    let reaper = {
        let d = daemon.clone();
        let mut stop = stop_rx.clone();
        tokio::spawn(async move {
            loop {
                if let Err(e) = d.reap_dead_sessions() {
                    tracing::warn!("checking for dead sessions: {e:#}");
                }
                tokio::select! {
                    _ = stop.wait_for(|v| *v) => return,
                    _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
                }
            }
        })
    };
    let api = tokio::spawn(api::run_listeners(daemon.clone(), api_cfg, stop_rx));
    tracing::info!(socket = %paths.socket.display(), db = %paths.db.display(), "styrd {} ready", styr_core::VERSION);
    let result = ipc::serve(daemon, listener, shutdown).await;
    let _ = stop_tx.send(true);
    let _ = api.await;
    let _ = notifier.await;
    let _ = reaper.await;
    let _ = std::fs::remove_file(&paths.socket);
    tracing::info!("styrd stopped");
    result
}
