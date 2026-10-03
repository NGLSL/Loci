[CmdletBinding()]
param()
$ErrorActionPreference = 'Stop'
# Reuse the same literal-ancestor pins and unique isolated target for ordinary checks.
& (Join-Path $PSScriptRoot 'verify-ntfs.ps1') -OrdinaryOnly
exit $LASTEXITCODE
