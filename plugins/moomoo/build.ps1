# Builds the moomoo plugin package for Windows: compiles the plugin program, zips it with the manifest set for
# Windows, signs the zip with the release key (the catalog's packages must be signed) and writes the catalog entry.
#
#   ./build.ps1 [-Key ~/.tauri/opentrader-updater.key]
#
# Out: plugins/moomoo/dist/moomoo-<version>.zip, .zip.sig and catalog-entry.json. A missing key stops the build
# before any catalog entry is written.
param(
  [string]$Key = "$HOME\.tauri\opentrader-updater.key"
)
$ErrorActionPreference = "Stop"
$here = $PSScriptRoot
$manifest = Get-Content "$here\opentrader-plugin.json" -Raw | ConvertFrom-Json
$version = $manifest.version
$cargoVersion = (Select-String -Path "$here\Cargo.toml" -Pattern '^version = "(.+)"' | Select-Object -First 1).Matches[0].Groups[1].Value
if ($cargoVersion -ne $version) { throw "Cargo.toml says $cargoVersion, opentrader-plugin.json says $version" }
if (-not (Test-Path $Key)) { throw "No release key at ${Key}: the package cannot be signed, so no catalog entry is written" }
$dist = "$here\dist"
$stage = "$dist\stage"
Remove-Item -Recurse -Force $stage -ErrorAction SilentlyContinue
New-Item -ItemType Directory -Force $stage | Out-Null

Push-Location $here
cargo build --release
$built = $LASTEXITCODE
Pop-Location
if ($built -ne 0) { throw "The plugin did not build" }
Copy-Item "$here\target\release\opentrader-moomoo.exe" "$stage\opentrader-moomoo.exe"
# The Windows package's manifest: its program and its platform.
$manifest.runtime.entry = "opentrader-moomoo.exe"
$manifest.platforms = @("windows")
# UTF-8 without a byte order mark, in Windows PowerShell 5 as in PowerShell 7.
[IO.File]::WriteAllText("$stage\opentrader-plugin.json", ($manifest | ConvertTo-Json -Depth 6))

$zip = "$dist\moomoo-$version.zip"
Remove-Item -Force $zip, "$zip.sig", "$dist\catalog-entry.json" -ErrorAction SilentlyContinue
Compress-Archive -Path "$stage\*" -DestinationPath $zip
Remove-Item -Recurse -Force $stage

# The release key's password comes from the environment, as in the app's release workflow.
if (-not $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD -and (Test-Path "$Key.password")) {
  $env:TAURI_SIGNING_PRIVATE_KEY_PASSWORD = (Get-Content "$Key.password" -Raw).Trim()
}
Push-Location "$here\..\.."
npx tauri signer sign -f $Key $zip | Out-Null
Pop-Location
if (-not (Test-Path "$zip.sig")) { throw "The package was not signed" }

$sha = (Get-FileHash $zip -Algorithm SHA256).Hash.ToLower()
$entry = [ordered]@{
  id = $manifest.id; name = $manifest.name; version = $version; publisher = $manifest.publisher; description = $manifest.description
  provides = $manifest.provides; api = $manifest.api; platforms = @("windows"); homepage = $manifest.homepage
  url = "https://github.com/deepentropy/opentrader/releases/download/plugin-moomoo-v$version/moomoo-$version.zip"
  sha256 = $sha; signature = (Get-Content "$zip.sig" -Raw).Trim()
}
[IO.File]::WriteAllText("$dist\catalog-entry.json", ($entry | ConvertTo-Json -Depth 4))
Write-Host "Package: $zip"
Write-Host "Catalog entry: $dist\catalog-entry.json"
