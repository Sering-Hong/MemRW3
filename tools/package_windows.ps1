param([switch]$SkipBuild, [string]$Python = 'python.exe')
$ErrorActionPreference = 'Stop'
$repo = Split-Path -Parent $PSScriptRoot
function Invoke-Native {
    param([string]$FilePath, [string[]]$Arguments, [switch]$Capture)
    $command = Get-Command -Name $FilePath -CommandType Application -ErrorAction Stop
    $quoted = $Arguments | ForEach-Object {
        '"' + ($_ -replace '(\\*)"', '$1$1\"' -replace '(\\+)$', '$1$1') + '"'
    }
    $options = @{
        FilePath = $command.Source; ArgumentList = ($quoted -join ' ')
        WorkingDirectory = $repo; NoNewWindow = $true; Wait = $true; PassThru = $true
    }
    $outputFile = $null
    try {
        if ($Capture) {
            $outputFile = [IO.Path]::GetTempFileName()
            $options.RedirectStandardOutput = $outputFile
        }
        $process = Start-Process @options
        if ($process.ExitCode -ne 0) { throw "$FilePath failed with exit code $($process.ExitCode)" }
        if ($Capture) { Get-Content -LiteralPath $outputFile -Encoding UTF8 }
    } finally {
        if ($outputFile) { Remove-Item -LiteralPath $outputFile -Force }
    }
}
Push-Location -LiteralPath $repo
try {
    if (-not $SkipBuild) {
        Invoke-Native -FilePath 'cargo.exe' -Arguments @('build', '--release', '--locked')
        Invoke-Native -FilePath 'cargo.exe' -Arguments @('test', '--release', '--locked')
    }
    $targetDir = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { Join-Path $repo 'target' }
    if (-not [IO.Path]::IsPathRooted($targetDir)) { $targetDir = Join-Path $repo $targetDir }
    $exe = Join-Path $targetDir 'release\MemRW3.exe'
    if (-not (Test-Path -LiteralPath $exe)) { throw "Executable not found: $exe" }
    $dist = Join-Path $repo 'dist'
    $stage = Join-Path $dist 'MemRW3'
    $archive = Join-Path $dist 'MemRW3-windows.zip'
    New-Item -ItemType Directory -Path $dist -Force | Out-Null
    # Only recreate the dedicated staging directory under this repository's dist.
    if ((Split-Path -Parent $stage) -ne $dist) { throw 'Invalid staging directory' }
    if (Test-Path -LiteralPath $stage) { Remove-Item -LiteralPath $stage -Recurse -Force }
    New-Item -ItemType Directory -Path $stage | Out-Null
    Copy-Item -LiteralPath $exe -Destination (Join-Path $stage 'MemRW3.exe')
    New-Item -ItemType Directory -Path (Join-Path $stage 'assets') | Out-Null
    Copy-Item -LiteralPath (Join-Path $repo 'assets\MemRW3-pink.png') -Destination (Join-Path $stage 'assets')
    Copy-Item -LiteralPath (Join-Path $repo 'assets\MemRW3-pink.ico') -Destination (Join-Path $stage 'assets')
    Invoke-Native -FilePath $Python -Arguments @((Join-Path $repo 'tools\set_windows_icon.py'), (Join-Path $stage 'MemRW3.exe'), (Join-Path $stage 'assets\MemRW3-pink.ico'))
    Copy-Item -LiteralPath (Join-Path $repo 'tools\create-desktop-shortcut.ps1') -Destination (Join-Path $stage '创建桌面快捷方式.ps1')
    Copy-Item -LiteralPath (Join-Path $repo 'docs\使用说明.md') -Destination (Join-Path $stage '使用说明.md')
    Copy-Item -LiteralPath (Join-Path $repo 'docs\项目技术文档.md') -Destination (Join-Path $stage '项目技术文档.md')
    Copy-Item -LiteralPath (Join-Path $repo 'CHANGELOG.md') -Destination $stage
    @'
# MemRW3 Enhanced Pink

运行 MemRW3.exe，详细步骤见“使用说明.md”。完整源码、技术及构建说明见 source/README.md。

保留 Watch 表格、完整曲线历史、粉色图标和原有 Debug 调试器。源码修改和编译请在外部 IDE 进行。

GitHub: https://github.com/RP27-EC/MemRW3
'@ | Set-Content -LiteralPath (Join-Path $stage 'README.md') -Encoding UTF8
    $sourceOut = Join-Path $stage 'source'
    $tracked = @(Invoke-Native -FilePath 'git.exe' -Arguments @('-c', 'core.quotepath=false', 'ls-files') -Capture)
    if ($tracked.Count -eq 0) { throw 'Cannot enumerate version-controlled source files' }
    foreach ($name in $tracked) {
        $dest = Join-Path $sourceOut $name
        New-Item -ItemType Directory -Path (Split-Path -Parent $dest) -Force | Out-Null
        Copy-Item -LiteralPath (Join-Path $repo $name) -Destination $dest
    }
    @{
        name = 'MemRW3'; edition = 'Enhanced Pink'; base_version = 'v2.2.1'
        packaged_date = (Get-Date -Format 'yyyy-MM-dd'); portable = $true
        repository = 'https://github.com/RP27-EC/MemRW3'
        executable_sha256 = (Get-FileHash -LiteralPath (Join-Path $stage 'MemRW3.exe') -Algorithm SHA256).Hash.ToLowerInvariant()
    } | ConvertTo-Json | Set-Content -LiteralPath (Join-Path $stage 'package-info.json') -Encoding UTF8
    $prefix = $stage.TrimEnd('\') + '\'
    $sums = Get-ChildItem -LiteralPath $stage -Recurse -File | Sort-Object FullName | ForEach-Object {
        $hash = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant()
        $relative = $_.FullName.Substring($prefix.Length).Replace('\', '/')
        "$hash  $relative"
    }
    $sums | Set-Content -LiteralPath (Join-Path $stage 'SHA256SUMS.txt') -Encoding UTF8
    if (Test-Path -LiteralPath $archive) { Remove-Item -LiteralPath $archive -Force }
    Compress-Archive -LiteralPath $stage -DestinationPath $archive -CompressionLevel Optimal
    $zipHash = (Get-FileHash -LiteralPath $archive -Algorithm SHA256).Hash.ToLowerInvariant()
    "$zipHash  MemRW3-windows.zip" | Set-Content -LiteralPath ($archive + '.sha256') -Encoding ASCII
    Write-Output "Created $archive"
    Write-Output "SHA256 $zipHash"
} finally {
    Pop-Location
}
