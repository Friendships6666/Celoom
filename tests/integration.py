#!/usr/bin/env python3
"""Offline black-box regression tests; never contacts a real model provider."""
import base64
import http.cookiejar
import json
import os
import re
from pathlib import Path
import socket
import struct
import zlib
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

ROOT = Path(__file__).resolve().parents[1]
BINARY = Path(os.environ.get('JUSTAI_TEST_BINARY', ROOT / 'target/debug/justaiagent'))


def free_port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


class Provider(BaseHTTPRequestHandler):
    requests = []
    lock = threading.Lock()
    gate_started = threading.Event()
    gate_release = threading.Event()
    web_authorizations = []
    completed_requests = []
    interject_started = threading.Event()
    interject_release = threading.Event()
    network_up = threading.Event()
    network_up.set()
    probes = []
    quality_gate_enabled = False
    quality_gate_started = threading.Event()
    quality_gate_release = threading.Event()

    def log_message(self, *_):
        pass

    def do_HEAD(self):
        self.probes.append({'path': self.path, 'auth': self.headers.get('Authorization'), 'cookie': self.headers.get('Cookie')})
        if not self.network_up.is_set():
            self.close_connection = True
            return
        self.send_response(200)
        self.send_header('Content-Length', '0')
        self.send_header('CF-Ray', 'synthetic-loopback-fixture')
        self.send_header('CF-Cache-Status', 'DYNAMIC')
        self.end_headers()

    def do_GET(self):
        self.web_authorizations.append(self.headers.get('Authorization'))
        if self.path.startswith('/search'):
            data = json.dumps({'results': [{'title': 'Rust source', 'url': f'http://127.0.0.1:{self.server.server_port}/article', 'content': 'Verified search fixture.'}]}).encode()
            mime = 'application/json'
        else:
            data = b'<html><head><title>Article fixture</title></head><body><main><h1>Verified article</h1><p>PUBLIC_ARTICLE_OK</p><script>NEVER_EXPOSE_SCRIPT</script></main></body></html>'
            mime = 'text/html; charset=utf-8'
        self.send_response(200)
        self.send_header('Content-Type', mime)
        self.send_header('Content-Length', str(len(data)))
        self.end_headers()
        self.wfile.write(data)

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        with self.lock:
            self.requests.append((self.headers.get('Authorization'), body))
        if body['model'] == 'offline-model' and not self.network_up.is_set():
            self.close_connection = True
            return
        if self.headers.get('Authorization') not in ('Bearer fake-key-one', 'Bearer fake-key-two'):
            self.send_error(401)
            return
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.send_header('Connection', 'close')
        self.end_headers()
        prompt = json.dumps(body.get('input', []), ensure_ascii=False)
        quality = body.get('instructions', '').startswith('You are responding to an isolated synthetic API quality check.')
        if quality and self.quality_gate_enabled:
            self.quality_gate_started.set()
            assert self.quality_gate_release.wait(8), 'Quality cancellation gate timed out'
        outputs = [x for x in body.get('input', []) if x.get('type') == 'function_call_output']
        if quality:
            if isinstance(body.get('tool_choice'), dict):
                output = self.call('quality_echo', {'answer': 323})
            elif 'Do not spawn subagents.' in prompt:
                output = self.call('write', {'path': 'must-not-execute-quality.txt', 'content': 'not allowed', 'expected_revision': None})
            elif 'Return exactly' in prompt:
                output = self.message('{"answer":323}')
            else:
                output = self.message('323')
        elif 'Summarize this completed conversation' in prompt:
            output = self.message('Continuation summary: preserve COMPACTION_TARGET task and continue its next unfinished step. No side effects were performed.')
        elif 'INTERJECT_TOOL_BATCH' in prompt and not outputs:
            output = [self.call('ask_user', {'action': 'ask', 'question': 'Safe boundary check?', 'blocking': True, 'options': None, 'id': None}), self.call('write', {'path': 'must-not-run-waiting-tool.txt', 'content': 'bad', 'expected_revision': None})]
        elif 'INTERJECT_AFTER_CALL' in prompt and not outputs:
            self.interject_started.set()
            assert self.interject_release.wait(8), 'Deferred interjection gate timed out'
            output = self.call('read', {'path': 'created-by-agent.txt', 'limit': 1})
        elif 'INTERJECT_IMMEDIATE' in prompt and 'OVERRIDE_NOW' not in prompt:
            self.interject_started.set()
            assert self.interject_release.wait(8), 'Immediate interjection gate timed out'
            output = self.call('write', {'path': 'must-not-run-after-interrupt.txt', 'content': 'bad', 'expected_revision': None})
        elif 'INTERJECT_IMMEDIATE' in prompt:
            output = self.message('IMMEDIATE_MESSAGE_RECEIVED')
        elif body['model'] == 'offline-model':
            output = self.message('NETWORK_TASK_RESUMED')
        elif 'BLOCKING_QUESTION_TEST' in prompt and not outputs:
            output = [self.call('ask_user', {'action': 'ask', 'question': 'Which color?', 'blocking': True, 'options': ['red', 'blue'], 'id': None}), self.call('write', {'path': 'after-answer.txt', 'content': 'answer received', 'expected_revision': None})]
        elif 'OPTIONAL_ANSWER_TEST' in prompt and not outputs:
            output = self.call('ask_user', {'action': 'ask', 'question': 'Any optional preference?', 'blocking': False, 'options': ['fast', 'careful'], 'id': None})
        elif 'OPTIONAL_QUESTION_TEST' in prompt and not any(item.get('name') == 'ask_user' for item in body.get('input', [])):
            output = self.call('ask_user', {'action': 'ask', 'question': 'Optional preferred style?', 'blocking': False, 'options': None, 'id': None})
        elif 'OPTIONAL_QUESTION_TEST' in prompt and not any('withdrawn' in item.get('output', '') for item in outputs):
            question = json.loads(outputs[-1]['output'])
            output = self.call('ask_user', {'action': 'withdraw', 'question': None, 'blocking': None, 'options': None, 'id': question['id']})
        elif 'GOAL_DRIVER_TEST' in prompt and 'Continue the active long task' in prompt and not any(item.get('name') == 'goal' for item in body.get('input', [])):
            texts = '\n'.join(item.get('content', '') if isinstance(item.get('content'), str) else '\n'.join(part.get('text','') for part in item.get('content', []) if isinstance(part, dict)) for item in body.get('input', []))
            match = re.search(r'"goal_id"\s*:\s*"([^"]+)"', texts)
            assert match, texts
            output = self.call('goal', {'action': 'complete', 'id': match.group(1), 'title': None, 'objective': None, 'progress': 'Verified mock task objective is achieved.'})
        elif 'POOL_CHOICE_TEST' in prompt and 'Subagent task:' in prompt:
            assert body['model'] == 'test-model-alt' and body['reasoning']['effort'] == 'high', body.get('model')
            if not any(i.get('name') == 'read' for i in body['input']):
                assert not any(i.get('type') == 'reasoning' for i in body['input']), 'Provider-bound reasoning crossed route'
                assert all(not i.get('id') for i in body['input'] if i.get('type') in ('message', 'function_call'))
                assert all('metadata' not in i and 'internal_chat_message_metadata_passthrough' not in i for i in body['input']), 'Foreign vendor extension crossed into another model'
                output = [self.call('read', {'path': 'created-by-agent.txt', 'limit': 1})]
            else:
                assert 'verified' in prompt, 'Child tool result missing'
                output = self.message('POOL_CHILD_OK')
            assert any(i.get('type') == 'function_call_output' for i in body['input']), 'Visible tool history was lost'
        elif 'POOL_CHOICE_TEST' in prompt and not outputs:
            parent_call = self.call('glob', {'pattern': '*.txt'})
            parent_call['metadata'] = {'turn_id': 'parent-only-turn'}
            parent_call['internal_chat_message_metadata_passthrough'] = {'create_time': 1700000000, 'turn_id': 'parent-only-turn'}
            output = [{'type': 'reasoning', 'id': 'rs-pool-parent', 'summary': [], 'encrypted_content': 'parent-route-only-cipher'}, parent_call]
        elif 'POOL_CHOICE_TEST' in prompt and not any(i.get('name') == 'subagent' for i in body['input']):
            output = self.call('subagent', {'action': 'models'})
        elif 'POOL_CHOICE_TEST' in prompt and not any(i.get('name') == 'subagent' and json.loads(i['arguments'])['action'] == 'spawn' for i in body['input']):
            catalog = json.loads(outputs[-1]['output'])
            assert catalog['choices'][0]['model_id'] == 'reviewer'
            output = self.call('subagent', {'action': 'spawn', 'task': 'Return POOL_CHILD_OK.', 'model_id': 'reviewer'})
        elif 'POOL_CHOICE_TEST' in prompt:
            output = self.message('POOL_PARENT_OK')
        elif 'UNKNOWN_POOL_TEST' in prompt and not outputs:
            output = self.call('subagent', {'action': 'spawn', 'task': 'Do not run', 'model_id': 'not-configured'})
        elif 'NATIVE_PREFIX_TEST' in prompt and not outputs:
            commentary = self.message('Inspecting the fixture.')
            commentary['phase'] = 'commentary'
            call = self.call('glob', {'pattern': '*.txt', 'path': None, 'max_results': None})
            call['arguments'] = '{ "pattern" : "*.txt", "path":null, "max_results":null }'
            output = [{'type': 'reasoning', 'id': 'rs-fixture-native', 'summary': [{'type': 'summary_text', 'text': 'Keep original native state.'}], 'encrypted_content': 'opaque-provider-reasoning-test-fixture'}, commentary, call]
        elif 'COMPUTER_STATUS_TEST' in prompt and not outputs:
            output = self.call('computer', {'action': 'status', 'target': 'browser', 'ref_id': None, 'url': None, 'text': None, 'x': None, 'y': None, 'key': None, 'button': None, 'delta': None, 'argv': None})
        elif 'COMPUTER_STATUS_TEST' in prompt:
            status = json.loads(outputs[-1]['output'])
            assert status['policy'] == 'isolated_first' and not status['browser']['running'] and not status['desktop']['running']
            output = self.message('COMPUTER_STATUS_OK')
        elif 'READ_IMAGE_TEST' in prompt and not outputs:
            output = self.call('read_image', {'path': 'vision.png'})
        elif 'READ_IMAGE_TEST' in prompt:
            images = [part for item in body.get('input', []) for part in item.get('content', []) if isinstance(part, dict) and part.get('type') == 'input_image']
            if images:
                assert base64.b64decode(images[-1]['image_url'].split(',', 1)[1]).startswith(b'\x89PNG\r\n\x1a\n')
            output = self.message('VISION_CONTEXT_OK' if images else 'NO_VISION_CONTEXT')
        elif 'WEB_SEARCH_TEST' in prompt and not outputs:
            output = self.call('web_search', {'query': 'Rust official source', 'max_results': 3})
        elif 'WEB_SEARCH_TEST' in prompt and not any(item.get('name') == 'web_fetch' for item in body.get('input', [])):
            output = self.call('web_fetch', {'url': f'http://127.0.0.1:{self.server.server_port}/article', 'max_chars': 2000})
        elif 'WEB_SEARCH_TEST' in prompt:
            assert 'PUBLIC_ARTICLE_OK' in prompt and 'NEVER_EXPOSE_SCRIPT' not in prompt
            output = self.message('WEB_TOOLS_OK')
        elif 'SWITCH_DURING_REQUEST' in prompt and not outputs:
            self.gate_started.set()
            assert self.gate_release.wait(5), 'Test did not release in-flight request'
            output = self.call('glob', {'pattern': '*.txt', 'path': None, 'max_results': 10})
        elif 'Summarize this completed conversation' in prompt:
            output = self.message('Manual summary: integration test context, no pending side effects.')
        elif 'MISSING_TERMINAL' in prompt:
            call = self.call('write', {'path': 'must-not-exist.txt', 'content': 'bad', 'expected_revision': None})
            self.emit({'type': 'response.output_item.added', 'output_index': 0, 'item': call})
            self.close_connection = True
            return
        elif 'INVALID_JSON' in prompt:
            output = self.call('write', {})
            output['arguments'] = '{"path":"must-not-exist.txt","content":'
        elif 'OPTIONAL_BASH_ARGS_TEST' in prompt and not outputs:
            output = self.call('bash', {'command': 'printf OPTIONAL_ARGS_OK', 'background': False, 'timeout_ms': 1000})
        elif 'REQUIRED_ARG_FAILURE_TEST' in prompt and not outputs:
            output = self.call('bash', {'background': False, 'timeout_ms': 1000})
        elif 'OPTIONAL_SPAWN_ARGS_TEST' in prompt and 'Subagent task:' not in prompt and not outputs:
            output = self.call('subagent', {'action': 'spawn', 'task': 'Return CHILD_OK without tools.'})
        elif 'TOOL_WRITE' in prompt and not outputs:
            output = self.call('write', {'path': 'created-by-agent.txt', 'content': 'verified\n', 'expected_revision': None})
        elif 'TOOL_INVALID' in prompt and not outputs:
            output = self.call('write', {'path': 'must-not-exist.txt', 'content': 'bad', 'expected_revision': None, 'extra': True})
        elif 'SUBAGENT_TEST' in prompt and 'Subagent task:' not in prompt and not outputs:
            output = self.call('subagent', {'action': 'spawn', 'task': 'Return CHILD_OK without tools.', 'id': None, 'wait_ms': None})
        elif 'Subagent task:' in prompt:
            output = self.message('CHILD_OK')
        else:
            output = self.message('VERIFIED_OK')
        output_items = output if isinstance(output, list) else [output]
        for index, item in enumerate(output_items):
            self.emit({'type': 'response.output_item.added', 'output_index': index, 'item': item})
            self.emit({'type': 'response.output_item.done', 'output_index': index, 'item': item})
        # A Responses bridge may issue different replay item IDs in the terminal
        # snapshot. Keep executable call_id/name/raw arguments exactly unchanged.
        if body['model'] == 'test-model-alt' and 'Subagent task:' in prompt:
            output_items = json.loads(json.dumps(output_items))
            for item in output_items:
                item['id'] = 'terminal-reissued-' + item.get('id', 'item')
        usage = {'input_tokens': 100, 'output_tokens': 10}
        if 'CACHE_USAGE_MISSING' not in prompt:
            usage['input_tokens_details'] = {'cached_tokens': 0 if 'CACHE_USAGE_ZERO' in prompt else 80, 'cache_write_tokens': 0}
        if quality:
            usage = {'input_tokens': 12000 if body.get('tools') else 200, 'output_tokens': 10}
            if body.get('tools'):
                usage['input_tokens_details'] = {'cached_tokens': 896 if any(t.get('strict') for t in body['tools']) else 11000}
        reported_model = 'alternate-reported-model' if quality and 'Ignore all previous project tasks.' in prompt else body['model']
        with self.lock:
            self.completed_requests.append({'body': body, 'output': output_items})
        self.emit({'type': 'response.completed', 'response': {
            'id': 'resp-integration', 'status': 'completed', 'output': output_items,
            'model': reported_model, 'reasoning': {'context': 'all_turns'}, 'service_tier': 'default',
            'error': None, 'incomplete_details': None,
            'usage': usage,
        }})
        self.close_connection = True

    def emit(self, item):
        try:
            self.wfile.write(('data: ' + json.dumps(item) + '\n\n').encode())
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError):
            # Cancellation regression deliberately closes in-flight generations.
            self.close_connection = True

    @staticmethod
    def message(text):
        return {'id': 'msg-' + str(time.time_ns()), 'type': 'message', 'status': 'completed', 'role': 'assistant', 'content': [{'type': 'output_text', 'text': text, 'annotations': []}]}

    @staticmethod
    def call(name, args):
        return {'id': 'fc-' + str(time.time_ns()), 'type': 'function_call', 'status': 'completed', 'call_id': 'call-' + str(time.time_ns()), 'name': name, 'arguments': json.dumps(args)}


