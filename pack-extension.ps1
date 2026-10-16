# Packs src-tauri\extension for the add-on stores:
#   dist\FluxDM-extension-firefox.zip   addons.mozilla.org ("On your own" = unlisted, signed in minutes)
#   dist\FluxDM-extension-chromium.zip  Chrome Web Store, Edge Add-ons, Opera add-ons
# The Chromium copy gets a manifest without the Firefox-only parts (same rule as ext.rs chromium_manifest).
# Bump "version" in src-tauri\extension\manifest.json first: stores refuse a version they already have.
# Put the signed .xpi from Mozilla at src-tauri\extension\signed\FluxDM-extension.xpi, then rebuild FluxDM.
$ErrorActionPreference = "Stop"
$src = Join-Path $PSScriptRoot "src-tauri\extension"
$dist = Join-Path $PSScriptRoot "dist"
New-Item -ItemType Directory -Force $dist | Out-Null
Add-Type -AssemblyName System.IO.Compression, System.IO.Compression.FileSystem
$manifest = Get-Content (Join-Path $src "manifest.json") -Raw

$m = $manifest | ConvertFrom-Json
$m.PSObject.Properties.Remove("browser_specific_settings")
$m.background.PSObject.Properties.Remove("scripts")
$m.permissions = @($m.permissions | Where-Object { $_ -ne "webRequestBlocking" })
$chromium = $m | ConvertTo-Json -Depth 10

function Pack($out, $manifestText) {
    if (Test-Path $out) { Remove-Item $out }
    $zip = [IO.Compression.ZipFile]::Open($out, "Create")
    try {
        Get-ChildItem -Recurse -File $src | Where-Object { $_.FullName -notlike "$src\signed\*" } | ForEach-Object {
            # Stores want forward slashes inside the zip.
            $rel = $_.FullName.Substring($src.Length + 1).Replace("\", "/")
            if ($rel -eq "manifest.json") {
                $w = New-Object IO.StreamWriter(($zip.CreateEntry($rel)).Open(), (New-Object Text.UTF8Encoding $false))
                $w.Write($manifestText); $w.Dispose()
            } else {
                [void][IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $_.FullName, $rel)
            }
        }
    } finally { $zip.Dispose() }
    "Packed extension $($m.version) -> $out"
}
Pack (Join-Path $dist "FluxDM-extension-firefox.zip") $manifest
Pack (Join-Path $dist "FluxDM-extension-chromium.zip") $chromium
