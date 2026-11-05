//! Bounded request decoding on the client side; model transport remains native.
use crate::error::Error;
use axum::{
    body::Bytes,
    extract::{FromRequest, Request},
    response::Response,
};
use serde_json::Value;
use std::io::Read;

pub struct JsonBody(pub Value);
impl<S: Send + Sync> FromRequest<S> for JsonBody {
    type Rejection = Response;
    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        let fail = crate::client_api::client_error;
        if request.headers().get_all("content-encoding").iter().count() > 1 {
            return Err(fail(Error::new(
                "UNSUPPORTED_MEDIA_TYPE",
                "Content-Encoding 不接受多层或重复编码。",
            )));
        }
        let encoding = request
            .headers()
            .get("content-encoding")
            .map(|v| v.to_str().unwrap_or("invalid").to_owned())
            .unwrap_or_default();
        let json = request
            .headers()
            .get("content-type")
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| {
                v.split(';').next().is_some_and(|v| {
                    let v = v.trim().to_ascii_lowercase();
                    v == "application/json"
                        || (v.starts_with("application/") && v.ends_with("+json"))
                })
            });
        if !json {
            return Err(fail(Error::new(
                "UNSUPPORTED_MEDIA_TYPE",
                "请求 Content-Type 必须是 application/json。",
            )));
        }
        let bytes = Bytes::from_request(request, state)
            .await
            .map_err(|_| fail(Error::new("PAYLOAD_TOO_LARGE", "请求正文超过 32 MiB。")))?;
        tokio::task::spawn_blocking(move || decode(&bytes, &encoding))
            .await
            .map_err(|_| fail(Error::new("INVALID_INPUT", "请求解压失败。")))?
            .map(Self)
            .map_err(fail)
    }
}
pub(crate) fn decode(bytes: &[u8], encoding: &str) -> crate::error::Result<Value> {
    let invalid = || Error::new("INVALID_INPUT", "请求压缩内容或 JSON 无效。");
    let reader: Box<dyn Read + '_> = match encoding.trim().to_ascii_lowercase().as_str() {
        "" | "identity" => Box::new(bytes),
        "gzip" => Box::new(flate2::read::MultiGzDecoder::new(bytes)),
        "zstd" => {
            let mut decoder = zstd::stream::read::Decoder::new(bytes).map_err(|_| invalid())?;
            decoder.window_log_max(27).map_err(|_| invalid())?;
            Box::new(decoder)
        }
        _ => {
            return Err(Error::new(
                "UNSUPPORTED_MEDIA_TYPE",
                "Content-Encoding 仅支持 identity、gzip 和 zstd。",
            ));
        }
    };
    let mut decoded = Vec::new();
    reader
        .take(crate::media::BODY_LIMIT as u64 + 1)
        .read_to_end(&mut decoded)
        .map_err(|_| invalid())?;
    if decoded.len() > crate::media::BODY_LIMIT {
        return Err(Error::new("PAYLOAD_TOO_LARGE", "解压后的请求超过 32 MiB。"));
    }
    serde_json::from_slice(&decoded).map_err(|_| invalid())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    #[test]
    fn compressed_requests_decode_and_expansion_is_bounded() {
        let body = br#"{"model":"m","input":"hello"}"#;
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        gz.write_all(body).unwrap();
        for (data, encoding) in [
            (body.to_vec(), "identity"),
            (gz.finish().unwrap(), "gzip"),
            (zstd::stream::encode_all(&body[..], 1).unwrap(), "zstd"),
        ] {
            assert_eq!(decode(&data, encoding).unwrap()["input"], "hello");
        }
        let large = vec![b' '; crate::media::BODY_LIMIT + 1];
        assert_eq!(
            decode(&zstd::stream::encode_all(&large[..], 1).unwrap(), "zstd")
                .unwrap_err()
                .code,
            "PAYLOAD_TOO_LARGE"
        );
        assert!(decode(b"broken", "gzip").is_err());
        assert_eq!(
            decode(body, "br").unwrap_err().code,
            "UNSUPPORTED_MEDIA_TYPE"
        );
    }
}
