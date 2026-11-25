//! Unix-socket server for `styr` (CLI and hook helper).

use crate::daemon::Daemon;
use anyhow::{Context, Result, bail};
use std::future::Future;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::sync::Arc;
use styr_core::ipc::{MAX_FRAME_BYTES, Request, Response};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};

/// macOS limits `sun_path` to 104 bytes including the NUL.
const MAX_SOCKET_PATH: usize = 103;

pub fn bind(socket: &Path) -> Result<UnixListener> {
    if socket.as_os_str().len() > MAX_SOCKET_PATH {
        bail!(
            "socket path too long ({} bytes): {}",
            socket.as_os_str().len(),
            socket.display()
        );
    }
    if let Some(dir) = socket.parent() {
        std::fs::create_dir_all(dir)?;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    // The caller holds the instance lock, so an existing socket file is stale.
    match std::fs::remove_file(socket) {
        Ok(()) => tracing::info!("removed stale socket {}", socket.display()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e).context("removing stale socket"),
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn serve(
    daemon: Arc<Daemon>,
    listener: UnixListener,
    shutdown: impl Future<Output = ()>,
) -> Result<()> {
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            _ = &mut shutdown => return Ok(()),
            accepted = listener.accept() => match accepted {
                Ok((stream, _)) => {
                    let d = daemon.clone();
                    tokio::spawn(async move {
                        if let Err(e) = connection(d, stream).await {
                            tracing::debug!("ipc connection: {e:#}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("accept failed: {e}");
                    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                }
            },
        }
    }
}

async fn connection(daemon: Arc<Daemon>, stream: UnixStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut reader = BufReader::new(read.take(MAX_FRAME_BYTES as u64));
    let mut line = String::new();
    if reader.read_line(&mut line).await? == 0 {
        return Ok(());
    }
    let response = match serde_json::from_str::<Request>(&line) {
        Ok(req) => daemon.handle(req).await,
        Err(e) => Response::Error {
            message: format!("bad request: {e}"),
        },
    };
    let mut out = serde_json::to_vec(&response)?;
    out.push(b'\n');
    write.write_all(&out).await?;
    write.shutdown().await?;
    Ok(())
}
