[CmdletBinding()]
param(
    [switch]$OrdinaryOnly,
    [switch]$RequestElevation,
    # Used only to carry the same evidence directory across an explicit UAC launch.
    [string]$RunDirectory
)
$ErrorActionPreference = 'Stop'
if ($OrdinaryOnly -and ($RequestElevation -or $RunDirectory)) {
    throw 'OrdinaryOnly cannot be combined with elevation or carried administrator evidence.'
}
$probeRoot = [IO.Path]::GetFullPath($PSScriptRoot)
$engineeringRoot = [IO.Path]::GetFullPath((Join-Path $probeRoot '../..'))
$runRoot = [IO.Path]::GetFullPath((Join-Path $engineeringRoot '.scratch/windows-ntfs-stage-b/run'))
$manifest = Join-Path $probeRoot 'Cargo.toml'
$identity = [Security.Principal.WindowsIdentity]::GetCurrent()
$principal = [Security.Principal.WindowsPrincipal]::new($identity)
$isAdministrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
$directoryPins = [Collections.Generic.List[IDisposable]]::new()
$pinnedPaths = [Collections.Generic.HashSet[string]]::new([StringComparer]::OrdinalIgnoreCase)

function Pin-Directory([string]$Path) {
    if ($script:pinnedPaths.Contains($Path)) { return }
    $handle = [LociStageBNtfs.DirectoryPin]::Open($Path)
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
        if ($AllowCreate) { [LociStageBNtfs.DirectoryPin]::Create($cursor, $true) }
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
    [LociStageBNtfs.DirectoryPin]::Create($Path, $false)
    Pin-Directory $Path
}

