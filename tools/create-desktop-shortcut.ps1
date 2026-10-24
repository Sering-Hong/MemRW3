$ErrorActionPreference = 'Stop'
$exe = Join-Path $PSScriptRoot 'MemRW3.exe'
$icon = Join-Path $PSScriptRoot 'assets\MemRW3-pink.ico'
if (-not (Test-Path -LiteralPath $exe)) { throw 'MemRW3.exe was not found' }
$desktop = [Environment]::GetFolderPath('Desktop')
$shell = New-Object -ComObject WScript.Shell
$linkPath = Join-Path $desktop 'MemRW3.lnk'
$link = $shell.CreateShortcut($linkPath)
$link.TargetPath = $exe
$link.WorkingDirectory = $PSScriptRoot
$link.IconLocation = "$icon,0"
$link.Description = 'MemRW3 - Variable Watch and Full Capture History'
$link.WindowStyle = 1
$link.Save()
$check = $shell.CreateShortcut($linkPath)
if ($check.TargetPath -ne $exe -or $check.WorkingDirectory -ne $PSScriptRoot) { throw 'Shortcut verification failed' }
[pscustomobject]@{Shortcut=$linkPath;Target=$check.TargetPath;Icon=$check.IconLocation;WorkingDirectory=$check.WorkingDirectory} | ConvertTo-Json
