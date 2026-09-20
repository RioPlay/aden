#!/usr/bin/env python3
# SPDX-License-Identifier: AGPL-3.0-or-later
"""Checks the observer's raw evidence, error recording, and source boundaries."""
import json
import io
from pathlib import Path
import tempfile
import unittest
from types import SimpleNamespace
from unittest.mock import patch

import agent_bench_probe as probe


class ProbeTests(unittest.TestCase):
    def setUp(self):
        self.work = tempfile.TemporaryDirectory()
        self.addCleanup(self.work.cleanup)
        self.base = Path(self.work.name)
        self.project = self.base / 'run' / 'project'
        self.project.mkdir(parents=True)
        self.config = self.base / 'config.json'
        self.config.write_text(json.dumps({'runs':{'case':{
            'scenario':'known', 'project':str(self.project), 'data':str(self.base/'data')
        }}}), encoding='utf-8')

    def events(self):
        return [json.loads(line) for line in (self.project.parent/'events.jsonl').read_text().splitlines()]

    def test_guard_result_is_preserved_and_correlated_not_changed_to_failure(self):
        response = {'result':{'isError':False,'content':[{'type':'text','text':json.dumps({
            'result_state':'needs_narrowing','context':''})}]}}
        with patch.object(probe, 'mcp_request', return_value=response):
            self.assertEqual(probe.observe(self.config,'case','ask',{'question':'all'},'events.jsonl'), response)
        start, end = self.events()
        self.assertEqual(start['item']['id'], end['item']['id'])
        self.assertEqual(end['item']['status'], 'completed')
        self.assertEqual(end['item']['result'], response['result'])

    def test_transport_and_protocol_failures_keep_distinct_observed_outcomes(self):
        with patch.object(probe, 'mcp_request', side_effect=TimeoutError('deadline')):
            self.assertIn('observer_error', probe.observe(self.config,'case','ask',{},'events.jsonl'))
        with patch.object(probe, 'mcp_request', return_value={'error':{'code':-32602}}):
            probe.observe(self.config,'case','ask',{},'events.jsonl')
        events = self.events()
        self.assertEqual(events[1]['item']['error']['kind'], 'transport')
        self.assertEqual(events[3]['item']['error']['kind'], 'protocol')
        self.assertNotEqual(events[0]['item']['id'], events[2]['item']['id'])

    def test_source_reads_have_exact_ranges_and_refuse_parent_escape(self):
        (self.project/'sample.rs').write_text('one\ntwo\nthree\n', encoding='utf-8')
        self.assertEqual(probe.source_range(self.project, {'file':'sample.rs','start':2,'end':3}), '2: two\n3: three')
        with self.assertRaisesRegex(ValueError, 'assigned project'):
            probe.source_range(self.project, {'file':'../../config.json','start':1,'end':2})
        with self.assertRaisesRegex(ValueError, '160 lines'):
            probe.source_range(self.project, {'file':'sample.rs','start':1,'end':161})

    def test_refused_host_operation_still_leaves_an_error_record(self):
        result = probe.observe(self.config,'case','tests',{},'events.jsonl')
        self.assertIn('observer_error', result)
        self.assertEqual(self.events()[1]['item']['error']['kind'], 'shell')

    def test_invalid_arguments_record_host_failure_without_fabricating_mcp_call(self):
        with patch.object(probe, 'mcp_request') as call:
            result = probe.observe(self.config,'case','asm',{},'events.jsonl', 'missing equals')
        call.assert_not_called()
        self.assertIn('observer_error', result)
        self.assertEqual(self.events()[1]['item']['type'], 'command_execution')
        self.assertEqual(self.events()[1]['item']['error']['kind'], 'shell')

    def test_describe_preserves_tools_list_protocol_error(self):
        process = SimpleNamespace(stdin=io.StringIO(), stderr=io.StringIO(),
            stdout=io.StringIO('{"id":1,"result":{}}\n{"id":2,"error":{"code":-32601}}\n'),
            kill=lambda:None, wait=lambda:None)
        manifest = {'binary':{'aden.exe':{'path':'aden'},'aden-mcp.exe':{'path':'aden-mcp'}}}
        with patch.object(probe.subprocess, 'Popen', return_value=process):
            result = probe.mcp_request(manifest, {'project':str(self.project),'data':'data'}, 'describe', {})
        self.assertEqual(result['error']['code'], -32601)

    def test_expired_deadline_is_enforced_before_reading_queued_messages(self):
        process = SimpleNamespace(stdin=io.StringIO(), stderr=io.StringIO(),
            stdout=io.StringIO('{"method":"notification"}\n'), kill=lambda:None, wait=lambda:None)
        manifest = {'binary':{'aden.exe':{'path':'aden'},'aden-mcp.exe':{'path':'aden-mcp'}}}
        with patch.object(probe.subprocess, 'Popen', return_value=process), \
             patch.object(probe.time, 'monotonic', side_effect=[0, 91]):
            with self.assertRaisesRegex(TimeoutError, 'response 1'):
                probe.mcp_request(manifest, {'project':str(self.project),'data':'data'}, 'ask', {})

    def test_malformed_message_is_a_protocol_failure_not_empty_success(self):
        process = SimpleNamespace(stdin=io.StringIO(), stderr=io.StringIO(),
            stdout=io.StringIO('{not-json}\n'), kill=lambda:None, wait=lambda:None)
        manifest = {'binary':{'aden.exe':{'path':'aden'},'aden-mcp.exe':{'path':'aden-mcp'}}}
        with patch.object(probe.subprocess, 'Popen', return_value=process):
            with self.assertRaises(probe.ProtocolError):
                probe.mcp_request(manifest, {'project':str(self.project),'data':'data'}, 'ask', {})
        with patch.object(probe, 'mcp_request', side_effect=probe.ProtocolError('bad message')):
            probe.observe(self.config, 'case', 'ask', {}, 'events.jsonl')
        self.assertEqual(self.events()[1]['item']['error']['kind'], 'protocol')


if __name__ == '__main__':
    unittest.main()
