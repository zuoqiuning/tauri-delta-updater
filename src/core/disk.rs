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
///
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
    let exe_dir = exe_path.parent().unwrap_or(Path::new("."));

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

    let _ = fs::remove_file(work_dir.join(artifacts::staged_file())).await;
    let _ = fs::remove_file(work_dir.join(artifacts::updater_script())).await;
    // 全量安装的临时解包目录（替换脚本已在执行时不会走到清理，安全）
    let _ = fs::remove_dir_all(work_dir.join(artifacts::FULL_STAGING_DIR)).await;

    // 整包产物：暂存目录与脚本。
    //
    // 不删除 `BUNDLE_BACKUP_DIR`：它是替换脚本在"已改了一半文件"时唯一能整体
    // 回滚的旧内容。上一轮脚本崩溃后，下一次脚本启动会先还原它；
    // 客户端清理把它删掉，安装目录就会永久停在半新半旧的状态。
    let _ = fs::remove_file(work_dir.join(artifacts::bundle_updater_script())).await;
    let _ = fs::remove_file(work_dir.join(artifacts::bundle_repair_script())).await;
    // 暂存路径可能被上次崩溃残留成一个普通文件（极端情况），
    // 只删目录会静默失败，导致本轮解压无法创建目标目录。
    let _ = fs::remove_file(work_dir.join(artifacts::bundle_staging_dir())).await;
    let _ = fs::remove_dir_all(work_dir.join(artifacts::bundle_staging_dir())).await;

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

    // 旧备份：只删与当前目标 exe 同名的那一个
    if let Some(name) = exe_path.file_name().and_then(|n| n.to_str()) {
        let _ = fs::remove_file(exe_dir.join(format!("{name}.bak"))).await;
        let _ = fs::remove_file(exe_dir.join(format!("{name}.old"))).await;
    }

    // 注意：不要删除 exe 旁边的 tdx_update_dir 指针。
    // 上一轮更新可能刚刚启动新版本、还在等它写启动标志；
    // 删掉指针会让新版本把标志写到错误目录，导致旧脚本误判回滚。
    // 指针由 confirm_startup_success 在读取后删除。
}
