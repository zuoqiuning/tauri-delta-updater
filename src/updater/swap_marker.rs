//! “替换进行中”标记：让宿主在下一次启动时知道上一次更新没有走完。
//!
//! 宿主在启动替换脚本前写入，脚本在成功确认或回滚完成时清除，
//! 新版本确认启动时也会清理。正常情况下 UI 不会看到它；它的价值在异常路径：
//! 脚本中途崩溃、被杀或替换卡住时，下一次启动能明确提示“上次更新未完成”，
//! 而不是只表现为“版本没变、什么都不知道”。

use crate::core::artifacts;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// 一次已启动、但尚未确认完成的替换。
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct UpdateInProgress {
    /// 目标版本。
    pub version: String,
    /// 启动替换脚本的时间（Unix 秒）。
    pub started_at: u64,
}

/// 写入标记；尽力而为，写失败不影响替换流程。
pub(crate) fn write(work_dir: &Path, version: &str) {
    let marker = UpdateInProgress {
        version: version.to_string(),
        started_at: now_secs(),
    };
    if let Ok(json) = serde_json::to_vec(&marker) {
        let _ = std::fs::write(work_dir.join(artifacts::UPDATE_IN_PROGRESS_FILE), json);
    }
}

/// 读取标记；不存在或损坏时为 `None`。
pub(crate) fn read(work_dir: &Path) -> Option<UpdateInProgress> {
    let text = std::fs::read_to_string(work_dir.join(artifacts::UPDATE_IN_PROGRESS_FILE)).ok()?;
    serde_json::from_str(&text).ok()
}

/// 清除标记。
pub(crate) fn clear(work_dir: &Path) {
    let _ = std::fs::remove_file(work_dir.join(artifacts::UPDATE_IN_PROGRESS_FILE));
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::{clear, read, write};
    use tempfile::tempdir;

    #[test]
    fn write_read_clear_round_trip() {
        let dir = tempdir().unwrap();
        assert!(read(dir.path()).is_none(), "没有标记时必须是 None");

        write(dir.path(), "0.3.0");
        let marker = read(dir.path()).unwrap();
        assert_eq!(marker.version, "0.3.0");
        assert!(marker.started_at > 0);

        clear(dir.path());
        assert!(read(dir.path()).is_none());
    }

    /// 损坏的标记按不存在处理：不能因为一个坏文件让诊断永久报错。
    #[test]
    fn corrupt_marker_is_ignored() {
        let dir = tempdir().unwrap();
        std::fs::write(
            dir.path()
                .join(crate::core::artifacts::UPDATE_IN_PROGRESS_FILE),
            b"not json",
        )
        .unwrap();
        assert!(read(dir.path()).is_none());
    }
}
