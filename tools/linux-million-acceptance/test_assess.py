"""Public assessment CLI checks with literal external driver evidence only."""
import json
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ASSESS = Path(__file__).with_name('assess.py')
SHA = 'a' * 40

class EventEvidenceCLI(unittest.TestCase):
    def read_assessment(self, events):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)
            rows = [{'phase': 'manifest', 'row': {'sha': SHA, 'phase': 'all'}}]
            rows += [{'phase': 'event-summary', 'row': event} for event in events]
            raw = ''.join(json.dumps(row) + '\n' for row in rows)
            (output / 'timings.jsonl').write_text(raw)
            result = subprocess.run([sys.executable, str(ASSESS), str(output), '--sha', SHA],
                                    capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual((output / 'timings.jsonl').read_text(), raw)
            report = json.loads(result.stdout)
            self.assertFalse(report['full_product_acceptance'])
            self.assertFalse(report['reference_hardware_verified'])
            return report['checks']['ordinary_native_event_200_each_class']

    def canonical(self):
        return [{'class': 'add', 'samples': 200, 'p95_ns': 32_948_385},
                {'class': 'delete', 'samples': 200, 'p95_ns': 32_821_376},
                {'class': 'file-rename', 'samples': 200, 'p95_ns': 47_531_609}]

    def test_public_driver_file_rename_class_meets_existing_gate(self):
        events = self.canonical()
        gate = self.read_assessment(events)
        self.assertEqual(gate['status'], 'passed')
        self.assertEqual(gate['details'], events)

    def test_unknown_missing_and_duplicate_event_classes_cannot_pass(self):
        for events in [self.canonical()[:2], self.canonical()[1:],
                       [self.canonical()[0], self.canonical()[1], self.canonical()[1]],
                       [*self.canonical(), {'class': 'other', 'samples': 200, 'p95_ns': 1}],
                       [*self.canonical()[:2], {'class': 'rename', 'samples': 200, 'p95_ns': 1}],
                       [*self.canonical()[:2], {'class': 'unknown', 'samples': 200, 'p95_ns': 1}]]:
            with self.subTest(events=events):
                self.assertEqual(self.read_assessment(events)['status'], 'failed')
        self.assertEqual(self.read_assessment([])['status'], 'unverified')

    def test_fewer_than_200_samples_and_over_500ms_cannot_pass(self):
        for key, value in [('samples', 199), ('p95_ns', 500_000_001)]:
            for index in range(3):
                events = self.canonical()
                events[index][key] = value
                with self.subTest(key=key, index=index):
                    self.assertEqual(self.read_assessment(events)['status'], 'failed')

    def test_existing_sample_and_latency_boundaries_are_inclusive(self):
        events = [{'class': name, 'samples': 200, 'p95_ns': 500_000_000}
                  for name in ['add', 'delete', 'file-rename']]
        self.assertEqual(self.read_assessment(events)['status'], 'passed')

if __name__ == '__main__':
    unittest.main()
