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
$inProgress   = __IN_PROGRESS__
$noticeEnabled = __NOTICE_ENABLED__

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

# 回滚后给用户的最小提示：应用已经在旧版本上重新启动。
# 提示在独立进程里显示，脚本本身不阻塞；无交互桌面（服务/计划任务）直接跳过。
function Show-RollbackNotice {
    if (-not $noticeEnabled) { return }
    try {
        if (-not [Environment]::UserInteractive) { return }
        $lang = [System.Globalization.CultureInfo]::CurrentUICulture.Name
        if ($lang -like 'zh*') {
            $title = '更新失败'
            $text = '更新未完成，已恢复到上一版本。'
        } else {
            $title = 'Update failed'
            $text = 'The update did not complete. The previous version has been restored.'
        }
        # 用 -EncodedCommand 传递文本，避免引号/空格在 Start-Process 参数里被二次解析。
        $code = "Add-Type -AssemblyName PresentationFramework; `$null = [System.Windows.MessageBox]::Show('" + ($text -replace "'", "''") + "', '" + ($title -replace "'", "''") + "', 'OK', 'Warning')"
        $encoded = [Convert]::ToBase64String([Text.Encoding]::Unicode.GetBytes($code))
        Start-Process -FilePath 'powershell.exe' -ArgumentList '-NoProfile', '-WindowStyle', 'Hidden', '-EncodedCommand', $encoded -ErrorAction SilentlyContinue | Out-Null
    } catch { }
}

# 回滚原因落盘：宿主进程已经退出，进程内无法上报，
# 下一次启动的 diagnose() 读取这个文件告诉用户发生了什么。
function Write-Rollback([string]$reason) {
    Write-Log ('event=delta.rollback.triggered reason=' + $reason)
    # 临时文件 + 原子替换：进程在写一半时被杀死也不会留下空原因。
    try {
        $tmp = $rollbackFlag + '.tmp'
        [System.IO.File]::WriteAllText($tmp, $reason)
        Move-Item -LiteralPath $tmp -Destination $rollbackFlag -Force
    } catch { }
    try {
        $tmp = $stateFile + '.tmp'
        [System.IO.File]::WriteAllText($tmp, 'rolled_back')
        Move-Item -LiteralPath $tmp -Destination $stateFile -Force
    } catch { }
    # 结构化事件：宿主下次启动时可以直接解析 JSONL，不必正则匹配日志。
    try {
        $evt = @{ event = 'delta.rollback.triggered'; reason = $reason; time = (Get-Date).ToString('o') } | ConvertTo-Json -Compress
        [System.IO.File]::AppendAllText($eventsFile, $evt + [Environment]::NewLine)
    } catch { }
}

