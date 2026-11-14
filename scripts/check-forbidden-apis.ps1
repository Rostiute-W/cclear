# 检查破坏性 API 与危险能力是否只出现在允许的位置。被 AGENTS.md 规则 1、2、8 引用，执行者不得修改。
$ErrorActionPreference = 'Stop'
$root = Split-Path -Parent $PSScriptRoot
$scanDirs = @('crates', 'src-tauri\src') | ForEach-Object { Join-Path $root $_ } | Where-Object { Test-Path -LiteralPath $_ }
$files = foreach ($d in $scanDirs) {
    Get-ChildItem -LiteralPath $d -Recurse -File -Filter *.rs |
        Where-Object { $_.FullName -notmatch '\\crates\\[^\\]+\\tests\\' }
}
$checks = @(
    @{ Name = 'std::fs 删除/改名'; Pattern = '\b(remove_file|remove_dir|remove_dir_all|rename)\s*\('; Allowed = @('crates/cclear-core/src/guard/') },
    @{ Name = 'Win32 破坏性文件 API'; Pattern = '\b(DeleteFileW?|RemoveDirectoryW?|MoveFileW?|MoveFileExW?|MoveFileWithProgressW?|ReplaceFileW?|SetFileInformationByHandle|FileDispositionInfo\w*|FILE_DELETE_ON_CLOSE|FSCTL_SET_REPARSE_POINT|FSCTL_DELETE_REPARSE_POINT|SHFileOperationW?|IFileOperation)\b'; Allowed = @('crates/cclear-core/src/guard/') },
    @{ Name = '清空回收站'; Pattern = '\bSHEmptyRecycleBinW?\b'; Allowed = @('crates/cclear-core/src/special/recycle_bin.rs') },
    @{ Name = '启动外部进程'; Pattern = '\bCommand::new\b|\bCreateProcess\w*\b|\bShellExecute\w*\b|\bWinExec\b'; Allowed = @('crates/cclear-core/src/exec/command.rs', 'crates/cclear-core/src/elevation/') },
    @{ Name = '危险 Tauri 插件'; Pattern = 'tauri_plugin_(fs|shell|opener|process|http)\b'; Allowed = @() },
    @{ Name = 'unsafe'; Pattern = '\bunsafe\b'; Allowed = @('crates/cclear-core/src/win/', 'crates/cclear-core/src/guard/', 'crates/cclear-core/src/special/', 'crates/cclear-core/src/elevation/') }
)
$violations = @()
foreach ($f in $files) {
    $rel = $f.FullName.Substring($root.Length + 1).Replace('\', '/')
    $lines = @(Get-Content -LiteralPath $f.FullName)
    for ($i = 0; $i -lt $lines.Count; $i++) {
        $line = $lines[$i]
        if ($line -match '^\s*//') { continue }
        foreach ($c in $checks) {
            if ($line -match $c.Pattern) {
                $ok = $false
                foreach ($a in $c.Allowed) { if ($rel.StartsWith($a)) { $ok = $true; break } }
                if (-not $ok) { $violations += "$($c.Name): ${rel}:$($i + 1): $($line.Trim())" }
            }
        }
    }
}
$cap = Join-Path $root 'src-tauri\capabilities\main.json'
if (Test-Path -LiteralPath $cap) {
    $j = Get-Content -LiteralPath $cap -Raw | ConvertFrom-Json
    if ($null -ne $j.remote) { $violations += 'capability: 不允许 remote 字段' }
    foreach ($p in @($j.permissions)) {
        $id = if ($p -is [string]) { $p } else { $p.identifier }
        if ($id -match '^(core:default|fs:|shell:|opener:|process:|http:)') { $violations += "capability: 不允许的权限 $id" }
    }
}
if ($violations.Count -gt 0) {
    $violations | ForEach-Object { Write-Host $_ }
    Write-Host "共 $($violations.Count) 处违规"
    exit 1
}
Write-Host "check-forbidden-apis: OK（检查了 $(@($files).Count) 个文件）"
