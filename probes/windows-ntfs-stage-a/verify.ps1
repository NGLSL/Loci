param([ValidatePattern('^[A-Za-z]:$')][string]$Volume = 'D:')
$ErrorActionPreference = 'Stop'
$probeRoot = $PSScriptRoot
$engineeringRoot = [IO.Path]::GetFullPath((Join-Path $probeRoot '../..'))
$probeRun = Join-Path $engineeringRoot '.scratch/windows-ntfs-stage-a/run'
$probeTarget = Join-Path $engineeringRoot '.scratch/windows-ntfs-stage-a/target'
$manifest = Join-Path $probeRoot 'Cargo.toml'
New-Item -ItemType Directory -Force -Path $probeRun | Out-Null

& cargo +1.99.0 fmt --check --manifest-path $manifest
if ($LASTEXITCODE -ne 0) { throw 'probe format check failed' }
& cargo +1.99.0 check --all-targets --offline --locked --manifest-path $manifest --target-dir $probeTarget
if ($LASTEXITCODE -ne 0) { throw 'probe static check failed' }
foreach ($profile in @('debug', 'release')) {
    $profileArgs = @()
    if ($profile -eq 'release') { $profileArgs = @('--release') }
    & cargo +1.99.0 test @profileArgs --offline --locked --manifest-path $manifest --target-dir $probeTarget -- --test-threads=1
    if ($LASTEXITCODE -ne 0) { throw "probe $profile tests failed" }
    & cargo +1.99.0 build @profileArgs --offline --locked --manifest-path $manifest --target-dir $probeTarget
    if ($LASTEXITCODE -ne 0) { throw "probe $profile build failed" }
    $probeBinary = Join-Path $probeTarget "$profile/loci-ntfs-stage-a.exe"
    # A rejected volume capability is an expected BLOCKED evidence result, not
    # a successful empty index. Save its actual nonzero exit code separately.
    & $probeBinary capabilities-repeat $Volume 2>&1 | Tee-Object -FilePath (Join-Path $probeRun "capabilities-$profile.txt")
    $capabilityExit = $LASTEXITCODE
    "capability_exit=$capabilityExit" | Add-Content -LiteralPath (Join-Path $probeRun "capabilities-$profile.txt")
    & $probeBinary fixture $probeRun 2>&1 | Tee-Object -FilePath (Join-Path $probeRun "fixture-$profile.txt")
    if ($LASTEXITCODE -ne 0) { throw "probe $profile native fixture failed" }
}
Write-Output 'Probe checks/fixtures completed. Read capability_exit and API errors before considering NTFS validation. No MFT volume enumeration was run.'
