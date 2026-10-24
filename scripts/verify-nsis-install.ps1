<#
.SYNOPSIS
    M8 真机 NSIS 安装包验收器（在干净 Windows 机器 / VM 内运行）。

.DESCRIPTION
    覆盖「安装 → 首次启动握手 → 卸载 → 残留检查」全链路，输出 JSON + Markdown 报告。
    本脚本只做验收，不改系统策略、不删用户数据目录（%APPDATA%\AI IDE 按
    electron-builder.yml 的 deleteAppDataOnUninstall:false 预期保留）。

.PARAMETER InstallerPath
    NSIS 安装包绝对路径，例如 \\vmware-host\Shared Folders\AI-IDE-0.1.0-win-x64.exe

.PARAMETER PreviousInstallerPath
    覆盖升级场景的旧版安装包。给了就先装旧版、验数据保留，再装新版。

.PARAMETER ReportDir
    报告目录，默认桌面 ai-ide-m8-acceptance。

.PARAMETER SkipLaunch
    跳过首次启动握手（只验安装/卸载）。

.PARAMETER SkipUninstall
    保留安装结果，不跑卸载（想手工点开看看时用）。

.EXAMPLE
    powershell -ExecutionPolicy Bypass -File .\verify-nsis-install.ps1 -InstallerPath "$env:USERPROFILE\Downloads\AI-IDE-0.1.0-win-x64.exe"
#>
[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$InstallerPath,
    [string]$PreviousInstallerPath,
    [string]$ReportDir = (Join-Path ([Environment]::GetFolderPath('Desktop')) 'ai-ide-m8-acceptance'),
    [switch]$SkipLaunch,
    [switch]$SkipUninstall
)

$ErrorActionPreference = 'Continue'
$ProgressPreference = 'SilentlyContinue'

$AppName       = 'AI IDE'
$AppProcess    = 'AI IDE'
$CoreProcess   = 'ai-ide-core'
$AppId         = 'com.aiide.desktop'
$Results       = New-Object System.Collections.ArrayList
$Stamp         = Get-Date -Format 'yyyyMMdd-HHmmss'

function Add-Result {
    param(
        [string]$Phase,
        [string]$Item,
        [string]$Status,   # PASS / FAIL / WARN / SKIP / INFO
        [string]$Evidence = ''
    )
    $null = $Results.Add([pscustomobject]@{
            Phase    = $Phase
            Item     = $Item
            Status   = $Status
            Evidence = $Evidence
        })
    $color = switch ($Status) { 'PASS' { 'Green' } 'FAIL' { 'Red' } 'WARN' { 'Yellow' } default { 'Gray' } }
    Write-Host ("[{0,-4}] {1} — {2}{3}" -f $Status, $Item, $Evidence, '') -ForegroundColor $color
}

function Get-FileVersionString([string]$Path) {
    if (-not (Test-Path -LiteralPath $Path)) { return $null }
    $vi = (Get-Item -LiteralPath $Path).VersionInfo
    if ($vi.ProductVersion) { return $vi.ProductVersion }
    return $vi.FileVersion
}

function Get-UninstallEntries {
    $roots = @(
        'HKCU:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKLM:\Software\Microsoft\Windows\CurrentVersion\Uninstall',
        'HKLM:\Software\WOW6432Node\Microsoft\Windows\CurrentVersion\Uninstall'
    )
    $found = @()
    foreach ($root in $roots) {
        if (-not (Test-Path $root)) { continue }
        Get-ChildItem $root -ErrorAction SilentlyContinue | ForEach-Object {
            $p = $null
            try { $p = Get-ItemProperty -LiteralPath $_.PSPath -ErrorAction Stop } catch { return }
            if ($p.DisplayName -eq $AppName) {
                $found += [pscustomobject]@{
                    Root            = $root
                    Key             = $_.PSChildName
                    DisplayName     = $p.DisplayName
                    DisplayVersion  = $p.DisplayVersion
                    InstallLocation = $p.InstallLocation
                    UninstallString = $p.UninstallString
                }
            }
        }
    }
    return $found
}

function Get-InstallDir([object[]]$Entries) {
    foreach ($e in $Entries) {
        if ($e.InstallLocation -and (Test-Path -LiteralPath $e.InstallLocation)) { return $e.InstallLocation }
    }
    $fallback = Join-Path $env:LOCALAPPDATA 'Programs\AI IDE'
    if (Test-Path -LiteralPath $fallback) { return $fallback }
    return $null
}

