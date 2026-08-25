//! Bounded length-prefixed JSON framing.

use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::TransportError;

pub(crate) async fn read_frame<R>(
    reader: &mut R,
    max_frame_bytes: usize,
) -> Result<Option<Vec<u8>>, TransportError>
where
    R: AsyncRead + Unpin,
{
    let mut header = [0_u8; 4];
    let first = reader.read(&mut header[..1]).await?;
    if first == 0 {
        return Ok(None);
    }
    reader.read_exact(&mut header[1..]).await.map_err(|error| {
        TransportError::InvalidFrame(format!("truncated length header: {error}"))
    })?;
    let length = u32::from_be_bytes(header) as usize;
    if length == 0 || length > max_frame_bytes {
        return Err(TransportError::InvalidFrame(format!(
            "payload length {length} is outside the configured bound"
        )));
    }
    let mut payload = vec![0_u8; length];
    reader
        .read_exact(&mut payload)
        .await
        .map_err(|error| TransportError::InvalidFrame(format!("truncated payload: {error}")))?;
    Ok(Some(payload))
}

pub(crate) async fn write_frame<W>(
    writer: &mut W,
    payload: &[u8],
    max_frame_bytes: usize,
) -> Result<(), TransportError>
where
    W: AsyncWrite + Unpin,
{
    if payload.is_empty() || payload.len() > max_frame_bytes || payload.len() > u32::MAX as usize {
        return Err(TransportError::InvalidFrame(format!(
            "payload length {} is outside the configured bound",
            payload.len()
        )));
    }
    let length = u32::try_from(payload.len()).map_err(|_| {
        TransportError::InvalidFrame("payload length exceeds the u32 framing limit".to_owned())
    })?;
    writer.write_all(&length.to_be_bytes()).await?;
    writer.write_all(payload).await?;
    writer.flush().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_oversized_frame_before_allocating_payload() {
        let mut input = &1_024_u32.to_be_bytes()[..];
        let error = read_frame(&mut input, 32).await.expect_err("reject frame");
        assert!(matches!(error, TransportError::InvalidFrame(_)));
    }
}
