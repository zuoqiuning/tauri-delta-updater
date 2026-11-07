//! 全量安装包：下载、验签、落盘与安装执行。

use super::{
    emit, DeltaUpdater, FullDownload, FullDownloadOutcome, StagedFullInstaller, UpdateResult,
};
use crate::error::{DeltaError, Result};
use crate::session::UpdateEvent;
use std::path::{Path, PathBuf};

impl DeltaUpdater {
    /// 下载全量安装包并校验 `platforms[].signature`（**不执行安装**）。
    ///
    /// 这是"增量不可用"时的完整回退路径，等价于官方 updater 的下载阶段。
    /// 安装执行由上层负责：Windows 静默运行 NSIS/MSI、macOS 解包 `.app.tar.gz`、
    /// Linux 替换 AppImage。
    ///
    /// - 版本不高于当前版本时返回 [`FullDownloadOutcome::UpToDate`]，不发起下载；
    /// - 文件写到工作目录，名称带版本号，已存在的同名半截文件会被续传；
    /// - 装包签名不匹配时删除文件并返回 [`DeltaError::SignatureInvalid`]。
    ///
    /// ```no_run
    /// # async fn example(updater: &tauri_delta_updater::DeltaUpdater)
    /// #     -> tauri_delta_updater::Result<()> {
    /// use tauri_delta_updater::FullDownloadOutcome;
    /// match updater.download_full().await? {
    ///     FullDownloadOutcome::Downloaded { installer } => {
    ///         println!("安装包: {}", installer.path().display());
    ///     }
    ///     FullDownloadOutcome::UpToDate { version } => println!("已是最新 {version}"),
    ///     _ => {}
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub async fn download_full(&self) -> Result<FullDownloadOutcome> {
        let cancel = crate::core::cancel::CancelToken::new();
        self.download_full_with_events(&cancel, |_| {}).await
    }

    /// 带阶段事件与取消的全量下载。
    ///
    /// 与 [`Self::download_full`] 相同，额外按阶段回调 [`UpdateEvent`]：
    /// `Checking` → `DownloadingFull` → 返回结果。
    pub async fn download_full_with_events<F>(
        &self,
        cancel: &crate::core::cancel::CancelToken,
        on_event: F,
    ) -> Result<FullDownloadOutcome>
    where
        F: FnMut(UpdateEvent) + Send,
    {
        let mut on_event: Option<Box<dyn FnMut(UpdateEvent) + Send + '_>> =
            Some(Box::new(on_event));

        emit(&mut on_event, UpdateEvent::Checking);
        let manifest = self.fetch_verified_manifest().await?;

        // 防降级：清单合法不代表比当前版本新，绝不能把旧安装包交给安装器。
        if !manifest.is_newer_than(&self.config.current_version)? {
            let version = manifest.version.clone();
            emit(
                &mut on_event,
                UpdateEvent::UpToDate {
                    version: version.clone(),
                },
            );
            return Ok(FullDownloadOutcome::UpToDate { version });
        }

        let full =
            FullDownload::from_manifest(&manifest, &self.config.platform).ok_or_else(|| {
                DeltaError::ManifestInvalid(format!(
                    "清单缺少平台 {} 的安装包信息，无法回退全量下载",
                    self.config.platform
                ))
            })?;

        let path = full_installer_path(
            &self.work_dir,
            &full.url,
            &full.version,
            &self.config.platform,
        );
        // 旧版本的安装包只在占空间：本次下载成功后它已经没有用途。
        cleanup_old_full_installers(&self.work_dir, &path);

        crate::core::downloader::download_full_with_progress_controlled(
            &self.client,
            &full.url,
            &path,
            MAX_FULL_INSTALLER_BYTES,
            Some(cancel),
            &self.work_dir,
            |downloaded, total| {
                emit(
                    &mut on_event,
                    UpdateEvent::DownloadingFull { downloaded, total },
                );
            },
        )
        .await?;

        if cancel.is_cancelled() {
            let _ = tokio::fs::remove_file(&path).await;
            emit(&mut on_event, UpdateEvent::Cancelled);
            return Err(DeltaError::Cancelled);
        }

        // 安装包可能几十到几百 MB：用 mmap 验签，不把整个文件读进内存。
        if let Err(e) = crate::core::verify::verify_file_signature_multi(
            &path,
            &full.signature,
            &self.config.trusted_keys(),
            self.config.trust_policy,
        ) {
            let _ = tokio::fs::remove_file(&path).await;
            return Err(e);
        }

        Ok(FullDownloadOutcome::Downloaded {
            installer: StagedFullInstaller {
                version: full.version,
                path,
            },
        })
    }

