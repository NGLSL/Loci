#!/usr/bin/env python3
"""Read retained driver evidence. Completion and exit status do not prove acceptance."""
import argparse
import collections
import json
from pathlib import Path

MIB = 1024 * 1024


def assess(output, expected_sha=None):
    rows = collections.defaultdict(list)
    with (output / 'timings.jsonl').open() as stream:
        for line in stream:
            record = json.loads(line)
            rows[record['phase']].append(record['row'])
    manifest = rows['manifest'][0]
    checks = {}

    def check(name, present, valid, details=None):
        status = 'unverified' if not present else 'passed' if valid else 'failed'
        checks[name] = {'status': status, 'details': details}

    check('source_binding', bool(expected_sha), manifest.get('sha') == expected_sha,
          {'recorded_sha': manifest.get('sha'), 'expected_sha': expected_sha})
    completed = output / 'completed.json'
    check('measurement_completed', completed.exists(),
          completed.exists() and json.loads(completed.read_text()).get('measurement_completed') is True)
    initial = [row for row in rows['correctness'] if row.get('stage') == 'initial']
    check('real_million_initial_byte_kind_oracle', bool(initial),
          bool(initial) and initial[0].get('count') == 1_000_000
          and initial[0].get('directories') == 20_000
          and initial[0].get('full_byte_set_equal') is True
          and initial[0].get('full_kind_set_equal') is True, initial)

    queries = rows['query-summary']
    classes = {(row.get('pass'), row.get('query')) for row in queries}
    query_valid = (len(queries) == 104 and len(classes) == 104
                   and {row.get('pass') for row in queries} == {0, 1}
                   and all(len({row.get('query') for row in queries
                                if row.get('pass') == group}) == 52 for group in (0, 1))
                   and all(row.get('samples', 0) >= 200
                           and row.get('p95_ns', float('inf')) <= 50_000_000
                           for row in queries))
    check('first50_standard52_two_passes_200_repetitions', bool(queries), query_valid,
          {'records': len(queries), 'failed': [row for row in queries
            if row.get('samples', 0) < 200 or row.get('p95_ns', float('inf')) > 50_000_000]})
    filtered = rows['filtered-query-correctness']
    suite = rows['filtered-query-suite']
    check('independent_all52_query_path_kind_count_sort', bool(suite),
          bool(suite) and len(filtered) == 52
          and {row.get('query_id') for row in filtered} == set(range(52))
          and all(row.get('full_filtered_path_set_equal') is True
                  and row.get('full_filtered_kind_set_equal') is True
                  and row.get('count_exact') is True for row in filtered)
          and suite[-1].get('representative_sort_oracles_equal') is True,
          {'queries_verified': len(filtered), 'suite': suite})

    events = rows['event-summary']
    check('ordinary_native_event_200_each_class', bool(events),
          {row.get('class') for row in events} == {'add', 'delete', 'file-rename'}
          and len(events) == 3 and all(row.get('samples', 0) >= 200
              and row.get('p95_ns', float('inf')) <= 500_000_000 for row in events), events)
    corrections = rows['full-correction']
    check('three_full_correction_publications', bool(corrections),
          len(corrections) >= 3 and all(row.get('after', {}).get('status') == 'Validated'
                                     for row in corrections),
          [{'trial': row.get('trial'), 'elapsed_ns': row.get('elapsed_ns'),
            'resources': row.get('resources')} for row in corrections])
    settles = rows['maintenance-settle']
    def steady_valid(row):
        resource = row.get('last_resources', {})
        return (row.get('quiet_single_epoch') is True
                and resource.get('vmrss_bytes', float('inf')) <= 200 * MIB
                and resource.get('vmhwm_bytes', float('inf')) <= 512 * MIB)
    check('post_epoch_unleased_steady_rss_hwm', bool(settles),
          {'compaction', 'correction-0', 'correction-1', 'correction-2'}
          <= {row.get('stage') for row in settles}
          and all(steady_valid(row) for row in settles),
          [{'stage': row.get('stage'), 'rss_bytes': row.get('last_resources', {}).get('vmrss_bytes'),
            'hwm_bytes': row.get('last_resources', {}).get('vmhwm_bytes'),
            'numeric_pass': steady_valid(row)} for row in settles])
    idle = rows['idle-summary']
    def idle_valid(row):
        resource = row.get('last_sample', {})
        return (row.get('actual_ns', 0) >= 600_000_000_000
                and row.get('cpu_percent_one_core', float('inf')) <= 1.0
                and resource.get('vmrss_bytes', float('inf')) <= 200 * MIB
                and resource.get('vmhwm_bytes', float('inf')) <= 512 * MIB)
    check('real600_second_idle_cpu_rss_hwm', bool(idle),
          len(idle) == 1 and idle_valid(idle[0]), idle)
    saved = rows['save-summary']
    # A stages-only run deliberately saves once; it leaves save200 unverified.
    check('save200_independently_timed', bool(saved) and
          (manifest.get('phase') == 'all' or saved[0].get('samples', 0) >= 200),
          len(saved) == 1 and saved[0].get('samples', 0) >= 200, saved)
    restarts = rows['restart-summary']
    check('crossprocess_stale_open200_p95_2seconds', bool(restarts),
          len(restarts) == 1 and restarts[0].get('samples', 0) >= 200
          and restarts[0].get('p95_ns', float('inf')) <= 2_000_000_000
          and restarts[0].get('full_correction_separate') is True, restarts)

    stops = rows['stop'] + rows['restart-stop']
    stop_failures = []
    for row in stops:
        reply = row.get('stop_reply', {})
        baseline = reply.get('baseline_resources', {})
        post = row.get('independent_post_resources', {})
        valid = (reply.get('joined') is True and baseline.get('pid') is not None
                 and baseline.get('pid') == post.get('pid')
                 and baseline.get('process_start_ticks') is not None
                 and baseline.get('process_start_ticks') == post.get('process_start_ticks')
                 and baseline.get('fd_count') is not None
                 and baseline.get('fd_count') == post.get('fd_count')
                 and baseline.get('kernel_watch_count') == post.get('kernel_watch_count') == 0)
        if not valid:
            stop_failures.append(row)
    check('same_live_pid_stop_resource_baselines', bool(stops), not stop_failures,
          {'stops': len(stops), 'failed': stop_failures})
    return {'schema': 1, 'output': str(output.resolve()), 'source_sha': manifest.get('sha'),
            'phase': manifest.get('phase'), 'checks': checks, 'full_product_acceptance': False,
            'reference_hardware_verified': False,
            'native_filesystem_windows_24h_gates': 'separate evidence required'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('output', type=Path)
    parser.add_argument('--sha', help='expected exact source SHA; omitted means binding unverified')
    args = parser.parse_args()
    print(json.dumps(assess(args.output, args.sha), ensure_ascii=False, indent=2))


if __name__ == '__main__':
    main()
