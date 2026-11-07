//! 清单与签名下载：体积上限、退避重试与验签解析。

use super::config::default_signature_url;
use super::DeltaUpdater;
use crate::core::bundle::BundleManifest;
use crate::core::verify::verify_manifest_signature_with;
use crate::error::{DeltaError, Result};
use crate::models::UpdateManifest;
use futures_util::StreamExt;

/// 清单与签名的最大下载体积。
///
/// 清单与签名正常只有几 KB。设定上限可以避免服务端被劫持后
/// 返回一个超大响应把进程内存打满。
const MAX_MANIFEST_BYTES: usize = 10 * 1024 * 1024;

impl DeltaUpdater {
    /// 下载并校验整包清单签名。
    pub(super) async fn fetch_verified_bundle_manifest(&self, url: &str) -> Result<BundleManifest> {
        // 清单与签名并行拉取：两者互不依赖，串行会让每次检查多一个 RTT。
        let signature_url = default_signature_url(url);
        let (manifest_bytes, signature) = tokio::join!(
            fetch_bytes(&self.client, url),
            fetch_text(&self.client, &signature_url)
        );
        let manifest_bytes = manifest_bytes?;
        let signature = signature?;

        verify_manifest_signature_with(
            &manifest_bytes,
            &signature,
            &self.config.public_key,
            self.config.allow_legacy_signatures,
        )?;

        let manifest: BundleManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| DeltaError::ManifestInvalid(format!("整包清单格式错误: {e}")))?;

        if let Some(bound) = crate::core::verify::manifest_version_binding(&signature) {
            if bound != manifest.version {
                return Err(DeltaError::SignatureInvalid(format!(
                    "整包清单签名的版本绑定（{bound}）与清单声明版本（{}）不一致，疑似发布端配置错误",
                    manifest.version
                )));
            }
        }

        Ok(manifest)
    }

    /// 下载并校验清单签名，返回可信的清单。
    ///
    /// 按主端点 → 备用端点顺序尝试；切换策略由
    /// [`super::FallbackPolicy`] 控制。**验签失败永远是终止条件**，
    /// 不会因为存在备用端点而继续尝试。
    pub(super) async fn fetch_verified_manifest(&self) -> Result<UpdateManifest> {
        let mut last_error: Option<DeltaError> = None;
        for url in self.manifest_endpoints() {
            match self.fetch_verified_manifest_at(url).await {
                Ok(manifest) => return Ok(manifest),
                Err(err) if self.config.fallback_policy.should_fallback(&err) => {
                    crate::core::events::endpoint_fallback(url, &err.to_string());
                    last_error = Some(err);
                }
                Err(err) => return Err(err),
            }
        }
        Err(last_error
            .unwrap_or_else(|| DeltaError::Config("没有可用的清单端点：manifest_url 为空".into())))
    }

    /// 从指定端点拉取并验签清单（不做端点切换）。
    async fn fetch_verified_manifest_at(&self, url: &str) -> Result<UpdateManifest> {
        let signature_url = default_signature_url(url);
        let (manifest_bytes, signature) = tokio::join!(
            fetch_bytes(&self.client, url),
            fetch_text(&self.client, &signature_url)
        );
        let manifest_bytes = manifest_bytes?;
        let signature = signature?;

        verify_manifest_signature_with(
            &manifest_bytes,
            &signature,
            &self.config.public_key,
            self.config.allow_legacy_signatures,
        )?;

        let manifest: UpdateManifest = serde_json::from_slice(&manifest_bytes)
            .map_err(|e| DeltaError::ManifestInvalid(format!("清单格式错误: {e}")))?;

        // 版本绑定（发布端一致性）：签名合法 ≠ 版本正确。
        // 注意：绑定值与 manifest.version 都来自同一份签名材料，
        // 因此它**不能**替代客户端的版本单调性判断（见 is_newer_than）。
        // 这里只用于发现发布端把一个版本号写进另一版本清单的错误。
        if let Some(bound) = crate::core::verify::manifest_version_binding(&signature) {
            if bound != manifest.version {
                return Err(DeltaError::SignatureInvalid(format!(
                    "清单签名的版本绑定（{bound}）与清单声明版本（{}）不一致，疑似发布端配置错误",
                    manifest.version
                )));
            }
        }

        Ok(manifest)
    }
}

/// 下载二进制内容，可重试错误按下载同一套退避策略重试。
///
/// 清单与签名只有几十 KB，但同样会被 CDN 抖动、连接重置影响：
/// 一次 502 就让用户看到"检查更新失败"既不合理，也会让人误以为功能坏了。
/// 确定性错误（4xx、体积超限、非法内容）立即返回，不浪费时间重试。
async fn fetch_bytes(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let mut attempt = 0u32;
    loop {
        match fetch_bytes_once(client, url).await {
            Ok(bytes) => return Ok(bytes),
            Err(e) if e.is_retryable() && attempt < crate::core::downloader::MAX_RETRIES => {
                tokio::time::sleep(crate::core::downloader::backoff_delay(attempt)).await;
                attempt += 1;
            }
            Err(e) => return Err(e),
        }
    }
}

/// 单次请求：不做重试。
async fn fetch_bytes_once(client: &reqwest::Client, url: &str) -> Result<Vec<u8>> {
    let response = client.get(url).send().await.map_err(|e| {
        if e.is_timeout() {
            DeltaError::Timeout(format!("请求 {url} 超时: {e}"))
        } else {
            DeltaError::Network(format!("请求 {url} 失败: {e}"))
        }
    })?;

    if !response.status().is_success() {
        return Err(DeltaError::HttpStatus(response.status().as_u16()));
    }

    // 先看服务端声明的长度，超限直接拒绝，避免把超大响应读进内存
    if let Some(len) = response.content_length() {
        if len > MAX_MANIFEST_BYTES as u64 {
            return Err(DeltaError::ResponseTooLarge {
                url: url.to_string(),
                limit: MAX_MANIFEST_BYTES as u64,
            });
        }
    }

    // 服务端可能不给 Content-Length，边读边计数兜底
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(|e| DeltaError::Network(format!("读取 {url} 失败: {e}")))?;
        if body.len() + chunk.len() > MAX_MANIFEST_BYTES {
            return Err(DeltaError::ResponseTooLarge {
                url: url.to_string(),
                limit: MAX_MANIFEST_BYTES as u64,
            });
        }
        body.extend_from_slice(&chunk);
    }

    Ok(body)
}

/// 下载文本内容。
async fn fetch_text(client: &reqwest::Client, url: &str) -> Result<String> {
    let bytes = fetch_bytes(client, url).await?;
    String::from_utf8(bytes)
        .map_err(|_| DeltaError::InvalidResponse(format!("{url} 不是合法的 UTF-8 文本")))
}
