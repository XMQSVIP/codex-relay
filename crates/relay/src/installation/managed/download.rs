//! Fetch only the official npm namespace; never run package scripts or follow redirects.
use super::{Run, cancelled, version};
use crate::error::{Error, Result};
use base64::Engine;
use serde_json::Value;
use sha2::{Digest, Sha512};
use std::{
    path::Path,
    time::{Duration, Instant},
};
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;

pub(super) const MAX_ARCHIVE_BYTES: u64 = 512 * 1024 * 1024;
pub(super) const MAX_UNPACKED_BYTES: u64 = 2 * 1024 * 1024 * 1024;
const MAX_METADATA: usize = 1024 * 1024;

pub(super) struct Registry {
    origin: url::Url,
    proxy: crate::app_server_proxy::ProxyConfig,
}
pub(super) struct Package {
    url: url::Url,
    digest: Vec<u8>,
    pub integrity: String,
    pub unpacked_size: u64,
}

impl Registry {
    pub(super) fn official() -> Self {
        Self {
            origin: url::Url::parse("https://registry.npmjs.org/").unwrap(),
            proxy: Default::default(),
        }
    }

    async fn client(&self) -> Result<reqwest::Client> {
        self.proxy
            .resolved()
            .await
            .map_err(|_| network_error())?
            .client_builder()
            .map_err(|_| network_error())?
            .https_only(self.origin.scheme() == "https")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(Duration::from_secs(20))
            .timeout(Duration::from_secs(20 * 60))
            .user_agent(concat!("codex-relay/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|_| network_error())
    }

    async fn metadata(&self, suffix: &str, cancel: &CancellationToken) -> Result<Value> {
        let client = self.client().await?;
        let url = self
            .origin
            .join(&format!("@openai/codex/{suffix}"))
            .map_err(|_| invalid_metadata())?;
        let mut response = tokio::select! {
            _ = cancel.cancelled() => return Err(cancelled()),
            response = tokio::time::timeout(Duration::from_secs(30), client.get(url).send()) =>
                response.map_err(|_| network_error())?.map_err(|_| network_error())?,
        };
        if !response.status().is_success() {
            return Err(network_error());
        }
        let mut bytes = Vec::new();
        let body = async {
            loop {
                let chunk = tokio::select! {
                    _ = cancel.cancelled() => return Err(cancelled()),
                    chunk = response.chunk() => chunk.map_err(|_| network_error())?,
                };
                let Some(chunk) = chunk else { break };
                if bytes.len().saturating_add(chunk.len()) > MAX_METADATA {
                    return Err(invalid_metadata());
                }
                bytes.extend_from_slice(&chunk);
            }
            serde_json::from_slice(&bytes).map_err(|_| invalid_metadata())
        };
        tokio::time::timeout(Duration::from_secs(30), body)
            .await
            .map_err(|_| network_error())?
    }

    pub(super) async fn latest(&self, cancel: &CancellationToken) -> Result<String> {
        let metadata = self.metadata("latest", cancel).await?;
        if metadata["name"] != "@openai/codex" {
            return Err(invalid_metadata());
        }
        metadata["version"]
            .as_str()
            .filter(|value| version(value).is_some())
            .map(str::to_owned)
            .ok_or_else(invalid_metadata)
    }

    pub(super) async fn package(
        &self,
        selected: &str,
        architecture: &str,
        cancel: &CancellationToken,
    ) -> Result<Package> {
        if version(selected).is_none() {
            return Err(invalid_metadata());
        }
        target(architecture)?;
        let package_version = format!("{selected}-linux-{architecture}");
        let metadata = self.metadata(&package_version, cancel).await?;
        self.parse_package(&metadata, &package_version, architecture)
    }

    fn parse_package(
        &self,
        metadata: &Value,
        package_version: &str,
        architecture: &str,
    ) -> Result<Package> {
        if metadata["name"] != "@openai/codex"
            || metadata["version"] != package_version
            || !metadata["os"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == "linux"))
            || !metadata["cpu"]
                .as_array()
                .is_some_and(|values| values.iter().any(|value| value == architecture))
        {
            return Err(invalid_metadata());
        }
        let raw = metadata["dist"]["tarball"]
            .as_str()
            .ok_or_else(invalid_metadata)?;
        let url = url::Url::parse(raw).map_err(|_| invalid_metadata())?;
        if url.origin() != self.origin.origin()
            || url.path() != format!("/@openai/codex/-/codex-{package_version}.tgz")
            || !url.username().is_empty()
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(invalid_metadata());
        }
        let integrity = metadata["dist"]["integrity"]
            .as_str()
            .ok_or_else(invalid_metadata)?;
        let digest = base64::engine::general_purpose::STANDARD
            .decode(
                integrity
                    .strip_prefix("sha512-")
                    .ok_or_else(invalid_metadata)?,
            )
            .map_err(|_| invalid_metadata())?;
        if digest.len() != 64 {
            return Err(invalid_metadata());
        }
        let unpacked_size = metadata["dist"]["unpackedSize"]
            .as_u64()
            .filter(|size| *size > 0 && *size <= MAX_UNPACKED_BYTES)
            .ok_or_else(invalid_metadata)?;
        Ok(Package {
            url,
            digest,
            integrity: integrity.into(),
            unpacked_size,
        })
    }

