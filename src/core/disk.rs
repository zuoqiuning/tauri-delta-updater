use crate::core::artifacts;
use crate::error::{DeltaError, Result};
use std::path::{Path, PathBuf};
use tokio::fs;

/// 获取指定路径所在磁盘的剩余可用空间（字节）。
///
/// 路径可以指向文件或目录；若路径不存在，会向上查找最近的已存在目录，
/// 因此对尚未创建的输出文件同样有效。
pub fn get_available_space(path: &Path) -> Result<u64> {
    let dir = existing_ancestor(path).ok_or_else(|| {
        DeltaError::Io(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("无法定位磁盘空间查询路径: {}", path.display()),
        ))
    })?;
    // fs4 0.13 的空间查询自由函数在 crate 根（fs_std 模块只含 FileExt）
    fs4::available_space(&dir).map_err(DeltaError::Io)
}

/// 检查目标路径所在磁盘的剩余空间是否满足需求。
///
/// `path` 可以是待写入的文件路径，也可以是目录。
pub fn check_disk_space(path: &Path, required_bytes: u64) -> Result<()> {
    let available = get_available_space(path)?;
    if available < required_bytes {
        return Err(DeltaError::InsufficientDiskSpace);
    }
    Ok(())
}

/// 向上查找最近的已存在目录，并转换为绝对路径。
fn existing_ancestor(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };

    let mut current = Some(absolute.as_path());
    while let Some(dir) = current {
        if dir.is_dir() {
            return Some(dir.to_path_buf());
        }
        current = dir.parent();
    }
    None
}

/// 日志/事件文件超过上限时轮转一次（保留 `.1`）。
///
/// 桌面应用可能运行多年，`tdx_update.log` 与 `tdx_events.jsonl` 会无限追加。
/// 在构造更新器时（每次启动）检查一次，成本只是两次 `metadata`。
pub fn rotate_logs(work_dir: &Path) {
    /// 单个文件的上限：足够保留最近若干次更新的完整排障信息。
    const MAX_LOG_BYTES: u64 = 2 * 1024 * 1024;
    for name in [artifacts::LOG_FILE, artifacts::EVENTS_FILE] {
        let path = work_dir.join(name);
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.len() <= MAX_LOG_BYTES {
            continue;
        }
        // rename 会覆盖旧的 `.1`，即只保留上一代，不会无限堆积。
        let _ = std::fs::rename(&path, work_dir.join(format!("{name}.1")));
    }
}

