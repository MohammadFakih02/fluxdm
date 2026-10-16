# Builds FluxDM and installs it for the current user (no admin needed):
#   %LOCALAPPDATA%\FluxDM\FluxDM.exe + a Start Menu shortcut.
# For a setup .exe to give to other people, run build-setup.ps1 instead.
param([switch]$NoBuild)
$ErrorActionPreference = "Stop"
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$PSScriptRoot\src-tauri\target" }

if (-not $NoBuild) {
    # Tauri caches compressed copies of ui/ and never rewrites them; a build cut
    # short (e.g. a PC crash) leaves broken copies and the window says "asset not found".
    Remove-Item "$target\release\build\fluxdm-*\out\tauri-codegen-assets" -Recurse -Force -ErrorAction SilentlyContinue
    Push-Location "$PSScriptRoot\src-tauri"
    $ErrorActionPreference = "Continue"  # cargo writes progress to stderr
    try { cargo build --release; if ($LASTEXITCODE) { throw "cargo build failed" } } finally { Pop-Location; $ErrorActionPreference = "Stop" }
}

$dir = Join-Path $env:LOCALAPPDATA "FluxDM"
$exe = Join-Path $dir "FluxDM.exe"
if (Get-Process FluxDM -ErrorAction SilentlyContinue) {
    # A clean quit: pauses downloads and saves torrent resume data first.
    if (Test-Path $exe) { & $exe --exit; Start-Sleep -Seconds 4 }
    if (Get-Process FluxDM -ErrorAction SilentlyContinue) {
        Write-Host "FluxDM is still running. Quit it from the tray icon, then run this again." -ForegroundColor Yellow
        exit 1
    }
}
New-Item -ItemType Directory -Force $dir | Out-Null
Copy-Item "$target\release\fluxdm.exe" $exe -Force
$sh = New-Object -ComObject WScript.Shell
$lnk = $sh.CreateShortcut("$env:APPDATA\Microsoft\Windows\Start Menu\Programs\FluxDM.lnk")
$lnk.TargetPath = $exe
$lnk.WorkingDirectory = $dir
$lnk.Description = "Download manager - files, videos and torrents"
$lnk.Save()
Write-Host "Installed to $dir" -ForegroundColor Green