class API:
    def __init__(self, port):
        self.base = f'http://127.0.0.1:{port}'
        self.cookies = http.cookiejar.CookieJar()
        self.opener = urllib.request.build_opener(urllib.request.ProxyHandler({}), urllib.request.HTTPCookieProcessor(self.cookies))

    def request(self, path, data=None, method=None):
        req = urllib.request.Request(self.base + path, data=None if data is None else json.dumps(data).encode(), method=method, headers={'Content-Type': 'application/json', 'X-JustAI-Request': '1', 'Origin': self.base})
        try:
            with self.opener.open(req, timeout=20) as response:
                return response.status, json.load(response)
        except urllib.error.HTTPError as error:
            return error.code, json.load(error)

    def session(self, selection):
        status, session = self.request('/api/sessions', {'selection': selection})
        assert status == 200, session
        return session['id']

    def run(self, id, text, client_id=None):
        status, accepted = self.request(f'/api/sessions/{id}/messages', {'text': text, 'client_message_id': client_id or str(time.time_ns())})
        assert status == 202, accepted
        return self.wait(id)

    def wait_quality(self, id):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            status, run = self.request(f'/api/model-checks/{id}')
            assert status == 200, run
            if run['status'] not in ('queued', 'running'): return run
            time.sleep(.03)
        raise AssertionError('Quality check did not settle')

    def wait(self, id):
        deadline = time.monotonic() + 15
        while time.monotonic() < deadline:
            _, detail = self.request(f'/api/sessions/{id}')
            if detail['session']['status'] not in ('running', 'compacting', 'auto_compacting', 'waiting_for_answer', 'waiting_for_network'):
                return detail
            time.sleep(.04)
        raise AssertionError('Agent did not settle')


