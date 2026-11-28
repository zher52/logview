# msi 静默安装冒烟测试:安装 → 校验文件/注册表/快捷方式 → 卸载 → 校验清理。
# 必须在 Windows 上运行(GitHub Actions windows-latest)。
#
# 用法:
#   pwsh ./scripts/test_msi.ps1                          # 取 target 下最新的 logview-*-x64.msi
#   pwsh ./scripts/test_msi.ps1 -Msi target\logview-0.5.2-x64.msi
param([string]$Msi = "")

$ErrorActionPreference = "Stop"
$root = Split-Path -Parent $PSScriptRoot
Set-Location $root

if (-not $Msi) {
    $found = Get-ChildItem "target" -Filter "logview-*-x64.msi" -ErrorAction SilentlyContinue |
        Sort-Object LastWriteTime | Select-Object -Last 1
    if ($null -eq $found) { throw "未找到 target\logview-*-x64.msi,先运行 scripts/make_msi.ps1" }
    $Msi = $found.FullName
}
if (-not (Test-Path $Msi)) { throw "找不到 msi: $Msi" }
$Msi = (Resolve-Path $Msi).Path

$installDir = Join-Path $env:LOCALAPPDATA "LogViewer"
$exe = Join-Path $installDir "logview.exe"
$startMenu = Join-Path $env:APPDATA "Microsoft\Windows\Start Menu\Programs\日志查看器\日志查看器.lnk"
$progId = "HKCU:\Software\Classes\LogViewer.Log"
$appKey = "HKCU:\Software\Classes\Applications\logview.exe"
$log = Join-Path $env:TEMP "logview-msi-setup.log"

function Assert-True($cond, $msg) {
    if (-not $cond) { throw "断言失败: $msg" }
}
function Invoke-Msi([string]$verb) {
    $args = "$verb `"$Msi`" /qn /norestart /l*v `"$log`""
    Write-Host "==> msiexec $args"
    $p = Start-Process msiexec.exe -ArgumentList $args -Wait -PassThru
    # 3010 = 成功但需要重启,同样视为成功
    Assert-True ($p.ExitCode -eq 0 -or $p.ExitCode -eq 3010) "msiexec $verb 退出码 $($p.ExitCode)(日志: $log)"
}

# 安装前确认环境干净,避免上一次残留掩盖问题
Assert-True (-not (Test-Path $exe)) "安装前已存在 $exe,环境不干净"

Invoke-Msi "/i"

Assert-True (Test-Path $exe) "安装后缺少 $exe"
Assert-True (Test-Path $startMenu) "安装后缺少开始菜单快捷方式: $startMenu"
Assert-True (Test-Path $progId) "安装后缺少 ProgID: $progId"
$openCmd = (Get-ItemProperty "$progId\shell\open\command" -ErrorAction SilentlyContinue).'(default)'
Assert-True ($openCmd -like "*logview.exe*%1*") "ProgID 打开命令不正确: $openCmd"
Assert-True (Test-Path $appKey) "安装后缺少 Applications\logview.exe"

foreach ($ext in @(".log", ".out", ".err", ".txt")) {
    $key = "HKCU:\Software\Classes\$ext\OpenWithProgids"
    Assert-True (Test-Path $key) "缺少 OpenWithProgids: $ext"
    $values = Get-ItemProperty $key
    Assert-True ($null -ne $values.'LogViewer.Log') "扩展名 $ext 未登记 LogViewer.Log"
}
foreach ($ext in @(".json", ".csv")) {
    $key = "HKCU:\Software\Classes\$ext\OpenWithProgids"
    if (Test-Path $key) {
        $values = Get-ItemProperty $key
        Assert-True ($null -eq $values.'LogViewer.Log') "扩展名 $ext 不应再登记 LogViewer.Log"
    }
}

Invoke-Msi "/x"

Assert-True (-not (Test-Path $exe)) "卸载后仍存在 $exe"
Assert-True (-not (Test-Path $startMenu)) "卸载后仍存在开始菜单快捷方式"
# 键本身可能保留为空壳,以打开命令是否还在为准
$openCmdAfter = (Get-ItemProperty "$progId\shell\open\command" -ErrorAction SilentlyContinue).'(default)'
Assert-True ($null -eq $openCmdAfter) "卸载后仍存在打开命令: $openCmdAfter"
foreach ($ext in @(".log", ".txt")) {
    $key = "HKCU:\Software\Classes\$ext\OpenWithProgids"
    if (Test-Path $key) {
        $values = Get-ItemProperty $key
        Assert-True ($null -eq $values.'LogViewer.Log') "卸载后 $ext 仍登记 LogViewer.Log"
    }
}

Write-Host "==> msi 冒烟测试通过(安装/关联/卸载均正常)"