use crate::core::artifacts;
use crate::error::{DeltaError, Result};
use fs4::fs_std::FileExt;
use std::fs::File;
use std::path::Path;

/// 更新互斥锁，防止多开导致的文件覆盖冲突
#[derive(Debug)]
pub struct UpdateLock {
    _file: File,
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
            Ok(true) => Ok(Self { _file: file }),
            Ok(false) => Err(DeltaError::UpdateInProgress),
            Err(e) => Err(DeltaError::Io(e)),
        }
    }

    /// 在可执行文件所在目录获取锁，使用统一的锁文件名。
    pub fn acquire_in(base_dir: &Path) -> Result<Self> {
        Self::acquire(&base_dir.join(artifacts::LOCK_FILE))
    }
}
