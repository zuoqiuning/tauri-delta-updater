# 由 tauri-delta-updater 生成，请勿手工修改。
# 需要 UTF-8 BOM，否则 Windows PowerShell 5.1 会按 ANSI 代码页解析而报错。

$ErrorActionPreference = 'SilentlyContinue'

$target       = __TARGET__
$installDir   = Split-Path -Parent $target
$backupDir    = __BACKUP__
$journalFile  = __JOURNAL__
$logFile      = __LOG__
$rollbackFlag = __ROLLBACK__
$oldPid       = __OLD_PID__
$waitTimeout  = __WAIT_TIMEOUT__
$workDir      = __WORK_DIR__
$self         = __SELF__
$eventsFile   = __EVENTS__

function Write-Log([string]$message) {
    $line = '[' + (Get-Date -Format 'yyyy-MM-dd HH:mm:ss') + '] ' + $message
    try { [System.IO.File]::AppendAllText($logFile, $line + [Environment]::NewLine) } catch { }
}

function Write-Rollback([string]$reason) {
    Write-Log ('event=delta.rollback.triggered reason=' + $reason)
    try { [System.IO.File]::WriteAllText($rollbackFlag, $reason) } catch { }
    # 结构化事件：宿主下次启动时可以直接解析 JSONL，不必正则匹配日志。
    try {
        $evt = @{ event = 'delta.rollback.triggered'; reason = $reason; time = (Get-Date).ToString('o') } | ConvertTo-Json -Compress
        [System.IO.File]::AppendAllText($eventsFile, $evt + [Environment]::NewLine)
    } catch { }
}

$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = New-Object Security.Principal.WindowsPrincipal($identity)
$isAdmin = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)

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
            Start-Process -FilePath $path -WorkingDirectory (Split-Path -Parent $path)
        }
    } catch {
        # 无效 PE 会让 Start-Process 抛终止性错误；修复路径里抛出会跳过后续收尾。
        Write-Log ('无法启动应用: ' + $path + ' - ' + $_.Exception.Message)
    }
}

# 删除本次更新新增的文件：旧版本没有它们，留着可能被误加载。
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

# 把备份目录里的旧文件全部还原回安装目录。
function Restore-Backup {
    if (-not (Test-Path -LiteralPath $backupDir)) { return }
    # 用 .NET 枚举而不是 Get-ChildItem：返回路径严格以 $backupDir 为前缀，
    # 避免 8.3 短路径（如 RUNNER~1）与长路径混用时相对路径算错。
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
}

if (-not $isAdmin) {
    Write-Log '权限不足，正在请求管理员权限...'
    try {
        $elevated = Start-Process -FilePath 'powershell.exe' -Verb RunAs -WorkingDirectory $workDir -ErrorAction Stop -PassThru `
            -ArgumentList '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', ('"' + $self + '"')
        if ($elevated) { $elevated.WaitForExit() }
    } catch {
        Write-Log ('管理员授权被拒绝或失败，修复中止: ' + $_.Exception.Message)
    }
    exit 0
}

Write-Log '开始修复上次未完成的更新...'

$deadline = (Get-Date).AddSeconds($waitTimeout)
while ((Get-Date) -lt $deadline) {
    $p = Get-Process -Id $oldPid -ErrorAction SilentlyContinue
    if (-not $p) { break }
    if ($p.Path -and $p.Path -ne $target) { break }
    Start-Sleep -Milliseconds 500
}
$still = Get-Process -Id $oldPid -ErrorAction SilentlyContinue
if ($still -and (-not $still.Path -or $still.Path -eq $target)) {
    Write-Log '等待旧进程退出超时，修复中止。'
    exit 1
}

if ((-not (Test-Path -LiteralPath $backupDir)) -and (-not (Test-Path -LiteralPath $journalFile))) {
    Write-Log '没有需要修复的内容。'
    exit 0
}

Undo-Added
Restore-Backup
Remove-Item -LiteralPath $journalFile -Force -ErrorAction SilentlyContinue
Remove-Item -LiteralPath $backupDir -Recurse -Force -ErrorAction SilentlyContinue
Write-Rollback 'repaired_partial_update'
Write-Log '修复完成，正在重新启动旧版本。'
Start-App $target
