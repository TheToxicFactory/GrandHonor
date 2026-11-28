param([string]$OutputDirectory = '')

$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$cargoToml = Get-Content -Raw -LiteralPath (Join-Path $root 'Cargo.toml')
if ($cargoToml -notmatch '(?m)^version\s*=\s*"(\d+\.\d+\.\d+)"') {
    throw 'Could not determine version from Cargo.toml.'
}
$version = $Matches[1]

$payload = Join-Path $root "obj\installer-runtime-$version"
$target = Join-Path $root 'target'

$compiler = Get-Command ISCC.exe -ErrorAction SilentlyContinue
$iscc = if ($compiler) {
    $compiler.Source
} else {
    @(Join-Path $env:ProgramFiles 'Inno Setup 7\ISCC.exe'
        Join-Path $env:LOCALAPPDATA 'Programs\Inno Setup 7\ISCC.exe') |
        Where-Object { Test-Path -LiteralPath $_ } | Select-Object -First 1
}
if (!$iscc) { throw 'Inno Setup 7 is required.' }

$cargoCommand = Get-Command cargo -ErrorAction SilentlyContinue
$cargo = if ($cargoCommand) { $cargoCommand.Source } else { Join-Path $env:USERPROFILE '.cargo\bin\cargo.exe' }
if (!(Test-Path -LiteralPath $cargo -PathType Leaf)) { throw 'Rust Cargo was not found.' }
$env:PATH = "$(Split-Path -Parent $cargo);$env:PATH"
$cargoPath = $cargo

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

& $cargo build --locked --release -p grandhonor-panel --target-dir $target
if ($LASTEXITCODE -ne 0) { throw 'GPUI panel build failed.' }

$panelInput = Join-Path $target 'release\grandhonor-panel.exe'
$panelHash = (Get-FileHash -LiteralPath $panelInput -Algorithm SHA256).Hash
$env:GRANDHONOR_PANEL_HASH = $panelHash

& $cargo build --locked --release -p grandhonor --target-dir $target
if ($LASTEXITCODE -ne 0) { throw 'Host build failed.' }


if (Test-Path -LiteralPath $payload) {
    $existingPayload = Get-Item -LiteralPath $payload -Force
    $expectedPayload = [IO.Path]::GetFullPath($payload)
    if (!$existingPayload.PSIsContainer -or $existingPayload.FullName -ne $expectedPayload -or
        ($existingPayload.Attributes -band [IO.FileAttributes]::ReparsePoint) -or
        ($existingPayload.Parent.Attributes -band [IO.FileAttributes]::ReparsePoint)) {
        throw "Installer payload is not a regular directory: $payload"
    }
    Remove-Item -LiteralPath $existingPayload.FullName -Recurse -Force
}
$null = New-Item -ItemType Directory -Path $payload -Force

$hostExe = Join-Path $payload 'GrandHonor.exe'
$panelExe = Join-Path $payload 'GrandHonor.Panel.exe'
Copy-Item -LiteralPath (Join-Path $target 'release\grandhonor.exe') -Destination $hostExe -Force
Copy-Item -LiteralPath $panelInput -Destination $panelExe -Force
Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination (Join-Path $payload 'LICENSE.txt') -Force
Copy-Item -LiteralPath (Join-Path $root 'Assets\fonts\OFL.txt') -Destination (Join-Path $payload 'Inter-OFL.txt') -Force
Copy-Item -LiteralPath (Join-Path $root 'Assets\GrandHonor.ico') -Destination (Join-Path $payload 'GrandHonor.ico') -Force
Write-ThirdPartyNotices (Join-Path $payload 'THIRD-PARTY-NOTICES.txt')
Copy-BrowserExtension $payload

$hostHash = (Get-FileHash -LiteralPath $hostExe -Algorithm SHA256).Hash
$spec = Join-Path $PSScriptRoot 'GrandHonor.iss'
$branding = Join-Path $root 'obj\installer-branding'
& (Join-Path $PSScriptRoot 'theme.ps1') -OutputDirectory $branding
$outputOption = @()
if ($OutputDirectory) {
    $outputPath = $ExecutionContext.SessionState.Path.GetUnresolvedProviderPathFromPSPath($OutputDirectory)
    $null = New-Item -ItemType Directory -Path $outputPath -Force
    $outputOption = @("/O$outputPath")
}
& $iscc @outputOption "/DAppVersion=$version" "/DPayloadDir=$payload" "/DHostHash=$hostHash" "/DPanelHash=$panelHash" "/DBrandDir=$branding" $spec
if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed.' }
