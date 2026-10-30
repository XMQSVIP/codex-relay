use super::Rpc;
use crate::error::{Error, Result};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio_tungstenite::tungstenite::{
    client::IntoClientRequest,
    protocol::{Message, WebSocketConfig},
};
use tokio_util::codec::{FramedRead, FramedWrite, LinesCodec};

impl Rpc {
    pub fn from_io<R, W>(reader: R, writer: W) -> Self
    where
        R: AsyncRead + Unpin + Send + 'static,
        W: AsyncWrite + Unpin + Send + 'static,
    {
        let (rpc, mut outgoing) = Self::new();
        let read_rpc = rpc.clone();
        let write_rpc = rpc.clone();
        tokio::spawn(async move {
            let mut framed =
                FramedRead::new(reader, LinesCodec::new_with_max_length(super::MAX_BYTES));
            loop {
                tokio::select! {
                    _ = read_rpc.inner.cancel.cancelled() => break,
                    next = framed.next() => match next {
                        Some(Ok(text)) if read_rpc.receive(&text).is_ok() => {},
                        _ => break,
                    },
                }
            }
            read_rpc.close();
        });
        tokio::spawn(async move {
            let mut framed =
                FramedWrite::new(writer, LinesCodec::new_with_max_length(super::MAX_BYTES));
            loop {
                tokio::select! {
                    _ = write_rpc.inner.cancel.cancelled() => break,
                    next = outgoing.recv() => match next {
                        Some(value) => {
                            if tokio::time::timeout(Duration::from_secs(10), framed.send(value.to_string())).await.map_or(true, |v| v.is_err()) {
                                break;
                            }
                            #[cfg(test)]
                            crate::wire_capture::record_rpc(&value);
                        },
                        None => break,
                    },
                }
            }
            write_rpc.close();
        });
        rpc
    }

    pub async fn connect(endpoint: &str, token: &str) -> Result<Self> {
        let url =
            url::Url::parse(endpoint).map_err(|_| Error::new("RPC_ENDPOINT", "原生地址无效。"))?;
        if url.scheme() != "ws"
            || url.host_str() != Some("127.0.0.1")
            || url.port().is_none()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
            || !url.username().is_empty()
            || url.password().is_some()
        {
            return Err(Error::new(
                "RPC_ENDPOINT",
                "原生连接必须使用明确的本机端口。",
            ));
        }
        if token.len() < 32
            || !token
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        {
            return Err(Error::new("RPC_CREDENTIAL", "原生连接凭据无效。"));
        }
        let mut request = endpoint
            .into_client_request()
            .map_err(|_| Error::new("RPC_ENDPOINT", "原生地址无效。"))?;
        request
            .headers_mut()
            .insert("authorization", format!("Bearer {token}").parse().unwrap());
        let config = WebSocketConfig::default()
            .max_message_size(Some(super::MAX_BYTES))
            .max_frame_size(Some(super::MAX_BYTES));
        let (socket, _) = tokio::time::timeout(
            Duration::from_secs(3),
            tokio_tungstenite::connect_async_with_config(request, Some(config), true),
        )
        .await
        .map_err(|_| Error::new("RPC_CONNECT", "原生连接超时。"))?
        .map_err(|_| Error::new("RPC_CONNECT", "原生认证连接失败。"))?;
        let (mut writer, mut reader) = socket.split();
        let (rpc, mut outgoing) = Self::new();
        let read_rpc = rpc.clone();
        let write_rpc = rpc.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = read_rpc.inner.cancel.cancelled() => break,
                    next = reader.next() => match next {
                        Some(Ok(Message::Text(text))) if read_rpc.receive(&text).is_ok() => {},
                        Some(Ok(Message::Ping(_) | Message::Pong(_))) => {},
                        _ => break,
                    },
                }
            }
            read_rpc.close();
        });
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = write_rpc.inner.cancel.cancelled() => break,
                    next = outgoing.recv() => match next {
                        Some(value) => {
                            if tokio::time::timeout(Duration::from_secs(10), writer.send(Message::Text(value.to_string().into()))).await.map_or(true, |v| v.is_err()) {
                                break;
                            }
                        },
                        None => break,
                    },
                }
            }
            let _ = writer.close().await;
            write_rpc.close();
        });
        Ok(rpc)
    }
}
