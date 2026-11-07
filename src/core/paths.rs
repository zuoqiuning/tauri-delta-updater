//! 运行时路径推导：默认工作目录与可写性探测。
//!
//! 工作目录不能默认放在可执行文件旁边——`C:\Program Files` 与
//! `/usr/bin` 对普通用户只读，锁、补丁、暂存文件全部写不进去，
//! 更新会在下载阶段失败并累计进安全模式，而 UAC/提权发生在更晚的替换阶段。

use crate::error::{DeltaError, Result};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};

/// 推导应用的工作目录。
///
/// 优先使用用户缓存目录，并在其下按"可执行文件名 + 路径指纹"分目录：
///
/// - 同一应用的多次更新共享同一目录（续传、失败计数、互斥锁才有意义）；
/// - 不同应用即使可执行文件同名也不会互相覆盖。
///
/// 缓存目录不可用时退回可执行文件所在目录（旧行为），
/// 由 [`ensure_writable`] 在构造阶段给出明确错误。
pub fn default_work_dir(exe: &Path) -> PathBuf {
    match cache_root() {
        Some(root) => {
            let stem = exe
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| "app".to_string());
            let mut hasher = Sha256::new();
            hasher.update(exe.to_string_lossy().as_bytes());
            let fingerprint = format!("{:x}", hasher.finalize());
            root.join("tauri-delta-updater")
                .join(format!("{stem}-{}", &fingerprint[..8]))
        }
        None => exe
            .parent()
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(".")),
    }
}

/// 平台用户缓存根目录。
fn cache_root() -> Option<PathBuf> {
    if cfg!(windows) {
        return std::env::var_os("LOCALAPPDATA")
            .filter(|v| !v.is_empty())
            .map(PathBuf::from);
    }
    if cfg!(target_os = "macos") {
        return std::env::var_os("HOME")
            .filter(|v| !v.is_empty())
            .map(|home| PathBuf::from(home).join("Library/Caches"));
    }
    // Linux / 其它 Unix：遵守 XDG 约定
    if let Some(cache) = std::env::var_os("XDG_CACHE_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(cache));
    }
    std::env::var_os("HOME")
        .filter(|v| !v.is_empty())
        .map(|home| PathBuf::from(home).join(".cache"))
}

/// 写入并删除一个探针文件，返回目录是否可写。
///
/// 探针名带进程 ID、进程内序号与时间戳：多个实例/线程同时探测同一目录时，
/// 固定文件名会互相争用（Windows 上表现为共享冲突）；PID 被复用时，
/// 上一次残留的同名文件也可能仍被杀毒软件持有。
fn probe_write(dir: &Path) -> bool {
    use std::sync::atomic::{AtomicU32, Ordering};
    static SEQ: AtomicU32 = AtomicU32::new(0);
    let seq = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let probe = dir.join(format!(
        ".tdx_write_probe-{}-{seq}-{nanos}",
        std::process::id()
    ));
    if std::fs::write(&probe, b"ok").is_ok() {
        let _ = std::fs::remove_file(&probe);
        true
    } else {
        false
    }
}

/// 确保目录存在且可写，否则返回 [`DeltaError::WorkDirNotWritable`]。
///
/// 用"创建探针文件再删除"判定真实可写性：仅检查 `metadata().permissions()`
/// 在 Windows 上会给出错误结论（只读位与 ACL 是两套机制）。
pub fn ensure_writable(dir: &Path) -> Result<()> {
    // 杀毒软件/索引器可能瞬时持有新建文件或目录；短重试一次，
    // 避免把可写目录误判成只读（真正的只读目录两次都会失败）。
    for attempt in 0..2 {
        if attempt > 0 {
            std::thread::sleep(std::time::Duration::from_millis(50));
        }
        if std::fs::create_dir_all(dir).is_ok() && probe_write(dir) {
            return Ok(());
        }
    }
    Err(DeltaError::WorkDirNotWritable(dir.to_path_buf()))
}

/// 只读探测：目录当前是否可写，不创建目录。
///
/// 用于诊断安装目录是否需要提权。与 [`ensure_writable`] 的区别是
/// 这里不会创建缺失的目录，也不会把错误带出去。
pub fn is_writable(dir: &Path) -> bool {
    dir.is_dir()
        && (probe_write(dir) || {
            std::thread::sleep(std::time::Duration::from_millis(50));
            probe_write(dir)
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_work_dir_is_stable_and_distinct() {
        let a = default_work_dir(Path::new("/tmp/app/MyApp.exe"));
        let b = default_work_dir(Path::new("/tmp/app/MyApp.exe"));
        let c = default_work_dir(Path::new("/tmp/other/MyApp.exe"));
        assert_eq!(a, b, "同一路径必须得到同一工作目录");
        assert_ne!(a, c, "不同路径不得互相覆盖");
    }

    #[test]
    fn ensure_writable_reports_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        ensure_writable(dir.path()).unwrap();
    }
}
