//! Bounded JSON framing over a host-supplied authenticated byte stream.

use std::io::ErrorKind;
use std::sync::Arc;

use nexus_cua_protocol::CuaError;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use thiserror::Error;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use zeroize::Zeroizing;

use crate::{
    GatewayArtifactRequest, GatewayArtifactResponse, GatewayExecuteRequest, GatewayExecuteResponse,
    WorkerGateway,
};

const HARD_MAX_GATEWAY_FRAME_BYTES: usize = 64 * 1024 * 1024;

/// Closed request variants transported over one remote gateway stream.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "operation", content = "input", rename_all = "snake_case")]
pub enum GatewayWireRequest {
    /// Execute or reconcile one core runtime command.
    Execute(GatewayExecuteRequest),
    /// Fetch one checksum-bound screenshot ticket.
    FetchArtifact(GatewayArtifactRequest),
}

/// Screenshot fetch outcome.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum GatewayArtifactOutcome {
    /// Artifact bytes were revalidated and base64 encoded.
    Success {
        /// Verified artifact response.
        artifact: GatewayArtifactResponse,
    },
    /// Fetch was denied or the artifact became stale.
    Error {
        /// Stable public error.
        error: CuaError,
    },
}

/// Closed response variants transported over one remote gateway stream.
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "result_type", content = "data", rename_all = "snake_case")]
pub enum GatewayWireResponse {
    /// Command execution/reconciliation response.
    Execute(GatewayExecuteResponse),
    /// Screenshot fetch response.
    Artifact(GatewayArtifactOutcome),
}

/// Serves gateway requests until the host-supplied stream reaches clean EOF.
/// The caller owns TLS/SSH/mTLS establishment and peer lifecycle; the gateway
/// still checks its bounded transport capability on every request.
///
/// # Errors
///
/// Returns a framing, JSON, I/O, or configured-bound error. No request payload
/// or sensitive text is included in the error.
pub async fn serve_gateway_stream<S>(
    gateway: Arc<WorkerGateway>,
    mut stream: S,
    max_frame_bytes: usize,
) -> Result<(), GatewayTransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    validate_frame_bound(max_frame_bytes)?;
    while let Some(request) =
        read_frame::<_, GatewayWireRequest>(&mut stream, max_frame_bytes).await?
    {
        let response = match request {
            GatewayWireRequest::Execute(request) => {
                GatewayWireResponse::Execute(gateway.execute(request).await)
            }
            GatewayWireRequest::FetchArtifact(request) => {
                let outcome = match gateway.fetch_artifact(request).await {
                    Ok(artifact) => GatewayArtifactOutcome::Success { artifact },
                    Err(error) => GatewayArtifactOutcome::Error { error },
                };
                GatewayWireResponse::Artifact(outcome)
            }
        };
        write_frame(&mut stream, &response, max_frame_bytes).await?;
    }
    Ok(())
}

/// Sends one request and reads one response over an already authenticated
/// host stream.
///
/// # Errors
///
/// Returns a bounded transport error without request contents.
pub async fn gateway_round_trip<S>(
    stream: &mut S,
    request: &GatewayWireRequest,
    max_frame_bytes: usize,
) -> Result<GatewayWireResponse, GatewayTransportError>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    validate_frame_bound(max_frame_bytes)?;
    write_frame(stream, request, max_frame_bytes).await?;
    read_frame(stream, max_frame_bytes)
        .await?
        .ok_or(GatewayTransportError::UnexpectedEof)
}

async fn read_frame<S, T>(
    stream: &mut S,
    max_frame_bytes: usize,
) -> Result<Option<T>, GatewayTransportError>
where
    S: AsyncRead + Unpin,
    T: DeserializeOwned,
{
    let mut length = [0_u8; 4];
    let first = stream.read(&mut length[..1]).await?;
    if first == 0 {
        return Ok(None);
    }
    stream
        .read_exact(&mut length[1..])
        .await
        .map_err(map_frame_read_error)?;
    let length = usize::try_from(u32::from_be_bytes(length))
        .map_err(|_| GatewayTransportError::FrameTooLarge)?;
    if length == 0 || length > max_frame_bytes {
        return Err(GatewayTransportError::FrameTooLarge);
    }
    let mut payload = Zeroizing::new(vec![0_u8; length]);
    stream
        .read_exact(payload.as_mut_slice())
        .await
        .map_err(map_frame_read_error)?;
    serde_json::from_slice(payload.as_slice())
        .map(Some)
        .map_err(|_| GatewayTransportError::InvalidJson)
}

