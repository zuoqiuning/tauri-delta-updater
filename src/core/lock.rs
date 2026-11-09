use crate::core::artifacts;
use crate::error::{DeltaError, Result};
use fs4::fs_std::FileExt;
use std::fs::File;
use std::path::Path;

/// 更新互斥锁，防止多开导致的文件覆盖冲突
#[derive(Debug)]
pub struct UpdateLock {
    _files: Vec<File>,
}

impl UpdateLock {
    /// 尝试获取锁。如果另一个进程正在更新，返回 [`DeltaError::UpdateInProgress`]，
    /// 调用方可据此与"真正的异常失败"区分开。
    ///
    /// # 为什么必须区分 `Ok(false)`
    ///
    /// `fs4` 的 `try_lock_exclusive` 返回 `Result<bool>`：
    /// `Ok(false)` 表示**文件已被其它进程锁住**，不是获取成功。
    /// 早期实现用 `Ok(_)` 匹配，把 `Ok(false)` 当成成功——锁实际上从未生效。
    pub fn acquire(lock_path: &Path) -> Result<Self> {
        let file = File::create(lock_path)?;
        match file.try_lock_exclusive() {
            Ok(true) => Ok(Self { _files: vec![file] }),
            Ok(false) => Err(DeltaError::UpdateInProgress),
            Err(e) => Err(DeltaError::Io(e)),
        }
    }

    /// 在可执行文件所在目录获取锁，使用统一的锁文件名。
    pub fn acquire_in(base_dir: &Path) -> Result<Self> {
        Self::acquire(&base_dir.join(artifacts::LOCK_FILE))
    }

    /// 同时获取工作目录锁与安装目录的跨用户锁（`LockScope::InstallDir`）。
    ///
    /// 安装目录锁是**尽力而为**的：per-machine 安装目录对普通用户只读，
    /// 创建失败时退回仅工作目录锁，并记录告警——否则会直接把原本可用的
    /// per-user 更新变成硬失败。宿主若要求强一致，应把安装目录设为可写
    /// 或自行在服务里串行化。
    pub fn acquire_scoped(work_dir: &Path, install_dir: Option<&Path>) -> Result<Self> {
        let mut lock = Self::acquire_in(work_dir)?;
        let Some(install_dir) = install_dir else {
            return Ok(lock);
        };
        let path = install_dir.join(artifacts::MACHINE_LOCK_FILE);
        match File::create(&path) {
            Ok(file) => match file.try_lock_exclusive() {
                Ok(true) => lock._files.push(file),
                Ok(false) => return Err(DeltaError::UpdateInProgress),
                Err(e) => return Err(DeltaError::Io(e)),
            },
            Err(e) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                #[cfg(feature = "tracing")]
                tracing::warn!(
                    target: "tauri_delta_updater",
                    event = "delta.lock.machine_scope_unavailable",
                    path = %path.display(),
                    "安装目录不可写，无法建立跨用户锁；退回工作目录锁"
                );
            }
            Err(e) => return Err(DeltaError::Io(e)),
        }
        Ok(lock)
    }
}
