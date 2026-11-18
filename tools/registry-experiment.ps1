# Revolc 注册表实验脚本（备份 / 清理 / 注入 / 验证）
#
# 用途：验证「清理 BHO 注册 → 接管失效 → 重新注入 → 接管恢复」这条链路。
#
# 用法（管理员 PowerShell）：
#   .\Revolc_注册表实验.ps1 -Action backup     # 完整备份 5 个 hive + Revolc 4 键
#   .\Revolc_注册表实验.ps1 -Action clean      # 删除 Revolc 的 4 个键
#   .\Revolc_注册表实验.ps1 -Action inject     # 按当前 DLL 路径重新注入
#   .\Revolc_注册表实验.ps1 -Action verify     # 只读核验当前状态
#   .\Revolc_注册表实验.ps1 -Action restore    # 用备份的 4 键回写（合并式）
#
# 重要限制（读一遍再跑）：
#   1. reg import 是"合并"语义：备份之后新增的键不会被删除。
#      真正的整机回滚只能靠系统还原点或 WinPE 离线还原 hive。
#   2. BHO 变更后必须重启 Explorer 才可观测：已加载 DLL 的 explorer 进程
#      不会因为注册表被删就卸载。
#   3. 清理/注入前先让 Revolc 退出（revolc.exe --shutdown），否则吸收中的
#      窗口会随进程一起销毁。
#   4. 本脚本运行需要管理员；否则 HKLM 写入会静默失败（脚本会显式报错）。

param(
    [ValidateSet('backup', 'clean', 'inject', 'verify', 'restore', 'restore-point')]
    [string]$Action = 'verify',
    [string]$OutDir = '',
    [string]$Dll = ''
)

$ErrorActionPreference = 'Stop'

# 默认路径全部相对脚本位置推导，换机器（虚拟机）不用改参数。
if (-not $OutDir) { $OutDir = Join-Path $env:USERPROFILE 'revolc-reg-backup' }
if (-not $Dll) {
    $repoRoot = Split-Path $PSScriptRoot -Parent
    $Dll = Join-Path $repoRoot 'target\release\revolc_bho.dll'
    if (-not (Test-Path $Dll)) {
        throw "找不到默认 DLL：$Dll`n请用 -Dll 指定，或先 cargo build --release -p revolc-bho"
    }
}

$CLSID = '{9F1C4E8B-6A2D-4E7B-8C13-5D04F7A2B916}'
$KeyClsid = "HKLM\SOFTWARE\Classes\CLSID\$CLSID"
$KeyBho = "HKLM\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Browser Helper Objects\$CLSID"
$KeyClsidCu = "HKCU\SOFTWARE\Classes\CLSID\$CLSID"
$KeyBhoCu = "HKCU\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Browser Helper Objects\$CLSID"

function Assert-Admin {
    $id = [Security.Principal.WindowsIdentity]::GetCurrent()
    $p = New-Object Security.Principal.WindowsPrincipal($id)
    if (-not $p.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw '需要管理员权限：请用管理员 PowerShell 运行。'
    }
}

function Reg-Export($key, $file) {
    $null = & reg.exe export $key $file /y /reg:64 2>&1
    if (Test-Path $file) { "  [ok]   $file  ($([math]::Round((Get-Item $file).Length / 1KB, 1)) KB)" }
    else { "  [fail] $key  (键可能不存在)" }
}

function Get-State {
    $r = [ordered]@{}
    $r.HklmClsid = Test-Path "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\CLSID\$CLSID"
    $r.HklmBho = Test-Path "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Browser Helper Objects\$CLSID"
    $r.HkcuClsid = Test-Path "Registry::HKEY_CURRENT_USER\SOFTWARE\Classes\CLSID\$CLSID"
    $r.HkcuBho = Test-Path "Registry::HKEY_CURRENT_USER\SOFTWARE\Microsoft\Windows\CurrentVersion\Explorer\Browser Helper Objects\$CLSID"
    $r.Path = $null
    if ($r.HklmClsid) {
        $p = "Registry::HKEY_LOCAL_MACHINE\SOFTWARE\Classes\CLSID\$CLSID\InprocServer32"
        if (Test-Path $p) { $r.Path = (Get-ItemProperty -Path $p).'(default)' }
    }
    $r.DllExists = $false
    if ($r.Path) { $r.DllExists = Test-Path $r.Path }
    , $r
}

function Show-State($why) {
    $s = Get-State
    "== 状态检查：$why"
    "   HKLM\CLSID      : $(if($s.HklmClsid){'存在'}else{'缺失'})"
    "   HKLM\BHO        : $(if($s.HklmBho){'存在'}else{'缺失'})"
    "   HKCU\CLSID      : $(if($s.HkcuClsid){'存在（兜底残留）'}else{'缺失'})"
    "   HKCU\BHO        : $(if($s.HkcuBho){'存在（兜底残留）'}else{'缺失'})"
    "   InprocServer32  : $(if($s.Path){$s.Path}else{'(无)'})"
    "   DLL 存在        : $($s.DllExists)"
    if ($s.HklmClsid -and $s.HklmBho -and $s.DllExists) { "   判定：已注入（可用）" }
    elseif ($s.HklmClsid -or $s.HklmBho) { "   判定：半注入（不可用，需修复）" }
    else { "   判定：未注入（不接管）" }
}