async fn write_frame<S, T>(
    stream: &mut S,
    value: &T,
    max_frame_bytes: usize,
) -> Result<(), GatewayTransportError>
where
    S: AsyncWrite + Unpin,
    T: Serialize,
{
    let payload =
        Zeroizing::new(serde_json::to_vec(value).map_err(|_| GatewayTransportError::InvalidJson)?);
    if payload.is_empty() || payload.len() > max_frame_bytes {
        return Err(GatewayTransportError::FrameTooLarge);
    }
    let length = u32::try_from(payload.len()).map_err(|_| GatewayTransportError::FrameTooLarge)?;
    stream.write_all(&length.to_be_bytes()).await?;
    stream.write_all(payload.as_slice()).await?;
    stream.flush().await?;
    Ok(())
}

fn validate_frame_bound(max_frame_bytes: usize) -> Result<(), GatewayTransportError> {
    if max_frame_bytes == 0 || max_frame_bytes > HARD_MAX_GATEWAY_FRAME_BYTES {
        Err(GatewayTransportError::InvalidConfiguration)
    } else {
        Ok(())
    }
}

fn map_frame_read_error(error: std::io::Error) -> GatewayTransportError {
    if error.kind() == ErrorKind::UnexpectedEof {
        GatewayTransportError::UnexpectedEof
    } else {
        GatewayTransportError::Io(error)
    }
}

/// Remote gateway stream failure without payload contents.
#[derive(Debug, Error)]
pub enum GatewayTransportError {
    /// Configured frame bound is zero or above the 64 MiB hard ceiling.
    #[error("gateway frame bound is invalid")]
    InvalidConfiguration,
    /// Declared or encoded frame exceeds the configured bound.
    #[error("gateway frame is empty or too large")]
    FrameTooLarge,
    /// Stream ended partway through a frame or before a response.
    #[error("gateway stream ended unexpectedly")]
    UnexpectedEof,
    /// Frame is not one closed gateway JSON variant.
    #[error("gateway frame JSON is invalid")]
    InvalidJson,
    /// Underlying host stream failed.
    #[error("gateway stream I/O failed")]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use nexus_cua_protocol::{AuthorizationToken, Command, RequestId};

    use super::*;
    use crate::{
        GATEWAY_PROTOCOL_VERSION, WorkerEpoch, WorkerGatewayConfig, WorkerLeaseId, WorkerRegistry,
        WorkerRegistryConfig,
    };

    #[tokio::test]
    async fn framed_stream_rejects_unauthorized_call_without_a_worker() {
        let registry = Arc::new(WorkerRegistry::new(WorkerRegistryConfig::default()).unwrap());
        let token = AuthorizationToken::new("g".repeat(64));
        let gateway =
            Arc::new(WorkerGateway::new(registry, &token, WorkerGatewayConfig::default()).unwrap());
        let (mut client, server) = tokio::io::duplex(16 * 1024);
        let service = tokio::spawn(serve_gateway_stream(gateway, server, 16 * 1024));
        let response = gateway_round_trip(
            &mut client,
            &GatewayWireRequest::Execute(GatewayExecuteRequest {
                protocol_version: GATEWAY_PROTOCOL_VERSION.to_owned(),
                request_id: RequestId::new("request_wire_denied"),
                timeout_ms: u32::try_from(Duration::from_secs(1).as_millis()).unwrap(),
                authorization: AuthorizationToken::new("x".repeat(64)),
                lease_id: WorkerLeaseId::random("lease"),
                worker_epoch: WorkerEpoch::random("epoch"),
                command: Command::GetCapabilities,
            }),
            16 * 1024,
        )
        .await
        .unwrap();
        let GatewayWireResponse::Execute(response) = response else {
            panic!("unexpected wire response");
        };
        let crate::GatewayOutcome::Error { error } = response.outcome else {
            panic!("unauthorized stream call succeeded");
        };
        assert_eq!(error.code, nexus_cua_protocol::ErrorCode::Unauthorized);
        drop(client);
        service.await.unwrap().unwrap();
    }
}