function Get-ShortcutState {
    $startMenu = Join-Path $env:APPDATA 'Microsoft\Windows\Start Menu\Programs\AI IDE.lnk'
    $desktop   = Join-Path ([Environment]::GetFolderPath('Desktop')) 'AI IDE.lnk'
    $publicSM  = Join-Path $env:ProgramData 'Microsoft\Windows\Start Menu\Programs\AI IDE.lnk'
    [pscustomobject]@{
        StartMenu       = (Test-Path -LiteralPath $startMenu)
        StartMenuPublic = (Test-Path -LiteralPath $publicSM)
        Desktop         = (Test-Path -LiteralPath $desktop)
    }
}

function Wait-ProcessGone([string]$Name, [int]$TimeoutSec = 30) {
    $deadline = (Get-Date).AddSeconds($TimeoutSec)
    do {
        if (-not (Get-Process -Name $Name -ErrorAction SilentlyContinue)) { return $true }
        Start-Sleep -Milliseconds 500
    } while ((Get-Date) -lt $deadline)
    return $false
}

function Invoke-SilentInstaller([string]$Path, [string]$Phase) {
    $sw = [Diagnostics.Stopwatch]::StartNew()
    $p = Start-Process -FilePath $Path -ArgumentList '/S' -PassThru -Wait
    $sw.Stop()
    Add-Result $Phase '静默安装（/S）完成' $(if ($p.ExitCode -eq 0) { 'PASS' } else { 'FAIL' }) `
        ("exit={0} 耗时={1:N1}s" -f $p.ExitCode, $sw.Elapsed.TotalSeconds)
    return $p.ExitCode
}

# ───────────────────────── 阶段 0：环境快照 ─────────────────────────
Write-Host "`n=== 阶段 0：环境快照 ===" -ForegroundColor Cyan

$os = Get-CimInstance Win32_OperatingSystem
$build = (Get-ItemProperty 'HKLM:\SOFTWARE\Microsoft\Windows NT\CurrentVersion').CurrentBuild
Add-Result 'S0' '操作系统' 'INFO' ("{0} {1} build={2}" -f $os.Caption, $os.Version, $build)
Add-Result 'S0' '当前是否管理员' 'INFO' ([bool](([Security.Principal.WindowsPrincipal][Security.Principal.WindowsIdentity]::GetCurrent()).IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)))

$vcruntime = (Test-Path "$env:WINDIR\System32\vcruntime140.dll")
$vcruntime1 = (Test-Path "$env:WINDIR\System32\vcruntime140_1.dll")
if ($vcruntime) {
    Add-Result 'S0' 'VC++ 运行库（vcruntime140.dll）' 'PASS' '已存在，S4 场景不成立'
} else {
    Add-Result 'S0' 'VC++ 运行库（vcruntime140.dll）' 'WARN' '缺失——本机即「运行库缺失」场景，用于验 S4'
}
Add-Result 'S0' 'vcruntime140_1.dll' 'INFO' $(if ($vcruntime1) { '已存在' } else { '缺失' })

if (-not (Test-Path -LiteralPath $InstallerPath)) {
    Add-Result 'S0' '安装包路径' 'FAIL' "找不到 $InstallerPath"
    Write-Host '安装包不存在，终止。' -ForegroundColor Red
    exit 2
}
$inst = Get-Item -LiteralPath $InstallerPath
Add-Result 'S0' '安装包' 'INFO' ("{0} · {1:N1} MB" -f $inst.Name, ($inst.Length / 1MB))

$sig = Get-AuthenticodeSignature -LiteralPath $InstallerPath
Add-Result 'S0' '安装包数字签名' $(if ($sig.Status -eq 'Valid') { 'PASS' } else { 'WARN' }) ("status={0}（未签名时 SmartScreen 会拦，S5 预期弹窗）" -f $sig.Status)

$zone = $null
try {
    $ads = Get-Item -LiteralPath $InstallerPath -Stream Zone.Identifier -ErrorAction Stop
    $zone = (Get-Content -LiteralPath $InstallerPath -Stream Zone.Identifier -Raw) -replace "`r?`n", ' '
} catch { $zone = $null }
Add-Result 'S0' 'Mark-of-the-Web' $(if ($zone) { 'WARN' } else { 'INFO' }) $(if ($zone) { $zone.Trim() } else { '无 Attachment Manager 标记（本地复制不带 MOTW，SmartScreen 可能不触发）' })

