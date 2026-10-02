# Bounded scale observation recovery

Scale queries retain immutable snapshots while correction is pending, cancelled,
failed, or blocked by readers. The native source labels actual kernel overflow
separately from application queue overflow. `View.observed_losses` is bounded
cumulative history of the event-source Loss variants; it remains available after
reliable recovery. Current `coverage_gaps` describe the affected scope/reason and
are cleared only when a complete, generation-matched correction publishes.
History alone does not mean current results are incomplete.

The scale source keeps its inotify descriptor after a truncated drain and uses
bounded eight-read polls until it reaches EAGAIN. This allows the actual
IN_Q_OVERFLOW marker behind ordinary events to be observed before replacement.
Each poll remains bounded; correction waits for this drain, and an explicit
maximum drain-poll budget prevents an endless automatic attempt under continuous
writers. The bounded compatibility source retains its original behavior.

`Engine::request_rebuild()` schedules correction while retaining old queries and
resets the finite retry/backoff budget. `poll_with_cancel(&AtomicBool)` checks
cancellation between scan records/directories. Cancelling drops the candidate and
open directory listing, marks the old snapshot pending with a Cancelled gap, and
pauses correction until a new rebuild request. Monitoring continues bounded
capture while the job is cancelled. Stop/drop still releases ownership.
Replacement of the selected root/mount fails that source and requires explicit
reopening; rebuilding never silently retargets it.

`RecoveryOptions` controls the scale retry limit/delay, coverage audit interval and
batch, and maximum loss-drain polls. Defaults are four failed correction attempts,
250 ms backoff, sixteen audited directories every five seconds, and 512 drain
polls. Directory IDs are maintained in a writer-side ordered set, so selecting an
audit batch does not traverse millions of file slots. Each audit probes identity
and listing access by opening/closing a directory without enumerating its tree.
There is no periodic full-root scan in this mode. Directory IN_ATTRIB refreshes
probe listing access immediately, so permission revocation is not delayed by the
audit cycle. Source metadata and uncertain directory identity invalidate coverage;
ordinary reliable file updates continue locally. `Metrics.audited_directories`
and `correction_attempts` make this work observable. The later idle/compaction
milestone must include it in CPU measurements.

In a scale `engine watch` session, `rebuild` uses the same owner and preserves the
published query snapshot; `cancel` pauses a correction. `query`, `export` and
`status` report pending/failed validity explicitly and may return old paths.
After restoring permission, `rebuild` resumes bounded correction. Source-loss
history and coverage diagnostics remain on stderr.

Verification at this milestone used public Engine/CLI boundaries on overlayfs
with an unprivileged UID 1000. It includes a real native kernel queue overflow
(capacity 16,384, read buffer 4 KiB), permission revocation/restoration, bounded
quiet audit, cancellation/resume, finite retry exhaustion, and CLI failed/pending
old-query recovery. Controlled EventSource loss/continuous-writer cases are
labelled simulated and do not prove native delivery. Native private-namespace
bind-mount tests separately exercise source-scope changes and unknown mount
metadata. These checks do not establish ext4/Btrfs scale, native Windows, million
entry performance, or long-run acceptance.
