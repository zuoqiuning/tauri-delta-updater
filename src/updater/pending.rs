//! 跨重启待安装：`prepare` 成功后落盘，下次启动或显式调用时消费。

use crate::core::artifacts;
use crate::core::bundle::BundleFile;
use crate::core::hasher::calculate_hash;
use crate::core::lock::UpdateLock;
use crate::error::{DeltaError, Result};
use crate::platform::target::normalize_version;
use crate::platform::PlatformUpdater;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use super::DeltaUpdater;

/// 待安装产物的类型。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[non_exhaustive]
pub enum PendingArtifact {
    /// 单文件增量：合并后的可执行文件已通过双端哈希校验。
    Exe {
        /// 合并产物路径。
        staged_exe: PathBuf,
        /// 合并产物的 SHA-256（清单中的 `target_hash`）。
        target_hash: String,
    },
    /// 整包增量：暂存目录已包含新版本的全部文件。
    Bundle {
        /// 新版本文件暂存目录。
        staging_dir: PathBuf,
        /// 新版本文件清单（路径相对安装目录）。
        files: Vec<BundleFile>,
    },
}

/// 一条跨重启待安装记录。
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct PendingUpdate {
    /// 目标版本号。
    pub version: String,
    /// 已生成的替换脚本。
    pub script_path: PathBuf,
    /// 产物信息。
    pub artifact: PendingArtifact,
    /// 记录创建时间（Unix 秒），用于诊断与过期判断。
    pub created_at: u64,
}

impl PendingUpdate {
    /// 目标版本号。
    pub fn version(&self) -> &str {
        &self.version
    }

    /// 替换脚本路径。
    pub fn script_path(&self) -> &Path {
        &self.script_path
    }

    /// 是否为整包更新。
    pub fn is_bundle(&self) -> bool {
        matches!(self.artifact, PendingArtifact::Bundle { .. })
    }

    /// 单文件更新时返回合并产物路径。
    pub fn staged_exe(&self) -> Option<&Path> {
        match &self.artifact {
            PendingArtifact::Exe { staged_exe, .. } => Some(staged_exe),
            PendingArtifact::Bundle { .. } => None,
        }
    }

    /// 整包更新时返回文件清单。
    pub fn bundle_files(&self) -> Option<&[BundleFile]> {
        match &self.artifact {
            PendingArtifact::Bundle { files, .. } => Some(files),
            PendingArtifact::Exe { .. } => None,
        }
    }
}

impl DeltaUpdater {
    /// 读取并校验跨重启待安装记录。
    ///
    /// 校验失败或记录已过期（版本不高于当前版本）时删除记录并返回 `None`；
    /// 不发起网络请求。
    ///
    /// # 信任模型
    ///
    /// 记录本身不是签名产物，它绑定的是 **prepare 时已验签的产物哈希**：
    ///
    /// - 版本必须严格高于当前版本→ pending 不能变成降级后门；
    /// - 脚本与产物必须位于工作目录内→ 记录被改写也不能启动任意路径；
    /// - 单文件更新会重算 SHA-256，与 prepare 时的 `target_hash` 比对。
    ///
    /// 它**不感知发布撤回**：启动时不会联网复核清单（保证离线可安装）。
    /// 若要求 pending 与服务端当前清单一致，宿主应在安装前先 [`Self::check`]，
    /// 发现清单版本更高时直接安装新版本；旧 pending 会因版本不满足而自动丢弃。
    pub async fn pending_update(&self) -> Result<Option<PendingUpdate>> {
        let path = self.work_dir().join(artifacts::PENDING_FILE);
        let bytes = match tokio::fs::read(&path).await {
            Ok(bytes) => bytes,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(err) => return Err(err.into()),
        };

        let record: PendingUpdate = match serde_json::from_slice(&bytes) {
            Ok(record) => record,
            Err(_) => {
                let _ = tokio::fs::remove_file(&path).await;
                return Ok(None);
            }
        };

        let valid = self.validate_pending(&record).await;
        if !valid {
            let _ = tokio::fs::remove_file(&path).await;
            return Ok(None);
        }
        Ok(Some(record))
    }

