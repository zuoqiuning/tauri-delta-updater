# 由 tauri-delta-updater 生成，请勿手工修改。
# 需要 UTF-8 BOM，否则 Windows PowerShell 5.1 会按 ANSI 代码页解析而报错。

$ErrorActionPreference = 'SilentlyContinue'

$target       = __TARGET__
$staged       = __STAGED__
$stagedHash   = __STAGED_HASH__
$flag         = __FLAG__
$logFile      = __LOG__
$rollbackFlag = __ROLLBACK__
$oldPid       = __OLD_PID__
$waitTimeout  = __WAIT_TIMEOUT__
$confirmTimeout = __CONFIRM_TIMEOUT__
$workDir      = __WORK_DIR__
$marker       = __MARKER__
$self         = __SELF__
$backup       = __BACKUP__
$eventsFile   = __EVENTS__
$stateFile    = __STATE__

function Write-Log([string]$message) {
    # 直接追加写入而不是 Add-Content：Add-Content 在文件刚创建/被其它进程占用时
    # 可能静默丢行，失败路径的日志恰恰是排查回滚的唯一线索。
    $line = '[' + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + '] ' + $message
    try { [System.IO.File]::AppendAllText($logFile, $line + [Environment]::NewLine) } catch { }
}

# 回滚原因落盘：宿主进程已经退出，进程内无法上报，
# 下一次启动的 diagnose() 读取这个文件告诉用户发生了什么。
function Write-Rollback([string]$reason) {
    Write-Log ('event=delta.rollback.triggered reason=' + $reason)
    try { [System.IO.File]::WriteAllText($rollbackFlag, $reason) } catch { }
    try { [System.IO.File]::WriteAllText($stateFile, 'rolled_back') } catch { }
    # 结构化事件：宿主下次启动时可以直接解析 JSONL，不必正则匹配日志。
    try {
        $evt = @{ event = 'delta.rollback.triggered'; reason = $reason; time = (Get-Date).ToString('o') } | ConvertTo-Json -Compress
        [System.IO.File]::AppendAllText($eventsFile, $evt + [Environment]::NewLine)
    } catch { }
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

    try {
        if ($isAdmin -and $isPe) {
            # explorer.exe 代理只接收"要打开的目标路径"这一个参数。
            # 不能再附加工作目录、命令行开关等参数，否则它会忽略目标而只打开窗口，
            # 表现为"更新完成但新版本没启动"。
            Start-Process -FilePath 'explorer.exe' -ArgumentList ('"' + $path + '"')
        } else {
            # 工作目录用可执行文件所在目录：用缓存目录会让依赖相对路径的应用
            # 在新版本首次启动时读到错误的位置。
            Start-Process -FilePath $path -WorkingDirectory (Split-Path -Parent $path)
        }
    } catch {
        # 无效 PE 会让 Start-Process 抛终止性错误；回滚路径里抛出会跳过 exit。
        Write-Log ('无法启动应用: ' + $path + ' - ' + $_.Exception.Message)
    }
}

# 所有失败路径的统一收口：先把残留备份还原，再把旧版本拉起来。
# 少了这一步，用户在"提权被拒/复制失败"后会看到应用直接消失。
function Restart-Old {
    # 已经有明确原因（超时/健康检查失败）时不要覆盖成笼统的 update_failed。
    $existing = ''
    try { $existing = ([System.IO.File]::ReadAllText($rollbackFlag)).Trim() } catch { }
    if (-not $existing) { Write-Rollback 'update_failed' }
    if (-not (Test-Path -LiteralPath $target) -and (Test-Path -LiteralPath $backup)) {
        Move-Item -LiteralPath $backup -Destination $target -Force -ErrorAction SilentlyContinue
    }
    if (Test-Path -LiteralPath $target) {
        # 旧进程可能仍在运行（用户拒绝 UAC 时脚本刚启动，旧版本还没退出）。
        # 再拉起会得到两个实例，因此先按 PID + 路径确认它确实已退出。
        $old = Get-Process -Id $oldPid -ErrorAction SilentlyContinue
        if ($old -and (-not $old.Path -or $old.Path -eq $target)) {
            Write-Log '旧进程仍在运行，跳过重新启动。'
        } else {
            Write-Log '更新中止，重新启动旧版本。'
            Start-App $target
        }
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
# 清掉上一轮的回滚原因；健康检查失败时宿主会重新写入，脚本立即回滚。
Remove-Item -LiteralPath $rollbackFlag -Force -ErrorAction SilentlyContinue
[System.IO.File]::WriteAllText($stateFile, 'staged')
Start-App $target

$deadline = (Get-Date).AddSeconds($confirmTimeout)
while ((Get-Date) -lt $deadline) {
    if (Test-Path -LiteralPath $flag) { break }
    if (Test-Path -LiteralPath $rollbackFlag) { break }
    Start-Sleep -Milliseconds 500
}

if (Test-Path -LiteralPath $flag) {
    Write-Log '新版本启动成功，清理备份。'
    Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
} else {
    $reason = 'startup_timeout'
    if (Test-Path -LiteralPath $rollbackFlag) {
        try { $reason = ([System.IO.File]::ReadAllText($rollbackFlag)).Trim() } catch { }
        if (-not $reason) { $reason = 'health_check_failed' }
    } else {
        $state = ''
        try { $state = ([System.IO.File]::ReadAllText($stateFile)).Trim() } catch { }
        if ($state -eq 'staged') {
            $reason = 'startup_not_started'
        } elseif ($state -eq 'booting') {
            # 已经执行到宿主代码但进程已退出：崩溃/被杀，而不是初始化慢。
            $running = Get-Process -ErrorAction SilentlyContinue |
                Where-Object { $_.Path -eq $target } |
                Select-Object -First 1
            if (-not $running) { $reason = 'startup_crashed' }
        }
    }
    Write-Log ('新版本未完成启动确认（' + $reason + '），回滚...')
    Write-Rollback $reason
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
