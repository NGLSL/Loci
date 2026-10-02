# Native million acceptance draft protocol v1

Repository example uses the public production Engine/MonitorOwner in a child PID.
Controller/oracle/resources have a separate PID. Commands are opt-in; native
execution and numerical gates are reported separately from hardware eligibility.

Worker invocation:
    linux_million_acceptance --worker --root ABS --database ABS --output ABS --sha40HEX
    [--output-budget-mib4096] [--smoke]

Above spaced placeholders mean separateargv e.g --sha 40HEX. Controller:
    linux_million_acceptance --controller --root ABS --database ABS --output ABS
    --sha 40HEX --queries UTF8_ONE_QUERY_PER_LINE --phase all --repetitions200
    --idle-seconds600 [--output-budget-mib4096] [--smoke]

All valued options use separateargv. Root/db/output absolute, db/output outside
selectedroot. Phase all|smoke|correctness|queries|events|restart|idle|fixture.
Reduced repetitions/window require --smoke; never qualify acceptance.
The archived queryfile preserves the original32, full ticket12 39-query suite,\nand ii/ia/rr short-negative cases; an initial blank line is the empty query.
Current controller accepts existing verified-owned roots; fixturecopy optional
--fixture-input copies zero-byte realfiles/directories to a freshroot.
Btrfs newfixturecopy is explicitly Unsupported until physical metadata DUP pilot
and unallocated-budget guard integrates; existing realBtrfs roots are supported.

Framing in both directions:4byte little-endian u32 length followed by body,
0<length<=65536. EOF/partial/oversize/malformed body fail protocol; no detached
blocking reader. Request body ASCII TSV:
    monotonically_increasing_u64_id<TAB>OP<TAB>arg...
Queries/original arbitrarypath bytes/outputnames use lowercasehex. Empty query
is empty final field, retain trailingtab. Replies UTF8 JSON, commonfields schema1,
id,op,ok,pid; failure has error. Replies do not contain raw filenames unescaped.

Automatic HELLO id0 before reading requests:
sha/pid/process_start_ticks, baseline_resources sampled beforeEngineopen,
open_ns, searchable_stale_ns(null withoutpublishedsnapshot), loaded_status,
loaded_version, poll_ms20, live_entry_limit, output_budget_bytes, initialSTATUS.
HELLO sourceSHA supplied tag does not itself verify compiledsource; caller binds
exact frozencommit+binarySHA256 artifact manifest and /proc PID executabledigest.

Commands:
- STATUS ->status/version/leases/gaps(kind,path_hex,error,nullableerrno),
  losses(arraynames, realKernelOverflow distinct), metrics/resources.
- QUERY50 queryHex ->paths_hex/version/status_start/status_finish/complete/
  validated/cancelled/total_public_ns/page_only_ns. Actual publiclease+page elapsed
  includes lease acquisition/path construction; IPCroundtrip separately controller.
- HOLD_LEASE ->lease_id/version. Max2. RELEASE_LEASE leaseid ->released.
- EXPORT basenameHex queryHex ->job_id. LEASE_EXPORT leaseid basenameHex queryHex
  ->job_id, retains heldlease until RELEASE_LEASE (export ownArc).
- COUNT_START queryHex / SORT_START queryHex ->job_id. Max2 productionworkers.
- SORT_PAGE jobid offset size1..50 ->paths_hex/version/complete, offset+=returned.
- SAVE / REBUILD / COMPACT ->job_id, asynchronous monitorcommand admission.
  OP_STATE Complete forREBUILD/COMPACT means requestack, notcoverage completion;
  continue STATUS untilValidated and requiredcompaction epoch/counters.
- OP_STATE jobid ->Pending/Running/Complete/Cancelled/Failed,error/progress/
  version/count/elapsed whereavailable. ExportComplete includes path_hex/
  kinds_path_hex/count/version/bytes/complete/validated_start_finish.
- OP_DROP jobid ->released; required afterterminal observation to keepjobtable
  bounded12 and release sortlease/output state. At most1 foregroundexportjob,
  and it mustdropbeforeanotherexport. Droppingunfinishedexport cancels+joins.
- CANCEL jobid: query/export cooperativecancel; monitorcommands cannotcancel.
  CANCEL_CORRECTION: shared ownercancel request; observe actualgap/status afterward.
- STOP timeout_ms<=60000 [keep_alive1] cancels/joins/drops jobs+leases+owner,
  replies stopped/joined/baseline_resources/post_resources. Onlysuccessprovesjoin.
  Keepalive1 lets externalcontroller independently sample samePID afterrelease;
  QUIT thenexits. DefaultSTOP exits afterreply. EOF cancels/joins owner oncleanup.

Artifact exports rawNUL absolute bytepaths plushexpath<TAB>F/D/L kinds sidecar.
Exports use QueryPage.kinds directly at matching IDs, aligned with paths.
No path-based entry_kind lookup or live filesystem kind substitution is used.
Fresh fullcut requires complete+validated_start_finish+unchanged publishedversion.
Oldheldexport can becomplete/coherent butvalidated_start_finish=false afternew
publication; caller comparesit againstthe independentlyheldoldoracle/version.

Resource samples read actual /proc/PID VmRSS/VmHWM/cpu ticks/fdinfo watches.
fd enumeration dropsiterator thenrechecks links, avoidingowntransientdirectoryfd.
Kernel slabbytes unavailable, never inferred. STOP reply and independent external
postcounts corroboratesameboundPID release. PIDidentity checked viastart_ticks;
no reusingold numericPID afterexit.

Run aggregate budget defaults4096MiB, configurable512..65536MiB. Reserve320MiB
forboundedtiming/resourceslogs separately; admitexport/oracle/sort diskspill
againstremainingaggregate usage and actualfilesystem15%/64MiB reserve.
Filepath andkindsidecar each<=512MiB, periodicaggregatewrite guards; atmostoneexport.
Successfulcontrollerfullcuts hashcanonicalrawsortedexports/oracles/kinds, retain
initialcut+latestcut, discardonlyowned successfulduplicate/olderartifacts.
Failures keeprawmismatch/partialsets andtimings; do notdeletefailedset toresumegate.
17 supervisor mustuseequivalentboundedartifactretention, OP_DROP and declaredbudget.

Nativeoverflow requires controllerboundPID SIGSTOP, realfilesystemeventstorm,
SIGCONT andactual KernelOverflow marker inSTATUS.losses. Noinjectedoverflowopcode.
Realpermissions handled viafixturechmod, sourceidentity/mount policies remainnative.
EOF/timeout/death/interruption isafailedstage, never600s/24h completion.
