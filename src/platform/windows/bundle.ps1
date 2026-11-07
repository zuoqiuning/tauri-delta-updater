# 由 tauri-delta-updater 生成，请勿手工修改。
# 需要 UTF-8 BOM，否则 Windows PowerShell 5.1 会按 ANSI 代码页解析而报错。

$ErrorActionPreference = 'SilentlyContinue'

$target       = __TARGET__
$installDir   = Split-Path -Parent $target
$stagingDir   = __STAGING__
$backupDir    = __BACKUP__
$flag         = __FLAG__
$logFile      = __LOG__
$rollbackFlag = __ROLLBACK__
$oldPid       = __OLD_PID__
$waitTimeout  = __WAIT_TIMEOUT__
$confirmTimeout = __CONFIRM_TIMEOUT__
$workDir      = __WORK_DIR__
$marker       = __MARKER__
$self         = __SELF__
$journalFile  = __JOURNAL__
$stateFile    = __STATE__
$eventsFile   = __EVENTS__

$copies = @(
__COPIES__
)

$deletes = @(
__DELETES__
)

function Write-Log([string]$message) {
    # 直接追加写入而不是 Add-Content：Add-Content 在文件刚创建/被其它进程占用时
    # 可能静默丢行，失败路径的日志恰恰是排查回滚的唯一线索。
    $line = '[' + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + '] ' + $message
    try { [System.IO.File]::AppendAllText($logFile, $line + [Environment]::NewLine) } catch { }
}

# 正向阶段事件：主进程退出后仍能判断"卡在哪一步"。
function Write-Stage([string]$name) {
    try {
        $evt = @{ event = $name; time = (Get-Date).ToString('o') } | ConvertTo-Json -Compress
        [System.IO.File]::AppendAllText($eventsFile, $evt + [Environment]::NewLine)
    } catch { }
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

# 提权进程启动的子进程会继承管理员令牌。经 explorer 拉起可回到登录用户身份。
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
            Start-Process -FilePath 'explorer.exe' -ArgumentList ('"' + $path + '"')
        } else {
            # 工作目录用可执行文件所在目录：用缓存目录会让依赖相对路径的应用
            # 在新版本首次启动时读到错误的位置。
            Start-Process -FilePath $path -WorkingDirectory (Split-Path -Parent $path)
        }
    } catch {
        # Start-Process 对无效 PE 会抛终止性错误。在回滚路径中抛出时，
        # PowerShell 5.1 会跳过调用方的 exit，脚本继续走成功流程并清掉回滚原因。
        Write-Log ('无法启动应用: ' + $path + ' - ' + $_.Exception.Message)
    }
}

# 记录本次新增的文件：崩溃/回滚后要删除它们，旧版本没有这些路径。
function Record-Added([string]$relative) {
    try {
        [System.IO.File]::AppendAllText($journalFile, ('add ' + $relative) + [Environment]::NewLine)
    } catch { }
}

function Undo-Added {
    if (-not (Test-Path -LiteralPath $journalFile)) { return }
    Get-Content -LiteralPath $journalFile -ErrorAction SilentlyContinue | ForEach-Object {
        if ($_ -like 'add *') {
            $relative = $_.Substring(4)
            $path = [System.IO.Path]::Combine($installDir, $relative)
            Remove-Item -LiteralPath $path -Force -ErrorAction SilentlyContinue
        }
    }
}

# 把单个文件备份到备份目录（保留相对路径）。
function Backup-File([string]$relative) {
    $source = [System.IO.Path]::Combine($installDir, $relative)
    if (-not (Test-Path -LiteralPath $source)) { return }
    $dest = [System.IO.Path]::Combine($backupDir, $relative)
    $parent = [System.IO.Path]::GetDirectoryName($dest)
    [System.IO.Directory]::CreateDirectory($parent) | Out-Null
    [System.IO.File]::Copy($source, $dest, $true)
}

# 把备份目录里的旧文件全部还原回安装目录。
# 遍历备份目录而不是依赖脚本内嵌的清单：脚本被中断时也能恢复上次的现场。
function Restore-Backup {
    if (-not (Test-Path -LiteralPath $backupDir)) { return }
    # 用 .NET 枚举而不是 Get-ChildItem：返回路径严格以 $backupDir 为前缀。
    # CI 的 TEMP 可能是 8.3 短路径（RUNNER~1），Get-ChildItem 的 FullName
    # 可能变成长路径，用长度差算相对路径会算错，文件被复制到错误位置。
    $paths = [System.IO.Directory]::EnumerateFiles($backupDir, '*', [System.IO.SearchOption]::AllDirectories)
    foreach ($path in $paths) {
        $relative = $path.Substring($backupDir.Length + 1)
        $dest = [System.IO.Path]::Combine($installDir, $relative)
        $parent = [System.IO.Path]::GetDirectoryName($dest)
        try {
            [System.IO.Directory]::CreateDirectory($parent) | Out-Null
            [System.IO.File]::Copy($path, $dest, $true)
        } catch {
            Write-Log ('恢复备份失败: ' + $relative + ' - ' + $_.Exception.Message)
        }
    }
    try { [System.IO.Directory]::Delete($backupDir, $true) } catch { }
}

