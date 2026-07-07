# qb installer (Windows)
#
# Usage:
#   irm https://github.com/HectorBjernersjo/querybench/releases/latest/download/install.ps1 | iex
#
# Env vars:
#   QB_VERSION     Pin a specific tag (e.g. v0.1.0). Defaults to "latest".
#   QB_INSTALL_DIR Where to drop the binary. Defaults to %LOCALAPPDATA%\qb\bin.

$ErrorActionPreference = 'Stop'

$repo = 'HectorBjernersjo/querybench'
$version = if ($env:QB_VERSION) { $env:QB_VERSION } else { 'latest' }
$installDir = if ($env:QB_INSTALL_DIR) {
    $env:QB_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'qb\bin'
}

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { throw "unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)" }
}

$target = "$arch-pc-windows-msvc"
$asset = "qb-$target.zip"
$url = if ($version -eq 'latest') {
    "https://github.com/$repo/releases/latest/download/$asset"
} else {
    "https://github.com/$repo/releases/download/$version/$asset"
}

Write-Host "Installing qb ($version) for $target to $installDir"

New-Item -ItemType Directory -Force -Path $installDir | Out-Null

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "qb-install-$([guid]::NewGuid())")
try {
    $zipPath = Join-Path $tmp.FullName 'qb.zip'
    Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing
    Expand-Archive -Path $zipPath -DestinationPath $tmp.FullName -Force

    $binary = Get-ChildItem -Path $tmp.FullName -Filter 'qb.exe' -Recurse | Select-Object -First 1
    if (-not $binary) { throw 'release archive did not contain qb.exe' }

    Move-Item -Path $binary.FullName -Destination (Join-Path $installDir 'qb.exe') -Force
} finally {
    Remove-Item -Recurse -Force $tmp.FullName -ErrorAction SilentlyContinue
}

Write-Host "Installed: $(Join-Path $installDir 'qb.exe')"

Write-Host ""
Write-Host "Get started:"
Write-Host ""
Write-Host "  qb           # launch the TUI (press 'a' to add a database)"
Write-Host "  qb --check   # test config + connection + schema headlessly"
Write-Host "  ?            # in the app: help / all keybindings"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$pathEntries = if ($userPath) { $userPath -split ';' } else { @() }
if ($pathEntries -notcontains $installDir) {
    $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host ""
    Write-Host "Added $installDir to your user PATH. Open a new terminal for the change to take effect."
}
