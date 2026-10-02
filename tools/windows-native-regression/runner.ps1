[CmdletBinding()]
param(
    [Parameter(Mandatory = $true)]
    [ValidatePattern('^[0-9a-fA-F]{40}$')]
    [string] $Commit,
    [string] $RepoPath = 'D:\Project\Loci',
    [string] $BundlePath,
    [string] $OutputDirectory,
    [ValidatePattern('^1\.99\.0(?:-[A-Za-z0-9-]+)?$')]
    [string] $Toolchain = '1.99.0',
    [ValidateRange(60, 3600)]
    [int] $StepTimeoutSeconds = 900
)
# Development handoff: statically inspected in cloud only. PowerShell parsing
# and native Windows execution were not available there and remain unverified.
# No installations, remote fetches, execution-policy changes, reset, push or release.
$ErrorActionPreference = 'Stop'
$TaskOriginalDirectory = (Get-Location).Path
$TaskPreviousTarget = $env:CARGO_TARGET_DIR
$TaskPreviousColor = $env:CARGO_TERM_COLOR
$TaskStarted = [DateTime]::UtcNow
$TaskOutputCreated = $false
$TaskPhase = 'preflight'
$TaskSteps = New-Object System.Collections.Generic.List[object]
$TaskSummary = [ordered]@{
    schema = 'loci.native-windows-validation.v2'
    expected_commit = $Commit.ToLowerInvariant()
    started_utc = $TaskStarted.ToString('o')
    finished_utc = $null
    verified = $false
    status = 'unverified'
    native_windows = $false
    task18_resolved = $false
    scope = 'Existing Windows Engine/shared-core nonregression; no MFT/USN/GUI'
    worktree = $null
    environment = $null
    artifacts = @()
    steps = @()
    failure_phase = $null
    failure = $null
}

function Get-TaskLocalDriveRoot {
    param([string] $Path)
    $root = [System.IO.Path]::GetPathRoot($Path)
    if ($root -notmatch '^[A-Za-z]:\\$') {
        throw 'Use a local drive path; UNC/network locations are outside this handoff.'
    }
    $drive = New-Object System.IO.DriveInfo($root)
    if ($drive.DriveType -eq [System.IO.DriveType]::Network) {
        throw 'Mapped network drives are outside native local-filesystem acceptance.'
    }
    return $root
}

function Get-TaskTestEvidence {
    param([string] $Stdout, [string] $Stderr)
    $summaries = New-Object System.Collections.Generic.List[object]
    $unverified = New-Object System.Collections.Generic.List[object]
    $ignored = New-Object System.Collections.Generic.List[object]
    $passedNames = New-Object System.Collections.Generic.List[string]
    foreach ($file in @($Stdout, $Stderr)) {
        $current = $null
        foreach ($line in Get-Content -LiteralPath $file -Encoding UTF8) {
            if ($line -match '^test (\S+) \.\.\.\s*(.*)') {
                $current = $Matches[1]
                $tail = $Matches[2]
                if ($tail -match '^ok\s*$') { $passedNames.Add($current) }
                if ($tail -match '^ignored') {
                    $ignored.Add([ordered]@{ test = $current; reason = $tail; log = $file })
                }
            } elseif ($current -and $line -match '^ok\s*$') {
                $passedNames.Add($current)
            }
            if ($line -match '\b(SKIP|UNVERIFIED):\s*(.*)') {
                $unverified.Add([ordered]@{
                    test = if ($current) { $current } else { 'unknown; manual attribution required' }
                    kind = $Matches[1]; reason = $Matches[2]; log = $file; raw_line = $line
                })
            }
            if ($line -match '^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed; (\d+) ignored;') {
                $summaries.Add([ordered]@{
                    result = $Matches[1]; passed = [int]$Matches[2]
                    failed = [int]$Matches[3]; ignored = [int]$Matches[4]; log = $file
                })
            }
        }
    }
    return [ordered]@{
        libtest_summaries_not_acceptance_totals = @($summaries.ToArray())
        passed_test_names = @($passedNames.ToArray())
        ignored_not_passed = @($ignored.ToArray())
        environment_unverified = @($unverified.ToArray())
    }
}

