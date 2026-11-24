$ErrorActionPreference = 'Stop'

$root = Split-Path -Parent $PSScriptRoot
$cargoToml = Get-Content -Raw -LiteralPath (Join-Path $root 'Cargo.toml')
if ($cargoToml -notmatch '(?m)^version\s*=\s*"([^"]+)"') {
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

& $cargo build --locked --release -p grandhonor-panel --target-dir $target
if ($LASTEXITCODE -ne 0) { throw 'GPUI panel build failed.' }

$panelInput = Join-Path $target 'release\grandhonor-panel.exe'
$panelHash = (Get-FileHash -LiteralPath $panelInput -Algorithm SHA256).Hash
$env:GRANDHONOR_PANEL_HASH = $panelHash

& $cargo build --locked --release -p grandhonor --target-dir $target
if ($LASTEXITCODE -ne 0) { throw 'Host build failed.' }

if (Test-Path -LiteralPath $payload) { Remove-Item -LiteralPath $payload -Recurse -Force }
$null = New-Item -ItemType Directory -Path $payload -Force

$hostExe = Join-Path $payload 'GrandHonor.exe'
$panelExe = Join-Path $payload 'GrandHonor.Panel.exe'
Copy-Item -LiteralPath (Join-Path $target 'release\grandhonor.exe') -Destination $hostExe -Force
Copy-Item -LiteralPath $panelInput -Destination $panelExe -Force
Copy-Item -LiteralPath (Join-Path $root 'LICENSE') -Destination (Join-Path $payload 'LICENSE.txt') -Force
Copy-Item -LiteralPath (Join-Path $root 'Assets\fonts\OFL.txt') -Destination (Join-Path $payload 'Inter-OFL.txt') -Force
Copy-Item -LiteralPath (Join-Path $root 'Assets\GrandHonor.ico') -Destination (Join-Path $payload 'GrandHonor.ico') -Force

$hostHash = (Get-FileHash -LiteralPath $hostExe -Algorithm SHA256).Hash
$spec = Join-Path $PSScriptRoot 'GrandHonor.iss'
$branding = Join-Path $root 'obj\installer-branding'
& (Join-Path $PSScriptRoot 'theme.ps1') -OutputDirectory $branding
& $iscc "/DAppVersion=$version" "/DPayloadDir=$payload" "/DHostHash=$hostHash" "/DPanelHash=$panelHash" "/DBrandDir=$branding" $spec
if ($LASTEXITCODE -ne 0) { throw 'Installer compilation failed.' }