/// 删除工作目录里遗留的全量安装包（同步版本）。
///
/// 新版本确认启动成功后调用：此时安装包已经完成使命——Windows 安装器早已退出、
/// macOS/Linux 已完成替换。不清理的话，几十到几百 MB 会一直躺在用户缓存目录里。
pub fn cleanup_full_installers_sync(work_dir: &Path) {
    let Ok(entries) = std::fs::read_dir(work_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with(artifacts::FULL_INSTALLER_PREFIX) {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

/// 清理上一次更新遗留的临时文件。
/// 只删除更新流程自身产生的、可精确命名的产物，不会触碰用户数据。
///
/// 注意：**不能删除失败计数**。它记录的是连续失败次数，
/// 每次尝试更新前清理会让计数永远归零，安全模式形同虚设。
/// 失败计数只在更新成功时清除。
///
/// 也不删除启动成功标志：那是替换脚本判断是否保留新版本的依据，
/// 在替换流程进行期间删除它会导致误回滚。
///
/// # 只删自己产出的文件
///
/// 早期实现按扩展名（`.bak`/`.old`）遍历删除，会连用户的备份文件一起删掉。
/// 现在只删 `<当前 exe 文件名>.bak` 这一个精确名字。
///
/// # 两个目录
///
/// 产物分散在两处，缺一不可：
///
/// - `work_dir`：补丁、暂存文件与替换脚本
/// - `exe_dir`：替换脚本产生的 `.bak` 备份（必须与目标 exe 同目录）
pub async fn cleanup_previous_updates(work_dir: &Path, exe_path: &Path) {
    cleanup_previous_updates_except(work_dir, exe_path, &[]).await;
}

/// 与 [`cleanup_previous_updates`] 相同，但保留 `keep` 中列出的路径。
///
/// `keep` 用于跨重启待安装记录引用的脚本与产物：下一次更新在下载前清理时，
/// 不能删掉已经就绪的 pending 产物，否则新补丁下载失败会让用户"两头空"。
pub async fn cleanup_previous_updates_except(work_dir: &Path, exe_path: &Path, keep: &[PathBuf]) {
    let exe_dir = exe_path.parent().unwrap_or(Path::new("."));
    let keep = |path: &Path| keep.iter().any(|item| item == path);

    // 半截补丁只有在身份记录存在且新鲜时才保留（跨进程续传接着下）；
    // 否则按临时产物清理，避免中断的下载长期占用磁盘。
    for (patch, meta) in [
        (artifacts::PATCH_FILE, artifacts::PATCH_META),
        (artifacts::BUNDLE_PATCH_FILE, artifacts::BUNDLE_PATCH_META),
    ] {
        if !crate::core::downloader::resume_fresh(&work_dir.join(meta)).await {
            let _ = fs::remove_file(work_dir.join(patch)).await;
            let _ = fs::remove_file(work_dir.join(meta)).await;
        }
    }

    for name in [
        artifacts::staged_file(),
        artifacts::STAGED_HOP_A,
        artifacts::STAGED_HOP_B,
        artifacts::STAGED_BACKUP,
        artifacts::updater_script(),
        artifacts::bundle_updater_script(),
        artifacts::bundle_repair_script(),
    ] {
        let path = work_dir.join(name);
        if !keep(&path) {
            let _ = fs::remove_file(&path).await;
        }
    }
    // 全量安装的临时解包目录（替换脚本已在执行时不会走到清理，安全）
    let _ = fs::remove_dir_all(work_dir.join(artifacts::FULL_STAGING_DIR)).await;

    // 整包产物：暂存目录。
    //
    // 不删除 `BUNDLE_BACKUP_DIR`：它是替换脚本在"已改了一半文件"时唯一能整体
    // 回滚的旧内容。上一轮脚本崩溃后，下一次脚本启动会先还原它；
    // 客户端清理把它删掉，安装目录就会永久停在半新半旧的状态。
    let staging = work_dir.join(artifacts::bundle_staging_dir());
    if !keep(&staging) {
        // 暂存路径可能被上次崩溃残留成一个普通文件（极端情况），
        // 只删目录会静默失败，导致本轮解压无法创建目标目录。
        let _ = fs::remove_file(&staging).await;
        let _ = fs::remove_dir_all(&staging).await;
    }

    // 备份目录存在时日志必须保留：修复脚本要靠它删除上次新增的文件。
    if !work_dir.join(artifacts::BUNDLE_BACKUP_DIR).is_dir() {
        let _ = fs::remove_file(work_dir.join(artifacts::BUNDLE_JOURNAL_FILE)).await;
    }

    // 解压后的临时补丁（进程被杀时可能残留）
    let expanded = work_dir
        .join(artifacts::staged_file())
        .with_extension("patch.expanded");
    let _ = fs::remove_file(expanded).await;

    // 历史版本的脚本（含带应用品牌名的旧文件）
    for name in artifacts::LEGACY_SCRIPTS {
        let _ = fs::remove_file(exe_dir.join(name)).await;
        let _ = fs::remove_file(work_dir.join(name)).await;
    }

    // 旧备份：只删与当前目标 exe 同名的那一个。
    //
    // 但启动状态停在 `staged` / `booting` 时，替换脚本可能仍在等待本进程退出，
    // `.bak` 是它唯一的回滚源；此时删掉会让失败路径无法还原旧版本。
    let replacement_in_flight = matches!(
        crate::core::startup::read(work_dir),
        Some(
            crate::core::startup::StartupState::Staged
                | crate::core::startup::StartupState::Booting
        )
    );
    if !replacement_in_flight {
        if let Some(name) = exe_path.file_name().and_then(|n| n.to_str()) {
            let _ = fs::remove_file(exe_dir.join(format!("{name}.bak"))).await;
            let _ = fs::remove_file(exe_dir.join(format!("{name}.old"))).await;
        }
    }

    // 注意：不要删除 exe 旁边的 tdx_update_dir 指针。
    // 上一轮更新可能刚刚启动新版本、还在等它写启动标志；
    // 删掉指针会让新版本把标志写到错误目录，导致旧脚本误判回滚。
    // 指针由 confirm_startup_success 在读取后删除。
}

#[cfg(test)]
mod tests {
    use super::rotate_logs;
    use crate::core::artifacts;

    /// 替换脚本仍在等待时（staged/booting），不得删除唯一回滚源 `.bak`。
    #[tokio::test]
    async fn keeps_backup_while_replacement_is_in_flight() {
        use crate::core::startup::{write, StartupState};

        let dir = tempfile::tempdir().unwrap();
        let exe = dir.path().join("app.exe");
        std::fs::write(&exe, b"new").unwrap();
        let backup = dir.path().join("app.exe.bak");
        std::fs::write(&backup, b"old").unwrap();

        for state in [StartupState::Staged, StartupState::Booting] {
            write(dir.path(), state).unwrap();
            super::cleanup_previous_updates_except(dir.path(), &exe, &[]).await;
            assert!(backup.is_file(), "{state:?} 期间不得删除备份");
        }

        write(dir.path(), StartupState::Committed).unwrap();
        super::cleanup_previous_updates_except(dir.path(), &exe, &[]).await;
        assert!(!backup.exists(), "替换完成后陈旧备份应被清理");
    }

    /// 超过上限的日志/事件轮转为 `.1`；小文件保持原位。
    #[test]
    fn rotates_oversized_logs() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(artifacts::LOG_FILE), b"small").unwrap();
        std::fs::write(
            dir.path().join(artifacts::EVENTS_FILE),
            vec![b'x'; 3 * 1024 * 1024],
        )
        .unwrap();

        rotate_logs(dir.path());

        assert!(
            dir.path().join(artifacts::LOG_FILE).is_file(),
            "未超限的日志不得轮转"
        );
        assert!(
            dir.path()
                .join(format!("{}.1", artifacts::EVENTS_FILE))
                .is_file(),
            "超限事件流必须保留上一代"
        );
        assert!(
            !dir.path().join(artifacts::EVENTS_FILE).exists(),
            "轮转后旧路径不得残留"
        );
    }
}