def main():
    assert BINARY.is_file(), f'Build {BINARY} first'
    upstream = ThreadingHTTPServer(('127.0.0.1', 0), Provider)
    threading.Thread(target=upstream.serve_forever, daemon=True).start()
    results = []
    with tempfile.TemporaryDirectory(prefix='justai-offline-') as temp:
        work = Path(temp) / 'work'
        work.mkdir()
        port = free_port()
        process = subprocess.Popen([str(BINARY), '--port', str(port), '--data-dir', str(Path(temp) / 'data'), '--workspace', str(work)], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
        try:
            api = API(port)
            for _ in range(100):
                try:
                    _, boot = api.request('/api/bootstrap')
                    break
                except urllib.error.URLError:
                    assert process.poll() is None, process.stderr.read().decode()
                    time.sleep(.05)
            else:
                raise AssertionError('Server did not start')
            assert not boot['initialized']
            assert api.request('/api/config')[0] == 401
            assert api.request('/api/setup', {'password': 'integration-password-2026'})[0] == 200
            results.append('local password initialization and API authentication')
            model = {'id': 'model', 'name': 'Test model', 'model': 'test-model', 'supports_images': True, 'max_output_tokens': 512, 'context_window': 128000, 'reasoning_efforts': [], 'reasoning_effort': '', 'strict_tools': True, 'reasoning_replay': False, 'send_store': True}
            config = {'revision': 0, 'sites': [{'id': 'site', 'name': 'Mock site', 'base_url': f'http://127.0.0.1:{upstream.server_port}/v1', 'keys': [{'id': 'key', 'name': 'Mock key', 'secret': 'fake-key-one', 'models': [model]}]}], 'network': {'proxy_mode': 'direct', 'proxy_url': '', 'no_proxy': '', 'connect_timeout_secs': 2, 'headers_timeout_secs': 5, 'idle_timeout_secs': 2, 'request_timeout_secs': 10, 'max_attempts': 1}}
            status, saved = api.request('/api/config', config, 'PUT')
            assert status == 200, saved
            assert 'fake-key-one' not in json.dumps(saved)
            assert saved['sites'][0]['keys'][0]['has_secret']
            assert api.request('/api/config', config, 'PUT')[0] == 409
            results.append('configuration revision conflicts and secret redaction')
            status, computer = api.request('/api/computer')
            assert status == 200 and computer['policy'] == 'isolated_first'
            assert computer['browser']['running'] is False and computer['desktop']['running'] is False
            assert computer['privilege']['mode'] in ('root', 'sudo', 'user')
            assert computer['host']['physical_monitor'] is False
            assert api.request('/api/computer/host', {'enabled': False})[0] == 200
            assert api.request('/api/computer/host', {'enabled': True})[0] == 200
            assert api.request('/api/computer/stop', {})[0] == 200
            results.append('computer status and host policy never start browsers desktop or input capture')
            selection = {'site_id': 'site', 'key_id': 'key', 'model_id': 'model'}
            computer_session = api.run(api.session(selection), 'COMPUTER_STATUS_TEST')
            assert computer_session['session']['status'] == 'completed'
            assert any(e['type'] == 'assistant_message' and e['data']['text'] == 'COMPUTER_STATUS_OK' for e in computer_session['events'])
            results.append('fixed computer tool returns real capability state without launching UI')
            with Provider.lock:
                Provider.requests.clear()
            id = api.session(selection)
            detail = api.run(id, 'Hello', 'same-id')
            assert detail['session']['status'] == 'completed', detail
            assert any(e['type'] == 'assistant_message' and e['data']['text'] == 'VERIFIED_OK' for e in detail['events'])
            before = len(detail['events'])
            status, _ = api.request(f'/api/sessions/{id}/messages', {'text': 'Hello', 'client_message_id': 'same-id'})
            assert status == 202
            assert len(api.request(f'/api/sessions/{id}')[1]['events']) == before
            results.append('Responses completion and idempotent message submission')
            rotated = saved
            rotated['sites'][0]['keys'][0]['secret'] = 'fake-key-two'
            assert api.request('/api/config', rotated, 'PUT')[0] == 200
            api.run(id, 'After rotation')
            assert Provider.requests[-1][0] == 'Bearer fake-key-two'
            results.append('credential rotation takes effect on next request')
            first_body, second_body = Provider.requests[0][1], Provider.requests[1][1]
            assert first_body['tools'] and all('strict' not in tool for tool in first_body['tools'])
            assert second_body['input'][:len(first_body['input'])] == first_body['input']
            assert second_body['instructions'] == first_body['instructions']
            assert second_body['tools'] == first_body['tools']
            results.append('stable instructions/tools and append-only request prefixes')
            prefix_session = api.session(selection)
            before_native = len(Provider.completed_requests)
            native_detail = api.run(prefix_session, 'NATIVE_PREFIX_TEST')
            native_requests = Provider.completed_requests[before_native:]
            assert len(native_requests) == 2
            first_native, second_native = native_requests
            expected = first_native['body']['input'] + first_native['output']
            assert second_native['body']['input'][:len(expected)] == expected
            assert any(i.get('encrypted_content') == 'opaque-provider-reasoning-test-fixture' for i in second_native['body']['input'])
            assert any(i.get('phase') == 'commentary' for i in second_native['body']['input'])
            assert first_native['body']['prompt_cache_key'] == second_native['body']['prompt_cache_key'] == f'justai:v1:{prefix_session}'
            api.run(prefix_session, 'Continue without changing old items')
            _, native_detail = api.request(f'/api/sessions/{prefix_session}')
            audits = [e['data'] for e in native_detail['events'] if e['type'] == 'request_audit']
            assert audits[0]['change_kind'] == 'first_request'
            assert all(a['change_kind'] == 'append_only' and a['expected_prefix_preserved'] is True and a['native_output_replayed'] is True for a in audits[1:]), audits
            assert 'opaque-provider-reasoning-test-fixture' not in json.dumps(audits)
            results.append('complete native reasoning phase and original argument strings are replayed byte-for-byte with stable cache key')
            for marker, expected_cache, expected_present in [('CACHE_USAGE_MISSING', None, False), ('CACHE_USAGE_ZERO', 0, True), ('CACHE_USAGE_POSITIVE', 80, True)]:
                cache_detail = api.run(api.session(selection), marker)
                cache_usage = [e['data'] for e in cache_detail['events'] if e['type'] == 'usage'][-1]
                assert cache_usage['cached_tokens'] == expected_cache and cache_usage['cached_tokens_present'] is expected_present, cache_usage
                assert cache_usage['reasoning_context'] == 'all_turns' and cache_usage['effective_model'] == 'test-model'
            results.append('cache usage distinguishes unavailable zero and positive readings without storing raw reasoning')
            _, route_config = api.request('/api/config')
            route_config['sites'][0]['keys'][0]['models'][0]['reasoning_efforts'] = ['low', 'high']
            alternative_model = dict(model, id='model-alt', name='Alternative model', model='test-model-alt', reasoning_efforts=['low', 'high'], supports_images=False)
            route_config['sites'].append({'id': 'site-alt', 'name': 'Second site', 'base_url': f'http://127.0.0.1:{upstream.server_port}/v1', 'keys': [{'id': 'key-alt', 'name': 'Alternative key', 'secret': 'fake-key-two', 'models': [alternative_model]}]})
            assert api.request('/api/config', route_config, 'PUT')[0] == 200
            status, changed = api.request(f'/api/sessions/{id}/settings', {'selection': selection, 'reasoning_effort': 'high'}, 'PATCH')
            assert status == 200 and changed['reasoning_effort'] == 'high', changed
            api.run(id, 'After effort change')
            assert Provider.requests[-1][1]['reasoning']['effort'] == 'high'
            alt_selection = {'site_id': 'site-alt', 'key_id': 'key-alt', 'model_id': 'model-alt'}
            assert api.request(f'/api/sessions/{id}/settings', {'selection': alt_selection, 'reasoning_effort': 'low'}, 'PATCH')[0] == 200
            switched = api.run(id, 'After site/model switch')
            assert Provider.requests[-1][1]['model'] == 'test-model-alt'
            assert 'Hello' in json.dumps(Provider.requests[-1][1]['input'])
            sources = [e['data'] for e in switched['events'] if e['type'] == 'assistant_source']
            assert sources[0]['site_name'] == 'Mock site' and sources[-1]['site_name'] == 'Second site'
            assert sources[-1]['model_name'] == 'Alternative model'
            assert any(e['type'] == 'context_rebased' for e in switched['events'])
            assert not any(e['type'] == 'compacted' for e in switched['events'])
            assert api.request(f'/api/sessions/{id}/settings', {'selection': alt_selection, 'reasoning_effort': 'unsupported'}, 'PATCH')[0] == 400
            results.append('in-session site/model/effort changes preserve visible history and historical authors')
            in_flight = api.session(selection)
            assert api.request(f'/api/sessions/{in_flight}/messages', {'text': 'SWITCH_DURING_REQUEST', 'client_message_id': 'in-flight-switch'})[0] == 202
            assert Provider.gate_started.wait(3)
            assert api.request(f'/api/sessions/{in_flight}/settings', {'selection': alt_selection, 'reasoning_effort': 'high'}, 'PATCH')[0] == 200
            Provider.gate_release.set()
            moving = api.wait(in_flight)
            assert moving['session']['status'] == 'completed', moving
            request_sources = [e['data'] for e in moving['events'] if e['type'] == 'assistant_source']
            assert request_sources[0]['site_name'] == 'Mock site' and request_sources[-1]['site_name'] == 'Second site'
            assert Provider.requests[-1][1]['model'] == 'test-model-alt' and Provider.requests[-1][1]['reasoning']['effort'] == 'high'
            results.append('changing settings during generation only affects the next model-request boundary')
            _, current_config = api.request('/api/config')
            current_config['network']['proxy_mode'] = 'manual'
            current_config['network']['proxy_url'] = f'http://127.0.0.1:{free_port()}'
            assert api.request('/api/config', current_config, 'PUT')[0] == 200
            request_count = len(Provider.requests)
            status, diagnostic = api.request('/api/network/test', {'selection': selection})
            assert status == 200 and not diagnostic['ok'], diagnostic
            assert len(Provider.requests) == request_count, 'Proxy failure unexpectedly fell back to direct'
            _, current_config = api.request('/api/config')
            current_config['network']['proxy_mode'] = 'direct'
            assert api.request('/api/config', current_config, 'PUT')[0] == 200
            results.append('unavailable explicit proxy never falls back to direct')
            image_session = api.session(selection)
            status, image_error = api.request(f'/api/sessions/{image_session}/messages', {'text': 'Invalid image', 'images': [{'mime': 'image/png', 'data': base64.b64encode(b'not-a-png').decode()}]})
            assert status == 400, image_error
            assert len(Provider.requests) == request_count
            results.append('image MIME/signature validation occurs before model dispatch')
            write_session = api.session(selection)
            detail = api.run(write_session, 'TOOL_WRITE')
            assert detail['session']['status'] == 'completed', detail
            assert (work / 'created-by-agent.txt').read_text() == 'verified\n'
            assert any(e['type'] == 'tool_end' and not e['data']['is_error'] for e in detail['events'])
            results.append('durable model tool execution and tool-result continuation')
            turns = [e for e in detail['events'] if e['type'] == 'turn_files']
            assert turns and any(f['path'] == 'created-by-agent.txt' and f['kind'] == 'new' for f in turns[-1]['data']['files']), turns
            activities = [e['data']['status'] for e in detail['events'] if e['type'] == 'file_activity']
            assert activities == ['editing', 'done'], activities
            status, tree = api.request(f'/api/sessions/{write_session}/files')
            assert status == 200 and any(e['path'] == 'created-by-agent.txt' for e in tree['entries']), tree
            results.append('real workspace tree and per-turn artifacts include immediately written files')
            (work / 'preview.html').write_text('<!doctype html><h1>Preview</h1><script>fetch("/api/config")</script>')
            (work / 'preview.svg').write_text('<svg xmlns="http://www.w3.org/2000/svg"><circle r="5"/></svg>')
            for name, mime in [('preview.html', 'text/html'), ('preview.svg', 'image/svg+xml')]:
                with api.opener.open(api.base + f'/api/sessions/{write_session}/file?path={name}', timeout=5) as response:
                    assert response.headers['Content-Type'].startswith(mime)
                    policy = response.headers['Content-Security-Policy']
                    assert 'sandbox;' in policy and "script-src 'none'" in policy and 'allow-same-origin' not in policy, policy
                    assert response.headers['X-Content-Type-Options'] == 'nosniff'
                    assert response.headers['Cache-Control'] == 'no-store'
                    response.read()
            assert api.request(f'/api/sessions/{write_session}/file?path=../outside')[0] == 400
            results.append('authenticated HTML/SVG previews retain opaque sandbox policy and reject traversal')
            log = Path(temp) / 'data' / 'sessions' / f'{write_session}.jsonl'
            lines = [json.loads(line) for line in log.read_text().splitlines()]
            assert lines[0]['schema_version'] == 1 and any(e['type'] == 'tool_end' for e in lines[1:])
            assert 'fake-key-two' not in log.read_text()
            with api.opener.open(api.base + f'/api/sessions/{write_session}/export', timeout=5) as response:
                assert response.headers['Content-Type'].startswith('application/x-ndjson')
                assert 'TOOL_WRITE' in response.read().decode()
            results.append('automatic plaintext JSONL and authenticated export are readable without decryption')
            def png_chunk(kind, data):
                return struct.pack('>I', len(data)) + kind + data + struct.pack('>I', zlib.crc32(kind + data))
            png = b'\x89PNG\r\n\x1a\n' + png_chunk(b'IHDR', struct.pack('>IIBBBBB', 1, 1, 8, 6, 0, 0, 0)) + png_chunk(b'IDAT', zlib.compress(b'\x00\xff\x00\x00\xff')) + png_chunk(b'IEND', b'')
            (work / 'vision.png').write_bytes(png)
            vision = api.run(api.session(selection), 'READ_IMAGE_TEST')
            assert vision['session']['status'] == 'completed', vision
            assert any(e['type'] == 'tool_end' and e['data']['name'] == 'read_image' and not e['data']['is_error'] for e in vision['events'])
            assert any(e['type'] == 'assistant_message' and e['data']['text'] == 'VISION_CONTEXT_OK' for e in vision['events'])
            text_only = api.run(api.session(alt_selection), 'READ_IMAGE_TEST')
            assert any(e['type'] == 'tool_end' and e['data']['name'] == 'read_image' and e['data']['is_error'] for e in text_only['events'])
            assert any(e['type'] == 'assistant_message' and e['data']['text'] == 'NO_VISION_CONTEXT' for e in text_only['events'])
            results.append('read_image admits real image pixels and refuses vision input on text-only models')
            _, search_config = api.request('/api/config')
            search_config['search'] = {'provider': 'searxng', 'base_url': f'http://127.0.0.1:{upstream.server_port}', 'api_key': None}
            assert api.request('/api/config', search_config, 'PUT')[0] == 200
            web_result = api.run(api.session(selection), 'WEB_SEARCH_TEST')
            assert web_result['session']['status'] == 'completed', web_result
            assert [e['data']['name'] for e in web_result['events'] if e['type'] == 'tool_end'] == ['web_search', 'web_fetch']
            assert all(header is None for header in Provider.web_authorizations)
            assert any(e['type'] == 'assistant_message' and e['data']['text'] == 'WEB_TOOLS_OK' for e in web_result['events'])
            results.append('built-in search/fetch use anonymous transport and remove executable webpage content')
            optional_bash = api.run(api.session(selection), 'OPTIONAL_BASH_ARGS_TEST')
            bash_result = next(e['data'] for e in optional_bash['events'] if e['type'] == 'tool_end')
            assert not bash_result['is_error'] and 'OPTIONAL_ARGS_OK' in bash_result['output'], bash_result
            bash_start = next(e['data'] for e in optional_bash['events'] if e['type'] == 'tool_start')
            assert 'cwd' not in bash_start['args'], 'Raw model arguments were rewritten'
            optional_audits = [e['data'] for e in optional_bash['events'] if e['type'] == 'request_audit']
            assert optional_audits[-1]['expected_prefix_preserved'] and optional_audits[-1]['native_output_replayed']
            optional_parent = api.session(selection)
            optional_spawn = api.run(optional_parent, 'OPTIONAL_SPAWN_ARGS_TEST')
            spawn_result = next(e['data'] for e in optional_spawn['events'] if e['type'] == 'tool_end')
            assert spawn_result['name'] == 'subagent' and not spawn_result['is_error'], spawn_result
            results.append('reported bash/spawn optional omissions execute while original arguments and cached prefix stay intact')
            invalid_args = api.run(api.session(selection), 'REQUIRED_ARG_FAILURE_TEST')
            invalid_end = next(e['data'] for e in invalid_args['events'] if e['type'] == 'tool_end')
            assert invalid_end['is_error'] and 'command' in invalid_end['output']
            failure_audits = [e['data'] for e in invalid_args['events'] if e['type'] == 'request_audit']
            assert failure_audits[-1]['change_kind'] == 'append_only' and failure_audits[-1]['expected_prefix_preserved']
            results.append('mandatory arguments still fail safely and the error result does not rewrite the cached prefix')
            detail = api.run(api.session(selection), 'TOOL_INVALID')
            assert any(e['type'] == 'tool_end' and e['data']['is_error'] for e in detail['events'])
            assert not (work / 'must-not-exist.txt').exists()
            results.append('strict tool schema rejects extra properties without side effects')
            _, omit_config = api.request('/api/config')
            selected_model = omit_config['sites'][0]['keys'][0]['models'][0]
            assert selected_model.get('omit_tool_strict') is True
            selected_model['omit_tool_strict'] = False
            assert api.request('/api/config', omit_config, 'PUT')[0] == 200
            api.run(api.session(selection), 'STRICT_MARKER_OPT_OUT')
            assert all(tool.get('strict') is True for tool in Provider.requests[-1][1]['tools'])
            _, omit_config = api.request('/api/config')
            selected_model = omit_config['sites'][0]['keys'][0]['models'][0]
            selected_model['omit_tool_strict'] = True
            assert selected_model['strict_tools'] is True
            assert api.request('/api/config', omit_config, 'PUT')[0] == 200
            before_omit_requests = len(Provider.requests)
            omitted_session = api.run(api.session(selection), 'OPTIONAL_BASH_ARGS_TEST')
            assert omitted_session['session']['status'] == 'completed'
            omit_bodies = [entry[1] for entry in Provider.requests[before_omit_requests:]]
            for body in omit_bodies:
                assert all('strict' not in tool and tool['parameters']['additionalProperties'] is False and len(tool['parameters']['required']) == len(tool['parameters']['properties']) for tool in body['tools'])
            omit_audits = [e['data'] for e in omitted_session['events'] if e['type'] == 'request_audit']
            assert omit_audits[-1]['tools_unchanged'] and omit_audits[-1]['expected_prefix_preserved'] and omit_audits[-1]['native_output_replayed']
            omitted_invalid = api.run(api.session(selection), 'TOOL_INVALID')
            assert any(e['type'] == 'tool_end' and e['data']['is_error'] for e in omitted_invalid['events'])
            assert not (work / 'must-not-exist.txt').exists()
            _, restored_config = api.request('/api/config')
            restored_config['sites'][0]['keys'][0]['models'][0]['omit_tool_strict'] = True
            assert api.request('/api/config', restored_config, 'PUT')[0] == 200
            results.append('per-model strict omission changes only wire markers and keeps full schemas local validation and native prefixes')
            for prompt in ['MISSING_TERMINAL', 'INVALID_JSON']:
                detail = api.run(api.session(selection), prompt)
                assert detail['session']['status'] == 'failed', detail
                assert not any(e['type'] == 'tool_start' for e in detail['events'])
                assert not (work / 'must-not-exist.txt').exists()
            results.append('truncated SSE and partial JSON never dispatch tools')
            _, before_compact = api.request(f'/api/sessions/{id}')
            status, _ = api.request(f'/api/sessions/{id}/compact', {})
            assert status == 202
            detail = api.wait(id)
            assert detail['session']['context_epoch'] == before_compact['session']['context_epoch'] + 1
            assert any(e['type'] == 'compacted' for e in detail['events'])
            results.append('explicit manual compaction creates a new durable context epoch')
            parent = api.session(selection)
            api.run(parent, 'SUBAGENT_TEST')
            _, sessions = api.request('/api/sessions')
            children = [s for s in sessions if s.get('parent_id') == parent]
            assert len(children) == 1
            child = api.wait(children[0]['id'])
            assert child['session']['depth'] == 1 and child['session']['status'] == 'completed'
            results.append('built-in one-level subagent with isolated persisted session')
            family_requests = [body for _, body in Provider.requests if 'SUBAGENT_TEST' in json.dumps(body.get('input', []))]
            assert family_requests and all(body.get('prompt_cache_key') == f'justai:v1:{parent}' for body in family_requests)
            results.append('parent and subagent share one stable family cache routing key')
            last_seq = detail['events'][-1]['seq']
            request = urllib.request.Request(api.base + f'/api/sessions/{id}/events?after={last_seq - 1}')
            with api.opener.open(request, timeout=5) as stream:
                assert stream.headers['Content-Type'].startswith('text/event-stream')
                assert stream.headers['X-Accel-Buffering'] == 'no'
                replay = []
                while True:
                    line = stream.readline().decode().strip()
                    if line.startswith('data:'):
                        replay.append(json.loads(line[5:].strip()))
                        break
                assert replay[0]['seq'] == last_seq
            results.append('authenticated SSE replays by sequence and disables proxy buffering')
            _, pool_config = api.request('/api/config')
            pool_config['subagent_models'] = [{'id': 'reviewer', 'name': 'Cross-site reviewer', 'selection': alt_selection, 'reasoning_effort': 'high'}]
            status, pool_saved = api.request('/api/config', pool_config, 'PUT')
            assert status == 200 and pool_saved['subagent_models'][0]['selection'] == alt_selection, pool_saved
            invalid_pool = json.loads(json.dumps(pool_saved))
            invalid_pool['sites'] = [s for s in invalid_pool['sites'] if s['id'] != 'site-alt']
            status, pool_error = api.request('/api/config', invalid_pool, 'PUT')
            assert status == 400 and 'Subagent' in pool_error['error']
            pool_parent = api.session(selection)
            pool_detail = api.run(pool_parent, 'POOL_CHOICE_TEST')
            assert pool_detail['session']['status'] == 'completed', pool_detail
            _, all_sessions = api.request('/api/sessions')
            pool_child = next(s for s in all_sessions if s.get('parent_id') == pool_parent)
            assert pool_child['selection'] == alt_selection and pool_child['reasoning_effort'] == 'high'
            actual_child = api.wait(pool_child['id'])
            assert actual_child['session']['status'] == 'completed'
            assert any(e['type'] == 'tool_end' and e['data']['name'] == 'read' and not e['data']['is_error'] for e in actual_child['events'])
            assert any(e['type'] == 'usage' and e['data'].get('stream_item_ids_reissued', 0) > 0 for e in actual_child['events'])
            assert any(e['type'] == 'assistant_source' and e['data']['site_name'] == 'Second site' for e in actual_child['events'])
            assert any(e['type'] == 'subagent' and e['data'].get('model_id') == 'reviewer' and e['data'].get('model_name') == 'Alternative model' for e in pool_detail['events'])
            assert len([e for e in pool_detail['events'] if e['type'] == 'subagent_catalog']) == 1
            actual_child_requests = [r['body'] for r in Provider.completed_requests if r['body']['model'] == 'test-model-alt' and 'Subagent task:' in json.dumps(r['body']['input'])]
            assert actual_child_requests[-1]['prompt_cache_key'] == f'justai:v1:{pool_parent}'
            api.run(pool_parent, 'Continue POOL_CHOICE_TEST without spawning again')
            _, pool_detail = api.request(f'/api/sessions/{pool_parent}')
            assert len([e for e in pool_detail['events'] if e['type'] == 'subagent_catalog']) == 1
            unknown_parent = api.session(selection)
            unknown = api.run(unknown_parent, 'UNKNOWN_POOL_TEST')
            assert any(e['type'] == 'tool_end' and e['data']['name'] == 'subagent' and e['data']['is_error'] for e in unknown['events'])
            _, all_sessions = api.request('/api/sessions')
            assert not any(s.get('parent_id') == unknown_parent for s in all_sessions)
            results.append('AI selects user-configured cross-site subagent models and efforts with safe native history projection')
            results.append('model pool rejects dangling references and unconfigured choices without reappending stable catalogues')
            Provider.interject_started.clear(); Provider.interject_release.clear()
            deferred = api.session(selection)
            assert api.request(f'/api/sessions/{deferred}/messages', {'text': 'INTERJECT_AFTER_CALL', 'client_message_id': 'after-start'})[0] == 202
            assert Provider.interject_started.wait(3)
            followup = {'text': 'AFTER_CALL_MESSAGE', 'client_message_id': 'after-queued', 'delivery_mode': 'after_call', 'images': [{'mime': 'image/png', 'data': base64.b64encode(png).decode()}, {'mime': 'image/png', 'data': base64.b64encode(png).decode()}]}
            status, delivery = api.request(f'/api/sessions/{deferred}/messages', followup)
            assert status == 202, delivery
            assert api.request(f'/api/sessions/{deferred}/messages', followup)[0] == 202
            _, pending = api.request(f'/api/sessions/{deferred}')
            assert len([e for e in pending['events'] if e['type'] == 'user_message' and e['data'].get('client_message_id') == 'after-queued']) == 1
            assert not any(e['type'] == 'message_delivered' for e in pending['events'])
            Provider.interject_release.set()
            delivered = api.wait(deferred)
            assert delivered['session']['status'] == 'completed', delivered
            tool_seq = next(e['seq'] for e in delivered['events'] if e['type'] == 'tool_end')
            delivered_seq = next(e['seq'] for e in delivered['events'] if e['type'] == 'message_delivered')
            assert tool_seq < delivered_seq
            request_input = Provider.requests[-1][1]['input']
            assert sum(part.get('type') == 'input_image' for item in request_input for part in item.get('content', []) if isinstance(part, dict)) == 2
            results.append('running deferred messages are durable idempotent and deliver multiple images only after the current tool batch')
            Provider.interject_started.clear(); Provider.interject_release.clear()
            immediate = api.session(selection)
            assert api.request(f'/api/sessions/{immediate}/messages', {'text': 'INTERJECT_IMMEDIATE', 'client_message_id': 'immediate-start'})[0] == 202
            assert Provider.interject_started.wait(3)
            assert api.request(f'/api/sessions/{immediate}/messages', {'text': 'OVERRIDE_NOW', 'client_message_id': 'immediate-new', 'delivery_mode': 'immediate'})[0] == 202
            immediate_done = api.wait(immediate)
            Provider.interject_release.set()
            assert immediate_done['session']['status'] == 'completed', immediate_done
            assert not (work / 'must-not-run-after-interrupt.txt').exists()
            assert not any(e['type'] == 'tool_start' and e['data']['name'] == 'write' for e in immediate_done['events'])
            assert any(e['type'] == 'assistant_message' and e['data']['text'] == 'IMMEDIATE_MESSAGE_RECEIVED' for e in immediate_done['events'])
            results.append('immediate messages cancel only uncommitted generation and never execute its superseded tool calls')
            for early in (False, True):
                Provider.interject_started.clear(); Provider.interject_release.clear()
                esc_session = api.session(selection)
                assert api.request(f'/api/sessions/{esc_session}/messages', {'text': 'INTERJECT_IMMEDIATE', 'client_message_id': str(uuid.uuid4())})[0] == 202
                assert Provider.interject_started.wait(3)
                queued_id = str(uuid.uuid4())
                if early:
                    status, early_result = api.request(f'/api/sessions/{esc_session}/messages/interrupt', {'client_message_id': queued_id})
                    assert status == 200 and early_result['pending_acceptance'], early_result
                queued_message = {'text': 'OVERRIDE_NOW', 'client_message_id': queued_id, 'delivery_mode': 'after_call'}
                assert api.request(f'/api/sessions/{esc_session}/messages', queued_message)[0] == 202
                if not early:
                    status, promoted = api.request(f'/api/sessions/{esc_session}/messages/interrupt', {'client_message_id': queued_id})
                    assert status == 200 and promoted['promoted'], promoted
                esc_done = api.wait(esc_session)
                Provider.interject_release.set()
                assert esc_done['session']['status'] == 'completed', esc_done
                assert not (work / 'must-not-run-after-interrupt.txt').exists()
                assert len([e for e in esc_done['events'] if e['type'] == 'user_message' and e['data'].get('client_message_id') == queued_id]) == 1
                assert len([e for e in esc_done['events'] if e['type'] == 'message_interrupted' and e['data']['client_message_id'] == queued_id]) == 1
                status, delivered_result = api.request(f'/api/sessions/{esc_session}/messages/interrupt', {'client_message_id': queued_id})
                assert status == 200 and delivered_result['delivered'] and not delivered_result['promoted'], delivered_result
                requests_before = len(Provider.requests)
                assert api.request(f'/api/sessions/{esc_session}/messages', queued_message)[0] == 202
                assert len(Provider.requests) == requests_before
            results.append('Esc upgrades an already-sent queued message without duplicate acceptance and safely handles pre-acceptance races')
            batch_session = api.session(selection)
            assert api.request(f'/api/sessions/{batch_session}/messages', {'text': 'INTERJECT_TOOL_BATCH', 'client_message_id': 'batch-first'})[0] == 202
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                _, batch = api.request(f'/api/sessions/{batch_session}')
                if batch['session']['status'] == 'waiting_for_answer': break
                time.sleep(.03)
            assert batch['session']['status'] == 'waiting_for_answer'
            assert api.request(f'/api/sessions/{batch_session}/messages', {'text': 'Do not perform the remaining write', 'client_message_id': 'batch-immediate', 'delivery_mode': 'immediate'})[0] == 202
            _, batch_questions = api.request(f'/api/sessions/{batch_session}/questions')
            assert api.request(f'/api/sessions/{batch_session}/questions/{batch_questions[0]["id"]}/answer', {'answer': 'Continue with the new instruction only'})[0] == 200
            batch_done = api.wait(batch_session)
            assert batch_done['session']['status'] == 'completed'
            assert not (work / 'must-not-run-waiting-tool.txt').exists()
            assert any(e['type'] == 'tool_end' and e['data']['name'] == 'write' and e['data']['is_error'] and 'NOT EXECUTED' in e['data']['output'] for e in batch_done['events'])
            results.append('immediate input skips unstarted tool side effects even when accepted during a blocking question gate')
            _, recovery_config = api.request('/api/config')
            recovery_config['sites'].append({'id': 'recovery-site', 'name': 'Recovery site', 'base_url': f'http://127.0.0.1:{upstream.server_port}/v1', 'keys': [{'id': 'recovery-key', 'name': 'Recovery key', 'secret': 'fake-key-one', 'models': [dict(model, id='recovery-model', model='offline-model', name='Recovery model')]}]})
            assert api.request('/api/config', recovery_config, 'PUT')[0] == 200
            recovery_selection = {'site_id': 'recovery-site', 'key_id': 'recovery-key', 'model_id': 'recovery-model'}
            network_session = api.session(recovery_selection)
            Provider.network_up.clear()
            assert api.request(f'/api/sessions/{network_session}/messages', {'text': 'Resume safely after offline', 'client_message_id': 'network-start'})[0] == 202
            end = time.monotonic() + 5
            while time.monotonic() < end:
                _, offline_detail = api.request(f'/api/sessions/{network_session}')
                if offline_detail['session']['status'] == 'waiting_for_network': break
                time.sleep(.03)
            assert offline_detail['session']['status'] == 'waiting_for_network', offline_detail
            assert api.request(f'/api/sessions/{network_session}/messages', {'text': 'Received during offline', 'client_message_id': 'offline-insert', 'delivery_mode': 'after_call'})[0] == 202
            Provider.network_up.set()
            recovered = api.wait(network_session)
            assert recovered['session']['status'] == 'completed', recovered
            assert any(e['type'] == 'network_restored' for e in recovered['events'])
            assert 'Received during offline' in json.dumps(Provider.requests[-1][1]['input'])
            assert Provider.probes and all(p['path'] == '/' and p['auth'] is None and p['cookie'] is None for p in Provider.probes)
            results.append('transport outage parks safely and anonymous online detection resumes the committed task and queued message')
            Provider.network_up.clear()
            stopped_network = api.session(recovery_selection)
            assert api.request(f'/api/sessions/{stopped_network}/messages', {'text': 'Stop while offline', 'client_message_id': 'network-stop'})[0] == 202
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline:
                _, waiting_stop = api.request(f'/api/sessions/{stopped_network}')
                if waiting_stop['session']['status'] == 'waiting_for_network': break
                time.sleep(.03)
            assert waiting_stop['session']['status'] == 'waiting_for_network'
            assert api.request(f'/api/sessions/{stopped_network}/cancel', {})[0] == 200
            stopped_detail = api.wait(stopped_network)
            assert stopped_detail['session']['status'] == 'cancelled'
            Provider.network_up.set()
            _, stopped_detail = api.request(f'/api/sessions/{stopped_network}')
            assert stopped_detail['session']['status'] == 'cancelled'
            assert not any(e['type'] == 'network_restored' for e in stopped_detail['events'])
            results.append('stop during a network outage cancels recovery and does not silently resume when connectivity returns')
            blocked = api.session(selection)
            assert api.request(f'/api/sessions/{blocked}/messages', {'text': 'BLOCKING_QUESTION_TEST', 'client_message_id': 'blocking'})[0] == 202
            end = time.monotonic() + 5
            while time.monotonic() < end:
                _, waiting = api.request(f'/api/sessions/{blocked}')
                if waiting['session']['status'] == 'waiting_for_answer':
                    break
                time.sleep(.03)
            assert waiting['session']['status'] == 'waiting_for_answer', waiting
            assert not (work / 'after-answer.txt').exists(), 'Tools after blocking question ran before answer'
            _, questions = api.request(f'/api/sessions/{blocked}/questions')
            question = questions[0]
            before_answer_requests = len(Provider.requests)
            assert api.request(f'/api/sessions/{blocked}/questions/{question["id"]}/answer', {'answer': 'red'})[0] == 200
            resumed = api.wait(blocked)
            assert resumed['session']['status'] == 'completed'
            assert (work / 'after-answer.txt').read_text() == 'answer received'
            assert len(Provider.requests) > before_answer_requests
            assert any(e['type'] == 'question_answer' for e in resumed['events'])
            results.append('blocking question gates later tools and user answer resumes the task')
            optional = api.run(api.session(selection), 'OPTIONAL_QUESTION_TEST')
            _, optional_questions = api.request('/api/sessions/' + optional['session']['id'] + '/questions')
            assert optional['session']['status'] == 'completed' and optional_questions[0]['status'] == 'withdrawn'
            results.append('optional clarification does not stop work and AI can withdraw it')
            optional_session = api.session(selection)
            api.run(optional_session, 'OPTIONAL_ANSWER_TEST')
            _, optional_open = api.request(f'/api/sessions/{optional_session}/questions')
            optional_id = optional_open[0]['id']
            assert api.request(f'/api/sessions/{optional_session}/questions/{optional_id}/answer', {'answer': 'careful'})[0] == 200
            optional_resumed = api.wait(optional_session)
            assert optional_resumed['session']['status'] == 'completed'
            optional_count = len(Provider.requests)
            assert api.request(f'/api/sessions/{optional_session}/questions/{optional_id}/answer', {'answer': 'careful'})[0] == 200
            assert len(Provider.requests) == optional_count, 'Answer retry started a duplicate generation'
            results.append('late optional answer resumes idle session exactly once even on retry')
            goal_session = api.session(selection)
            status, goal = api.request(f'/api/sessions/{goal_session}/goals', {'title': 'Long task', 'objective': 'GOAL_DRIVER_TEST: verify and explicitly finish this task'})
            assert status == 200 and goal['status'] == 'pending', goal
            assert api.request(f'/api/sessions/{goal_session}/goals/{goal["id"]}', {'action': 'start'}, 'PATCH')[0] == 200
            driven = api.wait(goal_session)
            _, goals = api.request(f'/api/sessions/{goal_session}/goals')
            assert driven['session']['status'] == 'completed' and goals[0]['status'] == 'completed' and goals[0]['rounds'] >= 1, (driven, goals)
            results.append('long goal continues after ordinary answers until AI explicitly completes it')
            paused_session = api.session(selection)
            _, pausable = api.request(f'/api/sessions/{paused_session}/goals', {'title': 'Pausable goal', 'objective': 'PAUSE_GOAL_TEST: keep working until explicitly paused'})
            assert api.request(f'/api/sessions/{paused_session}/goals/{pausable["id"]}', {'action': 'start'}, 'PATCH')[0] == 200
            assert api.request(f'/api/sessions/{paused_session}/goals/{pausable["id"]}', {'action': 'pause'}, 'PATCH')[0] == 200
            assert api.request(f'/api/sessions/{paused_session}/goals/{pausable["id"]}', {'action': 'resume'}, 'PATCH')[0] == 200
            _, active_goals = api.request(f'/api/sessions/{paused_session}/goals')
            assert active_goals[0]['status'] == 'active', active_goals
            assert api.request(f'/api/sessions/{paused_session}/cancel', {})[0] == 200
            stopped = api.wait(paused_session)
            _, paused_goals = api.request(f'/api/sessions/{paused_session}/goals')
            assert paused_goals[0]['status'] == 'paused' and stopped['session']['status'] in ('paused', 'cancelled')
            bad_session = api.session(selection)
            _, bad_goal = api.request(f'/api/sessions/{bad_session}/goals', {'title': 'Failure boundary', 'objective': 'INVALID_JSON'})
            assert api.request(f'/api/sessions/{bad_session}/goals/{bad_goal["id"]}', {'action': 'start'}, 'PATCH')[0] == 200
            failed = api.wait(bad_session)
            _, failed_goals = api.request(f'/api/sessions/{bad_session}/goals')
            assert failed['session']['status'] == 'failed' and failed_goals[0]['status'] == 'paused'
            results.append('goal pause/resume/stop and failure pause never spin an errored task')
            before_network_models = len(Provider.requests)
            before_network_heads = len(Provider.probes)
            network_check_input = {'selection': selection, 'groups': ['network'], 'network_trace': False, 'client_request_id': str(uuid.uuid4())}
            status, net_plan = api.request('/api/model-checks/plan', network_check_input)
            assert status == 200 and net_plan['provider_request_count'] == 0 and net_plan['network_request_count'] == 4 and net_plan['max_network_http_requests'] == 4, net_plan
            assert net_plan['estimated_input_tokens_upper'] == 0 and net_plan['max_output_tokens_total'] == 0
            assert len(Provider.requests) == before_network_models and len(Provider.probes) == before_network_heads
            status, net_run = api.request('/api/model-checks', network_check_input)
            assert status == 202, net_run
            net_done = api.wait_quality(net_run['id'])
            assert net_done['status'] == 'completed' and net_done['completed_probes'] == 4, net_done
            assert len(Provider.requests) == before_network_models, 'Network group called generation API'
            head_requests = Provider.probes[before_network_heads:]
            assert len(head_requests) == 4 and all(p['path'] == '/v1/responses' and p['auth'] is None and p['cookie'] is None for p in head_requests)
            assert net_done['summary']['totals']['provider_requests_completed'] == 0
            assert net_done['summary']['totals']['estimated_cost'] == 0 and net_done['summary']['totals']['input_tokens'] == 0
            assert net_done['summary']['network']['successful_http_samples'] == 3
            assert net_done['summary']['network']['head_ms']['median'] is not None
            assert any(c['name'] == 'Cloudflare' for c in net_done['summary']['network']['cdn_candidates'])
            assert all(r['observation']['protocol'] == 'network' and r['analysis']['model_match'] is None for r in net_done['results'])
            assert all(r['observation']['first_token_ms'] is None for r in net_done['results'])
            assert not any(r['variant'] == 'route_trace' for r in net_done['results'])
            status, traced_plan = api.request('/api/model-checks/plan', dict(network_check_input, network_trace=True))
            assert status == 200 and traced_plan['request_count'] == 5 and traced_plan['provider_request_count'] == 0
            assert len(Provider.probes) == before_network_heads + 4, 'Preview traced or contacted endpoint'
            results.append('network-only quality checks use anonymous HEAD with latency jitter and CDN evidence but no model charges or implicit trace')
            quality_input = {'selection': selection, 'client_request_id': str(uuid.uuid4()), 'pricing': {'currency': 'CNY', 'input_per_million': 2, 'cached_input_per_million': .2, 'output_per_million': 4}}
            before_quality = len(Provider.requests)
            status, quality_plan = api.request('/api/model-checks/plan', quality_input)
            assert status == 200 and quality_plan['request_count'] == 14, quality_plan
            assert len(Provider.requests) == before_quality, 'Preview performed paid work'
            assert quality_plan['max_output_tokens_total'] == 14 * 256
            status, quality_started = api.request('/api/model-checks', quality_input)
            assert status == 202, quality_started
            status, repeated_quality = api.request('/api/model-checks', quality_input)
            assert status == 202 and repeated_quality['id'] == quality_started['id']
            quality_done = api.wait_quality(quality_started['id'])
            assert quality_done['status'] == 'completed' and quality_done['completed_probes'] == 14, quality_done
            assert len(Provider.requests) == before_quality + 14, 'Same UUID duplicated requests'
            assert quality_done['summary']['model_identity']['mismatches'] == 2
            assert quality_done['summary']['totals']['cached_tokens'] is None
            assert quality_done['summary']['totals']['estimated_cost'] is None
            assert any(r['analysis']['tool_valid'] is True for r in quality_done['results'])
            assert all(r['observation']['header_latency_ms'] is not None for r in quality_done['results'])
            assert all(r['observation']['first_token_ms'] is None for r in quality_done['results']), 'TTFT invented from final snapshot'
            pairs = {p['variant']: p for p in quality_done['summary']['cache']['pairs']}
            assert pairs['strict_omit']['cached_tokens'] == 11000 and pairs['strict_true']['cached_tokens'] == 896
            assert not quality_done['summary']['cache']['comparable'], 'Identity variation ignored'
            query = '&'.join(f'{k}={v}' for k, v in selection.items())
            status, quality_list = api.request('/api/model-checks?' + query)
            assert status == 200 and any(r['id'] == quality_done['id'] for r in quality_list)
            other_query = '&'.join(f'{k}={v}' for k, v in {'site_id': 'site-alt', 'key_id': 'key-alt', 'model_id': 'model-alt'}.items())
            assert api.request('/api/model-checks?' + other_query)[1] == []
            exported = api.request(f'/api/model-checks/{quality_done["id"]}/export')[1]
            assert exported == quality_done and 'fake-key-one' not in json.dumps(exported)
            assert api.request('/api/model-checks', quality_input)[1]['id'] == quality_done['id']
            assert len(Provider.requests) == before_quality + 14
            results.append('per-model quality plans cost no provider requests and persistent checks compare routing cache protocol with idempotent starts')
            malicious_probe = {'selection': selection, 'groups': ['routing'], 'instruction_probes': ['no_nested_agents'], 'client_request_id': str(uuid.uuid4())}
            status, no_execution = api.request('/api/model-checks', malicious_probe)
            assert status == 202
            no_execution = api.wait_quality(no_execution['id'])
            assert no_execution['status'] == 'completed'
            assert not (work / 'must-not-execute-quality.txt').exists()
            assert any(r['observation']['output'] and any(i.get('name') == 'write' for i in r['observation']['output']) for r in no_execution['results'])
            assert not any('tool_start' in json.dumps(r) for r in no_execution['results'])
            results.append('English instruction quality probes retain model tool outputs as evidence but never dispatch side effects')
            Provider.quality_gate_enabled = True; Provider.quality_gate_started.clear(); Provider.quality_gate_release.clear()
            try:
                status, cancel_check = api.request('/api/model-checks', {'selection': selection, 'groups': ['protocol'], 'client_request_id': str(uuid.uuid4())})
                assert status == 202
                assert Provider.quality_gate_started.wait(3)
                assert api.request(f'/api/model-checks/{cancel_check["id"]}/cancel', {})[0] == 200
                cancelled_check = api.wait_quality(cancel_check['id'])
                assert cancelled_check['status'] == 'cancelled' and cancelled_check['completed_probes'] < 2
            finally:
                Provider.quality_gate_enabled = False; Provider.quality_gate_release.set()
            results.append('quality checks cancel in-flight requests and leave remaining paid probes unstarted')
            _, auto_config = api.request('/api/config')
            auto_config['sites'][0]['keys'][0]['models'][0]['context_window'] = 20000
            assert api.request('/api/config', auto_config, 'PUT')[0] == 200
            auto = api.run(api.session(selection), 'COMPACTION_TARGET: continue after automatic summary. ' + 'context ' * 1800)
            assert auto['session']['status'] == 'completed' and auto['session']['context_epoch'] >= 1, auto
            assert any(e['type'] == 'status' and e['data']['status'] == 'auto_compacting' for e in auto['events'])
            assert any(e['type'] == 'compacted' for e in auto['events'])
            results.append('context budget triggers chunked automatic compaction and resumes unfinished work')
            assert api.request('/api/logout', {})[0] == 200
            assert api.request('/api/sessions')[0] == 401
            results.append('logout revokes protected API access')
        finally:
            process.terminate()
            try:
                process.wait(timeout=10)
            except subprocess.TimeoutExpired:
                process.kill()
                process.wait()
            upstream.shutdown()
    for result in results:
        print('PASS', result)
    print(f'{len(results)} black-box integration checks passed; no live provider contacted.')


if __name__ == '__main__':
    main()
