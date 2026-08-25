//! Small local client used by diagnostics and embedders.

use nexus_cua_protocol::{RequestEnvelope, ResponseEnvelope};

use crate::frame::{read_frame, write_frame};
use crate::{LocalEndpoint, TransportError};

/// Sends one request over a fresh private local connection.
///
/// # Errors
///
/// Returns an error when the endpoint cannot be reached, the frame violates
/// configured bounds, or the response is not valid protocol JSON.
pub async fn request(
    endpoint: &LocalEndpoint,
    request: &RequestEnvelope,
    max_frame_bytes: usize,
) -> Result<ResponseEnvelope, TransportError> {
    let payload = serde_json::to_vec(request)?;

    #[cfg(unix)]
    let mut stream = tokio::net::UnixStream::connect(endpoint.as_str()).await?;

    #[cfg(windows)]
    let mut stream =
        tokio::net::windows::named_pipe::ClientOptions::new().open(endpoint.as_str())?;

    #[cfg(not(any(unix, windows)))]
    return Err(TransportError::InvalidConfiguration(
        "local transport is supported only on Unix and Windows".to_owned(),
    ));

    #[cfg(any(unix, windows))]
    {
        write_frame(&mut stream, &payload, max_frame_bytes).await?;
        let response = read_frame(&mut stream, max_frame_bytes)
            .await?
            .ok_or_else(|| {
                TransportError::InvalidFrame("server closed without a response".into())
            })?;
        Ok(serde_json::from_slice(&response)?)
    }
}
