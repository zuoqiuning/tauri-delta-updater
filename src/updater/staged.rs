//! 已就绪、等待替换的产物，以及全量安装包信息。

use crate::core::bundle::{BundleFile, BundleManifest};
use crate::error::{DeltaError, Result};
use crate::models::UpdateManifest;
use crate::platform::PlatformUpdater;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};

/// 全量安装包信息（增量不可用时交给官方 updater / 安装流程）。
///
/// 由 [`CheckOutcome::full_download`] 或 [`CheckOutcome::full_download_for`] 获取。
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[non_exhaustive]
pub struct FullDownload {
    /// 目标版本号。
    pub version: String,
    /// 安装包下载地址。
    pub url: String,
    /// 安装包的 minisign 签名（`platforms[].signature`）。
    pub signature: String,
}

impl FullDownload {
    /// 从已验签的清单中取出指定平台的全量安装包信息。
    pub fn from_manifest(manifest: &UpdateManifest, platform: &str) -> Option<Self> {
        let info = manifest.platforms.get(platform)?;
        Some(Self {
            version: manifest.version.clone(),
            url: info.url.clone(),
            signature: info.signature.clone(),
        })
    }
}

/// 已下载并通过签名校验的全量安装包。
///
/// 由 [`DeltaUpdater::download_full`] 返回。文件放在工作目录，
/// 文件名带版本号；宿主拿到后负责执行安装（Windows NSIS/MSI、
/// macOS `.app.tar.gz`、Linux AppImage），安装完成后应删除该文件。
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct StagedFullInstaller {
    pub(super) version: String,
    pub(super) path: PathBuf,
}

impl StagedFullInstaller {
    /// 安装包对应的目标版本。
    pub fn version(&self) -> &str {
        &self.version
    }

    /// 安装包在磁盘上的路径。
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// 全量下载的结果。
#[derive(Debug)]
#[non_exhaustive]
#[must_use = "Downloaded 表示安装包已就绪，忽略结果会留下无用的下载文件"]
pub enum FullDownloadOutcome {
    /// 安装包已下载并通过签名校验，可以直接交给安装执行器。
    Downloaded {
        /// 已就绪的安装包。
        installer: StagedFullInstaller,
    },
    /// 清单版本不高于当前版本（含防降级重放），没有任何下载。
    UpToDate {
        /// 清单声明的版本号。
        version: String,
    },
    /// 新版本存在，但当前安装不在本次灰度范围内，没有任何下载。
    Deferred {
        /// 清单声明的版本号。
        version: String,
        /// 发布端声明的放量比例（0–100）。
        percentage: u8,
    },
    /// 本机系统版本低于清单要求，没有任何下载。
    UnsupportedOs {
        /// 清单声明的最低系统版本。
        required: String,
        /// 本机探测到的系统版本。
        current: String,
    },
}

/// 已就绪、等待替换的更新产物。
///
/// 由 [`DeltaUpdater::prepare`] 生成。宿主可以先做展示、提示等操作，
/// 再调用 [`Staged::launch`] 真正触发文件替换。
#[derive(Debug)]
pub struct Staged {
    /// 已通过双端哈希校验的合并产物（类型上排除未验证字节流入替换流程）
    pub(super) staged_exe: crate::core::diff::VerifiedArtifact,
    pub(super) script_path: PathBuf,
    pub(super) manifest: Box<UpdateManifest>,
    /// 防止替换脚本被启动两次：脚本与新进程竞争替换会让回滚逻辑误判。
    /// 用 Arc 保证所有克隆共享同一份状态，克隆体也无法绕过幂等。
    pub(super) launched: std::sync::Arc<AtomicBool>,
}

impl Clone for Staged {
    fn clone(&self) -> Self {
        Self {
            staged_exe: self.staged_exe.clone(),
            script_path: self.script_path.clone(),
            manifest: self.manifest.clone(),
            launched: std::sync::Arc::clone(&self.launched),
        }
    }
}

impl Staged {
    /// 合并后的新版本可执行文件路径。
    pub fn staged_exe(&self) -> &Path {
        self.staged_exe.path()
    }

    /// 替换脚本路径。
    pub fn script_path(&self) -> &Path {
        &self.script_path
    }

    /// 本次使用的清单。
    pub fn manifest(&self) -> &UpdateManifest {
        &self.manifest
    }

