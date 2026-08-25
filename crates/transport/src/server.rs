//! Concurrent local socket and named-pipe server.

use std::future::Future;
use std::sync::Arc;

use nexus_cua_protocol::{ErrorCode, RequestEnvelope, RequestId, ResponseEnvelope};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::sync::Semaphore;
use tracing::{debug, info, warn};

use crate::dispatcher::{failure, public_error};
use crate::frame::{read_frame, write_frame};
use crate::{Dispatcher, ServerConfig, TransportError};

const MAX_CONNECTIONS: usize = 64;

#[cfg(unix)]
struct UnixSocketGuard(std::path::PathBuf);

#[cfg(unix)]
impl Drop for UnixSocketGuard {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.0)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            warn!(path = %self.0.display(), reason = %error, "failed to remove local socket");
        }
    }
}

/// Serves until the supplied shutdown future resolves.
///
/// # Errors
///
/// Returns an error when configuration is invalid or the private local
/// endpoint cannot be created or accepted.
pub async fn serve_until<F>(
    dispatcher: Arc<Dispatcher>,
    config: ServerConfig,
    shutdown: F,
) -> Result<(), TransportError>
where
    F: Future<Output = ()> + Send,
{
    config.validate()?;

    #[cfg(unix)]
    return serve_unix(dispatcher, config, shutdown).await;

    #[cfg(windows)]
    return serve_windows(dispatcher, config, shutdown).await;

    #[cfg(not(any(unix, windows)))]
    Err(TransportError::InvalidConfiguration(
        "local transport is supported only on Unix and Windows".to_owned(),
    ))
}

async fn serve_connection<S>(
    mut stream: S,
    dispatcher: Arc<Dispatcher>,
    max_frame_bytes: usize,
) -> Result<(), TransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    loop {
        let Some(payload) = read_frame(&mut stream, max_frame_bytes).await? else {
            return Ok(());
        };
        let response = match serde_json::from_slice::<RequestEnvelope>(&payload) {
            Ok(request) => dispatcher.dispatch(request).await,
            Err(error) => {
                warn!(reason = %error, "rejected malformed local request");
                malformed_response()
            }
        };
        let payload = serde_json::to_vec(&response)?;
        write_frame(&mut stream, &payload, max_frame_bytes).await?;
    }
}

fn malformed_response() -> ResponseEnvelope {
    failure(
        RequestId::new("unparsed"),
        public_error(
            ErrorCode::InvalidRequest,
            "request payload does not match the closed protocol schema",
            false,
            Some("fix_request_payload"),
        ),
    )
}

#[cfg(unix)]
async fn serve_unix<F>(
    dispatcher: Arc<Dispatcher>,
    config: ServerConfig,
    shutdown: F,
) -> Result<(), TransportError>
where
    F: Future<Output = ()> + Send,
{
    use std::fs::Permissions;
    use std::os::unix::fs::PermissionsExt;
    use std::path::Path;

    let path = Path::new(config.endpoint.as_str());
    if path.exists() {
        return Err(TransportError::InvalidConfiguration(format!(
            "endpoint already exists: {}",
            path.display()
        )));
    }
    let listener = tokio::net::UnixListener::bind(path)?;
    std::fs::set_permissions(path, Permissions::from_mode(0o600))?;
    let _guard = UnixSocketGuard(path.to_path_buf());
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    info!(endpoint = %config.endpoint, "local CUA service ready");
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => {
                info!("local CUA service stopping");
                return Ok(());
            }
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                    warn!(max_connections = MAX_CONNECTIONS, "rejected excess local connection");
                    continue;
                };
                let dispatcher = Arc::clone(&dispatcher);
                let max_frame_bytes = config.max_frame_bytes;
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve_connection(stream, dispatcher, max_frame_bytes).await {
                        debug!(reason = %error, "local CUA connection closed with error");
                    }
                });
            }
        }
    }
}

#[cfg(windows)]
async fn serve_windows<F>(
    dispatcher: Arc<Dispatcher>,
    config: ServerConfig,
    shutdown: F,
) -> Result<(), TransportError>
where
    F: Future<Output = ()> + Send,
{
    use crate::windows_security::PipeSecurity;

    let mut first = true;
    let connections = Arc::new(Semaphore::new(MAX_CONNECTIONS));
    let mut security = PipeSecurity::owner_only()?;
    let mut server = security.create_pipe(config.endpoint.as_str(), first, MAX_CONNECTIONS + 1)?;
    info!(endpoint = %config.endpoint, "local CUA service ready");
    tokio::pin!(shutdown);
    loop {
        tokio::select! {
            biased;
            () = &mut shutdown => {
                info!("local CUA service stopping");
                return Ok(());
            }
            connected = server.connect() => {
                connected?;
                let stream = server;
                first = false;
                let Ok(permit) = Arc::clone(&connections).try_acquire_owned() else {
                    warn!(max_connections = MAX_CONNECTIONS, "rejected excess local connection");
                    drop(stream);
                    server = security.create_pipe(
                        config.endpoint.as_str(),
                        first,
                        MAX_CONNECTIONS + 1,
                    )?;
                    continue;
                };
                server = security.create_pipe(
                    config.endpoint.as_str(),
                    first,
                    MAX_CONNECTIONS + 1,
                )?;
                let dispatcher = Arc::clone(&dispatcher);
                let max_frame_bytes = config.max_frame_bytes;
                tokio::spawn(async move {
                    let _permit = permit;
                    if let Err(error) = serve_connection(stream, dispatcher, max_frame_bytes).await {
                        debug!(reason = %error, "local CUA connection closed with error");
                    }
                });
            }
        }
    }
}
