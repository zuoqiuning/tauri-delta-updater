//! macOS `.app` 整包替换脚本。
//!
//! 逐文件改写 `.app` 会破坏代码签名——即使内容一致，签名校验依赖的
//! 目录结构与元数据也可能被改写。macOS 的正确做法是**以目录为单位**切换：
//!
//! 1. 等待旧进程退出；
//! 2. 校验暂存目录里的每个文件；
//! 3. 把旧 `.app` 整体改名为备份，再把暂存目录整体移动到位；
//! 4. 启动新版本，等待启动成功标志；超时或失败则整体换回备份。
//!
//! 这里只负责生成脚本字符串（纯逻辑，跨平台编译便于测试），
//! 实际执行只发生在 macOS。

use crate::core::artifacts;
use crate::core::bundle::{BundleFile, BundleFileKind};
use crate::error::{DeltaError, Result};
use crate::platform::shared::shell_quote;
use crate::platform::ScriptTimings;
use std::path::{Path, PathBuf};

/// 生成目录切换脚本。
///
/// `app_root` 是 `.app` 目录本身，`staging_dir` 是解压出的新包内容
/// （与 `.app` 根目录同级布局），`target_files` 用于脚本侧的最终校验。
pub fn generate(
    app_root: &Path,
    staging_dir: &Path,
    current_exe: &Path,
    target_files: &[BundleFile],
    work_dir: &Path,
    old_pid: u32,
    timings: ScriptTimings,
) -> Result<PathBuf> {
    generate_with_options(
        app_root,
        staging_dir,
        current_exe,
        target_files,
        work_dir,
        old_pid,
        timings,
        false,
    )
}

/// 内置更新流程使用：回滚后是否提示用户。
#[cfg_attr(not(target_os = "macos"), allow(dead_code))]
#[allow(clippy::too_many_arguments)]
pub(crate) fn generate_with_notice(
    app_root: &Path,
    staging_dir: &Path,
    current_exe: &Path,
    target_files: &[BundleFile],
    work_dir: &Path,
    old_pid: u32,
    timings: ScriptTimings,
    notice: bool,
) -> Result<PathBuf> {
    generate_with_options(
        app_root,
        staging_dir,
        current_exe,
        target_files,
        work_dir,
        old_pid,
        timings,
        notice,
    )
}