# 记录“已有更具体原因、本次不覆盖”的事实，便于事后审计两次写入。
function Write-RollbackPreserved([string]$reason) {
    try {
        $evt = @{ event = 'delta.rollback.preserved'; reason = $reason; time = (Get-Date).ToString('o') } | ConvertTo-Json -Compress
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
# explorer 代理可能静默不启动（真实事故），因此要确认进程真的出现，
# 没有就退回直接启动并记录，否则只会在超时后得到一个没有原因的 startup_not_started。
function Test-PeFile([string]$path) {
    if (-not (Test-Path -LiteralPath $path)) { return $false }
    try {
        $fs = [System.IO.File]::OpenRead($path)
        try {
            $buf = New-Object byte[] 2
            $fs.Read($buf, 0, 2) | Out-Null
            return ($buf[0] -eq 0x4D -and $buf[1] -eq 0x5A)
        } finally { $fs.Dispose() }
    } catch { return $false }
}

function Wait-AppProcess([string]$path) {
    for ($i = 0; $i -lt 20; $i++) {
        $proc = Get-Process -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $path } | Select-Object -First 1
        if ($proc) { return $proc }
        Start-Sleep -Milliseconds 500
    }
    return $null
}

function Start-App([string]$path) {
    $script:launchProcess = $null
    if (-not (Test-Path -LiteralPath $path)) {
        Write-Log ('启动目标不存在: ' + $path)
        return 'via=none seen=false'
    }
    $via = 'direct'
    if ($isAdmin -and (Test-PeFile $path)) { $via = 'explorer' }
    $proc = $null
    try {
        if ($via -eq 'explorer') {
            Write-Log ('请求通过 explorer 启动: ' + $path)
            Start-Process -FilePath 'explorer.exe' -ArgumentList ('"' + $path + '"') -ErrorAction Stop
            $proc = Wait-AppProcess $path
            if (-not $proc) {
                $state = ''
                try { $state = ([System.IO.File]::ReadAllText($stateFile)).Trim() } catch { }
                if ((Test-Path -LiteralPath $flag) -or ($state -eq 'booting')) {
                    Write-Log '未找到进程，但新版本已写入启动状态，跳过兜底启动。'
                    $via = 'explorer-late'
                } else {
                    Write-Log 'explorer 代理未启动应用，改为直接启动。'
                    $via = 'direct'
                }
            }
        }
        if (-not $proc -and ($via -eq 'direct')) {
            Write-Log ('直接启动应用: ' + $path)
            # 工作目录用可执行文件所在目录：用缓存目录会让依赖相对路径的应用
            # 在新版本首次启动时读到错误的位置。
            $proc = Start-Process -FilePath $path -WorkingDirectory (Split-Path -Parent $path) -PassThru -ErrorAction Stop
        }
    } catch {
        # Start-Process 对无效 PE 会抛终止性错误。在回滚路径中抛出时，
        # PowerShell 5.1 会跳过调用方的 exit，脚本继续走成功流程并清掉回滚原因。
        Write-Log ('启动应用失败（' + $via + '）: ' + $path + ' - ' + $_.Exception.Message)
        return ('via=' + $via + ' seen=false error=' + $_.Exception.Message)
    }
    $script:launchProcess = $proc
    if ($proc) {
        Write-Log ('应用进程已启动: PID=' + $proc.Id)
        return ('via=' + $via + ' pid=' + $proc.Id + ' seen=true')
    }
    Write-Log '未能确认应用进程已启动。'
    return ('via=' + $via + ' pid=none seen=false')
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
    # 回滚完成：上一次替换已经结束（以失败告终），“进行中”标记不能留到下次启动。
    Remove-Item -LiteralPath $inProgress -Force -ErrorAction SilentlyContinue
    # 已经有明确原因（超时/健康检查失败）时不要覆盖成笼统的 update_failed。
    $existing = ''
    try { $existing = ([System.IO.File]::ReadAllText($rollbackFlag)).Trim() } catch { }
    if ($existing) {
        Write-RollbackPreserved $existing
    } else {
        Write-Rollback 'update_failed'
    }
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
            $null = Start-App $target
        }
    }
    Show-RollbackNotice
}

# 只有安装目录真的不可写时才提权：per-user 安装不需要 UAC。
function Test-DirWritable([string]$dir) {
    if (-not (Test-Path -LiteralPath $dir)) { return $false }
    $probe = Join-Path $dir ('.tdx_write_probe_' + [Guid]::NewGuid().ToString('N'))
    try {
        [System.IO.File]::WriteAllText($probe, 'x')
        Remove-Item -LiteralPath $probe -Force -ErrorAction SilentlyContinue
        return $true
    } catch {
        Remove-Item -LiteralPath $probe -Force -ErrorAction SilentlyContinue
        return $false
    }
}

if (-not $isAdmin -and -not (Test-DirWritable (Split-Path -Parent $target))) {
    Write-Log '安装目录不可写，正在请求管理员权限...'
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
if (-not $isAdmin) {
    Write-Log '安装目录可写，无需管理员权限。'
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
$launchNote = Start-App $target

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
    Remove-Item -LiteralPath $inProgress -Force -ErrorAction SilentlyContinue
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
    Write-Log ('新版本未完成启动确认（' + $reason + '），回滚... 启动记录: ' + $launchNote)
    if ($script:launchProcess) {
        try {
            if ($script:launchProcess.HasExited) {
                Write-Log ('新版本进程已退出: PID=' + $script:launchProcess.Id + ' exit=' + $script:launchProcess.ExitCode)
            } else {
                Write-Log ('新版本进程仍在运行但未确认: PID=' + $script:launchProcess.Id)
            }
        } catch { }
    }
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
