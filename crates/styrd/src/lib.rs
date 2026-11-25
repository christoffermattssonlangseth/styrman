//! `styrd`, the Styrman daemon. A library so integration tests can run it in-process.

pub mod daemon;
pub mod ipc;
pub mod store;

use anyhow::{Context, Result, bail};
use std::fs::File;
use std::future::Future;
use std::sync::Arc;
use styr_core::paths::Paths;

pub use daemon::Daemon;

/// Holds the single-instance lock for as long as it lives.
pub struct InstanceLock(#[allow(dead_code)] File);

pub fn lock_instance(paths: &Paths) -> Result<InstanceLock> {
    std::fs::create_dir_all(&paths.data_dir)?;
    let file = File::options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(paths.data_dir.join("styrd.lock"))?;
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

/// Run the daemon until `shutdown` resolves.
pub async fn run(paths: Paths, shutdown: impl Future<Output = ()>) -> Result<()> {
    let _lock = lock_instance(&paths)?;
    let store = store::Store::open(&paths.db)?;
    let daemon: Arc<Daemon> = Daemon::open(store)?;
    let listener = ipc::bind(&paths.socket)?;
    tracing::info!(socket = %paths.socket.display(), db = %paths.db.display(), "styrd {} ready", styr_core::VERSION);
    let result = ipc::serve(daemon, listener, shutdown).await;
    let _ = std::fs::remove_file(&paths.socket);
    tracing::info!("styrd stopped");
    result
}
