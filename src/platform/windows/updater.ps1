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
$inProgress   = __IN_PROGRESS__
$noticeEnabled = __NOTICE_ENABLED__

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
    # 先写临时文件再原子替换：进程在写一半时被杀死，也不会留下空原因，
    # 下一次启动的 diagnose() 不会把已知回滚原因当成“未知”。
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

# 只读文件头判断 PE：explorer 代理对非 exe 会弹 shell 错误框。
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

# 等待应用进程出现（按可执行文件完整路径匹配），返回 Process 或 $null。
function Wait-AppProcess([string]$path) {
    for ($i = 0; $i -lt 20; $i++) {
        $proc = Get-Process -ErrorAction SilentlyContinue |
            Where-Object { $_.Path -eq $path } | Select-Object -First 1
        if ($proc) { return $proc }
        Start-Sleep -Milliseconds 500
    }
    return $null
}

# 提权进程启动的子进程会继承管理员令牌。经 explorer 拉起可回到登录用户身份，
# 避免"更新一次之后应用一直以管理员运行"。
# 但 explorer 代理在部分提权/无桌面会话里会**静默不启动**（真实事故：
# 60 秒后只看到 startup_not_started，日志里没有任何原因），因此请求发出后
# 必须确认进程真的出现，没有就退回直接启动并记录。
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
                # 新版本可能已经开始写启动状态，只是进程枚举暂时没命中：
                # 这种情况再启动一次会得到两个实例。
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
        # 无效 PE 会让 Start-Process 抛终止性错误；回滚路径里抛出会跳过 exit。
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

# 所有失败路径的统一收口：先把残留备份还原，再把旧版本拉起来。
# 少了这一步，用户在"提权被拒/复制失败"后会看到应用直接消失。
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
            $null = Start-App $target
        }
    }
    Show-RollbackNotice
}

# 只有安装目录真的不可写时才提权：per-user 安装（%LocalAppData%）不需要 UAC，
# 无条件 RunAs 会让每次更新都弹管理员授权。与 Unix 的 [ ! -w "$TARGET_DIR" ] 对齐。
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
    Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue
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
    # 进程已退出时尽量记下退出码：启动方式、是否见到进程、退出码是
    # startup_not_started 唯一可复现的证据，事后不必再猜。
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
    Remove-Item -LiteralPath $target -Force -ErrorAction SilentlyContinue
    Restart-Old
}

# 脚本不删除自身：脚本执行期间 PowerShell 可能仍在按偏移读取文件，
# 自删会引发文件提前消失类的竞态。残留脚本由下一次更新的清理逻辑回收。
