# 构建 Windows 安装包(msi)。需要 WiX Toolset 3(`choco install wixtoolset`),
# 或把 WIX 环境变量指向 WiX 安装目录。
#
# 用法:
#   pwsh ./scripts/make_msi.ps1                    # 版本取自 Cargo.toml
#   pwsh ./scripts/make_msi.ps1 -Version 0.4.2     # 指定版本
#
# 产物:target\logview-<version>-x64.msi(每用户安装,装到 %LOCALAPPDATA%\LogViewer)
param([string]$Version = "")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $Version) {
    $m = Select-String -Path "Cargo.toml" -Pattern '^version\s*=\s*"([^"]+)"' | Select-Object -First 1
    if (-not $m) { throw "无法从 Cargo.toml 读取 version" }
    $Version = $m.Matches[0].Groups[1].Value
}

if (-not (Test-Path "target\release\logview.exe")) {
    Write-Host "==> 构建 release (v$Version)"
    cargo build --release --locked
}

$candle = "candle.exe"
$light = "light.exe"
if ($env:WIX) {
    $candle = Join-Path $env:WIX "bin\candle.exe"
    $light = Join-Path $env:WIX "bin\light.exe"
}

$obj = "target\msi"
New-Item -ItemType Directory -Force -Path $obj | Out-Null

Write-Host "==> 编译 WiX 源"
& $candle -nologo -arch x64 -dVersion=$Version "-dSourceDir=$root" -out "$obj\logview.wixobj" "installer\logview.wxs"
if ($LASTEXITCODE -ne 0) { throw "candle 失败(退出码 $LASTEXITCODE)" }

Write-Host "==> 链接 msi"
& $light -nologo -arch x64 -out "target\logview-$Version-x64.msi" "$obj\logview.wixobj"
if ($LASTEXITCODE -ne 0) { throw "light 失败(退出码 $LASTEXITCODE)" }

Write-Host "==> 完成: target\logview-$Version-x64.msi"