    /// 启动替换脚本，开始文件替换与重启。
    ///
    /// **只能成功启动一次**：重复调用返回 [`DeltaError::Config`]
    /// （两个脚本竞争替换会让回滚逻辑误判）。启动失败不占名额，可以重试。
    /// 调用后主进程应当尽快退出，否则旧程序会占用 exe 文件导致替换失败。
    ///
    /// # 并发语义
    ///
    /// 幂等状态经 `Arc` 在所有克隆间共享：并发调用时只有一个线程会真正启动
    /// 替换脚本，其余立即返回 [`DeltaError::Config`]。启动失败后状态重置，
    /// 之后任何线程（包括之前失败的）都可以重试；
    /// 需要串行化重试时序的调用方应自行加锁。
    pub fn launch(&self) -> Result<()> {
        launch_script_once(&self.launched, &self.script_path)
    }
}

/// 已就绪、等待替换的整包更新产物。
///
/// 由 [`DeltaUpdater::prepare`] 在启用整包增量时生成。宿主可以先展示提示，
/// 再调用 [`StagedBundle::launch`] 触发多文件替换。
#[derive(Debug)]
pub struct StagedBundle {
    pub(super) staging_dir: PathBuf,
    pub(super) files: Vec<BundleFile>,
    pub(super) script_path: PathBuf,
    pub(super) manifest: Box<BundleManifest>,
    pub(super) launched: std::sync::Arc<AtomicBool>,
}

impl Clone for StagedBundle {
    fn clone(&self) -> Self {
        Self {
            staging_dir: self.staging_dir.clone(),
            files: self.files.clone(),
            script_path: self.script_path.clone(),
            manifest: self.manifest.clone(),
            launched: std::sync::Arc::clone(&self.launched),
        }
    }
}

impl StagedBundle {
    /// 已校验的新版本文件暂存目录。
    pub fn staging_dir(&self) -> &Path {
        &self.staging_dir
    }

    /// 新版本安装目录应包含的文件清单。
    pub fn files(&self) -> &[BundleFile] {
        &self.files
    }

    /// 多文件替换脚本路径。
    pub fn script_path(&self) -> &Path {
        &self.script_path
    }

    /// 本次使用的整包清单。
    pub fn manifest(&self) -> &BundleManifest {
        &self.manifest
    }

    /// 启动替换脚本，开始多文件替换与重启。
    ///
    /// 幂等语义与 [`Staged::launch`] 相同：只能成功启动一次，失败可以重试。
    pub fn launch(&self) -> Result<()> {
        launch_script_once(&self.launched, &self.script_path)
    }
}

/// 已就绪、等待执行的整包修复产物。
///
/// 由 [`DeltaUpdater::prepare`] / [`DeltaUpdater::update`] 在检测到上次替换
/// 中断时生成。脚本会删除本次更新新增的文件、还原备份，然后重启旧版本。
#[derive(Debug)]
pub struct StagedRepair {
    pub(super) script_path: PathBuf,
    pub(super) reason: String,
    pub(super) launched: std::sync::Arc<AtomicBool>,
}

impl Clone for StagedRepair {
    fn clone(&self) -> Self {
        Self {
            script_path: self.script_path.clone(),
            reason: self.reason.clone(),
            launched: std::sync::Arc::clone(&self.launched),
        }
    }
}

impl StagedRepair {
    /// 修复脚本路径。
    pub fn script_path(&self) -> &Path {
        &self.script_path
    }

    /// 修复原因（例如上次回滚的 `startup_timeout`）。
    pub fn reason(&self) -> &str {
        &self.reason
    }

    /// 启动修复脚本。
    ///
    /// 幂等语义与 [`Staged::launch`] 相同：只能成功启动一次，失败可以重试。
    /// 调用后主进程必须尽快退出，否则脚本会等待超时。
    pub fn launch(&self) -> Result<()> {
        launch_script_once(&self.launched, &self.script_path)
    }
}

/// 启动替换脚本，并保证同一份产物只启动一次。
fn launch_script_once(launched: &std::sync::Arc<AtomicBool>, script_path: &Path) -> Result<()> {
    if launched
        .compare_exchange(false, true, Ordering::Relaxed, Ordering::Relaxed)
        .is_err()
    {
        return Err(DeltaError::Config(
            "替换脚本已经启动过，不能再次启动：重复启动会让两个脚本竞争替换，导致回滚误判".into(),
        ));
    }
    match PlatformUpdater::launch_updater(script_path) {
        Ok(()) => {
            crate::core::events::launch();
            Ok(())
        }
        // 启动失败不占名额，允许重试
        Err(e) => {
            launched.store(false, Ordering::Relaxed);
            Err(e)
        }
    }
}
