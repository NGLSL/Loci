import importlib.util, pathlib, unittest, tempfile, os, socket, struct, threading, json, time
spec = importlib.util.spec_from_file_location('soak', pathlib.Path(__file__).with_name('soak_controller.py'))
m = importlib.util.module_from_spec(spec)
spec.loader.exec_module(m)

class SafetyTests(unittest.TestCase):

    def test_framing_is_little_endian_and_bounds_requests(self):
        frame = m.request_frame(7, 'QUERY50', ['报告'.encode().hex()])
        n = struct.unpack('<I', frame[:4])[0]
        self.assertEqual(n, len(frame) - 4)
        self.assertEqual(frame[4:], b'7\tQUERY50\te68aa5e5918a')
        for arg in ['bad\tfield', 'bad\nfield', 'x' * 65536]:
            with self.assertRaises(m.Failure):
                m.request_frame(7, 'EXPORT', [arg])

    def test_bad_response_id_and_invalid_json_fail(self):
        with self.assertRaises(m.Failure):
            m.validate_reply({'schema': 1, 'id': 2, 'op': 'STATUS', 'ok': True, 'pid': 10}, 1, 'STATUS')
        with self.assertRaises(m.Failure):
            m.validate_reply({'schema': 1, 'id': 1, 'op': 'OTHER', 'ok': True, 'pid': 10}, 1, 'STATUS')

    def test_stat_identity_parses_parentheses_in_comm(self):
        fields = ['S', '1', '0', '0', '0', '0', '0', '0', '0', '0', '0', '3', '4', '0', '0', '0', '0', '0', '0', '123', '0']
        self.assertEqual(m.parse_stat('20 (odd ) name) ' + ' '.join(fields)), {'state': 'S', 'cpu_ticks': 7, 'start_ticks': 123})

    def test_oracle_raw_bytes_hardlinks_symlink_and_boundary(self):
        with tempfile.TemporaryDirectory() as d:
            root = os.fsencode(d) + b'/root'
            os.mkdir(root)
            os.mkdir(root + b'/plain')
            os.mkdir(root + b'/boundary')
            open(root + b'/plain/raw-\xff.txt', 'wb').close()
            os.link(root + b'/plain/raw-\xff.txt', root + b'/plain/alias')
            open(root + b'/boundary/hidden', 'wb').close()
            os.symlink(b'plain', root + b'/link')
            out = pathlib.Path(d) / 'oracle.nul'
            r = m.make_oracle(root, out, [root + b'/boundary'], [], 1024)
            self.assertEqual(r['count'], 5)
            self.assertEqual(out.read_bytes().split(b'\x00')[:-1], sorted([root + b'/plain', root + b'/boundary', root + b'/plain/raw-\xff.txt', root + b'/plain/alias', root + b'/link']))

    def test_export_files_are_create_new_and_bounded(self):
        with tempfile.TemporaryDirectory() as d:
            root = os.fsencode(d) + b'/r'
            os.mkdir(root)
            open(root + b'/long-name', 'wb').close()
            out = pathlib.Path(d) / 'x'
            with self.assertRaises(m.Failure):
                m.make_oracle(root, out, [], [], 2)
            self.assertLessEqual(out.stat().st_size, 2)
            with self.assertRaises(FileExistsError):
                m.make_oracle(root, out, [], [], 1024)

    def test_interrupted_window_starts_from_zero_not_previous_duration(self):
        with tempfile.TemporaryDirectory() as d:
            store = m.RunStore(pathlib.Path(d), 'a' * 40, 'b' * 64, True)
            a = store.begin_window({'reason': 'test'})
            store.finish_window('interrupted', {'actual_elapsed_seconds': 86401})
            b = store.begin_window({'reason': 'resume'})
            self.assertNotEqual(a['window_id'], b['window_id'])
            self.assertEqual(b['accumulated_previous_seconds'], 0)
            self.assertEqual(store.document['windows'][0]['status'], 'interrupted')

    def test_preparation_never_passes_day_gate(self):
        self.assertEqual(m.verdict(True, 86401, 24, True, True, []), 'prepared-not-accepted')
        self.assertEqual(m.verdict(False, 86399, 24, True, True, []), 'incomplete')
        self.assertEqual(m.verdict(False, 86401, 24, True, False, []), 'failed')
        self.assertEqual(m.verdict(False, 86401, 24, True, True, []), 'passed')

    def test_schedule_special_phases_are_explicit(self):
        self.assertEqual(m.hour_kind(0), 'kernel-overflow')
        self.assertEqual(m.hour_kind(12), 'kernel-overflow')
        self.assertEqual(m.hour_kind(6), 'user-overflow')
        self.assertEqual(m.restart_kind(11), 'crash')
        self.assertEqual(m.restart_kind(5), 'graceful')
        self.assertIsNone(m.restart_kind(4))

    def test_aggregate_admission_counts_old_logs_checkpoint_and_pending_spill(self):
        with tempfile.TemporaryDirectory() as d:
            base=pathlib.Path(d);run=base/'evidence';run.mkdir();root=base/'fixture';root.mkdir()
            old=run/'window-old';old.mkdir();(old/'logs').mkdir();(old/'logs'/'resources.jsonl').write_bytes(b'x'*5);(run/'state.db').write_bytes(b'y'*5)
            owner=m.Supervisor.__new__(m.Supervisor);owner.run=run;owner.root=os.fsencode(root);owner.cfg={'artifact_budget_bytes':12};owner.event=lambda *a,**k:None
            owner.check_budgets(2)
            with self.assertRaises(m.Failure):owner.check_budgets(3)
            (old/'unknown-link').symlink_to('/etc/passwd')
            with self.assertRaises(m.Failure):owner.check_budgets()

    def test_log_files_rotate_by_real_hour_without_resetting_global_budget(self):
        with tempfile.TemporaryDirectory() as d:
            logs=m.Logs(pathlib.Path(d)/'logs',max_bytes=20)
            try:
                logs.write('events',b'a');logs.started-=3600;logs.write('events',b'b')
                self.assertEqual(len(list(logs.root.glob('events-hour*.jsonl'))),2)
                self.assertEqual(logs.total,2)
            finally:logs.close()

    def test_resource_gate_cannot_be_relaxed_in_configuration(self):
        with tempfile.TemporaryDirectory() as d:
            base=pathlib.Path(d);run=base/'run';run.mkdir();cfg={'sha':'a'*40,'worker_binary_sha256':'b'*64,'worker_command':['/example/worker'],'root':str(base/'fixture'),'database':str(run/'state.db')}
            m.validate_config(cfg,run,False)
            for key,value in [('quiet_cpu_limit_percent',2),('steady_rss_limit_bytes',201*1024*1024),('peak_rss_limit_bytes',513*1024*1024)]:
                with self.assertRaises(m.Failure):m.validate_config({**cfg,key:value},run,False)

    def test_owned_mutation_receipt_restores_renamed_directory_without_deleting_original(self):
        with tempfile.TemporaryDirectory() as d:
            base=pathlib.Path(d);root=base/'fixture';root.mkdir();parent=root/'dir00000';parent.mkdir();original=parent/'original';original.write_bytes(b'keep');run=base/'evidence';run.mkdir()
            cfg={'sha':'a'*40,'worker_binary_sha256':'b'*64,'root':str(root),'database':str(run/'state.db'),'mutation_parent':'dir00000'}
            owner=m.Supervisor(cfg,run,True)
            try:
                added=os.fsencode(parent/'temporary');owner.create_file(added);owner.directory_rename()
                recovery=m.Supervisor(cfg,run,True)
                try:
                    recovery.restore_receipt();self.assertEqual(original.read_bytes(),b'keep');self.assertFalse(os.path.lexists(added));self.assertFalse(recovery.created)
                finally:recovery.shutdown()
            finally:owner.shutdown()

    def test_interrupted_unproven_create_is_kept_for_inspection(self):
        with tempfile.TemporaryDirectory() as d:
            base=pathlib.Path(d);root=base/'fixture';root.mkdir();parent=root/'dir00000';parent.mkdir();run=base/'evidence';run.mkdir();cfg={'sha':'a'*40,'worker_binary_sha256':'b'*64,'root':str(root),'database':str(run/'state.db')}
            owner=m.Supervisor(cfg,run,True)
            try:
                p=parent/'uncertain';p.write_bytes(b'evidence');owner.created={os.fsencode(p).hex():None};owner.journal()
                with self.assertRaises(m.Failure):owner.restore_receipt()
                self.assertEqual(p.read_bytes(),b'evidence')
            finally:owner.shutdown()

    def test_resume_rejects_different_binary_and_preparation_class(self):
        with tempfile.TemporaryDirectory() as d:
            m.RunStore(pathlib.Path(d),'a'*40,'b'*64,True)
            with self.assertRaises(m.Failure):m.RunStore(pathlib.Path(d),'a'*40,'c'*64,True)
            with self.assertRaises(m.Failure):m.RunStore(pathlib.Path(d),'a'*40,'b'*64,False)