# 所有失败路径的统一收口：还原备份，再把旧版本拉起来。
function Restart-Old {
    # 已经有明确原因（超时/健康检查失败）时不要覆盖成笼统的 update_failed。
    $existing = ''
    try { $existing = ([System.IO.File]::ReadAllText($rollbackFlag)).Trim() } catch { }
    if (-not $existing) { Write-Rollback 'update_failed' }
    Undo-Added
    Restore-Backup
    Remove-Item -LiteralPath $journalFile -Force -ErrorAction SilentlyContinue
    if (Test-Path -LiteralPath $target) {
        # 旧进程可能仍在运行（例如用户拒绝 UAC）：再拉起会得到两个实例。
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

Write-Log '开始整包更新流程...'

# 崩溃恢复：只要备份目录或日志还在，上一轮替换就没有完成。
# 先删掉上次新增的文件，再还原旧内容，然后继续本轮替换。
if ((Test-Path -LiteralPath $backupDir) -or (Test-Path -LiteralPath $journalFile)) {
    Write-Log '检测到上次中断遗留的现场，正在恢复...'
    Undo-Added
    Restore-Backup
    Remove-Item -LiteralPath $journalFile -Force -ErrorAction SilentlyContinue
}

# 等待旧进程退出。按 PID 判定，并校验可执行文件路径：
# 仅凭 PID 会被"PID 回收"骗过，仅凭重命名是否失败也不可靠。
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

# 复制前逐文件校验暂存目录：任何一项被替换都会让产物不可信。
foreach ($item in $copies) {
    $stagedFile = [System.IO.Path]::Combine($stagingDir, $item.Path)
    if (-not (Test-FileHash $stagedFile $item.Hash)) {
        Write-Log ('暂存文件哈希校验失败，更新中止: ' + $item.Path)
        Restart-Old
        exit 1
    }
}

# 逐文件替换。任一步失败都整体回滚——留下"一半新一半旧"的安装目录
# 比直接失败更糟：新主程序可能依赖尚未替换的资源。
$failed = $false
try {
    foreach ($item in $copies) {
        $dest = [System.IO.Path]::Combine($installDir, $item.Path)
        $existed = Test-Path -LiteralPath $dest
        # 先记录再写盘：崩溃发生在两步之间时，修复仍知道要删除这个新文件。
        if (-not $existed) { Record-Added $item.Path }
        Backup-File $item.Path
        $source = [System.IO.Path]::Combine($stagingDir, $item.Path)
        $parent = [System.IO.Path]::GetDirectoryName($dest)
        [System.IO.Directory]::CreateDirectory($parent) | Out-Null
        [System.IO.File]::Copy($source, $dest, $true)
        if (-not (Test-FileHash $dest $item.Hash)) {
            throw ('替换后的文件哈希不符: ' + $item.Path)
        }
    }
    foreach ($relative in $deletes) {
        Backup-File $relative
        $dest = [System.IO.Path]::Combine($installDir, $relative)
        if (Test-Path -LiteralPath $dest) {
            [System.IO.File]::Delete($dest)
        }
    }
} catch {
    Write-Log ('替换失败，正在回滚: ' + $_.Exception.Message)
    # 不在 catch 里直接 Restart-Old + exit：catch 内再抛终止性错误时，
    # PowerShell 5.1 会跳过 exit 继续执行成功路径（清掉回滚原因并重启新版本）。
    $failed = $true
}
if ($failed) {
    Restart-Old
    exit 1
}

Write-Log '启动新版本，等待启动确认...'
# 告诉新版本应该去哪个目录写启动成功标志（自定义 work_dir 时尤其重要）
Set-Content -LiteralPath $marker -Value $workDir -Encoding UTF8
# 清掉上一轮的回滚原因；健康检查失败时宿主会重新写入，脚本立即回滚。
Remove-Item -LiteralPath $rollbackFlag -Force -ErrorAction SilentlyContinue
[System.IO.File]::WriteAllText($stateFile, 'staged')
Write-Stage 'delta.stage.replace_started'
Start-App $target

$deadline = (Get-Date).AddSeconds($confirmTimeout)
while ((Get-Date) -lt $deadline) {
    if (Test-Path -LiteralPath $flag) { break }
    if (Test-Path -LiteralPath $rollbackFlag) { break }
    Start-Sleep -Milliseconds 500
}

if (Test-Path -LiteralPath $flag) {
    Write-Log '新版本启动成功，清理备份。'
    Write-Stage 'delta.stage.startup_confirmed'
    try { [System.IO.Directory]::Delete($backupDir, $true) } catch { }
    Remove-Item -LiteralPath $journalFile -Force -ErrorAction SilentlyContinue
    try { [System.IO.Directory]::Delete($stagingDir, $true) } catch { }
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
    Start-Sleep -Milliseconds 500
    Restart-Old
}

# 脚本不删除自身：执行期间 PowerShell 仍可能按偏移读取文件，
# 自删会引发文件提前消失类的竞态。残留脚本由下一次更新的清理逻辑回收。