#[allow(clippy::too_many_arguments)]
fn generate_with_options(
    app_root: &Path,
    staging_dir: &Path,
    current_exe: &Path,
    target_files: &[BundleFile],
    work_dir: &Path,
    old_pid: u32,
    timings: ScriptTimings,
    notice: bool,
) -> Result<PathBuf> {
    let exe_relative = current_exe.strip_prefix(app_root).map_err(|_| {
        DeltaError::Config(format!(
            "可执行文件 {} 不在应用目录 {} 内",
            current_exe.display(),
            app_root.display()
        ))
    })?;
    let exe_relative = slash_path(exe_relative)?;
    if !target_files
        .iter()
        .any(|file| file.path == exe_relative && file.kind == BundleFileKind::File)
    {
        return Err(DeltaError::Config(format!(
            "整包清单里没有主程序 {exe_relative}，无法启动新版本"
        )));
    }

    let app_dir = app_root
        .parent()
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到 .app 所在目录".into()))?;

    let script_path = work_dir.join(artifacts::bundle_updater_script());
    let success_flag = work_dir.join(artifacts::SUCCESS_FLAG);
    let log_file = work_dir.join(artifacts::LOG_FILE);
    let rollback_flag = work_dir.join(artifacts::ROLLBACK_FLAG);
    let target_exe = app_root.join(Path::new(&exe_relative));
    let marker = app_root
        .join(Path::new(&exe_relative))
        .parent()
        .map(|dir| dir.join(artifacts::WORK_DIR_MARKER))
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到主程序父目录".into()))?;
    // 隐藏命名：不在 Finder 里显眼，也不会与用户自己创建的同名目录冲突。
    let backup = app_root
        .file_name()
        .map(|name| app_root.with_file_name(format!(".{}.tdx-backup", name.to_string_lossy())))
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到 .app 名称".into()))?;

    // 旧的成功标志必须清掉，否则脚本会误判新版本已启动
    let _ = std::fs::remove_file(&success_flag);

    let verify = target_files
        .iter()
        .map(|file| verify_line(staging_dir, file))
        .collect::<Vec<_>>()
        .join("");

    let template = include_str!("unix/dir_swap.sh");

    let script = template
        .replace("__LOG__", &shell_quote(&log_file.display().to_string()))
        .replace(
            "__FLAG__",
            &shell_quote(&success_flag.display().to_string()),
        )
        .replace(
            "__ROLLBACK__",
            &shell_quote(&rollback_flag.display().to_string()),
        )
        .replace(
            "__EVENTS__",
            &shell_quote(&work_dir.join(artifacts::EVENTS_FILE).display().to_string()),
        )
        .replace(
            "__STATE__",
            &shell_quote(
                &work_dir
                    .join(artifacts::UPDATE_STATE_FILE)
                    .display()
                    .to_string(),
            ),
        )
        .replace(
            "__APP_ROOT__",
            &shell_quote(&app_root.display().to_string()),
        )
        .replace("__APP_DIR__", &shell_quote(&app_dir.display().to_string()))
        .replace(
            "__STAGING__",
            &shell_quote(&staging_dir.display().to_string()),
        )
        .replace("__BACKUP__", &shell_quote(&backup.display().to_string()))
        .replace(
            "__TARGET_EXE__",
            &shell_quote(&target_exe.display().to_string()),
        )
        .replace("__MARKER__", &shell_quote(&marker.display().to_string()))
        .replace("__OLD_PID__", &old_pid.to_string())
        .replace("__WAIT_TIMEOUT__", &timings.wait_secs.to_string())
        .replace("__CONFIRM_TIMEOUT__", &timings.confirm_secs.to_string())
        .replace(
            "__WORK_DIR__",
            &shell_quote(&work_dir.display().to_string()),
        )
        .replace("__NOTICE_ENABLED__", if notice { "1" } else { "0" })
        .replace("__VERIFY__", &verify);
    crate::platform::shared::assert_rendered(&script)?;

    std::fs::write(&script_path, script.as_bytes()).map_err(DeltaError::Io)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o755));
    }
    Ok(script_path)
}

/// 生成 macOS 修复脚本：把上次中断的目录切换还原。
pub fn generate_repair(
    app_root: &Path,
    current_exe: &Path,
    work_dir: &Path,
    old_pid: u32,
    timings: ScriptTimings,
) -> Result<PathBuf> {
    let exe_relative = current_exe.strip_prefix(app_root).map_err(|_| {
        DeltaError::Config(format!(
            "可执行文件 {} 不在应用目录 {} 内",
            current_exe.display(),
            app_root.display()
        ))
    })?;
    let exe_relative = slash_path(exe_relative)?;
    let app_dir = app_root
        .parent()
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到 .app 所在目录".into()))?;
    let backup = app_root
        .file_name()
        .map(|name| app_root.with_file_name(format!(".{}.tdx-backup", name.to_string_lossy())))
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到 .app 名称".into()))?;
    let script_path = work_dir.join(artifacts::bundle_repair_script());
    let log_file = work_dir.join(artifacts::LOG_FILE);
    let rollback_flag = work_dir.join(artifacts::ROLLBACK_FLAG);
    let target_exe = app_root.join(Path::new(&exe_relative));

    let template = include_str!("unix/repair.sh");

    let script = template
        .replace("__LOG__", &shell_quote(&log_file.display().to_string()))
        .replace(
            "__ROLLBACK__",
            &shell_quote(&rollback_flag.display().to_string()),
        )
        .replace(
            "__EVENTS__",
            &shell_quote(&work_dir.join(artifacts::EVENTS_FILE).display().to_string()),
        )
        .replace(
            "__APP_ROOT__",
            &shell_quote(&app_root.display().to_string()),
        )
        .replace("__APP_DIR__", &shell_quote(&app_dir.display().to_string()))
        .replace("__BACKUP__", &shell_quote(&backup.display().to_string()))
        .replace(
            "__TARGET_EXE__",
            &shell_quote(&target_exe.display().to_string()),
        )
        .replace("__OLD_PID__", &old_pid.to_string())
        .replace("__WAIT_TIMEOUT__", &timings.wait_secs.to_string());
    crate::platform::shared::assert_rendered(&script)?;

    std::fs::write(&script_path, script.as_bytes()).map_err(DeltaError::Io)?;
    Ok(script_path)
}

