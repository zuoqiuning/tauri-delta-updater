use crate::core::artifacts;
use crate::core::diff::VerifiedArtifact;
use crate::error::{DeltaError, Result};
use std::path::{Path, PathBuf};

use crate::platform::shared::{file_name, ps_literal};

/// 等待旧进程退出与等待新版本启动确认的最长秒数。
const WAIT_TIMEOUT_SECS: u32 = 60;
const CONFIRM_TIMEOUT_SECS: u32 = 60;

/// 生成 Windows 平台的可执行文件替换脚本（PowerShell）。
///
/// 脚本流程：
/// 1. 必要时请求管理员权限
/// 2. 等待旧进程真正退出（按 PID + 路径校验）
/// 3. 校验暂存文件哈希（封住"校验完成到脚本执行"之间的替换窗口）
/// 4. 把当前程序备份为 `.bak`
/// 5. 把新程序复制到当前程序名并启动（提权时降权到登录用户）
/// 6. 等待新程序写入启动成功标志，超时则回滚到旧版本并重新拉起
///
/// # 为什么用 PowerShell 而不是批处理
///
/// 批处理在这几个场景下都会出错，且都是实测复现的：
///
/// - cmd 按**字节**偏移读取批处理文件，文件里有 UTF-8 中文时偏移会错位，
///   表现为后续命令被截断并报"不是内部或外部命令"。
/// - `cmd /c "<路径>"` 无法处理含 `&` `(` `%` 的路径，四种写法实测只有一种可用。
/// - 批处理删除自身后 cmd 仍在读该文件，读取随即错乱。
/// - `cmd` 的 `set "VAR=值"` 与 `%VAR%` 展开规则组合起来，转义极易出错。
///
/// PowerShell 用 `-LiteralPath` 处理路径（不做通配符与转义解释），
/// 字符串用单引号字面量（只需把 `'` 转义成 `''`），没有任何这些坑。
///
/// # 脚本文件的编码
///
/// 必须是 **UTF-8 带 BOM**：Windows PowerShell 5.1 依赖 BOM 识别 UTF-8，
/// 无 BOM 时会按系统 ANSI 代码页解析，中文会变成乱码并导致语法错误。
pub fn generate_updater(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
) -> Result<PathBuf> {
    generate_updater_for_pid(current_exe, new_exe, work_dir, std::process::id())
}

