//! 替换后的启动状态指针：`staged` → `booting` → `committed`。
//!
//! 状态落盘而不是只靠内存或计时器：新进程崩溃、被 `kill -9`、或健康检查
//! 未通过时，下一次启动仍能从文件里读出"这次替换走到了哪一步"。
//! 替换脚本只负责写 `staged` 与 `rolled_back`；`booting` / `committed`
//! 由新版本的宿主代码推进。

use serde::Serialize;
use std::path::Path;

/// 一次替换的启动阶段。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum StartupState {
    /// 新版本已落盘并交给替换脚本，尚未启动。
    Staged,
    /// 新进程已执行到宿主代码，但尚未确认健康。
    Booting,
    /// 宿主已调用确认 API，替换完成。
    Committed,
    /// 宿主健康检查失败，等待替换脚本回滚。
    Failed,
    /// 替换脚本已回滚到旧版本。
    RolledBack,
}

impl StartupState {
    /// 写入状态文件的字符串形式。
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Staged => "staged",
            Self::Booting => "booting",
            Self::Committed => "committed",
            Self::Failed => "failed",
            Self::RolledBack => "rolled_back",
        }
    }

    /// 解析状态文件；未知值返回 `None`，兼容未来新增状态。
    pub fn parse(value: &str) -> Option<Self> {
        match value.trim() {
            "staged" => Some(Self::Staged),
            "booting" => Some(Self::Booting),
            "committed" => Some(Self::Committed),
            "failed" => Some(Self::Failed),
            "rolled_back" => Some(Self::RolledBack),
            _ => None,
        }
    }
}

/// 写入启动状态（覆盖旧值）。
pub fn write(work_dir: &Path, state: StartupState) -> std::io::Result<()> {
    std::fs::write(
        work_dir.join(crate::core::artifacts::UPDATE_STATE_FILE),
        state.as_str(),
    )
}

/// 读取启动状态；文件不存在或内容未知时返回 `None`。
pub fn read(work_dir: &Path) -> Option<StartupState> {
    let text =
        std::fs::read_to_string(work_dir.join(crate::core::artifacts::UPDATE_STATE_FILE)).ok()?;
    // PowerShell 5.1 的 Set-Content -Encoding UTF8 会带 BOM；兼容手工写入。
    StartupState::parse(text.trim_start_matches('\u{feff}'))
}

#[cfg(test)]
mod tests {
    use super::{read, write, StartupState};

    /// 状态文件是脚本周转的契约：写入与解析必须一致。
    #[test]
    fn round_trips_all_states() {
        let dir = tempfile::tempdir().unwrap();
        for state in [
            StartupState::Staged,
            StartupState::Booting,
            StartupState::Committed,
            StartupState::Failed,
            StartupState::RolledBack,
        ] {
            write(dir.path(), state).unwrap();
            assert_eq!(read(dir.path()), Some(state));
        }
    }

    /// 未知值不能被误读成某个已知状态，否则未来的格式扩展会静默出错。
    #[test]
    fn unknown_or_missing_state_is_none() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(read(dir.path()), None);

        std::fs::write(
            dir.path().join(crate::core::artifacts::UPDATE_STATE_FILE),
            "future_state",
        )
        .unwrap();
        assert_eq!(read(dir.path()), None);
    }
}
