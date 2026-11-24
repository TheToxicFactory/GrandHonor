$ErrorActionPreference = 'Stop'

$root = $PSScriptRoot
$dist = Join-Path $root 'dist'
$bundle = Join-Path $dist 'GrandHonor'
$targetDir = Join-Path $root 'target'
$archive = Join-Path $dist 'GrandHonor-win-x64.zip'

$cargo = Get-Command cargo -ErrorAction SilentlyContinue
$cargoPath = if ($cargo) { $cargo.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (!(Test-Path -LiteralPath $cargoPath -PathType Leaf)) { throw 'Rust Cargo was not found.' }

$env:PATH = "$(Split-Path -Parent $cargoPath);$env:PATH"

& $cargoPath build --locked --release -p grandhonor-panel --target-dir $targetDir
if ($LASTEXITCODE -ne 0) { throw "GPUI panel build failed ($LASTEXITCODE)." }

$panelInput = Join-Path $targetDir 'release\grandhonor-panel.exe'
$panelHash = (Get-FileHash -LiteralPath $panelInput -Algorithm SHA256).Hash
$env:GRANDHONOR_PANEL_HASH = $panelHash

& $cargoPath build --locked --release -p grandhonor --target-dir $targetDir
if ($LASTEXITCODE -ne 0) { throw "Host build failed ($LASTEXITCODE)." }

if (Test-Path -LiteralPath $bundle) { Remove-Item -LiteralPath $bundle -Recurse -Force }
$null = New-Item -ItemType Directory -Path $bundle -Force

$hostInput = Join-Path $targetDir 'release\grandhonor.exe'
Copy-Item -LiteralPath $hostInput -Destination (Join-Path $bundle 'GrandHonor.exe') -Force
Copy-Item -LiteralPath $panelInput -Destination (Join-Path $bundle 'GrandHonor.Panel.exe') -Force
Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination (Join-Path $bundle 'LICENSE.txt') -Force
Copy-Item -LiteralPath (Join-Path $root 'Assets\fonts\OFL.txt') -Destination (Join-Path $bundle 'Inter-OFL.txt') -Force
Copy-Item -LiteralPath (Join-Path $root 'Assets\GrandHonor.ico') -Destination (Join-Path $bundle 'GrandHonor.ico') -Force

$entries = @(Get-ChildItem -LiteralPath $bundle | ForEach-Object FullName)
Compress-Archive -LiteralPath $entries -DestinationPath $archive -CompressionLevel Optimal -Force
Write-Host "Built $archive"