function Invoke-TaskStep {
    param([string] $Name, [string] $Executable, [string[]] $Arguments, [string] $WorkingDirectory, [string] $EvidenceScope)
    # Arguments are fixed cargo tokens plus an installed, validated toolchain.
    # User filesystem paths are process properties/environment, not shell code.
    $stdout = Join-Path $OutputDirectory ($Name + '.stdout.log')
    $stderr = Join-Path $OutputDirectory ($Name + '.stderr.log')
    $start = [DateTime]::UtcNow
    $step = [ordered]@{
        name = $Name; executable = $Executable; arguments = $Arguments
        evidence_scope = $EvidenceScope
        started_utc = $start.ToString('o'); duration_ms = $null
        exit_code = $null; status = 'running'; stdout = $stdout; stderr = $stderr
        process_id = $null; process_creation_utc = $null
        timeout_termination = $null; test_evidence = $null; logs_complete = $false
    }
    $TaskSteps.Add($step)
    $process = $null
    $stdoutStream = $null
    $stderrStream = $null
    try {
        # Stream bytes to owned files without shell parsing or asynchronous
        # PowerShell callbacks. All argument tokens are fixed or validated ASCII
        # without whitespace; user paths are separate process properties.
        $stdoutStream = [System.IO.File]::Open($stdout, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
        $stderrStream = [System.IO.File]::Open($stderr, [System.IO.FileMode]::CreateNew, [System.IO.FileAccess]::Write, [System.IO.FileShare]::Read)
        $process = New-Object System.Diagnostics.Process
        $process.StartInfo.FileName = $Executable
        $process.StartInfo.Arguments = $Arguments -join ' '
        $process.StartInfo.WorkingDirectory = $WorkingDirectory
        $process.StartInfo.UseShellExecute = $false
        $process.StartInfo.CreateNoWindow = $true
        $process.StartInfo.RedirectStandardOutput = $true
        $process.StartInfo.RedirectStandardError = $true
        if (-not $process.Start()) { throw 'Owned process could not start.' }
        $copyTasks = [System.Threading.Tasks.Task[]]@(
            $process.StandardOutput.BaseStream.CopyToAsync($stdoutStream)
            $process.StandardError.BaseStream.CopyToAsync($stderrStream)
        )
        $drain = [System.Threading.Tasks.Task]::WhenAll($copyTasks)
        # Keep the returned Process and its native handle alive. Never reacquire
        # by PID, taskkill a PID, or use Stop-Process -Id after a reuse race.
        $heldHandle = $process.Handle
        $heldCreation = $process.StartTime.ToUniversalTime()
        $heldId = $process.Id
        $step.process_id = $heldId
        $step.process_creation_utc = $heldCreation.ToString('o')
        if (-not $process.WaitForExit($StepTimeoutSeconds * 1000)) {
            $step.status = 'timeout'
            $termination = [ordered]@{
                scope = 'held root Process only; descendants are not enumerated or killed by PID'
                requested = $false; root_exit_confirmed = $false
                child_tree_release_confirmed = $false
                child_tree_note = 'Descendants may remain; timeout is failed evidence and manual review is required.'
                error = $null
            }
            try {
                if (-not $process.HasExited) {
                    if ($process.Id -ne $heldId -or $process.StartTime.ToUniversalTime().Ticks -ne $heldCreation.Ticks) {
                        throw 'Owned process identity changed; refusing termination.'
                    }
                    $process.Kill()
                    $termination.requested = $true
                }
                # Bounded wait only. An unconditional WaitForExit() can wait on
                # redirected streams still held by descendants after root exit.
                $termination.root_exit_confirmed = $process.WaitForExit(5000)
            } catch {
                $termination.error = $_.Exception.Message
            }
            $step.timeout_termination = $termination
            $termination | ConvertTo-Json -Depth 5 | Set-Content -LiteralPath (Join-Path $OutputDirectory ($Name + '.timeout.json')) -Encoding UTF8
            throw "$Name exceeded $StepTimeoutSeconds seconds; only its held root process was targeted. Descendant cleanup is unconfirmed."
        }
        $step.exit_code = $process.ExitCode
        # Root exit is insufficient when descendants still hold pipe handles.
        # Require both copies to reach EOF within a bounded post-exit window.
        if (-not $drain.Wait(5000)) {
            $step.status = 'unverified'
            throw "$Name exited but redirected output did not close within five seconds; logs are incomplete."
        }
        $stdoutStream.Flush()
        $stderrStream.Flush()
        $step.logs_complete = $true
        $step.status = if ($process.ExitCode -eq 0) { 'passed' } else { 'failed' }
        if ($process.ExitCode -ne 0) {
            throw "$Name failed with exit code $($process.ExitCode); inspect its raw logs."
        }
    } catch {
        if ($step.status -eq 'running') { $step.status = 'failed' }
        throw
    } finally {
        $step.duration_ms = ([DateTime]::UtcNow - $start).TotalMilliseconds
        if ($process) { $process.Dispose() }
        if ($stdoutStream) { $stdoutStream.Dispose() }
        if ($stderrStream) { $stderrStream.Dispose() }
        # Retain parsed skip/failure evidence even when the step failed or timed
        # out. Raw logs remain authoritative if classification itself fails.
        if ($EvidenceScope -eq 'native-windows-tests' -and (Test-Path -LiteralPath $stdout) -and (Test-Path -LiteralPath $stderr)) {
            try {
                $step.test_evidence = Get-TaskTestEvidence $stdout $stderr
            } catch {
                $step.test_evidence = [ordered]@{ classification_error = $_.Exception.Message }
            }
        }
    }
    if ($EvidenceScope -eq 'native-windows-tests') {
        $evidence = $step.test_evidence
        if (-not $evidence -or $evidence.Contains('classification_error')) {
            $step.status = 'unverified'
            throw "$Name lacks classified complete logs; native coverage stays open."
        }
        $totalPassed = 0
        $totalFailed = 0
        foreach ($summary in $evidence.libtest_summaries_not_acceptance_totals) {
            $totalPassed += $summary.passed
            $totalFailed += $summary.failed
        }
        if ($evidence.environment_unverified.Count -gt 0) {
            $step.status = 'unverified'
            throw "$Name reported SKIP/UNVERIFIED; native coverage stays open."
        }
        if ($totalPassed -le 0 -or $totalFailed -gt 0) {
            $step.status = 'failed'
            throw "$Name lacks successful real test summaries; zero-test/filter output cannot satisfy the gate."
        }
        foreach ($required in @('native_create_and_delete_preserve_unicode_and_spaces', 'native_reliable_add_delete_file_and_directory_rename_stay_incremental')) {
            if ($evidence.passed_test_names -notcontains $required) {
                $step.status = 'unverified'
                throw "$Name does not prove required native test $required passed; inspect complete logs."
            }
        }
    }
}

try {
    if ([Environment]::OSVersion.Platform -ne [PlatformID]::Win32NT) {
        throw 'Native Windows execution is required; GNU cross-checks are separate evidence.'
    }
    $TaskSummary.native_windows = $true
    $RepoPath = (Resolve-Path -LiteralPath $RepoPath).Path
    $null = Get-TaskLocalDriveRoot $RepoPath
    if ([string]::IsNullOrWhiteSpace($OutputDirectory)) {
        $OutputDirectory = Join-Path (Split-Path -Parent $RepoPath) ('Loci-Windows-Validation-' + $Commit.Substring(0, 12) + '-' + [Guid]::NewGuid().ToString('N').Substring(0, 8))
    }
    if (Test-Path -LiteralPath $OutputDirectory) {
        throw 'OutputDirectory already exists; choose a fresh owned evidence directory.'
    }
    $null = New-Item -ItemType Directory -Path $OutputDirectory
    $TaskOutputCreated = $true
    $OutputDirectory = (Resolve-Path -LiteralPath $OutputDirectory).Path
    $driveRoot = Get-TaskLocalDriveRoot $OutputDirectory
    $volume = Get-Volume -DriveLetter $driveRoot.Substring(0, 1) -ErrorAction Stop
    if ($volume.FileSystem -ne 'NTFS') { throw 'Use an NTFS evidence/worktree location.' }
    # Reject redirected output ancestors so drive-letter metadata describes the
    # actual test/build location rather than a junction to another filesystem.
    $ancestor = Get-Item -LiteralPath $OutputDirectory
    while ($ancestor) {
        if (($ancestor.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0) {
            throw 'Use a direct local NTFS output path without reparse-point ancestors.'
        }
        $ancestor = $ancestor.Parent
    }
    $git = (Get-Command git -CommandType Application -ErrorAction Stop).Source
    $rustup = (Get-Command rustup -CommandType Application -ErrorAction Stop).Source
    $toolchains = & $rustup toolchain list
    if ($LASTEXITCODE -ne 0) { throw 'Could not enumerate already-installed Rust toolchains.' }
    $installed = @($toolchains | ForEach-Object { ($_ -split '\s+')[0] })
    $candidates = @($installed | Where-Object { $_ -eq $Toolchain })
    if ($candidates.Count -eq 0 -and $Toolchain -eq '1.99.0') {
        $candidates = @($installed | Where-Object { $_ -match '^1\.99\.0-.*-windows-' })
    }
    if ($candidates.Count -eq 0) {
        throw 'Rust 1.99.0 native Windows must already be installed; no toolchain download is attempted.'
    }
    $ResolvedToolchain = $candidates[0]
    $rustVersion = & $rustup run $ResolvedToolchain rustc -Vv
    if ($LASTEXITCODE -ne 0) { throw 'The installed Rust toolchain could not run rustc.' }
    $rustText = $rustVersion -join [Environment]::NewLine
    if ($rustText -notmatch 'host: .*windows' -or $rustText -notmatch '(?m)^release: 1\.99\.0(?:\r?$|-)') {
        throw 'Rust 1.99.0 on a native Windows host is required.'
    }
    $identity = [Security.Principal.WindowsIdentity]::GetCurrent()
    $principal = New-Object Security.Principal.WindowsPrincipal($identity)
    $os = Get-CimInstance Win32_OperatingSystem
    $cpu = Get-CimInstance Win32_Processor
    $computer = Get-CimInstance Win32_ComputerSystem
    $TaskSummary.environment = [ordered]@{
        os_caption = $os.Caption; os_version = $os.Version; os_build = $os.BuildNumber
        os_architecture = $os.OSArchitecture
        cpu = @($cpu | ForEach-Object { $_.Name })
        logical_processors = $computer.NumberOfLogicalProcessors
        memory_bytes = $computer.TotalPhysicalMemory
        filesystem = $volume.FileSystem; drive = $volume.DriveLetter
        free_bytes = $volume.SizeRemaining
        administrator = $principal.IsInRole([Security.Principal.WindowsBuiltInRole]::Administrator)
        rustc = $rustText; selected_toolchain = $ResolvedToolchain
        toolchains = @($toolchains); powershell = $PSVersionTable.PSVersion.ToString()
    }
    $TaskSummary.runner_sha256 = (Get-FileHash -LiteralPath $PSCommandPath -Algorithm SHA256).Hash.ToLowerInvariant()
    $TaskPhase = 'bundle'
    if ($BundlePath) {
        $BundlePath = (Resolve-Path -LiteralPath $BundlePath).Path
        $null = Get-TaskLocalDriveRoot $BundlePath
        & $git -C $RepoPath bundle verify $BundlePath *> (Join-Path $OutputDirectory 'bundle-verify.log')
        if ($LASTEXITCODE -ne 0) { throw 'Local bundle verification failed.' }
        $advertised = @(& $git -C $RepoPath bundle list-heads $BundlePath 'refs/heads/codex/linux-million-search')
        $bundleListExit = $LASTEXITCODE
        $advertisedCommit = if ($advertised.Count -eq 1) { (($advertised[0] -split '\s+')[0]).ToLowerInvariant() } else { $null }
        if ($bundleListExit -ne 0 -or $advertisedCommit -ne $Commit.ToLowerInvariant()) {
            throw 'Bundle branch tip is not the frozen exact commit.'
        }
        $TaskSummary.bundle_sha256 = (Get-FileHash -LiteralPath $BundlePath -Algorithm SHA256).Hash.ToLowerInvariant()
        $bundleRef = 'refs/codex-validation/windows/' + $Commit.ToLowerInvariant()
        & $git -C $RepoPath fetch --no-tags --no-recurse-submodules $BundlePath ("refs/heads/codex/linux-million-search:" + $bundleRef) *> (Join-Path $OutputDirectory 'bundle-import.log')
        if ($LASTEXITCODE -ne 0) { throw 'Local bundle import failed; no remote fetch is attempted.' }
    }
    $TaskPhase = 'checkout'
    $resolved = & $git -C $RepoPath rev-parse --verify ($Commit + '^{commit}')
    if ($LASTEXITCODE -ne 0 -or $resolved.Trim().ToLowerInvariant() -ne $Commit.ToLowerInvariant()) {
        throw 'The requested exact commit is unavailable locally.'
    }
    $TaskWorktree = Join-Path $OutputDirectory 'checkout'
    & $git -C $RepoPath worktree add --detach $TaskWorktree $Commit *> (Join-Path $OutputDirectory 'worktree-create.log')
    if ($LASTEXITCODE -ne 0) { throw 'Could not create an isolated detached worktree.' }
    $TaskSummary.worktree = $TaskWorktree
    $actual = & $git -C $TaskWorktree rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $actual.Trim().ToLowerInvariant() -ne $Commit.ToLowerInvariant()) {
        throw 'Checkout does not match the requested exact commit.'
    }
    $TaskSummary.actual_commit = $actual.Trim().ToLowerInvariant()
    $dirty = & $git -C $TaskWorktree status --porcelain --untracked-files=no
    if ($LASTEXITCODE -ne 0 -or $dirty) { throw 'The isolated tracked checkout is not clean.' }
    $env:CARGO_TARGET_DIR = Join-Path $OutputDirectory 'target'
    $env:CARGO_TERM_COLOR = 'never'
    $TaskPhase = 'steps'
    Invoke-TaskStep 'format' $rustup @('run', $ResolvedToolchain, 'cargo', 'fmt', '--check') $TaskWorktree 'format-only'
    Invoke-TaskStep 'all-targets' $rustup @('run', $ResolvedToolchain, 'cargo', 'check', '--all-targets', '--offline', '--locked') $TaskWorktree 'windows-type-check'
    Invoke-TaskStep 'linux-ffi-types' $rustup @('run', $ResolvedToolchain, 'cargo', 'check', '--all-targets', '--offline', '--locked', '--features', 'linux-ffi-check') $TaskWorktree 'linux-ffi-types-only'
    Invoke-TaskStep 'debug-native' $rustup @('run', $ResolvedToolchain, 'cargo', 'test', '--offline', '--locked', '--', '--nocapture', '--test-threads=1') $TaskWorktree 'native-windows-tests'
    Invoke-TaskStep 'release-native' $rustup @('run', $ResolvedToolchain, 'cargo', 'test', '--release', '--offline', '--locked', '--', '--nocapture', '--test-threads=1') $TaskWorktree 'native-windows-tests'
    $TaskPhase = 'verification'
    $after = & $git -C $TaskWorktree rev-parse HEAD
    if ($LASTEXITCODE -ne 0 -or $after.Trim().ToLowerInvariant() -ne $Commit.ToLowerInvariant()) {
        throw 'Test execution changed the requested commit.'
    }
    $dirty = & $git -C $TaskWorktree status --porcelain --untracked-files=no
    if ($LASTEXITCODE -ne 0 -or $dirty) { throw 'Test execution changed the tracked checkout.' }
    $TaskSummary.artifacts = @(Get-ChildItem -LiteralPath $env:CARGO_TARGET_DIR -Recurse -File -Filter '*.exe' | ForEach-Object {
        [ordered]@{ path = $_.FullName; bytes = $_.Length; sha256 = (Get-FileHash -LiteralPath $_.FullName -Algorithm SHA256).Hash.ToLowerInvariant() }
    })
    $TaskSummary.verified = $true
    $TaskSummary.status = 'passed'
} catch {
    $TaskSummary.failure = $_.Exception.Message
    $TaskSummary.failure_phase = $TaskPhase
    $lastStep = if ($TaskSteps.Count -gt 0) { $TaskSteps[$TaskSteps.Count - 1] } else { $null }
    $TaskSummary.status = if ($TaskPhase -eq 'preflight' -or ($lastStep -and $lastStep.status -eq 'unverified')) { 'unverified' } else { 'failed' }
} finally {
    $env:CARGO_TARGET_DIR = $TaskPreviousTarget
    $env:CARGO_TERM_COLOR = $TaskPreviousColor
    Set-Location -LiteralPath $TaskOriginalDirectory
    $TaskSummary.finished_utc = [DateTime]::UtcNow.ToString('o')
    $TaskSummary.steps = @($TaskSteps.ToArray())
    if ($TaskOutputCreated -and $OutputDirectory -and (Test-Path -LiteralPath $OutputDirectory)) {
        $TaskSummary | ConvertTo-Json -Depth 10 | Set-Content -LiteralPath (Join-Path $OutputDirectory 'summary.json') -Encoding UTF8
    }
}
if (-not $TaskSummary.verified) {
    Write-Error ($TaskSummary.failure) -ErrorAction Continue
    exit 1
}
Write-Output ("Exact native Windows steps completed for " + $Commit + "; review evidence: " + (Join-Path $OutputDirectory 'summary.json'))
