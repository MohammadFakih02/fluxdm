# Makes the two things to hand out, in dist\share\:
#   FluxDM-Setup-<version>.exe + "How to install FluxDM.txt"   for people who just want to use it
#   FluxDM-<version>-source.zip                                 the source, without anything generated
# Builds the setup first (build-setup.ps1) unless -NoBuild is given.
param([switch]$NoBuild)
$ErrorActionPreference = "Stop"
$root = $PSScriptRoot
if (-not $NoBuild) { & "$root\build-setup.ps1" | Out-Null }

$version = (Get-Content "$root\src-tauri\tauri.conf.json" -Raw | ConvertFrom-Json).version
$setup = Join-Path $root "dist\FluxDM-Setup-$version.exe"
if (-not (Test-Path $setup)) { throw "No $setup - run without -NoBuild." }
if (-not (Test-Path "$root\src-tauri\extension\signed\FluxDM-extension.xpi")) {
    Write-Host "Warning: no signed extension; regular Firefox won't accept the built-in one." -ForegroundColor Yellow
}

$share = Join-Path $root "dist\share"
if (Test-Path $share) { Remove-Item $share -Recurse -Force }
New-Item -ItemType Directory $share | Out-Null
Copy-Item $setup $share
# Notepad on older Windows 10 needs CRLF line endings.
$notes = (Get-Content "$root\How to install FluxDM.txt" -Raw) -replace "`r?`n", "`r`n"
[IO.File]::WriteAllText((Join-Path $share "How to install FluxDM.txt"), $notes, (New-Object Text.UTF8Encoding $false))

# Source: everything except build output. Paths inside the zip use forward slashes, under one top folder.
$skip = '^(dist|src-tauri\\target|\.vscode|\.idea)(\\|$)'
$top = "FluxDM-$version-source"
$zipPath = Join-Path $share "$top.zip"
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::Open($zipPath, "Create")
$count = 0
try {
    Get-ChildItem $root -Recurse -File -Force | ForEach-Object {
        $rel = $_.FullName.Substring($root.Length + 1)
        if ($rel -notmatch $skip) {
            [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $_.FullName, "$top/" + $rel.Replace("\", "/"), "Optimal")
            $count++
        }
    }
} finally { $zip.Dispose() }

"Source: $count files"
Get-ChildItem $share | Select-Object Name, @{n = 'MB'; e = { [math]::Round($_.Length / 1MB, 1) } } | Format-Table -AutoSize