$preEntries = Get-UninstallEntries
Add-Result 'S0' '安装前已有卸载项' $(if ($preEntries.Count -eq 0) { 'PASS' } else { 'WARN' }) ("count={0}（>0 说明不是干净机器）" -f $preEntries.Count)
$preDir = Get-InstallDir $preEntries
Add-Result 'S0' '安装前安装目录' 'INFO' ("{0}" -f ($(if ($preDir) { $preDir } else { '不存在（干净）' })))
$preProc = @(Get-Process -Name $CoreProcess -ErrorAction SilentlyContinue).Count
Add-Result 'S0' '安装前内核进程数' 'INFO' "count=$preProc"

# ───────────────────────── 阶段 1：覆盖升级前置（可选） ─────────────────────────
if ($PreviousInstallerPath) {
    Write-Host "`n=== 阶段 1：先装旧版（覆盖升级前置） ===" -ForegroundColor Cyan
    if (-not (Test-Path -LiteralPath $PreviousInstallerPath)) {
        Add-Result 'S1' '旧版安装包' 'FAIL' "找不到 $PreviousInstallerPath"
    } else {
        [void](Invoke-SilentInstaller $PreviousInstallerPath 'S1')
        Start-Sleep -Seconds 3
        $oldDir = Get-InstallDir (Get-UninstallEntries)
        Add-Result 'S1' '旧版安装结果' $(if ($oldDir) { 'PASS' } else { 'FAIL' }) ("dir={0}" -f $oldDir)
        $oldVer = if ($oldDir) { Get-FileVersionString (Join-Path $oldDir 'AI IDE.exe') } else { $null }
        Add-Result 'S1' '旧版版本号' 'INFO' "$oldVer"
        # 造一份用户数据用于验证升级后保留
        $probe = Join-Path $env:APPDATA 'AI IDE\acceptance-probe.txt'
        New-Item -ItemType Directory -Force -Path (Split-Path $probe) | Out-Null
        'upgrade-probe' | Set-Content -LiteralPath $probe -Encoding UTF8
        Add-Result 'S1' '写入升级探针文件' 'INFO' $probe
    }
}

# ───────────────────────── 阶段 2：安装 ─────────────────────────
Write-Host "`n=== 阶段 2：安装 ===" -ForegroundColor Cyan
[void](Invoke-SilentInstaller $InstallerPath 'S2')
Start-Sleep -Seconds 2

$entries = Get-UninstallEntries
Add-Result 'S2' '注册表卸载项存在' $(if ($entries.Count -ge 1) { 'PASS' } else { 'FAIL' }) ("count={0} root={1}" -f $entries.Count, ($entries | ForEach-Object { $_.Root }) -join ';')
$installDir = Get-InstallDir $entries
Add-Result 'S2' '安装目录' $(if ($installDir) { 'PASS' } else { 'FAIL' }) ("{0}" -f $installDir)

if ($installDir) {
    foreach ($f in @('AI IDE.exe', 'resources\app.asar', 'resources\core\ai-ide-core.exe')) {
        $full = Join-Path $installDir $f
        $ok = Test-Path -LiteralPath $full
        $size = if ($ok) { (Get-Item -LiteralPath $full).Length } else { 0 }
        Add-Result 'S2' ("安装产物 {0}" -f $f) $(if ($ok -and $size -gt 0) { 'PASS' } else { 'FAIL' }) ("{0} bytes" -f $size)
    }
    $exeVer = Get-FileVersionString (Join-Path $installDir 'AI IDE.exe')
    $coreVer = Get-FileVersionString (Join-Path $installDir 'resources\core\ai-ide-core.exe')
    Add-Result 'S2' '外壳版本号' $(if ($exeVer) { 'PASS' } else { 'FAIL' }) "$exeVer"
    Add-Result 'S2' '内核版本号' $(if ($coreVer) { 'PASS' } else { 'FAIL' }) "$coreVer"
    Add-Result 'S2' '外壳与内核版本一致' $(if ($exeVer -and $coreVer -and $exeVer -eq $coreVer) { 'PASS' } else { 'FAIL' }) "$exeVer vs $coreVer"
    Add-Result 'S2' '未把内核打进 asar' 'INFO' '内核路径 resources\core\（asar 之外）——见上一行产物检查'
}

