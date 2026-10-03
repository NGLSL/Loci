[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)][string]$ExecutablePath,
    [switch]$Uninstall,
    [switch]$StopOnly
)
$ErrorActionPreference = 'Stop'
try {
    if ($Uninstall -and $StopOnly) { throw 'Choose either Uninstall or StopOnly.' }
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = [Security.Principal.WindowsPrincipal]::new($identity)
    if (-not $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)) {
        throw 'Loci service installation requires an administrator PowerShell session.'
    }
    $serviceName = 'LociIndex'
    $expectedPath = [IO.Path]::GetFullPath($ExecutablePath)
    if ($expectedPath.Contains('"') -or $expectedPath.Contains("`n") -or $expectedPath.Contains("`r")) { throw 'Invalid executable path.' }
    if (-not $Uninstall -and -not $StopOnly -and -not [IO.File]::Exists($expectedPath)) { throw "Service executable does not exist: $expectedPath" }
    $existing = Get-CimInstance Win32_Service -Filter "Name='$serviceName'"
    if ($existing) {
        $imagePath = $existing.PathName.Trim()
        # This service has no command line switches. Never take ownership of a different installation.
        if ($imagePath.StartsWith('"') -and $imagePath.EndsWith('"')) { $imagePath = $imagePath.Substring(1, $imagePath.Length - 2) }
        if (-not [StringComparer]::OrdinalIgnoreCase.Equals($imagePath, $expectedPath)) { throw "Existing $serviceName belongs to another executable: $($existing.PathName)" }
        $service = Get-Service -Name $serviceName
        $serviceProcess = $null
        if ($existing.ProcessId -gt 0) {
            try { $serviceProcess = [Diagnostics.Process]::GetProcessById([int]$existing.ProcessId) } catch [ArgumentException] { }
            if ($serviceProcess) {
                # Capture the process handle before stop, so PID reuse cannot
                # make us wait for an unrelated process after the service exits.
                [void]$serviceProcess.Handle
                if (-not [StringComparer]::OrdinalIgnoreCase.Equals($serviceProcess.MainModule.FileName, $expectedPath)) { throw 'Running service process does not match the expected executable.' }
            }
        }
        $stopWatch = [Diagnostics.Stopwatch]::StartNew()
        if ($service.Status -ne 'Stopped') {
            $service.Stop()
            $service.WaitForStatus([ServiceProcess.ServiceControllerStatus]::Stopped, [TimeSpan]::FromSeconds(30))
        }
        if ($serviceProcess -and -not $serviceProcess.HasExited) {
            $remainingMilliseconds = [Math]::Max(0, 30000 - [int]$stopWatch.ElapsedMilliseconds)
            if (-not $serviceProcess.WaitForExit($remainingMilliseconds)) { throw 'Loci service reported stopped but its process has not exited; files were not replaced.' }
        }
        if ($serviceProcess) { $serviceProcess.Dispose() }
    }
    if ($StopOnly) { exit 0 }
    if ($Uninstall) {
        if ($existing) {
            & sc.exe delete $serviceName | Out-Host
            if ($LASTEXITCODE -ne 0) { throw "sc.exe delete failed: $LASTEXITCODE" }
        }
        exit 0
    }
    # Create the data directory with its restrictive ACL atomically. Never
    # repair or take ownership of a directory planted by another user.
    Add-Type -TypeDefinition @'
using System;
using System.Runtime.InteropServices;
public static class LociSecureDirectory {
    [StructLayout(LayoutKind.Sequential)] public struct SA { public int Size; public IntPtr Descriptor; public int Inherit; }
    [DllImport("advapi32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern bool ConvertStringSecurityDescriptorToSecurityDescriptor(string s, uint revision, out IntPtr descriptor, out uint size);
    [DllImport("kernel32.dll", CharSet=CharSet.Unicode, SetLastError=true)] public static extern bool CreateDirectory(string path, ref SA attributes);
    [DllImport("kernel32.dll")] public static extern IntPtr LocalFree(IntPtr memory);
}
'@
    $dataDirectory = Join-Path ([Environment]::GetFolderPath([Environment+SpecialFolder]::CommonApplicationData)) 'Loci'
    $dataParent = Get-Item -LiteralPath (Split-Path $dataDirectory -Parent)
    if ($dataParent.Attributes -band [IO.FileAttributes]::ReparsePoint) { throw 'ProgramData must not be a reparse point.' }
    [IntPtr]$descriptor = [IntPtr]::Zero
    [uint32]$descriptorSize = 0
    if (-not [LociSecureDirectory]::ConvertStringSecurityDescriptorToSecurityDescriptor('O:BAG:BAD:P(A;OICI;GA;;;SY)(A;OICI;GA;;;BA)', 1, [ref]$descriptor, [ref]$descriptorSize)) { throw 'Cannot prepare secure Loci data ACL.' }
    try {
        $attributes = [LociSecureDirectory+SA]::new()
        $attributes.Size = [Runtime.InteropServices.Marshal]::SizeOf($attributes)
        $attributes.Descriptor = $descriptor
        if (-not [LociSecureDirectory]::CreateDirectory($dataDirectory, [ref]$attributes)) {
            $directoryError = [Runtime.InteropServices.Marshal]::GetLastWin32Error()
            if ($directoryError -ne 183) { throw "Cannot create Loci data directory: $directoryError" }
        }
    } finally { [void][LociSecureDirectory]::LocalFree($descriptor) }
    $dataItem = Get-Item -LiteralPath $dataDirectory
    if (-not $dataItem.PSIsContainer -or ($dataItem.Attributes -band [IO.FileAttributes]::ReparsePoint)) { throw 'Loci data directory must not be a reparse point.' }
    $dataAcl = Get-Acl -LiteralPath $dataDirectory
    $trustedSids = @('S-1-5-18', 'S-1-5-32-544')
    $ownerSid = ([Security.Principal.NTAccount]::new($dataAcl.Owner)).Translate([Security.Principal.SecurityIdentifier]).Value
    if ($ownerSid -notin $trustedSids) { throw 'Existing Loci data directory has an untrusted owner; it was not modified.' }
    foreach ($rule in $dataAcl.Access) {
        $sid = $rule.IdentityReference.Translate([Security.Principal.SecurityIdentifier]).Value
        if ($rule.AccessControlType -eq 'Allow' -and $sid -notin $trustedSids) { throw 'Existing Loci data directory grants untrusted access; it was not modified.' }
    }
    $quotedPath = '"' + $expectedPath + '"'
    if ($existing) { & sc.exe config $serviceName binPath= $quotedPath start= auto obj= LocalSystem | Out-Host }
    else { & sc.exe create $serviceName binPath= $quotedPath start= auto obj= LocalSystem DisplayName= 'Loci File Index' | Out-Host }
    if ($LASTEXITCODE -ne 0) { throw "sc.exe create/config failed: $LASTEXITCODE" }
    & sc.exe description $serviceName 'Local NTFS file index used by Kite.' | Out-Host
    if ($LASTEXITCODE -ne 0) { throw "sc.exe description failed: $LASTEXITCODE" }
    $service = Get-Service -Name $serviceName
    $service.Start()
    $service.WaitForStatus([ServiceProcess.ServiceControllerStatus]::Running, [TimeSpan]::FromSeconds(15))
    exit 0
} catch {
    Write-Error $_ -ErrorAction Continue
    exit 1
}