function Quote-Argument([string]$Value) {
    # All arguments are controlled switches or normalized engineering paths.
    # Reject embedded quotes instead of interpreting any shell syntax.
    if ($Value.Contains('"')) { throw 'A command argument contains an unsupported quote.' }
    return '"' + $Value.TrimEnd('\') + '"'
}

function Invoke-Logged([string]$Executable, [string[]]$Arguments, [string]$Name, [int]$TimeoutSeconds = 180, [bool]$AllowFailure = $false) {
    $stdout = Join-Path $script:evidence "$Name.stdout.txt"
    $stderr = Join-Path $script:evidence "$Name.stderr.txt"
    if ((Test-Path -LiteralPath $stdout) -or (Test-Path -LiteralPath $stderr)) {
        throw 'Refusing to overwrite existing command evidence.'
    }
    $argumentLine = ($Arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
    $process = Start-Process -FilePath $Executable -ArgumentList $argumentLine -WorkingDirectory $engineeringRoot `
        -WindowStyle Hidden -PassThru -RedirectStandardOutput $stdout -RedirectStandardError $stderr
    if (-not $process.WaitForExit($TimeoutSeconds * 1000)) {
        $process.Kill($true)
        $process.WaitForExit()
        throw "Command $Name exceeded ${TimeoutSeconds}s; its process tree was stopped; evidence retained."
    }
    $process.WaitForExit()
    $code = $process.ExitCode
    if ($null -eq $code) { throw 'Child exited without an observable exit code; refusing completion.' }
    Get-Content -LiteralPath $stdout | ForEach-Object { Write-Host $_ }
    Get-Content -LiteralPath $stderr | ForEach-Object { Write-Host $_ }
    "${Name}_exit=$code" | Add-Content -LiteralPath (Join-Path $script:evidence 'exit-codes.txt')
    if ($code -ne 0 -and -not $AllowFailure) {
        Write-Output "NativeComplete=false; command=$Name; exit=$code; evidence=$script:evidence"
        exit $code
    }
    return $stdout
}

function Invoke-OfflineMutations([string]$Fixture, [int]$Count) {
    # Scenario names are fixed synthetic entries created by acceptance-build.
    # Never accept arbitrary paths or delete recursively.
    [IO.File]::WriteAllText((Join-Path $Fixture 'offline-added.txt'), 'offline addition')
    Remove-Item -LiteralPath (Join-Path $Fixture 'offline-delete.txt')
    Rename-Item -LiteralPath (Join-Path $Fixture 'offline-old.txt') -NewName 'offline-new.txt'
    Rename-Item -LiteralPath (Join-Path $Fixture 'offline-dir') -NewName 'offline-dir-new'
    Remove-Item -LiteralPath (Join-Path $Fixture 'offline-old-link.txt')
    New-Item -ItemType HardLink -Path (Join-Path $Fixture 'offline-new-link.txt') -Target (Join-Path $Fixture 'offline-source.txt') | Out-Null
    # Both endpoints stay within the explicit engineering evidence root; move
    # across the indexed scope boundary without touching unrelated directories.
    $outsideOut = Assert-RunPath (Join-Path $script:evidence "offline-out-$Count")
    $outsideIn = Assert-RunPath (Join-Path $script:evidence "offline-in-$Count")
    [LociStageBNtfs.DirectoryPin]::Create($outsideIn, $false)
    $inputPin = [LociStageBNtfs.DirectoryPin]::Open($outsideIn)
    try { [IO.File]::WriteAllText((Join-Path $outsideIn 'childin.txt'), 'offline incoming directory child') }
    finally { $inputPin.Dispose() }
    if (Test-Path -LiteralPath $outsideOut) { throw 'Refusing to reuse an outside scope movement destination.' }
    $moveSource = Join-Path $Fixture 'offline-move-out'
    $sourcePin = [LociStageBNtfs.DirectoryPin]::Open($moveSource)
    $sourcePin.Dispose()
    Move-Item -LiteralPath $moveSource -Destination $outsideOut
    Move-Item -LiteralPath $outsideIn -Destination (Join-Path $Fixture 'offline-move-in')
}

try {
    if (-not $isAdministrator -and -not $RequestElevation -and -not $OrdinaryOnly) {
        Write-Output 'NativeComplete=false; administrator_token=false; exit=20. This manual NTFS runner requires an administrator token. No elevation, journal changes, or volume enumeration were attempted.'
        exit 20
    }
    # Keep each literal ancestor open without FILE_SHARE_DELETE throughout all
    # writes and child processes. Inspect the opened object, not a racy path
    # attribute snapshot. No external compiler, module, or installation is used.
    if (-not ('LociStageBNtfs.DirectoryPin' -as [type])) {
        Add-Type -TypeDefinition @'
using System;
using System.ComponentModel;
using System.Runtime.InteropServices;
using Microsoft.Win32.SafeHandles;
namespace LociStageBNtfs {
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
    if (-not $isAdministrator -and -not $OrdinaryOnly) {
        # RunAs is opt-in only. This runner never silently elevates or changes
        # the volume journal. The visible UAC dialog belongs to Windows.
        $shell = (Get-Process -Id $PID).Path
        $arguments = @('-NoProfile', '-File', (Join-Path $probeRoot 'verify-ntfs.ps1'), '-RunDirectory', $script:evidence)
        'Explicit RequestElevation launch; no assumption of a clean or uninjected child process.' |
            Set-Content -LiteralPath (Join-Path $script:evidence 'elevation-request.txt')
        $line = ($arguments | ForEach-Object { Quote-Argument $_ }) -join ' '
        $child = Start-Process -FilePath $shell -ArgumentList $line -Verb RunAs -WindowStyle Hidden -PassThru
        if (-not $child.WaitForExit(900000)) {
            # Parent is not elevated and may be denied termination of an elevated child.
            # Report that explicitly rather than claiming the child stopped.
            try { $child.Kill($true); $child.WaitForExit() } catch {
                throw "Administrative child timeout; termination denied; child PID $($child.Id) may remain active. No completion is claimed."
            }
            throw "Administrative child exceeded 900s and was stopped. Existing evidence retained."
        }
        if ($null -eq $child.ExitCode) { throw 'Administrative child exit code unavailable; refusing completion.' }
        Write-Output "elevated_child_exit=$($child.ExitCode); evidence=$script:evidence"
        exit $child.ExitCode
    }
    @(
        "administrator_token=$isAdministrator"
        "process_id=$PID"
        "user_sid=$($identity.User.Value)"
        "powershell=$($PSVersionTable.PSVersion)"
        "os_process_observation=$([Environment]::OSVersion.VersionString)"
        "worktree=$engineeringRoot"
        'volume_mft_enumeration=false'
        'fixture_counts=1000,10000'
        'actual_file_changes=engineering_fixture_only'
        'No clean/uninjected host environment is presumed. Probe capabilities record native token and loaded module observations.'
    ) | Set-Content -LiteralPath (Join-Path $script:evidence 'identity.txt')
    # Every invocation builds in a new pinned target. Cargo creates only fresh
    # descendants; no existing shared target or its redirects are reused.
    $target = Assert-RunPath (Join-Path $script:evidence ('target-' + [Guid]::NewGuid().ToString('N')))
    New-UniqueDirectory $target
    $volume = [IO.Path]::GetPathRoot($runRoot).TrimEnd('\')
    if ($volume -notmatch '^[A-Za-z]:$') { throw 'The engineering fixture must be on a local drive-letter volume.' }
    $cargo = (Get-Command cargo -CommandType Application).Source
    $null = Invoke-Logged $cargo @('+1.99.0', 'fmt', '--check', '--manifest-path', $manifest) 'fmt' 120
    $null = Invoke-Logged $cargo @('+1.99.0', 'test', '--release', '--offline', '--locked', '--manifest-path', $manifest, '--target-dir', $target, '--', '--test-threads=1') 'test' 300
    $null = Invoke-Logged $cargo @('+1.99.0', 'build', '--release', '--offline', '--locked', '--manifest-path', $manifest, '--target-dir', $target) 'build' 300
    $binary = Join-Path $target 'release/loci-ntfs-stage-b.exe'
    $capabilitiesLog = Invoke-Logged $binary @('capabilities', $volume) 'capabilities' 30 $OrdinaryOnly.IsPresent
    if ($OrdinaryOnly) {
        $ordinaryFixture = Assert-RunPath (Join-Path $script:evidence ('fixture-scan-1000-' + [Guid]::NewGuid().ToString('N')))
        $ordinaryStorage = Assert-RunPath (Join-Path $script:evidence ('checkpoint-scan-1000-' + [Guid]::NewGuid().ToString('N')))
        New-UniqueDirectory $ordinaryFixture
        New-UniqueDirectory $ordinaryStorage
        $null = Invoke-Logged $binary @('acceptance-scan', $ordinaryFixture, $ordinaryStorage, '1000') 'ordinary-scan-1000' 180
        @('NativeComplete=false', "administrator_token=$isAdministrator", 'ordinary_static_tests=true', 'ordinary_real_files_scan=1000', 'ordinary_checkpoint=simulated_no_USN', 'Capability-only query, EOF enumeration sentinel and nonblocking journal-read attempts are logged with actual OS codes; no volume inventory enumeration or native USN fixture acceptance was run.') |
            Set-Content -LiteralPath (Join-Path $script:evidence 'result.txt')
        Write-Output "ordinary_checks_complete=true; evidence=$script:evidence; inspect capabilities exit for actual permission evidence"
        exit 0
    }
    foreach ($count in @(1000, 10000)) {
        $fixture = Assert-RunPath (Join-Path $script:evidence ("fixture-$count-" + [Guid]::NewGuid().ToString('N')))
        $checkpoint = Assert-RunPath (Join-Path $script:evidence ("checkpoint-$count-" + [Guid]::NewGuid().ToString('N')))
        New-UniqueDirectory $fixture
        New-UniqueDirectory $checkpoint
        $buildLog = Invoke-Logged $binary @('acceptance-build', $fixture, $checkpoint, "$count") "acceptance-build-$count" 180
        if (-not (Select-String -LiteralPath $buildLog -Pattern '(?i)native_complete\s*=\s*true' -Quiet)) {
            throw 'Build exited successfully without native_complete=true; refusing completion.'
        }
        # The build child has exited: mutations below are genuinely offline.
        Invoke-OfflineMutations $fixture $count
        "count=$count; offline mutations completed after build process exit" |
            Add-Content -LiteralPath (Join-Path $script:evidence 'offline-mutations.txt')
        $recoverLog = Invoke-Logged $binary @('acceptance-recover', $fixture, $checkpoint, "$count") "acceptance-recover-$count" 180
        if (-not (Select-String -LiteralPath $recoverLog -Pattern '(?i)native_complete\s*=\s*true' -Quiet)) {
            throw 'Recovery exited successfully without native_complete=true; refusing completion.'
        }
        @("count=$count", "fixture=$fixture", "checkpoint=$checkpoint", 'native_complete=true') |
            Set-Content -LiteralPath (Join-Path $script:evidence "result-$count.txt")
    }
    @('NativeComplete=true', 'administrator_token=true', 'real_fixture_files=1000,10000', 'Full independent path oracle is required by both native commands.', 'No MFT enumeration or journal/system-setting changes.') |
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
