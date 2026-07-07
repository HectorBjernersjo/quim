# quim installer (Windows)
#
# Usage:
#   irm https://github.com/HectorBjernersjo/quim/releases/latest/download/install.ps1 | iex
#
# Env vars:
#   QUIM_VERSION     Pin a specific tag (e.g. v0.1.0). Defaults to "latest".
#   QUIM_INSTALL_DIR Where to drop the binary. Defaults to %LOCALAPPDATA%\quim\bin.

$ErrorActionPreference = 'Stop'

$repo = 'HectorBjernersjo/quim'
$version = if ($env:QUIM_VERSION) { $env:QUIM_VERSION } else { 'latest' }
$installDir = if ($env:QUIM_INSTALL_DIR) {
    $env:QUIM_INSTALL_DIR
} else {
    Join-Path $env:LOCALAPPDATA 'quim\bin'
}

$arch = switch ($env:PROCESSOR_ARCHITECTURE) {
    'AMD64' { 'x86_64' }
    'ARM64' { 'aarch64' }
    default { throw "unsupported architecture: $($env:PROCESSOR_ARCHITECTURE)" }
}

$target = "$arch-pc-windows-msvc"
$asset = "quim-$target.zip"
$url = if ($version -eq 'latest') {
    "https://github.com/$repo/releases/latest/download/$asset"
} else {
    "https://github.com/$repo/releases/download/$version/$asset"
}

Write-Host "Installing quim ($version) for $target to $installDir"

New-Item -ItemType Directory -Force -Path $installDir | Out-Null

$tmp = New-Item -ItemType Directory -Path (Join-Path $env:TEMP "quim-install-$([guid]::NewGuid())")
try {
    $zipPath = Join-Path $tmp.FullName 'quim.zip'
    Invoke-WebRequest -Uri $url -OutFile $zipPath -UseBasicParsing
    Expand-Archive -Path $zipPath -DestinationPath $tmp.FullName -Force

    $binary = Get-ChildItem -Path $tmp.FullName -Filter 'quim.exe' -Recurse | Select-Object -First 1
    if (-not $binary) { throw 'release archive did not contain quim.exe' }

    Move-Item -Path $binary.FullName -Destination (Join-Path $installDir 'quim.exe') -Force
} finally {
    Remove-Item -Recurse -Force $tmp.FullName -ErrorAction SilentlyContinue
}

Write-Host "Installed: $(Join-Path $installDir 'quim.exe')"

Write-Host ""
Write-Host "Get started:"
Write-Host ""
Write-Host "  quim           # launch the TUI (press 'a' to add a database)"
Write-Host "  quim --check   # test config + connection + schema headlessly"
Write-Host "  ?            # in the app: help / all keybindings"

$userPath = [Environment]::GetEnvironmentVariable('Path', 'User')
$pathEntries = if ($userPath) { $userPath -split ';' } else { @() }
if ($pathEntries -notcontains $installDir) {
    $newPath = if ($userPath) { "$userPath;$installDir" } else { $installDir }
    [Environment]::SetEnvironmentVariable('Path', $newPath, 'User')
    Write-Host ""
    Write-Host "Added $installDir to your user PATH. Open a new terminal for the change to take effect."
}