class TransportTests(unittest.TestCase):

    def worker(self, mode, logs):
        return m.Client([__import__('sys').executable, str(pathlib.Path(__file__).with_name('fake_protocol_worker.py')), mode], logs, timeout=2)

    def test_real_pipe_frame_roundtrip_and_bound_pid(self):
        with tempfile.TemporaryDirectory() as d:
            logs = m.Logs(pathlib.Path(d) / 'logs')
            client = self.worker('normal', logs)
            try:
                self.assertEqual(client.call('QUERY50', '7265706f7274')['paths_hex'], ['2f666978747572652f7265706f7274'])
                self.assertEqual(client.transport.check()['pid'], client.hello['pid'])
                client.call('QUIT')
                client.finish()
            finally:
                client.abort()
                logs.close()

    def test_wrong_sequence_fails_without_hanging_or_claiming_acceptance(self):
        with tempfile.TemporaryDirectory() as d:
            logs = m.Logs(pathlib.Path(d) / 'logs')
            client = self.worker('wrong-id', logs)
            try:
                with self.assertRaises(m.Failure):
                    client.call('STATUS')
            finally:
                client.abort()
                logs.close()

    def test_overlength_startup_rejects_and_kills_owned_transport(self):
        with tempfile.TemporaryDirectory() as d:
            logs = m.Logs(pathlib.Path(d) / 'logs')
            try:
                with self.assertRaises(m.Failure):
                    self.worker('oversize', logs)
            finally:
                logs.close()

    def test_log_budget_is_global_and_sticky(self):
        with tempfile.TemporaryDirectory() as d:
            logs = m.Logs(pathlib.Path(d) / 'logs', max_bytes=10)
            try:
                logs.write('events', b'12345')
                logs.write('resources', b'12345')
                with self.assertRaises(m.Failure):
                    logs.write('stderr', b'1')
                self.assertEqual(logs.failed, 'log budget exhausted')
            finally:
                logs.close()
if __name__ == '__main__':
    unittest.main()