    /// 执行跨重启待安装的替换脚本。
    ///
    /// 返回 `true` 表示已启动替换（调用方应退出进程）；`false` 表示没有
    /// 可执行的待安装记录。启动成功后记录会被删除，避免下次重复触发。
    ///
    /// # 与"有更新版本可用"的关系
    ///
    /// 本方法不联网，只校验记录本身。若宿主希望在服务端已有更高版本时
    /// 直接安装新版本，应先 [`Self::check`]，发现清单版本高于
    /// [`Self::pending_update`] 的版本时调用 [`Self::discard_pending`]，
    /// 再执行新的更新；否则会先装到 pending 的旧版本，再走一次更新。
    pub async fn install_pending(&self) -> Result<bool> {
        let _lock = UpdateLock::acquire_in(self.work_dir())?;
        let Some(record) = self.pending_update().await? else {
            return Ok(false);
        };
        PlatformUpdater::launch_updater(&record.script_path)?;
        let _ = tokio::fs::remove_file(self.work_dir().join(artifacts::PENDING_FILE)).await;
        crate::core::events::launch();
        Ok(true)
    }

    /// 丢弃跨重启待安装记录；返回是否确实删除了记录。
    ///
    /// `pending_update()` 不联网，无从知道服务端是否已经发布了更高版本。
    /// 宿主应在 `check()` 拿到已验签清单后比较版本：清单更高时调用本方法，
    /// 再执行新的更新，避免先执行一个基于旧清单的暂存脚本。
    /// 暂存产物本身会在下一次更新开始时统一清理。
    pub async fn discard_pending(&self) -> Result<bool> {
        let _lock = UpdateLock::acquire_in(self.work_dir())?;
        match tokio::fs::remove_file(self.work_dir().join(artifacts::PENDING_FILE)).await {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(err) => Err(err.into()),
        }
    }

    /// 写入待安装记录；`prepare` 在未立即启动替换时调用。
    pub(super) async fn store_pending(
        &self,
        version: &str,
        script_path: &Path,
        artifact: PendingArtifact,
    ) -> Result<()> {
        let record = PendingUpdate {
            version: normalize_version(version).to_string(),
            script_path: script_path.to_path_buf(),
            artifact,
            created_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_secs())
                .unwrap_or(0),
        };
        let bytes = serde_json::to_vec(&record)
            .map_err(|err| DeltaError::Config(format!("序列化待安装记录失败: {err}")))?;
        tokio::fs::write(self.work_dir().join(artifacts::PENDING_FILE), bytes).await?;
        Ok(())
    }

    async fn validate_pending(&self, record: &PendingUpdate) -> bool {
        // 版本单调性：只有严格更高的版本才允许执行，防止 pending 变成降级后门。
        if !is_newer(&record.version, self.current_version()) {
            return false;
        }
        // 脚本与产物必须位于工作目录内，避免记录被改写后启动任意路径。
        if !record.script_path.starts_with(self.work_dir()) {
            return false;
        }
        if !record.script_path.is_file() {
            return false;
        }
        match &record.artifact {
            PendingArtifact::Exe {
                staged_exe,
                target_hash,
            } => {
                if !staged_exe.starts_with(self.work_dir()) || !staged_exe.is_file() {
                    return false;
                }
                match calculate_hash(staged_exe).await {
                    Ok(hash) => hash == *target_hash,
                    Err(_) => false,
                }
            }
            PendingArtifact::Bundle { staging_dir, files } => {
                if !staging_dir.starts_with(self.work_dir()) || !staging_dir.is_dir() {
                    return false;
                }
                files.iter().all(|file| {
                    let path =
                        staging_dir.join(file.path.replace('/', std::path::MAIN_SEPARATOR_STR));
                    path.is_file() || path.is_symlink()
                })
            }
        }
    }
}

