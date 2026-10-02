#!/usr/bin/env bash
set -euo pipefail
cd -- "$(dirname -- "$0")/.."
if [[ "$(uname -s)" != Linux || "$(uname -m)" != x86_64 ]]; then
  echo 'BLOCKED: this native FFI experiment requires x86_64 Linux' >&2; exit 2
fi
if ! command -v rustup >/dev/null || ! command -v timeout >/dev/null; then
  echo 'BLOCKED: existing rustup and timeout required; this script installs nothing' >&2; exit 2
fi
if ! rustup toolchain list | grep -q '^1[.]99[.]0-'; then
  echo 'BLOCKED: Rust 1.99.0 is not installed; no fallback or auto-install' >&2; exit 2
fi
overflow=false
live_metrics=false
for option in "$@"; do
  case "$option" in
    --kernel-overflow) overflow=true ;;
    --live-metrics) live_metrics=true ;;
    *) echo "BLOCKED: unknown option $option" >&2; exit 2 ;;
  esac
done
mkdir -p results-linux
{ uname -sr; rustc +1.99.0 -Vv; cargo +1.99.0 -V; } >results-linux/environment.txt
for name in max_user_watches max_user_instances max_queued_events; do
  if [[ -r /proc/sys/fs/inotify/$name ]]; then
    printf '%s=' "$name" >>results-linux/environment.txt
    cat "/proc/sys/fs/inotify/$name" >>results-linux/environment.txt
  fi
done
timeout 60s cargo +1.99.0 fmt --check 2>&1 | tee results-linux/fmt.txt
timeout 60s cargo +1.99.0 check --all-targets --offline 2>&1 | tee results-linux/check.txt
timeout 120s cargo +1.99.0 test --offline -- --nocapture 2>&1 | tee results-linux/debug-tests.txt
timeout 120s cargo +1.99.0 test --release --offline -- --nocapture 2>&1 | tee results-linux/tests.txt
if $overflow; then
  timeout 60s cargo +1.99.0 test --release --offline --test linux_watch native_kernel_overflow_bounded_fixture -- --ignored --nocapture 2>&1 | tee results-linux/kernel-overflow.txt
  if ! grep -q 'PASS: real IN_Q_OVERFLOW observed' results-linux/kernel-overflow.txt; then
    echo 'INCOMPLETE: actual kernel overflow was not observed; inspect SKIP/FAIL log' >&2; exit 2
  fi
fi
if $live_metrics; then
  timeout 180s python3 scripts/measure-live-linux.py
fi
echo 'Completed requested Linux checks; kernel overflow is untested unless explicitly selected and PASS observed.'