switch ($Action) {

    'backup' {
        Assert-Admin
        if (-not (Test-Path $OutDir)) { $null = New-Item -ItemType Directory -Path $OutDir -Force }
        $stamp = Get-Date -Format 'yyyyMMdd-HHmmss'
        "== 完整备份 5 个 hive（文本形式，可能数百 MB，耗时几分钟）"
        Reg-Export 'HKLM' (Join-Path $OutDir "HKLM-$stamp.reg")
        Reg-Export 'HKCU' (Join-Path $OutDir "HKCU-$stamp.reg")
        Reg-Export 'HKCR' (Join-Path $OutDir "HKCR-$stamp.reg")
        Reg-Export 'HKU'  (Join-Path $OutDir "HKU-$stamp.reg")
        Reg-Export 'HKCC' (Join-Path $OutDir "HKCC-$stamp.reg")
        "== 同时备份 Revolc 相关 4 键（用于合并式恢复）"
        Reg-Export $KeyClsid   (Join-Path $OutDir "revolc-clsid-hklm-$stamp.reg")
        Reg-Export $KeyBho     (Join-Path $OutDir "revolc-bho-hklm-$stamp.reg")
        Reg-Export $KeyClsidCu (Join-Path $OutDir "revolc-clsid-hkcu-$stamp.reg")
        Reg-Export $KeyBhoCu   (Join-Path $OutDir "revolc-bho-hkcu-$stamp.reg")
        "== 说明"
        "   完整 hive 的 .reg 只能【合并】回去，不能删除备份之后新增的键。"
        "   需要真回滚请先跑 -Action restore-point 建还原点。"
    }

    'restore-point' {
        Assert-Admin
        "== 检查系统保护状态"
        $sr = & vssadmin.exe list shadows 2>&1
        "== 创建还原点（若系统保护未启用会失败，需先启用）"
        try {
            Checkpoint-Computer -Description "Revolc registry experiment" -RestorePointType MODIFY_SETTINGS
            "[ok] 还原点已创建"
        } catch {
            "[fail] $($_.Exception.Message)"
            "       启用方式：系统属性 → 系统保护 → 选中 C: → 配置 → 启用系统保护"
        }
    }

    'clean' {
        Assert-Admin
        "== 关闭 Revolc（优雅退出，避免吸收中的窗口被销毁）"
        $exe = Join-Path (Split-Path $Dll) 'revolc.exe'
        if (Test-Path $exe) { $null = & $exe --shutdown; Start-Sleep -Seconds 2 }
        "== 删除 4 个键"
        foreach ($k in @($KeyBho, $KeyClsid, $KeyBhoCu, $KeyClsidCu)) {
            & reg.exe delete $k /f /reg:64 2>&1 | Out-Null
            $still = & reg.exe query $k /reg:64 2>&1
            if ($LASTEXITCODE -eq 0) { "  [fail] 仍存在：$k" } else { "  [ok]   已删除：$k" }
        }
        Show-State '清理后'
        "== 必须重启 Explorer 才能观测到失效（已加载 DLL 的进程不会自动卸载）"
        "   taskkill /f /im explorer.exe ; start explorer"
    }

    'inject' {
        Assert-Admin
        if (-not (Test-Path $Dll)) { throw "DLL 不存在：$Dll" }
        "== 写入 CLSID\InprocServer32"
        & reg.exe add $KeyClsid'\InprocServer32' /ve /t REG_SZ /d "$Dll" /f /reg:64 | Out-Null
        & reg.exe add $KeyClsid'\InprocServer32' /v ThreadingModel /t REG_SZ /d 'Apartment' /f /reg:64 | Out-Null
        "== 写入 BHO 名单"
        & reg.exe add $KeyBho /ve /t REG_SZ /d 'Revolc Tab Explorer Helper' /f /reg:64 | Out-Null
        Show-State '注入后'
        "== 注入完也要重启 Explorer 才算干净生效"
    }

    'restore' {
        Assert-Admin
        $prefixes = @('revolc-clsid-hklm-', 'revolc-bho-hklm-', 'revolc-clsid-hkcu-', 'revolc-bho-hkcu-')
        "== 合并式恢复 Revolc 键（各自取最新一份备份）"
        $found = 0
        foreach ($p in $prefixes) {
            $f = Get-ChildItem $OutDir -Filter "$p*.reg" -ErrorAction SilentlyContinue |
                 Sort-Object LastWriteTime -Descending | Select-Object -First 1
            if ($f) {
                & reg.exe import $f.FullName /reg:64 2>&1 | Out-Null
                "  [ok] $($f.Name)"
                $found++
            } else {
                "  [miss] 没有 $p*.reg"
            }
        }
        if ($found -eq 0) { throw "在 $OutDir 里找不到 Revolc 备份，请先跑 -Action backup" }
        Show-State '恢复后'
    }

    'verify' {
        Show-State '当前'
        "== 端到端探针（可选）"
        $probe = Join-Path $env:APPDATA 'revolc\bho.debug'
        "   探针文件：$probe  存在=$(Test-Path $probe)"
        "   若无：'New-Item -ItemType File -Path \$probe -Force'，然后开一个文件夹窗口"
        "   再看 %APPDATA%\revolc\bho.log 是否出现 'SetSite (notify only)'"
    }
}