/// 与 [`generate_updater`] 相同，但显式指定"要等待退出的进程 PID"。
///
/// 正常使用时 PID 就是当前进程；单独暴露出来是为了让脚本行为可被端到端测试
/// 覆盖——测试需要一个已经退出的 PID，而不是测试进程自己。
pub fn generate_updater_for_pid(
    current_exe: &Path,
    new_exe: &VerifiedArtifact,
    work_dir: &Path,
    old_pid: u32,
) -> Result<PathBuf> {
    let exe_dir = current_exe
        .parent()
        .ok_or_else(|| DeltaError::PatchApplyFailed("找不到父目录".into()))?;
    let target_exe = exe_dir.join(file_name(current_exe)?);

    let script_path = work_dir.join(artifacts::updater_script());
    let success_flag = work_dir.join(artifacts::SUCCESS_FLAG);
    let log_file = work_dir.join(artifacts::LOG_FILE);
    let backup = exe_dir.join(format!("{}.bak", file_name(current_exe)?));

    // 旧的成功标志必须清掉，否则脚本会误判新版本已启动
    let _ = std::fs::remove_file(&success_flag);

    let template = r#"# 由 tauri-delta-updater 生成，请勿手工修改。
# 需要 UTF-8 BOM，否则 Windows PowerShell 5.1 会按 ANSI 代码页解析而报错。

$ErrorActionPreference = 'SilentlyContinue'

$target       = __TARGET__
$staged       = __STAGED__
$stagedHash   = __STAGED_HASH__
$flag         = __FLAG__
$logFile      = __LOG__
$oldPid       = __OLD_PID__
$waitTimeout  = __WAIT_TIMEOUT__
$confirmTimeout = __CONFIRM_TIMEOUT__
$workDir      = __WORK_DIR__
$marker       = __MARKER__
$self         = __SELF__
$backup       = __BACKUP__

function Write-Log([string]$message) {
    Add-Content -LiteralPath $logFile -Encoding UTF8 -Value ('[' + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + '] ' + $message)
}

function Test-FileHash([string]$path, [string]$expected) {
    if (-not (Test-Path -LiteralPath $path)) { return $false }
    try {
        $sha = [System.Security.Cryptography.SHA256]::Create()
        $stream = [System.IO.File]::OpenRead($path)
        try {
            $actual = ([BitConverter]::ToString($sha.ComputeHash($stream)) -replace '-', '').ToLower()
        } finally {
            $stream.Dispose()
            $sha.Dispose()
        }
        return ($actual -eq $expected.ToLower())
    } catch {
        return $false
    }
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
$isAdmin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

# 提权进程启动的子进程会继承管理员令牌。经 explorer 拉起可回到登录用户身份，
# 避免"更新一次之后应用一直以管理员运行"。
# 只对真正的 PE 可执行文件走 explorer（它对非 exe 会弹 shell 错误框）。
function Start-App([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return }
    $isPe = $false
    try {
        $fs = [System.IO.File]::OpenRead($path)
        try {
            $buf = New-Object byte[] 2
            $fs.Read($buf, 0, 2) | Out-Null
            $isPe = ($buf[0] -eq 0x4D -and $buf[1] -eq 0x5A)
        } finally { $fs.Dispose() }
    } catch { $isPe = $false }

    if ($isAdmin -and $isPe) {
        # explorer.exe 代理只接收"要打开的目标路径"这一个参数。
        # 不能再附加工作目录、命令行开关等参数，否则它会忽略目标而只打开窗口，
        # 表现为"更新完成但新版本没启动"。
        Start-Process -FilePath 'explorer.exe' -ArgumentList ('"' + $path + '"')
    } else {
        Start-Process -FilePath $path -WorkingDirectory $workDir
    }
}

# 所有失败路径的统一收口：先把残留备份还原，再把旧版本拉起来。
# 少了这一步，用户在"提权被拒/复制失败"后会看到应用直接消失。
function Restart-Old {
    Write-Log 'event=delta.rollback.triggered reason=update_failed'
    if (-not (Test-Path -LiteralPath $target) -and (Test-Path -LiteralPath $backup)) {
        Move-Item -LiteralPath $backup -Destination $target -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $target) {
        Write-Log '更新中止，重新启动旧版本。'
        Start-App $target
    }
}

if (-not $isAdmin) {
    Write-Log '权限不足，正在请求管理员权限...'
    try {
        $elevated = Start-Process -FilePath 'powershell.exe' -Verb RunAs -WorkingDirectory $workDir -ErrorAction Stop -PassThru `
            -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"' + $self + '"')
        if ($elevated) { $elevated.WaitForExit() }
    } catch {
        Write-Log ('管理员授权被拒绝或失败，更新中止: ' + $_.Exception.Message)
        Restart-Old
    }
    exit 0
}

Write-Log '开始更新流程...'

# 崩溃恢复：上一轮脚本若在"exe → .bak"之后、替换完成之前被打断
# （断电、进程被杀），exe 会缺失而备份还在。先还原备份，
# 否则本轮更新会在一个残缺状态上继续，最终可能既没有新版本也没有旧版本。
if (-not (Test-Path -LiteralPath $target) -and (Test-Path -LiteralPath $backup)) {
    Write-Log '检测到上次中断遗留的备份，正在恢复...'
    Move-Item -LiteralPath $backup -Destination $target -Force -ErrorAction SilentlyContinue
}

# 等待旧进程退出。按 PID 判定，并校验可执行文件路径：
# 仅凭 PID 会被"PID 回收"骗过，仅凭"重命名是否失败"也不可靠——
# Windows 允许重命名正在运行的可执行文件。
$deadline = (Get-Date).AddSeconds($waitTimeout)
while ((Get-Date) -lt $deadline) {
    $p = Get-Process -Id $oldPid -ErrorAction SilentlyContinue
    if (-not $p) { break }
    if ($p.Path -and $p.Path -ne $target) { break }
    Start-Sleep -Milliseconds 500
}
$still = Get-Process -Id $oldPid -ErrorAction SilentlyContinue
if ($still -and (-not $still.Path -or $still.Path -eq $target)) {
    Write-Log '等待旧进程退出超时，更新中止。'
    exit 1
}

# 复制前再校验一次暂存文件：apply_patch 返回到这里之间文件可能被替换
if (-not (Test-FileHash $staged $stagedHash)) {
    Write-Log '暂存文件哈希校验失败，更新中止。'
    Restart-Old
    exit 1
}

# 备份旧版本
Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
Move-Item -LiteralPath $target -Destination $backup -Force -ErrorAction SilentlyContinue
if (-not (Test-Path -LiteralPath $backup)) {
    Write-Log '备份旧版本失败，更新中止。'
    Restart-Old
    exit 1
}

# 用新程序替换。Copy-Item 支持跨卷。
Copy-Item -LiteralPath $staged -Destination $target -Force
if (-not (Test-Path -LiteralPath $target)) {
    Write-Log '替换失败，正在回滚。'
    Restart-Old
    exit 1
}
if (-not (Test-FileHash $target $stagedHash)) {
    Write-Log '替换后的文件哈希不符，正在回滚。'
    Remove-Item -LiteralPath $target -Force -ErrorAction SilentlyContinue
    Restart-Old
    exit 1
}
Remove-Item -LiteralPath $staged -Force -ErrorAction SilentlyContinue

Write-Log '启动新版本，等待启动确认...'
# 告诉新版本应该去哪个目录写启动成功标志（自定义 work_dir 时尤其重要）
Set-Content -LiteralPath $marker -Value $workDir -Encoding UTF8
Start-App $target

$deadline = (Get-Date).AddSeconds($confirmTimeout)
while ((Get-Date) -lt $deadline) {
    if (Test-Path -LiteralPath $flag) { break }
    Start-Sleep -Milliseconds 500
}

if (Test-Path -LiteralPath $flag) {
    Write-Log '新版本启动成功，清理备份。'
    Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
} else {
    Write-Log '新版本启动超时，回滚...'
    Write-Log 'event=delta.rollback.triggered reason=startup_timeout'
    # 按可执行文件完整路径结束进程，而不是按进程名：
    # 按名字结束会连同用户手动打开的同名实例一起杀掉。
    Get-Process -ErrorAction SilentlyContinue |
        Where-Object { $_.Path -eq $target } |
        Stop-Process -Force -ErrorAction SilentlyContinue
    Remove-Item -LiteralPath $target -Force -ErrorAction SilentlyContinue
    Restart-Old
}

# 脚本不删除自身：脚本执行期间 PowerShell 可能仍在按偏移读取文件，
# 自删会引发文件提前消失类的竞态。残留脚本由下一次更新的清理逻辑回收。
"#;

    let script = template
        .replace("__TARGET__", &ps_literal(&target_exe.display().to_string()))
        .replace(
            "__STAGED__",
            &ps_literal(&new_exe.path().display().to_string()),
        )
        .replace("__STAGED_HASH__", &ps_literal(new_exe.sha256()))
        .replace("__FLAG__", &ps_literal(&success_flag.display().to_string()))
        .replace("__LOG__", &ps_literal(&log_file.display().to_string()))
        .replace("__OLD_PID__", &old_pid.to_string())
        .replace("__WAIT_TIMEOUT__", &WAIT_TIMEOUT_SECS.to_string())
        .replace("__CONFIRM_TIMEOUT__", &CONFIRM_TIMEOUT_SECS.to_string())
        .replace("__WORK_DIR__", &ps_literal(&work_dir.display().to_string()))
        .replace(
            "__MARKER__",
            &ps_literal(
                &exe_dir
                    .join(artifacts::WORK_DIR_MARKER)
                    .display()
                    .to_string(),
            ),
        )
        .replace("__SELF__", &ps_literal(&script_path.display().to_string()))
        .replace("__BACKUP__", &ps_literal(&backup.display().to_string()));

    // UTF-8 BOM：Windows PowerShell 5.1 靠它识别 UTF-8
    let mut bytes = vec![0xEF, 0xBB, 0xBF];
    bytes.extend_from_slice(script.as_bytes());
    std::fs::write(&script_path, bytes).map_err(DeltaError::Io)?;

    Ok(script_path)
}

/// 启动替换脚本。
///
/// 用 `powershell -File <脚本名>` 并把工作目录设为脚本所在目录：
/// 这样路径里即使有空格或特殊字符也不会被命令行解析出问题。
pub fn launch_updater(script_path: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;

        let work_dir = script_path
            .parent()
            .ok_or_else(|| DeltaError::PatchApplyFailed("找不到脚本所在目录".into()))?;
        let script_name = file_name(script_path)?;

        let mut cmd = std::process::Command::new("powershell.exe");
        cmd.arg("-NoProfile")
            .arg("-ExecutionPolicy")
            .arg("Bypass")
            .arg("-File")
            .arg(&script_name)
            .current_dir(work_dir);
        // 不弹出黑色控制台窗口
        cmd.creation_flags(CREATE_NO_WINDOW);
        cmd.spawn().map_err(DeltaError::Io)?;
    }
    Ok(())
}

/// 标记新版本启动成功。
pub use crate::platform::shared::confirm_startup_success;
