[CmdletBinding()]
param(
    [switch]$AllowVolumeEnum,
    [switch]$RequestElevation,
    # Used only to carry the same evidence directory across an explicit UAC launch.
    [string]$RunDirectory
)
$ErrorActionPreference = 'Stop'
$probeRoot = [IO.Path]::GetFullPath($PSScriptRoot)
$engineeringRoot = [IO.Path]::GetFullPath((Join-Path $probeRoot '../..'))
$runRoot = [IO.Path]::GetFullPath((Join-Path $engineeringRoot '.scratch/windows-ntfs-stage-a/run'))
$manifest = Join-Path $probeRoot 'Cargo.toml'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$isAdministrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$directoryPins = [Collections.Generic.List[IDisposable]]::new()
$pinnedPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)

function Pin-Directory([string]$Path) {
    if ($script:pinnedPaths.Contains($Path)) { return }
    $handle = [LociManualNtfs.DirectoryPin]::Open($Path)
    $script:directoryPins.Add($handle)
    $null = $script:pinnedPaths.Add($Path)
}

function Pin-Tree([string]$Path, [bool]$AllowCreate) {
    $absolute = [IO.Path]::GetFullPath($Path)
    $driveRoot = [IO.Path]::GetPathRoot($absolute)
    if ($driveRoot -notmatch '^[A-Za-z]:\\$') { throw 'Only local drive-letter engineering paths are supported.' }
    Pin-Directory $driveRoot
    $cursor = $driveRoot
    foreach ($component in $absolute.Substring($driveRoot.Length).Split([char]'\')) {
        if (-not $component) { continue }
        $cursor = Join-Path $cursor $component
        if ($AllowCreate) { [LociManualNtfs.DirectoryPin]::Create($cursor, $true) }
        Pin-Directory $cursor
    }
}

function Assert-RunPath([string]$Path) {
    $absolute = [IO.Path]::GetFullPath($Path)
    if (-not $absolute.StartsWith($runRoot + [IO.Path]::DirectorySeparatorChar, [StringComparison]::OrdinalIgnoreCase)) {
        throw 'Evidence and fixture directories must be inside this worktree engineering run directory.'
    }
    $ancestor = $absolute
    while ($ancestor -and $ancestor.StartsWith($engineeringRoot, [StringComparison]::OrdinalIgnoreCase)) {
        if (Test-Path -LiteralPath $ancestor) {
            $item = Get-Item -LiteralPath $ancestor -Force
            if (($item.Attributes -band [IO.FileAttributes]::ReparsePoint) -ne 0) {
                throw 'An engineering run path traverses a reparse point; refusing redirected fixture writes.'
            }
        }
        if ($ancestor.Equals($engineeringRoot, [StringComparison]::OrdinalIgnoreCase)) { break }
        $ancestor = [IO.Path]::GetDirectoryName($ancestor)
    }
    return $absolute
}

function New-UniqueDirectory([string]$Path) {
    [LociManualNtfs.DirectoryPin]::Create($Path, $false)
    Pin-Directory $Path
}

function Quote-Argument([string]$Value) {
    # All arguments are controlled switches or normalized engineering paths.
    # Reject embedded quotes instead of interpreting any shell syntax.
    if ($Value.Contains('"')) { throw 'A command argument contains an unsupported quote.' }
    return '"' + $Value.TrimEnd('\') + '"'
}

function Invoke-Logged([string]$Executable, [string[]]$Arguments, [string]$Name) {
    $stdout = Join-Path $script:evidence "$Name.stdout.txt"
    $stderr = Join-Path $script:evidence "$Name.stderr.txt"
    if ((Test-Path -LiteralPath $stdout) -or (Test-Path -LiteralPath $stderr)) {
        throw 'Refusing to overwrite existing command evidence.'
    }
    $argumentLine = ($Arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
    $process = Start-Process -FilePath $Executable -ArgumentList $argumentLine -WorkingDirectory $engineeringRoot `
        -WindowStyle Hidden -Wait -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    $code = $process.ExitCode
    Get-Content -LiteralPath $stdout | ForEach-Object { Write-Host $_ }
    Get-Content -LiteralPath $stderr | ForEach-Object { Write-Host $_ }
    "${Name}_exit=$code" | Add-Content -LiteralPath (Join-Path $script:evidence 'exit-codes.txt')
    if ($code -ne 0) {
        Write-Output "NativeComplete=false; command=$Name; exit=$code; evidence=$script:evidence"
        exit $code
    }
    return $stdout
}

try {
    if (-not $isAdministrator -and -not $RequestElevation) {
        Write-Output 'NativeComplete=false; administrator_token=false; exit=20. This manual NTFS runner requires an administrator token. No elevation, journal changes, or volume enumeration were attempted.'
        exit 20
    }
    # Keep each literal ancestor open without FILE_SHARE_DELETE throughout all
    # writes and child processes. Inspect the opened object, not a racy path
    # attribute snapshot. No external compiler, module, or installation is used.
    if (-not ('LociManualNtfs.DirectoryPin' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
namespace LociManualNtfs {
    public static class DirectoryPin {
        [StructLayout(LayoutKind.Sequential)]
        private struct AttributeTag { public uint Attributes; public uint ReparseTag; }
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern SafeFileHandle CreateFileW(string path, uint access, uint share,
            IntPtr security, uint disposition, uint flags, IntPtr template);
        [DllImport("kernel32.dll", SetLastError = true)]
        private static extern bool GetFileInformationByHandleEx(SafeFileHandle handle,
            int infoClass, out AttributeTag info, uint size);
        [DllImport("kernel32.dll", CharSet = CharSet.Unicode, SetLastError = true)]
        private static extern bool CreateDirectoryW(string path, IntPtr security);
        public static SafeFileHandle Open(string path) {
            // FILE_LIST_DIRECTORY, SHARE_READ|SHARE_WRITE (no DELETE),
            // OPEN_EXISTING, BACKUP_SEMANTICS|OPEN_REPARSE_POINT.
            SafeFileHandle handle = CreateFileW(path, 1, 3, IntPtr.Zero, 3,
                0x02200000, IntPtr.Zero);
            if (handle.IsInvalid) {
                int error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(error, "Directory pin open failed: " + path);
            }
            AttributeTag info;
            if (!GetFileInformationByHandleEx(handle, 9, out info, 8)) {
                int error = Marshal.GetLastWin32Error();
                handle.Dispose();
                throw new Win32Exception(error, "Directory pin attributes failed: " + path);
            }
            if ((info.Attributes & 0x10) == 0 || (info.Attributes & 0x400) != 0) {
                handle.Dispose();
                throw new InvalidOperationException("Pinned path must be an ordinary directory: " + path);
            }
            return handle;
        }
        public static void Create(string path, bool allowExisting) {
            if (CreateDirectoryW(path, IntPtr.Zero)) return;
            int error = Marshal.GetLastWin32Error();
            if (allowExisting && error == 183) return;
            throw new Win32Exception(error, "Directory creation failed or already exists: " + path);
        }
    }
}
'@
    }
    Pin-Tree $engineeringRoot $false
    Pin-Tree $runRoot $true
    if ($RunDirectory) {
        if (-not [IO.Path]::IsPathRooted($RunDirectory)) { throw 'RunDirectory must be absolute.' }
        $script:evidence = Assert-RunPath $RunDirectory
        Pin-Tree $script:evidence $false
        if (-not (Test-Path -LiteralPath $script:evidence -PathType Container)) { throw 'Carried evidence directory does not exist.' }
        if (Test-Path -LiteralPath (Join-Path $script:evidence 'identity.txt')) { throw 'Refusing to reuse completed or active command evidence.' }
    } else {
        $script:evidence = Assert-RunPath (Join-Path $runRoot ('native-' + [DateTime]::UtcNow.ToString('yyyyMMddTHHmmssZ') + '-' + [Guid]::NewGuid().ToString('N')))
        New-UniqueDirectory $script:evidence
    }
    if (-not $isAdministrator) {
        # RunAs is opt-in only. This runner never silently elevates or changes
        # the volume journal. The visible UAC dialog belongs to Windows.
        $shell = (Get-Process -Id $PID).Path
        $arguments = @('-NoProfile', '-File', (Join-Path $probeRoot 'verify-ntfs.ps1'), '-RunDirectory', $script:evidence)
        if ($AllowVolumeEnum) { $arguments += '-AllowVolumeEnum' }
        'Explicit RequestElevation launch; no assumption of a clean or uninjected child process.' |
            Set-Content -LiteralPath (Join-Path $script:evidence 'elevation-request.txt')
        $line = ($arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
        $child = Start-Process -FilePath $shell -ArgumentList $line -Verb RunAs -WindowStyle Hidden -Wait -PassThru
        Write-Output "elevated_child_exit=$($child.ExitCode); evidence=$script:evidence"
        exit $child.ExitCode
    }
    @(
        'administrator_token=true'
        "process_id=$PID"
        "user_sid=$($identity.User.Value)"
        "powershell=$($PSVersionTable.PSVersion)"
        "os_process_observation=$([Environment]::OSVersion.VersionString)"
        "worktree=$engineeringRoot"
        "allow_volume_enum=$($AllowVolumeEnum.IsPresent)"
        'No clean/uninjected host environment is presumed. Probe capabilities record native token and loaded module observations.'
    ) | Set-Content -LiteralPath (Join-Path $script:evidence 'identity.txt')
    # Every invocation builds in a new pinned target. Cargo creates only fresh
    # descendants; no existing shared target or its redirects are reused.
    $target = Assert-RunPath (Join-Path $script:evidence ('target-' + [Guid]::NewGuid().ToString('N')))
    New-UniqueDirectory $target
    $volume = [IO.Path]::GetPathRoot($runRoot).TrimEnd('\')
    if ($volume -notmatch '^[A-Za-z]:$') { throw 'The engineering fixture must be on a local drive-letter volume.' }
    $cargo = (Get-Command cargo -CommandType Application).Source
    $null = Invoke-Logged $cargo @('+1.99.0', 'build', '--release', '--offline', '--locked', '--manifest-path', $manifest, '--target-dir', $target) 'build'
    $binary = Join-Path $target 'release/loci-ntfs-stage-a.exe'
    $null = Invoke-Logged $binary @('capabilities-repeat', $volume) 'capabilities'
    if (-not $AllowVolumeEnum) {
        Write-Output "NativeComplete=false; capabilities_only=true; evidence=$script:evidence. Add -AllowVolumeEnum to authorize bounded MFT reads of the fixture's entire volume. Other file names are not printed or recorded."
        exit 0
    }
    # The checkpoint is a sibling of the fixture, never within its indexed scope.
    $fixture = Assert-RunPath (Join-Path $script:evidence ('fixture-native-' + [Guid]::NewGuid().ToString('N')))
    $checkpoint = Assert-RunPath (Join-Path $script:evidence ('checkpoint-' + [Guid]::NewGuid().ToString('N')))
    New-UniqueDirectory $fixture
    New-UniqueDirectory $checkpoint
    $offlineDirectory = Join-Path $fixture 'offline-dir-before'
    [LociManualNtfs.DirectoryPin]::Create($offlineDirectory, $false)
    [IO.File]::WriteAllText((Join-Path $fixture 'offline-delete.txt'), 'synthetic offline deletion')
    [IO.File]::WriteAllText((Join-Path $fixture 'offline-rename-before.txt'), 'synthetic offline file rename')
    $offlinePin = [LociManualNtfs.DirectoryPin]::Open($offlineDirectory)
    try {
        [IO.File]::WriteAllText((Join-Path $offlineDirectory 'child.txt'), 'synthetic directory rename child')
    } finally { $offlinePin.Dispose() }
    [IO.File]::WriteAllText((Join-Path $fixture 'offline-link-source.txt'), 'synthetic hard-link object')
    New-Item -ItemType HardLink -Path (Join-Path $fixture 'offline-link-remove.txt') -Target (Join-Path $fixture 'offline-link-source.txt') | Out-Null
    # bootstrap creates its own small native edge-case/concurrent-mutation fixture,
    # verifies full scoped paths, and durably commits inventory plus USN cursor.
    $bootstrapLog = Invoke-Logged $binary @('bootstrap', $fixture, $checkpoint, '--allow-volume-enum') 'bootstrap'
    if (-not (Select-String -LiteralPath $bootstrapLog -Pattern '(?i)native_complete\s*=\s*true' -Quiet)) {
        throw 'bootstrap exited successfully without NativeComplete=true; refusing to treat it as complete.'
    }
    # The bootstrap process has exited: these mutations are genuinely offline.
    [IO.File]::WriteAllText((Join-Path $fixture 'offline-added.txt'), 'synthetic offline addition')
    Remove-Item -LiteralPath (Join-Path $fixture 'offline-delete.txt')
    Rename-Item -LiteralPath (Join-Path $fixture 'offline-rename-before.txt') -NewName 'offline-rename-after.txt'
    Rename-Item -LiteralPath (Join-Path $fixture 'offline-dir-before') -NewName 'offline-dir-after'
    Remove-Item -LiteralPath (Join-Path $fixture 'offline-link-remove.txt')
    New-Item -ItemType HardLink -Path (Join-Path $fixture 'offline-link-added.txt') -Target (Join-Path $fixture 'offline-link-source.txt') | Out-Null
    'offline add/delete/file rename/directory rename/hard-link add/delete completed after bootstrap process exit' |
        Set-Content -LiteralPath (Join-Path $script:evidence 'offline-mutations.txt')
    $recoverLog = Invoke-Logged $binary @('recover', $fixture, $checkpoint) 'recover'
    if (-not (Select-String -LiteralPath $recoverLog -Pattern '(?i)native_complete\s*=\s*true' -Quiet)) {
        throw 'recover exited successfully without NativeComplete=true; refusing to treat it as complete.'
    }
    @('NativeComplete=true', 'administrator_token=true', "fixture=$fixture", "checkpoint=$checkpoint", 'Full path oracle is performed by both native commands. No large-scale acceptance is claimed.') |
        Set-Content -LiteralPath (Join-Path $script:evidence 'result.txt')
    Write-Output "NativeComplete=true; administrator_token=true; evidence=$script:evidence"
    exit 0
} catch {
    Write-Error -ErrorAction Continue $_
    Write-Output 'NativeComplete=false; runner_exit=21. Existing evidence and fixtures are preserved.'
    exit 21
} finally {
    for ($pinIndex = $directoryPins.Count - 1; $pinIndex -ge 0; $pinIndex--) {
        $directoryPins[$pinIndex].Dispose()
    }
    $identity.Dispose()
}