$sc = Get-ShortcutState
Add-Result 'S2' '开始菜单快捷方式' $(if ($sc.StartMenu -or $sc.StartMenuPublic) { 'PASS' } else { 'FAIL' }) ("user={0} public={1}" -f $sc.StartMenu, $sc.StartMenuPublic)
Add-Result 'S2' '桌面快捷方式' $(if ($sc.Desktop) { 'PASS' } else { 'WARN' }) ("desktop={0}" -f $sc.Desktop)

if ($entries.Count -ge 1) {
    $e = $entries[0]
    Add-Result 'S2' '卸载字符串' $(if ($e.UninstallString) { 'PASS' } else { 'FAIL' }) $e.UninstallString
    Add-Result 'S2' '安装范围' 'INFO' $(if ($e.Root -like 'HKCU*') { 'perUser（HKCU，与 electron-builder perMachine:false 一致）' } else { 'perMachine（HKLM）' })
}

# ───────────────────────── 阶段 3：首次启动握手 ─────────────────────────
if (-not $SkipLaunch -and $installDir) {
    Write-Host "`n=== 阶段 3：首次启动握手 ===" -ForegroundColor Cyan
    $launchStart = Get-Date
    $appProc = Start-Process -FilePath (Join-Path $installDir 'AI IDE.exe') -PassThru
    $coreSeen = $false
    for ($i = 0; $i -lt 60; $i++) {
        Start-Sleep -Milliseconds 500
        if (Get-Process -Name $CoreProcess -ErrorAction SilentlyContinue) { $coreSeen = $true; break }
        if ($appProc.HasExited) { break }
    }
    $handshakeSec = ((Get-Date) - $launchStart).TotalSeconds
    Add-Result 'S3' '外壳进程拉起' $(if (-not $appProc.HasExited) { 'PASS' } else { 'FAIL' }) ("pid={0} exited={1}" -f $appProc.Id, $appProc.HasExited)
    Add-Result 'S3' '内核进程被拉起（握手）' $(if ($coreSeen) { 'PASS' } else { 'FAIL' }) ("耗时={0:N1}s" -f $handshakeSec)

    $userData = Join-Path $env:APPDATA 'AI IDE'
    Start-Sleep -Seconds 3
    foreach ($f in @('sessions.db', 'logs')) {
        $full = Join-Path $userData $f
        Add-Result 'S3' ("用户数据 {0}" -f $f) $(if (Test-Path -LiteralPath $full) { 'PASS' } else { 'FAIL' }) $full
    }

    if ($PreviousInstallerPath) {
        $probe = Join-Path $userData 'acceptance-probe.txt'
        Add-Result 'S3' '覆盖升级后用户数据保留' $(if (Test-Path -LiteralPath $probe) { 'PASS' } else { 'FAIL' }) $probe
    }

    # 退出：先温和关闭，再兜底强杀（只针对本应用的两个进程名）
    $null = $appProc.CloseMainWindow()
    Start-Sleep -Seconds 3
    if (-not (Wait-ProcessGone -Name $AppProcess -TimeoutSec 15)) {
        Stop-Process -Name $AppProcess -Force -ErrorAction SilentlyContinue
        Add-Result 'S3' '外壳进程需强制结束' 'WARN' 'CloseMainWindow 后 15s 未退出'
    }
    $coreGone = Wait-ProcessGone -Name $CoreProcess -TimeoutSec 20
    if (-not $coreGone) { Stop-Process -Name $CoreProcess -Force -ErrorAction SilentlyContinue }
    Add-Result 'S3' '退出后无残留内核进程' $(if ($coreGone) { 'PASS' } else { 'FAIL' }) 'tasklist 比对'
}

