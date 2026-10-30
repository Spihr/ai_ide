#Requires -Version 5.1
<#
.SYNOPSIS
    AI IDE 一键打包（Windows x64 / NSIS 安装包）。

.DESCRIPTION
    1. 校验 D:\nodejs\node.exe（要求 >= 22.13，pnpm 11 的硬性下限）；
    2. 校验 pnpm（C:\Users\neroy\AppData\Roaming\npm\node_modules\pnpm\bin\pnpm.cjs）；
    3. 校验 cargo（内核 release 构建需要）；
    4. 预检版本号与运行中进程占用；
    5. 用显式 node + pnpm.cjs 执行仓库既有的 `dist:offline` 链路
       （verify:versions -> prepack-core -> electron-vite build -> electron-builder -> verify-package）；
    6. 校验并打印产物绝对路径与大小。

    为什么必须显式指定 node：当前 PATH 首位 node 为 v22.12.0，低于 pnpm 11.3.0
    要求的 >= 22.13，直接用 `pnpm` 会被 pnpm 拒绝执行，故全程以
    `D:\nodejs\node.exe pnpm.cjs` 的形式调用。

    为什么走 dist:offline：electron-builder 的二进制下载器不读 .npmrc 的
    electron_mirror，受限网络下会去 GitHub 拉 electron / NSIS 资源而超时；
    仓库 scripts/offline-pack.mjs 已固化「本地 electron dist + 国内镜像」两条绕行。

    本脚本不修改任何 git 跟踪的构建配置，只做校验与串联调用。

.PARAMETER SkipVerify
    跳过最后一步仓库自带的 verify-package 校验（产物存在性校验仍会执行）。

.EXAMPLE
    .\build-exe.ps1
#>
[CmdletBinding()]
param(
    [switch]$SkipVerify
)

$ErrorActionPreference = 'Stop'
$ProgressPreference = 'SilentlyContinue'

$Utf8NoBom = New-Object System.Text.UTF8Encoding($false)
try { [Console]::OutputEncoding = $Utf8NoBom } catch { }
$OutputEncoding = $Utf8NoBom

$RepoRoot = Split-Path -Parent $MyInvocation.MyCommand.Path
$NodeExe = 'D:\nodejs\node.exe'
$PnpmCjs = 'C:\Users\neroy\AppData\Roaming\npm\node_modules\pnpm\bin\pnpm.cjs'
$MinNodeVersion = [version]'22.13.0'

$TotalSteps = 6
$StepNo = 0
$Step = '准备'
$Watch = [System.Diagnostics.Stopwatch]::StartNew()
$StartTime = Get-Date

function Write-Step {
    param([string]$Title)
    $script:StepNo++
    $script:Step = $Title
    Write-Host ''
    Write-Host ('[{0}/{1}] {2}' -f $script:StepNo, $script:TotalSteps, $Title) -ForegroundColor Cyan
}

function Write-Info {
    param([string]$Message)
    Write-Host ('        ' + $Message) -ForegroundColor DarkGray
}

function Stop-Failed {
    param([string]$Message)
    Write-Host ''
    Write-Host ('[失败] 步骤 {0}：{1}' -f $script:StepNo, $script:Step) -ForegroundColor Red
    Write-Host ('       原因：' + $Message) -ForegroundColor Red
    Write-Host ('       已立即中止，后续步骤未执行。已耗时 {0:N1} 秒。' -f $script:Watch.Elapsed.TotalSeconds) -ForegroundColor Red
    exit 1
}

function Format-Size {
    param([long]$Bytes)
    if ($Bytes -ge 1GB) { return ('{0:N2} GB' -f ($Bytes / 1GB)) }
    if ($Bytes -ge 1MB) { return ('{0:N2} MB' -f ($Bytes / 1MB)) }
    if ($Bytes -ge 1KB) { return ('{0:N1} KB' -f ($Bytes / 1KB)) }
    return ('{0} B' -f $Bytes)
}

Write-Host '============================================================' -ForegroundColor White
Write-Host '  AI IDE 一键打包（Windows x64 / NSIS 安装包）' -ForegroundColor White
Write-Host ('  仓库：' + $RepoRoot) -ForegroundColor White
Write-Host '============================================================' -ForegroundColor White

# ---------------------------------------------------------------- 步骤 1
Write-Step '校验 Node.js（要求 >= 22.13）'

