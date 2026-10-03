[CmdletBinding()]
param([string]$Nsis = '')
$ErrorActionPreference = 'Stop'
$root = Split-Path $PSScriptRoot -Parent
$manifest = Join-Path $root 'Cargo.toml'
$output = Join-Path $root 'target/package'
$payload = Join-Path $output 'payload'
if ($env:OS -ne 'Windows_NT') { throw 'Loci packaging requires Windows.' }
if (-not $Nsis) {
    $Nsis = @((Get-Command makensis.exe -ErrorAction SilentlyContinue | Select-Object -ExpandProperty Source), 'C:\Program Files (x86)\NSIS\makensis.exe', 'C:\Program Files\NSIS\makensis.exe') |
        Where-Object { $_ -and (Test-Path -LiteralPath $_) } | Select-Object -First 1
}
if (-not $Nsis) { throw 'NSIS not found. Install NSIS before packaging Loci.' }
$metadataJson = & cargo +1.99.0 metadata --manifest-path $manifest --format-version 1 --locked
if ($LASTEXITCODE -ne 0) { throw 'Cannot resolve locked Cargo metadata.' }
$metadata = $metadataJson | ConvertFrom-Json
$package = $metadata.packages | Where-Object { $_.manifest_path -eq $manifest.Replace('\', '/') -or $_.manifest_path -eq $manifest } | Select-Object -First 1
if (-not $package) { throw 'Cannot find the Loci package version.' }
& cargo +1.99.0 build --manifest-path $manifest --release --locked --target x86_64-pc-windows-msvc --bin loci-service --bin loci --bin kite-plugin-loci
if ($LASTEXITCODE -ne 0) { throw 'Loci release build failed.' }
New-Item -ItemType Directory -Force -Path $payload | Out-Null
$binaryDirectory = Join-Path $metadata.target_directory 'x86_64-pc-windows-msvc/release'
Copy-Item -LiteralPath (Join-Path $binaryDirectory 'loci-service.exe'), (Join-Path $binaryDirectory 'loci.exe'), (Join-Path $PSScriptRoot 'install-service.ps1'), (Join-Path $root 'README.md'), (Join-Path $root 'THIRD_PARTY.md') -Destination $payload -Force
# Include the license files from the exact registry packages used by Cargo.
foreach ($dependency in $metadata.packages | Where-Object { $_.source -like 'registry+*' }) {
    $sourceDirectory = Split-Path $dependency.manifest_path -Parent
    $licenseFiles = @(Get-ChildItem -LiteralPath $sourceDirectory -File | Where-Object { $_.Name -match '^(LICENSE|COPYING|NOTICE)' })
    if (-not $licenseFiles) { throw "Missing license files for $($dependency.name) $($dependency.version)." }
    $licenseDirectory = Join-Path $payload "licenses/$($dependency.name)-$($dependency.version)"
    New-Item -ItemType Directory -Force -Path $licenseDirectory | Out-Null
    $licenseFiles | Copy-Item -Destination $licenseDirectory -Force
}
$installer = Join-Path $output 'loci-setup.exe'
& $Nsis /WX /INPUTCHARSET UTF8 "/DLOCI_VERSION=$($package.version)" "/DLOCI_PAYLOAD=$payload" "/DLOCI_OUTPUT=$installer" (Join-Path $root 'installer/loci.nsi')
if ($LASTEXITCODE -ne 0) { throw 'NSIS installer build failed.' }
$hash = (Get-FileHash -LiteralPath $installer -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText((Join-Path $output 'loci-setup.exe.sha256'), "$hash  loci-setup.exe`n", [Text.UTF8Encoding]::new($false))
Write-Host "Created: $installer"
Write-Host "SHA256: $hash"
$pluginManifestPath = Join-Path $root 'plugin/plugin.json'
$pluginManifest = Get-Content -LiteralPath $pluginManifestPath -Raw -Encoding UTF8 | ConvertFrom-Json
if ($pluginManifest.plugin.version -ne $package.version) { throw 'Plugin manifest version must match Cargo package version.' }
$pluginDirectory = Join-Path $output 'kite-plugin'
New-Item -ItemType Directory -Force -Path $pluginDirectory | Out-Null
Copy-Item -LiteralPath $pluginManifestPath, (Join-Path $binaryDirectory 'kite-plugin-loci.exe'), $installer, (Join-Path $root 'plugin/SDK-LICENSE'), (Join-Path $root 'plugin/SDK-SOURCE.md'), (Join-Path $root 'THIRD_PARTY.md') -Destination $pluginDirectory -Force
$pluginArchive = Join-Path $output 'loci-kite-plugin.zip'
# Explicit entries keep generated files from a previous build out of the archive.
$archiveEntries = @('plugin.json', 'kite-plugin-loci.exe', 'loci-setup.exe', 'SDK-LICENSE', 'SDK-SOURCE.md', 'THIRD_PARTY.md') | ForEach-Object { Join-Path $pluginDirectory $_ }
Compress-Archive -LiteralPath $archiveEntries -DestinationPath $pluginArchive -Force
# Append only the current dependency license directories, never stale staging files.
$licenseEntries = foreach ($dependency in $metadata.packages | Where-Object { $_.source -like 'registry+*' }) {
    Get-ChildItem -LiteralPath (Join-Path $payload "licenses/$($dependency.name)-$($dependency.version)") -File
}
Add-Type -AssemblyName System.IO.Compression.FileSystem
$zip = [IO.Compression.ZipFile]::Open($pluginArchive, [IO.Compression.ZipArchiveMode]::Update)
try {
    foreach ($licenseFile in $licenseEntries) {
        $entryName = "licenses/$($licenseFile.Directory.Name)/$($licenseFile.Name)"
        [IO.Compression.ZipFileExtensions]::CreateEntryFromFile($zip, $licenseFile.FullName, $entryName) | Out-Null
    }
} finally { $zip.Dispose() }
$pluginHash = (Get-FileHash -LiteralPath $pluginArchive -Algorithm SHA256).Hash.ToLowerInvariant()
[IO.File]::WriteAllText((Join-Path $output 'loci-kite-plugin.zip.sha256'), "$pluginHash  loci-kite-plugin.zip`n", [Text.UTF8Encoding]::new($false))
Write-Host "Created: $pluginArchive"
Write-Host "SHA256: $pluginHash"