/// 语义化版本比较；无法解析的版本一律判定为“不更新”。
fn is_newer(candidate: &str, current: &str) -> bool {
    let Ok(candidate) = semver::Version::parse(normalize_version(candidate)) else {
        return false;
    };
    let Ok(current) = semver::Version::parse(normalize_version(current)) else {
        return false;
    };
    candidate > current
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::updater::UpdateConfig;

    fn test_updater(dir: &Path, current: &str) -> DeltaUpdater {
        DeltaUpdater::new(
            UpdateConfig::new("https://cdn.example.com/latest.json", "dGVzdA==")
                .current_version(current)
                .executable_path(dir.join("app.exe"))
                .work_dir(dir),
        )
        .unwrap()
    }

    /// 落盘一条记录，并生成记录引用的脚本与产物。
    async fn write_exe_record(dir: &Path, version: &str, content: &[u8]) -> PendingUpdate {
        std::fs::write(dir.join("tdx_update.ps1"), b"exit 0").unwrap();
        let staged = dir.join(artifacts::staged_file());
        std::fs::write(&staged, content).unwrap();
        let target_hash = calculate_hash(&staged).await.unwrap_or_default();
        let record = PendingUpdate {
            version: version.into(),
            script_path: dir.join("tdx_update.ps1"),
            artifact: PendingArtifact::Exe {
                staged_exe: staged,
                target_hash,
            },
            created_at: 0,
        };
        std::fs::write(
            dir.join(artifacts::PENDING_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();
        record
    }

    /// 合法记录必须被读出；记录文件保留，供 `install_pending` 消费。
    #[tokio::test]
    async fn accepts_valid_record() {
        let dir = tempfile::tempdir().unwrap();
        write_exe_record(dir.path(), "0.3.0", b"new").await;

        let updater = test_updater(dir.path(), "0.2.0");
        let pending = updater.pending_update().await.unwrap().unwrap();
        assert_eq!(pending.version(), "0.3.0");
        assert!(pending.staged_exe().is_some());
        assert!(!pending.is_bundle());

        // 重复读取仍然有效（消费前不能被读一次就删）。
        assert!(updater.pending_update().await.unwrap().is_some());
    }

    /// 版本不高于当前版本时丢弃：pending 不能成为降级后门。
    #[tokio::test]
    async fn rejects_version_not_newer_than_current() {
        let dir = tempfile::tempdir().unwrap();
        write_exe_record(dir.path(), "0.2.0", b"new").await;

        let updater = test_updater(dir.path(), "0.2.0");
        assert!(updater.pending_update().await.unwrap().is_none());
        assert!(
            !dir.path().join(artifacts::PENDING_FILE).exists(),
            "无效记录必须被删除，否则每次启动都要重新校验"
        );
    }

    /// 产物被篡改（哈希不符）时丢弃，而不是执行一个来源不明的产物。
    #[tokio::test]
    async fn rejects_tampered_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let record = write_exe_record(dir.path(), "0.3.0", b"new").await;

        let staged = record.staged_exe().unwrap();
        std::fs::write(staged, b"tampered").unwrap();

        let updater = test_updater(dir.path(), "0.2.0");
        assert!(updater.pending_update().await.unwrap().is_none());
    }

    /// 脚本或产物位于工作目录之外时丢弃：记录被改写也不能启动任意路径。
    #[tokio::test]
    async fn rejects_paths_outside_work_dir() {
        let dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("evil.ps1"), b"exit 0").unwrap();
        std::fs::write(outside.path().join(artifacts::staged_file()), b"x").unwrap();

        let record = PendingUpdate {
            version: "0.3.0".into(),
            script_path: outside.path().join("evil.ps1"),
            artifact: PendingArtifact::Exe {
                staged_exe: outside.path().join(artifacts::staged_file()),
                target_hash: String::new(),
            },
            created_at: 0,
        };
        std::fs::write(
            dir.path().join(artifacts::PENDING_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .unwrap();

        let updater = test_updater(dir.path(), "0.2.0");
        assert!(updater.pending_update().await.unwrap().is_none());
    }

    /// 没有记录时不得启动任何东西。
    #[tokio::test]
    async fn install_pending_is_noop_without_record() {
        let dir = tempfile::tempdir().unwrap();
        let updater = test_updater(dir.path(), "0.2.0");
        assert!(!updater.install_pending().await.unwrap());
    }

    /// 显式丢弃：第一次返回 true，之后返回 false。
    #[tokio::test]
    async fn discard_pending_reports_whether_record_existed() {
        let dir = tempfile::tempdir().unwrap();
        let updater = test_updater(dir.path(), "0.2.0");
        assert!(!updater.discard_pending().await.unwrap());

        write_exe_record(dir.path(), "0.3.0", b"new").await;
        assert!(updater.discard_pending().await.unwrap());
        assert!(!dir.path().join(artifacts::PENDING_FILE).exists());
        assert!(!updater.discard_pending().await.unwrap());
    }

    #[test]
    fn pending_version_must_be_strictly_newer() {
        assert!(is_newer("0.2.0", "0.1.0"));
        assert!(is_newer("v0.3.0", "0.2.0"));
        assert!(!is_newer("0.2.0", "0.2.0"));
        assert!(!is_newer("0.1.0", "0.2.0"));
        assert!(!is_newer("not-a-version", "0.2.0"));
    }
}
