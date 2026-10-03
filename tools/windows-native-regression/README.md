# Exact-commit Windows native regression handoff

This runner prepares evidence for the existing Windows Engine and shared-core regression gate. **PowerShell parsing and native Windows execution remain unverified in the cloud environment.** Static source inspection does not satisfy the gate. Task 18 remains open until its native evidence and the other required gates are reviewed.

The runner uses an isolated detached worktree on local NTFS beside `D:\Project\Loci`. It preserves the existing checkout, including local documents. It neither installs tools nor changes execution policy, resets a checkout, fetches a remote, pushes, or publishes. The expected commit must be a complete 40-character SHA; an abbreviated SHA or a moving branch is rejected.

## Local prerequisites and invocation

Use a native Windows machine with Git, rustup, an **already installed** Rust 1.99.0 Windows toolchain, rustfmt, the selected toolchain's linker, and cached Cargo dependencies. Cargo checks and tests use `--offline --locked`; a missing dependency is a failure, never a request to install it. `Get-Volume` and Windows CIM metadata must be available. Use a direct local NTFS output path without junction/reparse-point ancestors, with sufficient free space. No administrator elevation is requested.

Review [runner.ps1](runner.ps1) locally and parse it with the PowerShell parser before execution. For example, this checks syntax without running the runner or changing execution policy:

```powershell
$tokens = $null
$parseErrors = $null
$runner = 'D:\Project\Loci\tools\windows-native-regression\runner.ps1'
[System.Management.Automation.Language.Parser]::ParseFile($runner, [ref]$tokens, [ref]$parseErrors) | Out-Null
$parseErrors
if ($parseErrors.Count -ne 0) { throw 'Runner parsing failed; do not execute it.' }
```

Execute only after that check succeeds and the exact source commit is present locally:

```powershell
& 'D:\Project\Loci\tools\windows-native-regression\runner.ps1' `
  -Commit '<EXACT_40_HEX_SHA>' `
  -RepoPath 'D:\Project\Loci' `
  -OutputDirectory 'D:\Project\Loci-Windows-Validation-<UNIQUE_RUN_ID>'
```

Replace the placeholders. The output directory must not already exist. If needed, `-Toolchain 1.99.0-x86_64-pc-windows-msvc` selects an already installed native toolchain and `-StepTimeoutSeconds 1800` changes each step's bounded timeout (60–3600 seconds). The runner restores the calling process's target-directory and color environment variables when it finishes.

## Local bundle transfer

When the exact commit is unavailable locally, transfer a full-history Git bundle through an approved local file transfer. The source checkout must already contain the frozen commit at `refs/heads/main`; creating this bundle requires no remote access or push:

```text
git bundle create loci-exact.bundle refs/heads/main
git bundle verify loci-exact.bundle
git bundle list-heads loci-exact.bundle refs/heads/main
```

Record the SHA-256 digest before transfer and compare it on Windows with `Get-FileHash -Algorithm SHA256`. Pass the local file with `-BundlePath 'D:\Project\Transfers\loci-exact.bundle'`. The runner verifies the bundle and requires that branch's advertised tip to equal the requested SHA, then imports it into `refs/codex-validation/windows/<SHA>`. It preserves the existing working branch and logs the local import. No remote fallback is attempted. The bundle must contain the required history, or already-present prerequisite objects must satisfy `git bundle verify`.

## Evidence and failure interpretation

The four sequential steps are formatting, all-target Windows type checking, full debug tests, and full release tests. Both test commands are unfiltered and retain `--nocapture --test-threads=1`. This gate covers the existing Windows event backend and shared core, not MFT/USN or GUI work. See [AGENTS.md](../../AGENTS.md) and the [validation guide](../../docs/VALIDATION.md) for repository scope.

Each step retains raw stdout/stderr, its command, duration, exit status, and process identity in `summary.json`. The summary records the exact checkout SHA, runner/bundle digests, Windows/NTFS/Rust metadata, and generated executable digests. The checkout, target directory and raw logs remain available after success or failure; cleanup is a separate local action after review.

A zero exit code alone does not pass either native test step. The runner requires libtest summaries with passing tests and the actual native cases `native_create_and_delete_preserve_unicode_and_spaces` and `native_reliable_add_delete_file_and_directory_rename_stay_incremental`. Missing proof, zero-test/filter output, or any `SKIP:`/`UNVERIFIED:` marker keeps coverage open. Parsed markers preserve their reason, log and test name where available; otherwise attribution explicitly requires manual review. Ignored tests are recorded separately and do not count as passed. Raw libtest totals may also include simulated/shared-core tests and must not be described as native test totals. Review platform exclusions in the exact source alongside the logs.

On timeout, termination targets only the held root `Process`, using its retained handle and creation-time check. It never reacquires a process by PID, invokes `taskkill`, or kills an unrelated process. **Descendants are not terminated or confirmed released.** They may continue running; the timeout sidecar explicitly records this uncertainty, and the gate fails. The runner uses a bounded five-second post-termination wait. After normal root exit, both raw byte-stream copies must reach EOF within five seconds; otherwise logs remain incomplete and the step is unverified. Descendants retaining pipe handles cannot force an unbounded output-drain wait. Review remaining descendants locally before cleanup or retry; no automatic PID-based cleanup is provided. A hung command, incomplete log or failed preflight is not native acceptance.

A successful runner summary is evidence for these listed steps, not approval to resolve Task 18, publish or merge. Preserve raw results and the exact-SHA receipt for the final review.