if (-not (Test-Path -LiteralPath $NodeExe)) {
    Stop-Failed ('未找到指定的 node：' + $NodeExe + '。请确认 Node.js 安装在 D:\nodejs，或修改脚本顶部的 $NodeExe。')
}
$nodeRaw = (& $NodeExe -v) 2>&1
if ($LASTEXITCODE -ne 0) {
    Stop-Failed ('node 无法执行（退出码 ' + $LASTEXITCODE + '）：' + ($nodeRaw | Out-String).Trim())
}
$nodeVerText = ($nodeRaw | Out-String).Trim().TrimStart('v')
try { $nodeVersion = [version]$nodeVerText }
catch { Stop-Failed ('无法解析 node 版本：' + $nodeVerText) }
if ($nodeVersion -lt $MinNodeVersion) {
    Stop-Failed ('node 版本过低：' + $nodeVerText + '，pnpm 11.3.0 要求 >= ' + $MinNodeVersion + '。')
}
Write-Info ('node   ：' + $NodeExe + ' (v' + $nodeVerText + ')')

$pathNode = Get-Command node -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
if ($pathNode) {
    Write-Info ('PATH 首位 node：' + $pathNode.Source + ' —— 本脚本不依赖它，全程用上面的绝对路径。')
}

# ---------------------------------------------------------------- 步骤 2
Write-Step '校验 pnpm'

if (-not (Test-Path -LiteralPath $PnpmCjs)) {
    Stop-Failed ('未找到 pnpm 入口：' + $PnpmCjs + '。请执行 npm i -g pnpm 后重跑。')
}
$pnpmRaw = (& $NodeExe $PnpmCjs -v) 2>&1
if ($LASTEXITCODE -ne 0) {
    Stop-Failed ('pnpm 无法执行（退出码 ' + $LASTEXITCODE + '）：' + ($pnpmRaw | Out-String).Trim())
}
$pnpmVersion = ($pnpmRaw | Out-String).Trim()
Write-Info ('pnpm   ：' + $PnpmCjs + ' (v' + $pnpmVersion + ')')

$lockfile = Join-Path $RepoRoot 'pnpm-lock.yaml'
if (-not (Test-Path -LiteralPath $lockfile)) {
    Stop-Failed ('缺少 pnpm-lock.yaml，仓库依赖未按 pnpm 安装？请先在仓库根执行 pnpm install。')
}
$workspaceYaml = Join-Path $RepoRoot 'pnpm-workspace.yaml'
if (-not (Test-Path -LiteralPath $workspaceYaml)) {
    Stop-Failed ('缺少 pnpm-workspace.yaml：' + $workspaceYaml)
}

# ---------------------------------------------------------------- 步骤 3
Write-Step '校验 cargo'

$Cargo = $null
$cargoCmd = Get-Command cargo -CommandType Application -ErrorAction SilentlyContinue | Select-Object -First 1
if ($cargoCmd) {
    $Cargo = $cargoCmd.Source
}
else {
    $fallback = Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe'
    if (Test-Path -LiteralPath $fallback) { $Cargo = $fallback }
}
if (-not $Cargo) {
    Stop-Failed '未找到 cargo。prepack-core 需要 cargo build --release -p ai-ide-core，请安装 Rust 工具链或把 %USERPROFILE%\.cargo\bin 加入 PATH。'
}
$cargoVersion = (& $Cargo --version) 2>&1
if ($LASTEXITCODE -ne 0) {
    Stop-Failed ('cargo 无法执行（退出码 ' + $LASTEXITCODE + '）：' + ($cargoVersion | Out-String).Trim())
}
Write-Info ('cargo  ：' + $Cargo)
Write-Info ('版本   ：' + ($cargoVersion | Out-String).Trim())
# prepack-core.mjs 内部用 execFileSync('cargo', ...) 调 PATH，这里兜底把 cargo 目录前置。
$cargoBinDir = Split-Path -Parent $Cargo
if ($cargoBinDir -and ($env:PATH -notlike ('*' + $cargoBinDir + '*'))) {
    $env:PATH = $cargoBinDir + ';' + $env:PATH
}

# ---------------------------------------------------------------- 步骤 4
Write-Step '预检版本号、进程占用与本地 electron dist'

