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

# Third-party notices: every crate in the shipped binaries with its version, licence and source.
function Write-ThirdPartyNotices([string]$Destination) {
    $meta = & $cargoPath metadata --locked --format-version 1 | ConvertFrom-Json
    $lines = @('GrandHonor includes these open-source components. Each is used under its own licence;', 'licence texts are at the listed source.', '')
    foreach ($package in ($meta.packages | Where-Object { $_.source } | Sort-Object name, version)) {
        $lines += '{0} {1} - {2} - {3}' -f $package.name, $package.version, $package.license, ($(if ($package.repository) { $package.repository } else { "https://crates.io/crates/$($package.name)" }))
    }
    $lines += '', 'Inter font - SIL Open Font License 1.1 (Inter-OFL.txt)'
    Set-Content -LiteralPath $Destination -Value $lines -Encoding utf8
}

# The browser extension, when its folder is given (GRANDHONOR_EXTENSION_DIR). Model weights
# whose licence does not allow redistribution (non-commercial) are left out.
function Copy-BrowserExtension([string]$Destination) {
    $source = $env:GRANDHONOR_EXTENSION_DIR
    if (!$source -or !(Test-Path -LiteralPath (Join-Path $source 'manifest.json') -PathType Leaf)) { return }
    $target = Join-Path $Destination 'BrowserVsr'
    $null = New-Item -ItemType Directory -Path $target -Force
    Get-ChildItem -LiteralPath $source -Recurse -File | Where-Object {
        $_.Name -notlike 'liveaction-webnn*' -and $_.Name -ne 'intel.js' -and $_.Name -notlike '*.md' -and $_.Name -notlike '*.pem'
    } | ForEach-Object {
        $relative = $_.FullName.Substring($source.TrimEnd('\').Length + 1)
        $out = Join-Path $target $relative
        $null = New-Item -ItemType Directory -Path (Split-Path -Parent $out) -Force
        Copy-Item -LiteralPath $_.FullName -Destination $out -Force
    }
    Add-Content -LiteralPath (Join-Path $Destination 'THIRD-PARTY-NOTICES.txt') -Encoding utf8 -Value 'RIFE 4.25-lite model weights (Practical-RIFE, hzwer) - MIT - https://github.com/hzwer/Practical-RIFE'
}

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
Write-ThirdPartyNotices (Join-Path $bundle 'THIRD-PARTY-NOTICES.txt')
Copy-BrowserExtension $bundle

$entries = @(Get-ChildItem -LiteralPath $bundle | ForEach-Object FullName)
Compress-Archive -LiteralPath $entries -DestinationPath $archive -CompressionLevel Optimal -Force
Write-Host "Built $archive"