# ───────────────────────── 阶段 4：卸载 ─────────────────────────
if (-not $SkipUninstall -and $installDir) {
    Write-Host "`n=== 阶段 4：卸载 ===" -ForegroundColor Cyan
    $uninst = $null
    if ($entries.Count -ge 1 -and $entries[0].UninstallString) {
        $uninst = ($entries[0].UninstallString -replace '^"', '' -replace '"$', '')
    }
    if (-not $uninst) { $uninst = Join-Path $installDir 'Uninstall AI IDE.exe' }
    Add-Result 'S4' '卸载器路径' $(if (Test-Path -LiteralPath $uninst) { 'PASS' } else { 'FAIL' }) $uninst

    if (Test-Path -LiteralPath $uninst) {
        $sw = [Diagnostics.Stopwatch]::StartNew()
        $up = Start-Process -FilePath $uninst -ArgumentList '/S' -PassThru -Wait
        $sw.Stop()
        Add-Result 'S4' '静默卸载（/S）完成' $(if ($up.ExitCode -eq 0) { 'PASS' } else { 'FAIL' }) ("exit={0} 耗时={1:N1}s" -f $up.ExitCode, $sw.Elapsed.TotalSeconds)

        # NSIS 卸载器会自删，等目录消失
        $deadline = (Get-Date).AddSeconds(60)
        do {
            if (-not (Test-Path -LiteralPath $installDir)) { break }
            Start-Sleep -Milliseconds 500
        } while ((Get-Date) -lt $deadline)

        $dirGone = -not (Test-Path -LiteralPath $installDir)
        Add-Result 'S4' '安装目录已删除' $(if ($dirGone) { 'PASS' } else { 'FAIL' }) $installDir
        if (-not $dirGone) {
            $left = (Get-ChildItem -LiteralPath $installDir -Recurse -File -ErrorAction SilentlyContinue | Measure-Object).Count
            Add-Result 'S4' '安装目录残留文件数' 'WARN' "$left"
        }

        Start-Sleep -Seconds 2
        Add-Result 'S4' '卸载项已移除' $(if ((Get-UninstallEntries).Count -eq 0) { 'PASS' } else { 'FAIL' }) 'registry Uninstall'
        $sc2 = Get-ShortcutState
        Add-Result 'S4' '开始菜单快捷方式已移除' $(if (-not ($sc2.StartMenu -or $sc2.StartMenuPublic)) { 'PASS' } else { 'FAIL' }) ''
        Add-Result 'S4' '桌面快捷方式已移除' $(if (-not $sc2.Desktop) { 'PASS' } else { 'FAIL' }) ''
        Add-Result 'S4' '用户数据保留（deleteAppDataOnUninstall:false）' $(if (Test-Path (Join-Path $env:APPDATA 'AI IDE')) { 'PASS' } else { 'WARN' }) (Join-Path $env:APPDATA 'AI IDE')
    }
}

# ───────────────────────── 阶段 5：汇总 ─────────────────────────
Write-Host "`n=== 阶段 5：汇总 ===" -ForegroundColor Cyan
New-Item -ItemType Directory -Force -Path $ReportDir | Out-Null
$jsonPath = Join-Path $ReportDir "m8-nsis-acceptance-$Stamp.json"
$mdPath = Join-Path $ReportDir "m8-nsis-acceptance-$Stamp.md"

$Results | ConvertTo-Json -Depth 4 | Set-Content -LiteralPath $jsonPath -Encoding UTF8

$fail = @($Results | Where-Object { $_.Status -eq 'FAIL' }).Count
$warn = @($Results | Where-Object { $_.Status -eq 'WARN' }).Count
$pass = @($Results | Where-Object { $_.Status -eq 'PASS' }).Count

$md = New-Object System.Text.StringBuilder
[void]$md.AppendLine("# M8 真机 NSIS 安装验收报告")
[void]$md.AppendLine('')
[void]$md.AppendLine("- 时间：$(Get-Date -Format 'yyyy-MM-dd HH:mm:ss')")
[void]$md.AppendLine("- 安装包：``$InstallerPath``")
[void]$md.AppendLine("- 主机：$($os.Caption) $($os.Version) build=$build")
[void]$md.AppendLine("- 结果：PASS **$pass** / FAIL **$fail** / WARN **$warn**")
[void]$md.AppendLine('')
[void]$md.AppendLine('| 阶段 | 检查项 | 结果 | 证据 |')
[void]$md.AppendLine('|---|---|---|---|')
foreach ($r in $Results) {
    [void]$md.AppendLine("| $($r.Phase) | $($r.Item) | $($r.Status) | $($r.Evidence -replace '\|', '\|') |")
}
[void]$md.AppendLine('')
[void]$md.AppendLine('> 判定：FAIL=0 视为本机通过；WARN 需在 `docs/18-M8-真机安装验收清单.md` 的场景表里逐条对照。')
$md.ToString() | Set-Content -LiteralPath $mdPath -Encoding UTF8

Write-Host ("`n报告：{0}`n      {1}" -f $mdPath, $jsonPath) -ForegroundColor Cyan
Write-Host ("结果：PASS {0} / FAIL {1} / WARN {2}" -f $pass, $fail, $warn) -ForegroundColor $(if ($fail -gt 0) { 'Red' } else { 'Green' })

if ($fail -gt 0) { exit 1 } else { exit 0 }