/// 生成对单个暂存条目的校验片段。
fn verify_line(staging_dir: &Path, file: &BundleFile) -> String {
    let staged = staged_quoted(staging_dir, &file.path);
    match file.kind {
        BundleFileKind::File => {
            let missing = shell_quote(&format!("暂存文件缺失: {}", file.path));
            let mismatch = shell_quote(&format!("暂存文件哈希不符: {}", file.path));
            format!(
                "    if [ ! -f {staged} ]; then\n        log {missing}\n        _verify_failed=1\n    else\n        _sha=$(file_sha {staged})\n        if [ -n \"$_sha\" ] && [ \"$_sha\" != {hash} ]; then\n            log {mismatch}\n            _verify_failed=1\n        fi\n    fi\n",
                hash = shell_quote(&file.sha256.to_lowercase())
            )
        }
        BundleFileKind::Symlink => {
            let missing = shell_quote(&format!("暂存符号链接缺失: {}", file.path));
            format!(
                "    if [ ! -L {staged} ]; then\n        log {missing}\n        _verify_failed=1\n    fi\n"
            )
        }
    }
}

/// 暂存目录内的路径，用相邻的单引号字符串拼接，避免路径中的特殊字符被解释。
fn staged_quoted(staging_dir: &Path, relative: &str) -> String {
    format!(
        "{}{}",
        shell_quote(&staging_dir.display().to_string()),
        shell_quote(&format!("/{relative}"))
    )
}

/// 把路径转成 `/` 分隔。
fn slash_path(path: &Path) -> Result<String> {
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            std::path::Component::Normal(part) => parts.push(part.to_string_lossy().to_string()),
            _ => {
                return Err(DeltaError::BundleInvalid(format!(
                    "非法相对路径: {}",
                    path.display()
                )))
            }
        }
    }
    Ok(parts.join("/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::bundle::describe_dir;

    fn write(root: &Path, relative: &str, content: &[u8]) {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        std::fs::write(path, content).unwrap();
    }

    /// 脚本必须包含目录切换、崩溃恢复与回滚，并且路径按字面量写入。
    #[test]
    fn script_contains_swap_and_rollback() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("My App.app");
        let staging = dir.path().join("staging");
        write(&app, "Contents/MacOS/MyApp", b"old");
        write(&app, "Contents/Resources/a.txt", b"old resource");
        write(&staging, "Contents/MacOS/MyApp", b"new");
        write(&staging, "Contents/Resources/a.txt", b"new resource");

        let files = describe_dir(&staging).unwrap();
        let script = generate(
            &app,
            &staging,
            &app.join("Contents/MacOS/MyApp"),
            &files,
            dir.path(),
            1234,
            ScriptTimings::default(),
        )
        .unwrap();
        let content = std::fs::read_to_string(&script).unwrap();

        assert!(
            content.contains("Contents/MacOS/MyApp"),
            "主程序路径必须写入"
        );
        assert!(
            content.contains("My App.app"),
            "含空格的应用名必须按字面量写入"
        );
        assert!(
            content.contains("mv \"$STAGING\" \"$APP_ROOT\""),
            "必须以目录为单位切换，而不是逐文件覆盖（逐文件会破坏签名）"
        );
        assert!(
            content.contains("event=delta.rollback.triggered"),
            "回滚必须写结构化事件"
        );
        assert!(
            content.contains("暂存文件哈希不一致") || content.contains("暂存文件哈希不符"),
            "脚本必须自行复核暂存文件哈希"
        );
        assert!(
            content.contains("检测到上次中断遗留的备份"),
            "脚本必须包含崩溃恢复分支"
        );
        assert!(
            content.contains("codesign --verify --deep --strict"),
            "已签名的包必须在切换前验证签名"
        );
        assert!(!content.contains("__"), "脚本仍有未替换的占位符");
    }

    /// 修复脚本与外置模板的占位符必须全部替换。
    #[test]
    fn repair_script_has_no_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("My.app");
        write(&app, "Contents/MacOS/MyApp", b"old");

        let script = generate_repair(
            &app,
            &app.join("Contents/MacOS/MyApp"),
            dir.path(),
            1234,
            ScriptTimings::default(),
        )
        .unwrap();
        let content = std::fs::read_to_string(&script).unwrap();
        assert!(!content.contains("__"), "修复脚本仍有未替换的占位符");
    }
}