    /// 下载并执行全量安装（回退路径的完整实现）。
    ///
    /// 下载与验签和 [`Self::download_full`] 完全一致，之后把安装包交给平台执行器：
    ///
    /// - **Windows**：以安装器自己的权限要求启动 NSIS（`/P /UPDATE /R`）、
    ///   MSI（`/passive /promptrestart AUTOLAUNCHAPP=True`）或 Inno Setup
    ///   （`/SILENT` / `/VERYSILENT`，由 [`crate::UpdateConfig::installer_kind`]
    ///   显式声明）。返回 [`UpdateResult::Updated`] 后，**调用方必须立即退出进程**：
    ///   安装器要替换正在运行的可执行文件，并会自行重新启动应用。
    ///
    ///   **重启契约**：NSIS 由 `/R` 参数重启；**Inno 没有 `/R` 等价参数**，
    ///   必须由 `.iss` 的 `[Run]` 段重启，且不能带 `skipifsilent`
    ///   （它的语义是"静默模式不执行"，与 `/R` 正好相反）。引擎只负责启动
    ///   安装器并退出，不会自行重启应用。
    /// - **macOS**：解包 `.app.tar.gz`，走与整包增量相同的目录切换脚本
    ///   （`codesign --verify`、备份、启动确认、失败回滚）。
    /// - **Linux**：AppImage 走单文件替换脚本；`.deb` / `.rpm` 明确拒绝，
    ///   交给包管理器处理。
    ///
    /// 与增量路径不同，全量安装的失败回滚由安装器负责：我们只负责
    /// "下载对的文件、验签、以正确参数启动安装器"。
    pub async fn install_full(&self) -> Result<UpdateResult> {
        let cancel = crate::core::cancel::CancelToken::new();
        self.install_full_with_events(&cancel, |_| {}).await
    }

    /// 带阶段事件与取消的全量安装。
    pub async fn install_full_with_events<F>(
        &self,
        cancel: &crate::core::cancel::CancelToken,
        on_event: F,
    ) -> Result<UpdateResult>
    where
        F: FnMut(UpdateEvent) + Send,
    {
        let outcome = self.download_full_with_events(cancel, on_event).await?;

        match outcome {
            FullDownloadOutcome::Downloaded { installer } => {
                crate::platform::full_install::install_with_mode_and_kind(
                    &self.executable_path,
                    installer.path(),
                    &self.work_dir,
                    self.script_timings(),
                    self.config.install_mode,
                    self.config.installer_kind,
                )?;
                Ok(UpdateResult::Updated {
                    version: installer.version().to_string(),
                })
            }
            FullDownloadOutcome::UpToDate { version } => Ok(UpdateResult::UpToDate { version }),
        }
    }
}

/// 全量安装包体积上限（4 GB）。
///
/// 只防"被劫持的服务端用超大响应灌盘"；正常安装包远小于它。
const MAX_FULL_INSTALLER_BYTES: u64 = 4 * 1024 * 1024 * 1024;

/// 全量安装包在工作目录里的落点。
fn full_installer_path(work_dir: &Path, url: &str, version: &str, platform: &str) -> PathBuf {
    work_dir.join(format!(
        "{}_{}{}",
        crate::core::artifacts::FULL_INSTALLER_PREFIX,
        version,
        installer_suffix(url, platform)
    ))
}

/// 按 URL（其次是平台）推断安装包扩展名。
///
/// 扩展名不是装饰：Windows 上双击/静默运行 NSIS 需要 `.exe`，
/// MSI 需要 `.msi`，macOS 解包需要 `.tar.gz` 的识别，Linux 需要可执行位。
fn installer_suffix(url: &str, platform: &str) -> String {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    let lower = path.to_lowercase();
    if lower.ends_with(".tar.gz") {
        return ".tar.gz".to_string();
    }
    match Path::new(path).extension().and_then(|e| e.to_str()) {
        Some(ext) if !ext.is_empty() => format!(".{ext}"),
        _ => match platform.split_once('-').map(|(os, _)| os).unwrap_or("") {
            "windows" => ".exe".to_string(),
            "darwin" => ".tar.gz".to_string(),
            _ => ".bin".to_string(),
        },
    }
}

/// 删除本次下载之外的旧全量安装包（文件名前缀 + 版本号保证不会误删别的文件）。
fn cleanup_old_full_installers(work_dir: &Path, keep: &Path) {
    let Ok(entries) = std::fs::read_dir(work_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path == *keep {
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(crate::core::artifacts::FULL_INSTALLER_PREFIX) {
            let _ = std::fs::remove_file(&path);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{full_installer_path, installer_suffix};
    use std::path::Path;

    /// 私有 CDN 的 URL 常带查询参数或无扩展名，后缀必须仍然正确。
    #[test]
    fn suffix_ignores_query_and_falls_back_to_platform() {
        assert_eq!(
            installer_suffix(
                "https://cdn.example.com/app.exe?token=abc",
                "windows-x86_64"
            ),
            ".exe"
        );
        assert_eq!(
            installer_suffix("https://cdn.example.com/app.tar.gz?v=2", "darwin-aarch64"),
            ".tar.gz"
        );
        assert_eq!(
            installer_suffix("https://cdn.example.com/download?id=7", "windows-x86_64"),
            ".exe"
        );
        assert_eq!(
            installer_suffix("https://cdn.example.com/download", "darwin-aarch64"),
            ".tar.gz"
        );
        assert_eq!(
            installer_suffix("https://cdn.example.com/download", "linux-x86_64"),
            ".bin"
        );
    }

    /// 落盘文件名必须带版本号，避免不同版本的同名安装包互相续传。
    #[test]
    fn installer_path_contains_version() {
        let path = full_installer_path(
            Path::new("work"),
            "https://cdn.example.com/app.exe",
            "1.2.3",
            "windows-x86_64",
        );
        assert_eq!(
            path.file_name().unwrap().to_string_lossy(),
            "tdx_full_installer_1.2.3.exe"
        );
    }
}
