use std::io;
use std::io::ErrorKind;
use std::path::Path;
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;
use codex_uds::UnixStream;
use serde::Serialize;
use tokio::fs;
use tokio::io::AsyncWriteExt;
use tokio::io::WriteHalf;
use tokio::sync::Mutex;
use tracing::warn;

use super::protocol::ServerLine;

pub(super) async fn prepare_broker_socket_directory(socket_path: &Path) -> io::Result<()> {
    let Some(version_dir) = socket_path.parent() else {
        return Ok(());
    };
    if version_dir.file_name().and_then(|name| name.to_str()) == Some(super::SOCKET_VERSION_DIR)
        && let Some(socket_dir) = version_dir.parent()
        && socket_dir.file_name().and_then(|name| name.to_str()) == Some(super::SOCKET_DIR_NAME)
    {
        codex_uds::prepare_private_socket_directory(socket_dir).await?;
        return codex_uds::prepare_private_socket_directory(version_dir).await;
    }
    codex_uds::prepare_private_socket_directory(version_dir).await
}

pub(super) async fn prepare_broker_socket_path(socket_path: &Path) -> io::Result<()> {
    prepare_broker_socket_directory(socket_path).await?;

    match UnixStream::connect(socket_path).await {
        Ok(_stream) => {
            return Err(io::Error::new(
                ErrorKind::AddrInUse,
                format!(
                    "MCP broker socket is already in use at {}",
                    socket_path.display()
                ),
            ));
        }
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(()),
        Err(err) if err.kind() == ErrorKind::ConnectionRefused => {}
        Err(err) => {
            if !socket_path.exists() {
                return Ok(());
            }
            return Err(err);
        }
    }

    if !socket_path.try_exists()? {
        return Ok(());
    }

    if !codex_uds::is_stale_socket_path(socket_path).await? {
        return Err(io::Error::new(
            ErrorKind::AlreadyExists,
            format!(
                "MCP broker socket path exists and is not a socket: {}",
                socket_path.display()
            ),
        ));
    }
    fs::remove_file(socket_path).await
}

#[cfg(unix)]
pub(super) async fn set_control_socket_permissions(socket_path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600)).await
}

#[cfg(not(unix))]
pub(super) async fn set_control_socket_permissions(_socket_path: &Path) -> io::Result<()> {
    Ok(())
}

pub(super) struct SocketFileGuard {
    pub(super) socket_path: PathBuf,
}

impl Drop for SocketFileGuard {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.socket_path)
            && error.kind() != ErrorKind::NotFound
        {
            warn!("failed to remove MCP broker socket file: {error}");
        }
    }
}

#[allow(
    clippy::await_holding_invalid_type,
    reason = "The JSONL writer must serialize complete lines; the guard spans write and flush for one message."
)]
pub(super) async fn write_line<T>(
    writer: &Arc<Mutex<WriteHalf<UnixStream>>>,
    value: &T,
) -> Result<()>
where
    T: Serialize,
{
    let mut line = serde_json::to_vec(value)?;
    line.push(b'\n');
    let mut writer = writer.lock().await;
    writer.write_all(&line).await?;
    writer.flush().await?;
    Ok(())
}

pub(super) async fn write_response(
    writer: &Arc<Mutex<WriteHalf<UnixStream>>>,
    id: String,
    result: Option<serde_json::Value>,
    error: Option<String>,
) -> Result<()> {
    write_line(writer, &ServerLine::Response { id, result, error }).await
}

pub(super) fn is_recoverable_accept_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        ErrorKind::ConnectionAborted | ErrorKind::Interrupted | ErrorKind::WouldBlock
    )
}
