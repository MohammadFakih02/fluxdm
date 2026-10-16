# Builds the files to hand out or publish as a GitHub release:
#   dist\FluxDM-Setup-<version>.exe   per-user installer (no admin), Start Menu + .torrent association
#   dist\FluxDM.exe + FluxDM.exe.sha256   portable copy; also what "Check for updates" downloads
# Needs the Tauri CLI (cargo install tauri-cli). NSIS is fetched by Tauri on first use.
$ErrorActionPreference = "Stop"
$env:Path = "$env:USERPROFILE\.cargo\bin;$env:Path"
$target = if ($env:CARGO_TARGET_DIR) { $env:CARGO_TARGET_DIR } else { "$PSScriptRoot\src-tauri\target" }
Remove-Item "$target\release\build\fluxdm-*\out\tauri-codegen-assets" -Recurse -Force -ErrorAction SilentlyContinue

Push-Location "$PSScriptRoot\src-tauri"
$ErrorActionPreference = "Continue"  # cargo writes progress to stderr
try { cargo tauri build; if ($LASTEXITCODE) { throw "tauri build failed" } } finally { Pop-Location; $ErrorActionPreference = "Stop" }

$version = (Get-Content "$PSScriptRoot\src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json).version
$dist = Join-Path $PSScriptRoot "dist"
New-Item -ItemType Directory -Force $dist | Out-Null
$setup = Get-ChildItem "$target\release\bundle\nsis\*.exe" | Sort-Object LastWriteTime -Descending | Select-Object -First 1
Copy-Item $setup.FullName (Join-Path $dist "FluxDM-Setup-$version.exe") -Force
$exe = if (Test-Path "$target\release\FluxDM.exe") { "$target\release\FluxDM.exe" } else { "$target\release\fluxdm.exe" }
Copy-Item $exe (Join-Path $dist "FluxDM.exe") -Force
$hash = (Get-FileHash (Join-Path $dist "FluxDM.exe") -Algorithm SHA256).Hash.ToLower()
[IO.File]::WriteAllText((Join-Path $dist "FluxDM.exe.sha256"), "$hash  FluxDM.exe`n")
Get-ChildItem $dist | Select-Object Name, @{n = 'MB'; e = { [math]::Round($_.Length / 1MB, 1) } } | Format-Table -AutoSize