$desktopPkgPath = Join-Path $RepoRoot 'apps\desktop\package.json'
if (-not (Test-Path -LiteralPath $desktopPkgPath)) {
    Stop-Failed ('缺少 apps\desktop\package.json：' + $desktopPkgPath)
}
$desktopPkg = Get-Content -LiteralPath $desktopPkgPath -Raw -Encoding UTF8 | ConvertFrom-Json
$Version = $desktopPkg.version
if ([string]::IsNullOrWhiteSpace($Version)) {
    Stop-Failed 'apps/desktop/package.json 没有 version 字段。'
}
Write-Info ('外壳版本：' + $Version + '（另由 verify:versions 与内核 Cargo workspace 版本交叉校验）')

$busy = @(Get-Process -Name 'AI IDE', 'ai-ide-core' -ErrorAction SilentlyContinue)
if ($busy.Count -gt 0) {
    $detail = ($busy | ForEach-Object { $_.ProcessName + '(PID ' + $_.Id + ')' }) -join '、'
    Stop-Failed ('检测到程序正在运行：' + $detail + '。运行中的进程会占用 release\win-unpacked\AI IDE.exe，导致打包报 EPERM。请先退出应用后重跑。')
}

$releaseDir = Join-Path $RepoRoot 'apps\desktop\release'
$installerPath = Join-Path $releaseDir ('AI-IDE-{0}-win-x64.exe' -f $Version)
Write-Info ('预期产物：' + $installerPath)

# electron-builder 的 electron 本体必须来自本地解包目录，否则会去 GitHub 下载。
# 本机把 electron 的 dist 移到了 apps\desktop\.electron-dist-<ver>（避免被 pnpm 清理），
# offline-pack.mjs 只认 node_modules\electron\dist。这里用目录联接补齐，不下载、不复制 200MB。
$electronRoot = Join-Path $RepoRoot 'apps\desktop\node_modules\electron'
$electronDist = Join-Path $electronRoot 'dist'
if (Test-Path -LiteralPath (Join-Path $electronDist 'electron.exe')) {
    Write-Info ('本地 electron dist：' + $electronDist)
}
else {
    $electronVer = $null
    $electronPkg = Join-Path $electronRoot 'package.json'
    if (Test-Path -LiteralPath $electronPkg) {
        $electronVer = (Get-Content -LiteralPath $electronPkg -Raw -Encoding UTF8 | ConvertFrom-Json).version
    }
    $candidates = @(Get-ChildItem -LiteralPath (Join-Path $RepoRoot 'apps\desktop') -Directory -Filter '.electron-dist-*' -ErrorAction SilentlyContinue)
    $picked = $null
    if ($electronVer) {
        $picked = $candidates | Where-Object { $_.Name -eq ('.electron-dist-' + $electronVer) } | Select-Object -First 1
    }
    if (-not $picked -and $candidates.Count -gt 0) {
        $picked = $candidates | Sort-Object Name -Descending | Select-Object -First 1
        if ($electronVer) {
            Write-Info ('警告：未找到与声明版本 ' + $electronVer + ' 完全匹配的本地 dist，退用 ' + $picked.Name + '。')
        }
    }
    if (-not $picked) {
        Stop-Failed ('未找到本地 electron dist（node_modules\electron\dist 与 apps\desktop\.electron-dist-* 均缺失）。请先执行 pnpm install（.npmrc 已配 electron_mirror），再重跑本脚本。')
    }
    if (-not (Test-Path -LiteralPath (Join-Path $picked.FullName 'electron.exe'))) {
        Stop-Failed ('本地 electron dist 不完整（缺少 electron.exe）：' + $picked.FullName)
    }
    if (-not (Test-Path -LiteralPath $electronRoot)) {
        Stop-Failed ('缺少 node_modules\electron，请先执行 pnpm install。')
    }
    try {
        New-Item -ItemType Junction -Path $electronDist -Target $picked.FullName -ErrorAction Stop | Out-Null
    }
    catch {
        Stop-Failed ('无法建立 electron dist 目录联接（' + $electronDist + ' -> ' + $picked.FullName + '）：' + $_.Exception.Message)
    }
    Write-Info ('本地 electron dist 缺失，已建立目录联接：' + $electronDist + ' -> ' + $picked.FullName)
    Write-Info '（只动 node_modules，不下载 electron，不修改任何 git 跟踪文件。）'
}

# ---------------------------------------------------------------- 步骤 5
Write-Step '执行打包链路（pnpm dist:offline）'