    pub(super) async fn download(
        &self,
        package: &Package,
        directory: &Path,
        run: &Run,
    ) -> Result<tempfile::NamedTempFile> {
        run.check_cancelled()?;
        run.phase("downloading", "正在下载官方 Codex…");
        let client = self.client().await?;
        let mut response = tokio::select! {
            _ = run.cancel.cancelled() => return Err(cancelled()),
            response = client.get(package.url.clone()).send() => response.map_err(|_| network_error())?,
        };
        if !response.status().is_success() {
            return Err(network_error());
        }
        let total = response.content_length();
        if total.is_some_and(|size| size == 0 || size > MAX_ARCHIVE_BYTES) {
            return Err(too_large());
        }
        let temporary = tempfile::Builder::new()
            .prefix(".download-")
            .suffix(".tgz")
            .tempfile_in(directory)?;
        let mut output = tokio::fs::File::from_std(temporary.reopen()?);
        let mut hash = Sha512::new();
        let mut received = 0u64;
        let mut last_progress = Instant::now();
        run.progress(0, total);
        loop {
            let chunk = tokio::select! {
                _ = run.cancel.cancelled() => return Err(cancelled()),
                chunk = response.chunk() => chunk.map_err(|_| network_error())?,
            };
            let Some(chunk) = chunk else { break };
            received = received
                .checked_add(chunk.len() as u64)
                .ok_or_else(too_large)?;
            if received > MAX_ARCHIVE_BYTES {
                return Err(too_large());
            }
            output.write_all(&chunk).await?;
            hash.update(&chunk);
            if last_progress.elapsed() >= Duration::from_millis(150) {
                run.progress(received, total);
                last_progress = Instant::now();
            }
        }
        output.sync_all().await?;
        run.progress(received, total);
        run.check_cancelled()?;
        run.phase("verifying", "正在核对官方 SHA-512 校验值…");
        if received == 0
            || total.is_some_and(|expected| expected != received)
            || hash.finalize().as_slice() != package.digest
        {
            return Err(Error::new(
                "CODEX_INTEGRITY",
                "下载文件校验失败，已丢弃下载内容；请重试。",
            ));
        }
        Ok(temporary)
    }
}

pub(super) fn target(architecture: &str) -> Result<&'static str> {
    match architecture {
        "x64" => Ok("x86_64-unknown-linux-musl"),
        "arm64" => Ok("aarch64-unknown-linux-musl"),
        _ => Err(invalid_metadata()),
    }
}
fn invalid_metadata() -> Error {
    Error::new(
        "CODEX_METADATA",
        "官方发布记录或平台包格式无法识别，未更改当前版本。",
    )
}
fn network_error() -> Error {
    Error::new(
        "CODEX_DOWNLOAD",
        "无法读取官方 Codex 下载源，请检查服务器网络或代理后重试。",
    )
}
fn too_large() -> Error {
    Error::new(
        "CODEX_PACKAGE_SIZE",
        "Codex 下载包超过允许大小，未更改当前版本。",
    )
}

#[cfg(test)]
pub(super) mod test_support {
    use super::*;
    pub(in crate::installation::managed) fn registry(origin: url::Url) -> Registry {
        Registry {
            origin,
            proxy: crate::app_server_proxy::ProxyConfig {
                mode: crate::app_server_proxy::ProxyMode::Direct,
                url: String::new(),
            },
        }
    }
    pub(in crate::installation::managed) fn proxied(origin: url::Url, url: String) -> Registry {
        Registry {
            origin,
            proxy: crate::app_server_proxy::ProxyConfig {
                mode: crate::app_server_proxy::ProxyMode::Custom,
                url,
            },
        }
    }
    pub(in crate::installation::managed) fn parse(
        registry: &Registry,
        metadata: &Value,
        version: &str,
        architecture: &str,
    ) -> Result<Package> {
        registry.parse_package(metadata, version, architecture)
    }
}
