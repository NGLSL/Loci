# Independent Linux Engine regressions

This test-only verification branch is intentionally **expected red** on base
`4cedb870361922a65a00d430f0afc320e3dd2724` (code parent
`657412e0eec11d762537df4c34193ee24844b18b`). It contains no production fix.
Do not merge the failing tests into main without the corresponding repairs.

## Run

On x86_64 Linux with the already installed Rust 1.99.0 toolchain:

```sh
cargo +1.99.0 fmt --check
cargo +1.99.0 check --all-targets --offline --locked
cargo +1.99.0 test --offline --locked --test linux_engine_regressions -- --test-threads=1
cargo +1.99.0 test --release --offline --locked --test linux_engine_regressions -- --test-threads=1
```

On the affected base, each test command reports **0 passed, 2 failed** and exits
101. The assertions express correct behavior; they are not ignored or inverted
to make known defects pass. Other platforms exclude this Linux-specific file.
Only ordinary scoped fixture operations and actual native notifications are used;
no system configuration or sysctl changes are needed.

## P1: excluded directory replaces an indexed empty directory

Before opening the Engine, create empty `root/active` and excluded
`root/target/hidden.txt`. Rename `target` over `active`, then poll. The affected
code reports Validated while omitting `active/hidden.txt`. A subsequent write of
`active/later.txt` is also missed while the query stays Validated.

The test requires both the incoming subtree and subsequent child to appear in a
validated snapshot. It permits either safe incremental admission or bounded
reconciliation; it does not require a particular implementation or scan count.
A later periodic audit does not justify the intervening false Validated result.

Relevant base-code paths: the missing-source rename branch in
`src/incremental.rs` calls `refresh(to)`, whose already-directory branch returns
without importing the replacement inode's subtree or ensuring its monitoring.

## P2: database parent is redirected after open

Open and save `outside-data/state.loci`, outside the monitored root. Rename that
parent directory, then place a symlink at its old path pointing to a pre-existing
directory inside the monitored root. The affected Engine's next save succeeds
and writes its database into the monitored tree.

The test accepts an error with the old database preserved, or a successful write
that remains bound to the original outside directory. It rejects writes inside
the root. It does not prescribe a canonicalize-only repair: checking a pathname
again detects this between-call change but does not eliminate a concurrent
ancestor-swap TOCTOU. A complete boundary needs verified directory identity and
write semantics, or an explicitly narrower supported concurrency contract.

Relevant base-code paths: `engine::database_path` checks containment at open;
`Engine::save` and `storage::atomic_save` later reuse the saved pathname.

## Evidence and integration

The affected base's existing Linux debug and release suites each passed 119
checks with 6 ignored; the new Engine's actual kernel-overflow recovery ran.
Both added regressions failed independently in debug and release on Linux.
Those green existing suites do not establish acceptance of the omitted cases.

The branch adds only this document and `tests/linux_engine_regressions.rs`.
It leaves `.gitignore` unchanged; the new files are explicitly tracked in the
commit. The existing workflow targets the assembly branch, so publishing this
separate validation branch does not change main or its CI configuration. No raw logs, private paths, or transfer metadata are
included.

The fixing developer can fetch this validation branch and cherry-pick its single
test-only commit onto the repair branch. Cherry-picking adds the tracked files
without needing ignore-rule changes. If copying manually under the original
allowlist, explicitly add these two paths with `git add -f`. After repairing the behavior, keep both tests enabled and run the
commands above, then the full debug/release suites against the exact repair SHA.
There are no new performance, power-loss, other-filesystem, or long-run claims.