# 让 npm scripts 内部的 `node` 也解析到 D:\nodejs\node.exe（v24.16.0）。
$env:PATH = 'D:\nodejs;' + $env:PATH
if (-not $env:ELECTRON_BUILDER_BINARIES_MIRROR) {
    $env:ELECTRON_BUILDER_BINARIES_MIRROR = 'https://npmmirror.com/mirrors/electron-builder-binaries/'
    Write-Info '已注入 ELECTRON_BUILDER_BINARIES_MIRROR（NSIS / winCodeSign 走国内镜像）。'
}

$pnpmScript = 'dist:offline'
if ($SkipVerify) {
    # 仍然走同一条链路，只是跳过末尾的 verify-package：改为逐步显式串联。
    Write-Info '已指定 -SkipVerify：改为分步调用各阶段，不执行 verify-package。'
}

Push-Location $RepoRoot
try {
    if ($SkipVerify) {
        & $NodeExe scripts\verify-versions.mjs
        if ($LASTEXITCODE -ne 0) { throw ('verify:versions 失败（退出码 ' + $LASTEXITCODE + '）') }
        & $NodeExe scripts\prepack-core.mjs
        if ($LASTEXITCODE -ne 0) { throw ('prepack-core 失败（退出码 ' + $LASTEXITCODE + '）') }
        & $NodeExe $PnpmCjs --filter '@ai-ide/desktop' build
        if ($LASTEXITCODE -ne 0) { throw ('electron-vite build 失败（退出码 ' + $LASTEXITCODE + '）') }
        & $NodeExe scripts\offline-pack.mjs
        if ($LASTEXITCODE -ne 0) { throw ('electron-builder 出包失败（退出码 ' + $LASTEXITCODE + '）') }
    }
    else {
        & $NodeExe $PnpmCjs run $pnpmScript
        if ($LASTEXITCODE -ne 0) { throw ('pnpm run ' + $pnpmScript + ' 失败（退出码 ' + $LASTEXITCODE + '）') }
    }
}
catch {
    Stop-Failed $_.Exception.Message
}
finally {
    Pop-Location
}
Write-Info '打包链路执行完成。'

# ---------------------------------------------------------------- 步骤 6
Write-Step '校验并汇总产物'

if (-not (Test-Path -LiteralPath $installerPath)) {
    Stop-Failed ('未找到预期产物：' + $installerPath + '。请检查 electron-builder 输出目录是否被改过。')
}
$installerItem = Get-Item -LiteralPath $installerPath
if ($installerItem.Length -le 0) {
    Stop-Failed ('安装包为 0 字节：' + $installerPath)
}
if ($installerItem.LastWriteTime -lt $StartTime.AddMinutes(-1)) {
    Stop-Failed ('安装包不是本次构建产出的（修改时间 ' + $installerItem.LastWriteTime + ' 早于本次开始时间），可能是上一次的残留。')
}

$unpackedExe = Join-Path $releaseDir 'win-unpacked\AI IDE.exe'
$coreExe = Join-Path $releaseDir 'win-unpacked\resources\core\ai-ide-core.exe'

$Watch.Stop()
$elapsed = $Watch.Elapsed

Write-Host ''
Write-Host ('[完成] AI IDE 打包成功（版本 ' + $Version + '）') -ForegroundColor Green
Write-Host '产物（绝对路径）与大小：' -ForegroundColor Green
Write-Host ('  ' + $installerPath) -ForegroundColor White
Write-Host ('      ' + (Format-Size $installerItem.Length) + '   NSIS 安装包（x64）') -ForegroundColor DarkGray

if (Test-Path -LiteralPath $unpackedExe) {
    $s = (Get-Item -LiteralPath $unpackedExe).Length
    Write-Host ('  ' + $unpackedExe) -ForegroundColor White
    Write-Host ('      ' + (Format-Size $s) + '   免安装主程序') -ForegroundColor DarkGray
}
if (Test-Path -LiteralPath $coreExe) {
    $s = (Get-Item -LiteralPath $coreExe).Length
    Write-Host ('  ' + $coreExe) -ForegroundColor White
    Write-Host ('      ' + (Format-Size $s) + '   Rust 内核（extraResources）') -ForegroundColor DarkGray
}
Write-Host ''
Write-Host ('总耗时：{0:N1} 秒（{1:N1} 分钟）' -f $elapsed.TotalSeconds, $elapsed.TotalMinutes) -ForegroundColor Green
exit 0
