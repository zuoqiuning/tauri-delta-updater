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
    /// - 系统版本低于清单要求、或灰度未命中时返回对应变体，不发起下载；
    /// - 上次整包替换未完成时返回 [`DeltaError::RepairRequired`]，
    ///   必须先用 [`Self::update`] / [`Self::prepare`] 修复安装目录；
    /// - 与增量路径共用 [`crate::core::lock::UpdateLock`]，并发调用会返回
    ///   [`DeltaError::UpdateInProgress`]；
    /// - 文件写到工作目录，名称带版本号，已存在的同名半截文件会被续传；
    /// - 装包签名不匹配时删除文件并返回 [`DeltaError::SignatureInvalid`]。
    ///
    /// **安全模式不阻断本路径**：安全模式限制的是自动增量重试，
    /// 全量下载/安装是用户显式选择的回退手段。
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
    /// `Checking` →（灰度未命中时 `Deferred`）→ `DownloadingFull` → 返回结果。
    pub async fn download_full_with_events<F>(
        &self,
        cancel: &crate::core::cancel::CancelToken,
        on_event: F,
    ) -> Result<FullDownloadOutcome>
    where
        F: FnMut(UpdateEvent) + Send,
    {
        // 与增量路径共用同一把锁：两个 install_full、或 install 与 install_full
        // 并发时会写同一工作目录、启动两个安装器。
        let _lock = self.acquire_lock()?;
        self.download_full_locked(cancel, on_event).await
    }

    /// 全量下载的实际实现；调用方必须已持有 [`crate::core::lock::UpdateLock`]。
    async fn download_full_locked<F>(
        &self,
        cancel: &crate::core::cancel::CancelToken,
        on_event: F,
    ) -> Result<FullDownloadOutcome>
    where
        F: FnMut(UpdateEvent) + Send,
    {
        let mut on_event: Option<Box<dyn FnMut(UpdateEvent) + Send + '_>> =
            Some(Box::new(on_event));

        // 修复优先：安装目录处于半新半旧状态时继续安装会覆盖现场，
        // 但下一次启动仍会看到残留备份并生成修复脚本，把新版本回滚。
        if self.needs_bundle_repair() {
            return Err(DeltaError::RepairRequired);
        }
        if cancel.is_cancelled() {
            emit(&mut on_event, UpdateEvent::Cancelled);
            return Err(DeltaError::Cancelled);
        }

        emit(&mut on_event, UpdateEvent::Checking);
        let manifest = self.fetch_verified_manifest().await?;

        // 与增量路径共用同一套版本/系统/灰度门槛，全量安装包同样不能被
        // 装到不兼容系统或灰度未命中的版本上。
        let gates = self.evaluate_gates(&manifest)?;

        // 防降级：清单合法不代表比当前版本新，绝不能把旧安装包交给安装器。
        if !gates.is_newer {
            let version = manifest.version.clone();
            emit(
                &mut on_event,
                UpdateEvent::UpToDate {
                    version: version.clone(),
                },
            );
            return Ok(FullDownloadOutcome::UpToDate { version });
        }

        if let Some((required, current)) = gates.unsupported_os {
            return Ok(FullDownloadOutcome::UnsupportedOs { required, current });
        }
        if gates.deferred_by_rollout {
            let version = manifest.version.clone();
            let percentage = gates.rollout_percentage.unwrap_or(0);
            emit(
                &mut on_event,
                UpdateEvent::Deferred {
                    version: version.clone(),
                    percentage,
                },
            );
            return Ok(FullDownloadOutcome::Deferred {
                version,
                percentage,
            });
        }

        let platform_url = manifest
            .platforms
            .get(&self.config.platform)
            .map(|info| info.url.as_str())
            .unwrap_or_default();
        let path = platform_url
            .split('?')
            .next()
            .unwrap_or(platform_url)
            .to_ascii_lowercase();
        if path.ends_with(".deb") || path.ends_with(".rpm") {
            return Err(DeltaError::Config(format!(
                "Linux .deb/.rpm 不支持自动安装（{}）：请使用发行版包管理器；AppImage 可直接更新",
                self.config.platform
            )));
        }

        let full =
            FullDownload::from_manifest(&manifest, &self.config.platform).ok_or_else(|| {
                DeltaError::ManifestInvalid(format!(
                    "清单缺少平台 {} 的安装包信息，无法回退全量下载",
                    self.config.platform
                ))
            })?;

        // 服务端不给 Content-Length（chunked）时下载器无法预检磁盘；
        // 清单里的 size 受签名保护，用它兜底，避免把磁盘写满后才发现。
        if let Some(size) = manifest
            .platforms
            .get(&self.config.platform)
            .and_then(|info| info.size)
        {
            crate::core::disk::check_disk_space(&self.work_dir, size)?;
        }

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
        // 放到 blocking 线程：大文件哈希/证书链检查会把 tokio worker 占住。
        let verify_path = path.clone();
        let signature = full.signature.clone();
        let keys: Vec<String> = self
            .config
            .trusted_keys()
            .into_iter()
            .map(str::to_string)
            .collect();
        let policy = self.config.trust_policy;
        let allow_legacy = self.config.allow_legacy_signatures;
        let verified = tokio::task::spawn_blocking(move || {
            let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
            crate::core::verify::verify_file_signature_multi_with_legacy(
                &verify_path,
                &signature,
                &keys,
                policy,
                allow_legacy,
            )
        })
        .await
        .map_err(|e| DeltaError::WorkerFailed(format!("验签线程失败: {e}")))?;
        if let Err(e) = verified {
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
    /// 与增量路径不同，文件层面的失败回滚由安装器负责；引擎在启动安装器前
    /// 写入 `tdx_full_pending.json`，下次启动对比版本确认是否真的生效：
    /// 超过 1 小时仍未升级会计入失败次数，与增量路径共用安全模式。
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
        // 整个“下载 + 启动安装器”持锁：两个安装器并发会互相替换同一个安装目录。
        let _lock = self.acquire_lock()?;
        let outcome = self.download_full_locked(cancel, on_event).await?;

        match outcome {
            FullDownloadOutcome::Downloaded { installer } => {
                // 先记录待确认：安装器由平台启动，是否真的装上都只能下次启动判断。
                super::full_pending::write(&self.work_dir, installer.version());
                let launched = crate::platform::full_install::install_with_mode_kind_and_args(
                    &self.executable_path,
                    installer.path(),
                    &self.work_dir,
                    self.script_timings(),
                    self.config.install_mode,
                    self.config.installer_kind,
                    &self.config.installer_args,
                );
                match launched {
                    Ok(()) => Ok(UpdateResult::Updated {
                        version: installer.version().to_string(),
                    }),
                    Err(err) => {
                        // 安装器根本没起来：清除待确认，并计入失败次数。
                        super::full_pending::clear(&self.work_dir);
                        let _ = crate::core::safety::record_failed_attempt_with_reason(
                            &self.work_dir,
                            crate::updater::SafeModeReason::EnvironmentFailure,
                        )
                        .await;
                        Err(err)
                    }
                }
            }
            FullDownloadOutcome::UpToDate { version } => Ok(UpdateResult::UpToDate { version }),
            FullDownloadOutcome::Deferred {
                version,
                percentage,
            } => Ok(UpdateResult::Deferred {
                version,
                percentage,
            }),
            FullDownloadOutcome::UnsupportedOs { required, current } => {
                Ok(UpdateResult::UnsupportedOs { required, current })
            }
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
