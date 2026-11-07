//! 把本次主程序归档为下一次发布的补丁基线。

use super::files::extract_version;
use crate::{detect_arch, normalize_version};
use std::path::Path;

/// 把本次主程序归档为下一次发布的补丁基线。
///
/// 文件名必须让发布端下次能自动识别版本与架构。发布端产物通常已经满足
/// （`App_0.1.1_x64.exe`），此时原样复制；只有名字里缺版本/架构时
/// 才补成 `App_0.1.1_x86_64.exe`，避免出现版本与架构被重复拼接。
pub(crate) fn archive_baseline(
    dir: &Path,
    app_binary: &Path,
    version: &str,
    platform: &str,
) -> Result<(), String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("创建归档目录失败: {e}"))?;

    let original = app_binary
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .ok_or("主程序文件名无效")?;
    let arch = platform
        .split_once('-')
        .map(|(_, a)| a)
        .unwrap_or("unknown");

    let name = if extract_version(&original)
        .map(|v| normalize_version(&v) == version)
        .unwrap_or(false)
        && detect_arch(&original) == arch
    {
        original
    } else {
        let stem = app_binary
            .file_stem()
            .map(|s| s.to_string_lossy().to_string())
            .ok_or("主程序文件名无效")?;
        let ext = app_binary
            .extension()
            .map(|e| format!(".{}", e.to_string_lossy()))
            .unwrap_or_default();
        format!("{stem}_{version}_{arch}{ext}")
    };

    let target = dir.join(name);
    if target.exists() {
        println!("[INFO] 基线已存在，跳过归档: {}", target.display());
        return Ok(());
    }
    std::fs::copy(app_binary, &target).map_err(|e| format!("归档基线失败: {e}"))?;
    println!("[OK] 已归档补丁基线: {}", target.display());
    Ok(())
}
