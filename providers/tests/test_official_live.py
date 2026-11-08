"""Offline official-protocol tests. No credentials, devices or network calls."""
import array
import base64
import gzip
import hashlib
import hmac
import io
import json
from pathlib import Path
import queue
import struct
import sys
import threading
import time
import unittest
import uuid
from unittest.mock import patch
from urllib.parse import parse_qs, urlsplit

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
import official_live as live


def config(kind, **extra):
    values = {'providerType': kind, 'appId': '12345678', **extra}
    credential = {'apiKey': 'synthetic-fixture', 'secretId': 'fixture-id', 'secretKey': 'fixture-key'}
    return live.Config(values, credential)


def server_packet(data, final=False):
    payload = gzip.compress(json.dumps(data).encode())
    return bytes((0x11, 0x93 if final else 0x91, 0x11, 0)) + struct.pack('>iI', -2 if final else 1, len(payload)) + payload


class FakeTimeout(Exception):
    pass


class FakeSocket:
    def __init__(self, kind):
        self.kind = kind
        self.messages = queue.Queue()
        self.audio = bytearray()
        self.headers = None
        self.closed = False
        if kind == 'tencent':
            self.put({'code': 0})

    def put(self, value):
        self.messages.put(json.dumps(value))

    def settimeout(self, value):
        pass

    def send(self, raw):
        event = json.loads(raw)
        kind = event['type']
        if kind == 'session.update':
            self.config_event = event
            self.put({'type': 'session.updated'})
        elif kind == 'input_audio_buffer.append':
            self.audio.extend(base64.b64decode(event['audio']))
            if self.kind == 'qwen':
                self.put({'type': 'conversation.item.input_audio_transcription.text', 'item_id': 'a', 'text': '你好', 'stash': '世界'})
        elif kind in ('input_audio_buffer.commit', 'session.finish'):
            self.put({'type': 'conversation.item.input_audio_transcription.completed', 'item_id': 'a', 'transcript': '你好世界'})
            if self.kind == 'qwen':
                self.put({'type': 'session.finished'})
        elif kind == 'end':
            self.put({'code': 0, 'result': {'index': 0, 'slice_type': 2, 'voice_text_str': '你好世界'}, 'final': 1})

    def send_binary(self, data):
        if self.kind == 'tencent':
            self.audio.extend(data)
            return
        payload = gzip.decompress(data[8:])
        if data[1] >> 4 == 1:
            self.config_event = json.loads(payload)
            self.messages.put(server_packet({}))
        elif data[1] & 2:
            self.messages.put(server_packet({'result': {'text': '你好世界', 'utterances': [{'text': '你好世界', 'definite': True}]}}, final=True))
        else:
            self.audio.extend(payload)

    def recv(self):
        try:
            return self.messages.get(timeout=.01)
        except queue.Empty:
            raise FakeTimeout from None

    def close(self, timeout=0):
        self.closed = True


class FakeWebsocket:
    WebSocketTimeoutException = FakeTimeout

    def __init__(self, kind):
        self.socket = FakeSocket(kind)

    def create_connection(self, url, **kwargs):
        self.url = url
        self.kwargs = kwargs
        return self.socket


class FreshWebsocket(FakeWebsocket):
    def __init__(self, kind='openai'):
        self.kind = kind
        self.sockets = []
        self.gate = None
        self.entered = threading.Event()
        self.fail = False

    def create_connection(self, url, **kwargs):
        socket = FakeSocket(self.kind)
        self.sockets.append(socket)
        self.entered.set()
        if self.gate is not None:
            self.gate.wait(3)
        if self.fail:
            raise RuntimeError('synthetic-private-key')
        return socket


class OfficialTests(unittest.TestCase):
    def test_close_while_receiver_starts_never_joins_an_unstarted_thread(self):
        original_thread = threading.Thread
        for after_start in (False, True):
            with self.subTest(after_start=after_start):
                entered, release = threading.Event(), threading.Event()

                class GatedThread(original_thread):
                    def start(self):
                        if after_start:
                            super().start()
                        entered.set()
                        if not release.wait(2):
                            raise RuntimeError('fixture gate timed out')
                        if not after_start:
                            super().start()

                socket = FakeWebsocket('openai')
                connection = live.Connection(config('openai'), socket)
                opener = original_thread(target=connection.open)
                try:
                    with patch.object(live.threading, 'Thread', GatedThread):
                        opener.start()
                        self.assertTrue(entered.wait(1))
                        connection.close()
                        self.assertTrue(socket.socket.closed)
                        release.set()
                        opener.join(2)
                        self.assertFalse(opener.is_alive())
                    self.assertTrue(connection.ready.is_set())
                    self.assertIsNotNone(connection.receiver)
                    self.assertFalse(connection.receiver.is_alive())
                    self.assertFalse(hasattr(socket.socket, 'config_event'))
                    self.assertEqual(socket.socket.audio, b'')
                finally:
                    release.set()
                    opener.join(2)
                    connection.close()

    def wait_until(self, predicate):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            if predicate():
                return
            time.sleep(.005)
        self.fail('Expected asynchronous state was not reached')

    def ready_standby(self, bridge):
        self.wait_until(lambda: bridge.warmup.prepared is not None and bridge.warmup.prepared.ready.is_set())
        return bridge.warmup.prepared

    def wait_event(self, output, kind, sid=1):
        deadline = time.monotonic() + 3
        while time.monotonic() < deadline:
            events = [json.loads(line) for line in output.getvalue().splitlines()]
            if any(e['type'] == kind and e.get('id') == sid for e in events):
                return events
            time.sleep(.01)
        self.fail('Expected protocol event was not emitted: ' + kind)

    def test_official_endpoint_and_required_fields(self):
        for kind in live.DEFAULTS:
            config(kind)
            for url in ('ws://api.openai.com/v1/realtime', 'wss://api.openai.com.attacker.test/v1/realtime',
                        'wss://user@api.openai.com/v1/realtime', 'wss://api.openai.com:444/v1/realtime'):
                with self.assertRaises(live.ServiceError):
                    config(kind, endpoint=url)
        with self.assertRaises(live.ServiceError):
            live.Config({'providerType': 'openai'}, {})
        with self.assertRaises(live.ServiceError):
            config('tencent', model='8k_zh')
        with self.assertRaises(live.ServiceError):
            config('qwen', model='paraformer-realtime-v2')
        config('qwen', endpoint='wss://dashscope-intl.aliyuncs.com/api-ws/v1/realtime')
        config('tencent', model='Hy-ASR-3.0-preview')

    def test_authentication_headers_and_api_key_modes(self):
        c = config('doubao')
        _, headers = live.Adapter(c, lambda **_: None).connection()
        self.assertEqual(headers['X-Api-Key'], 'synthetic-fixture')
        self.assertNotIn('X-Api-Access-Key', headers)
        self.assertEqual(str(uuid.UUID(headers['X-Api-Request-Id'])), headers['X-Api-Connect-Id'])
        legacy = live.Config({'providerType': 'doubao', 'appId': 'app-fixture'}, {'accessKey': 'access-fixture'})
        _, headers = live.Adapter(legacy, lambda **_: None).connection()
        self.assertEqual(headers['X-Api-App-Key'], 'app-fixture')
        self.assertEqual(headers['X-Api-Access-Key'], 'access-fixture')
        _, headers = live.Adapter(config('openai'), lambda **_: None).connection()
        self.assertEqual(headers['Authorization'], 'Bearer synthetic-fixture')

    def test_resampling_is_chunk_invariant_and_length_correct(self):
        pcm = array.array('h', [int(16000 * ((i % 41) / 41 - .5)) for i in range(640)]).tobytes()
        one = live.Resample24k()
        expected = one.feed(pcm) + one.feed(b'', final=True)
        split = live.Resample24k()
        actual = b''.join(split.feed(pcm[i:i+22]) for i in range(0, len(pcm), 22)) + split.feed(b'', final=True)
        self.assertEqual(expected, actual)
        self.assertEqual(len(expected), 960 * 2)
        dc = live.Resample24k()
        self.assertEqual(set(array.array('h', dc.feed(struct.pack('<h', 1234) * 100, final=True))), {1234})

    def test_batched_resampling_matches_sample_by_sample_conversion(self):
        def reference(chunks):
            samples, position, inputs, outputs, out = [], 0, 0, 0, array.array('h')
            for pcm, final in chunks:
                incoming = array.array('h', pcm)
                inputs += len(incoming)
                samples.extend(incoming)
                if final and samples:
                    samples.append(samples[-1])
                while position // 3 + 1 < len(samples) and outputs < inputs * 3 // 2:
                    index, fraction = divmod(position, 3)
                    out.append(round((samples[index] * (3 - fraction) + samples[index + 1] * fraction) / 3))
                    position += 2
                    outputs += 1
                consumed = position // 3
                samples = samples[consumed:]
                position -= consumed * 3
            return out.tobytes()

        pcm = array.array('h', [int(30000 * ((i * 7919) % 997 / 997 - .5)) for i in range(4801)]).tobytes()
        for size in (2, 6, 22, 640, 1280, 9602):
            with self.subTest(size=size):
                chunks = [(pcm[i:i + size], False) for i in range(0, len(pcm), size)] + [(b'', True)]
                resampler = live.Resample24k()
                got = b''.join(resampler.feed(part, final) for part, final in chunks)
                self.assertEqual(got, reference(chunks))

    def test_next_session_connects_while_the_previous_waits_and_messages_keep_order(self):
        release = threading.Event()

        class HeldFinal(FakeSocket):
            def send(self, raw):
                if json.loads(raw)['type'] == 'session.finish' and not release.is_set():
                    threading.Thread(target=lambda: (release.wait(3), FakeSocket.send(self, raw))).start()
                    return
                FakeSocket.send(self, raw)

        class Sockets(FreshWebsocket):
            def create_connection(self, url, **kwargs):
                socket = (HeldFinal if not self.sockets else FakeSocket)(self.kind)
                self.sockets.append(socket)
                return socket

        output = io.StringIO()
        ws = Sockets('qwen')
        bridge = live.Bridge(config('qwen'), live.Output(output), ws)
        pcm = base64.b64encode(b'\x00\x00' * 3200).decode()
        try:
            for sid in (1, 2):
                bridge.handle({'type': 'start', 'id': sid, 'sample_rate': 16000})
                bridge.handle({'type': 'audio', 'id': sid, 'data': pcm})
                bridge.handle({'type': 'stop', 'id': sid})
            deadline = time.monotonic() + 2
            while len(ws.sockets) < 2 and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertEqual(len(ws.sockets), 2, 'the next session connects before the previous final')
            time.sleep(.2)
            self.assertEqual({json.loads(line)['id'] for line in output.getvalue().splitlines()}, {1})
            release.set()
            events = self.wait_event(output, 'end', 2)
        finally:
            release.set()
            bridge.shutdown()
        ids = [e['id'] for e in events]
        self.assertEqual(ids, sorted(ids), 'every message of session 1 comes first')
        self.assertEqual([(e['id'], e['text']) for e in events if e['type'] == 'final'], [(1, '你好世界'), (2, '你好世界')])
        self.assertFalse(any(e['type'] == 'error' for e in events))

    def test_doubao_binary_framing_and_stable_prefix(self):
        packet = live.doubao_packet(b'\x00\x01', audio=True, final=True)
        self.assertEqual(packet[:4], b'\x11\x22\x01\x00')
        self.assertEqual(struct.unpack('>I', packet[4:8])[0], len(packet) - 8)
        self.assertEqual(gzip.decompress(packet[8:]), b'\x00\x01')
        seen = []
        adapter = live.Adapter(config('doubao'), lambda **e: seen.append(e))
        adapter.receive(server_packet({'result': {'text': '甲乙', 'utterances': [{'text': '甲', 'definite': True}, {'text': '乙', 'definite': False}]}}))
        self.assertEqual(seen[-1]['stable'], 1)
        adapter.receive(server_packet({'result': {'text': '甲乙。'}}, final=True))
        self.assertTrue(adapter.finished.is_set())
        self.assertEqual(seen[-1]['stable'], 3)
        with self.assertRaises(live.ServiceError):
            live.doubao_response(b'bad')
        remote_error = b'\x11\xf0\x10\x00' + struct.pack('>II', 45000001, 0)
        with self.assertRaises(live.ServiceError) as raised:
            live.doubao_response(remote_error)
        self.assertEqual(raised.exception.code, 'service_parameter')

    def test_tencent_signature_and_url_encoding(self):
        c = config('tencent')
        url = live.tencent_url(c, now=1000, nonce=123, voice_id='fixture')
        parsed = urlsplit(url)
        query = parse_qs(parsed.query)
        signature = query.pop('signature')[0]
        canonical = parsed.netloc + parsed.path + '?' + '&'.join(k+'='+v[0] for k,v in sorted(query.items()))
        expected = base64.b64encode(hmac.new(b'fixture-key', canonical.encode(), hashlib.sha1).digest()).decode()
        self.assertEqual(signature, expected)
        self.assertIn('%3D', url)
        self.assertNotIn('fixture-key', url)
        with patch.object(live.secrets, 'randbelow', return_value=0):
            positive_nonce = parse_qs(urlsplit(live.tencent_url(c)).query)['nonce'][0]
        self.assertEqual(positive_nonce, '1', 'Tencent requires a positive nonce, not zero')

    def test_openai_models_use_transcription_session_and_model_specific_language(self):
        for model in ('gpt-live-transcribe', 'gpt-transcribe', 'gpt-4o-transcribe', 'gpt-4o-mini-transcribe'):
            with self.subTest(model=model):
                adapter = live.Adapter(config('openai', model=model, language='zh'), lambda **_: None)
                adapter.socket = FakeSocket('openai')
                adapter.begin()
                event = adapter.socket.config_event
                self.assertEqual(event['type'], 'session.update')
                self.assertEqual(event['session']['type'], 'transcription')
                audio_input = event['session']['audio']['input']
                self.assertEqual(audio_input['format'], {'type': 'audio/pcm', 'rate': 24000})
                self.assertIsNone(audio_input['turn_detection'])
                transcription = audio_input['transcription']
                self.assertEqual(transcription['model'], model)
                if model == 'gpt-live-transcribe':
                    self.assertEqual(transcription['languages'], ['zh'])
                    self.assertNotIn('language', transcription)
                else:
                    self.assertEqual(transcription['language'], 'zh')
                    self.assertNotIn('languages', transcription)
                endpoint, _ = adapter.connection()
                self.assertEqual(parse_qs(urlsplit(endpoint).query), {'intent': ['transcription']})

    def test_service_error_categories_use_machine_fields_and_never_raw_text(self):
        cases = [
            ('openai', {'code': 'expired_secret_key', 'type': 'invalid_request_error'}, 'credential_expired'),
            ('openai', {'code': 'invalid_api_key'}, 'credential_rejected'),
            ('openai', {'code': 'unsupported_country_region_territory'}, 'service_region'),
            ('openai', {'code': 'insufficient_quota'}, 'service_quota'),
            ('openai', {'code': 'invalid_value', 'param': 'session.audio.input.transcription.model'}, 'service_model'),
            ('openai', {'code': 'unknown_parameter'}, 'service_parameter'),
            ('qwen', {'code': 'InvalidApiKey'}, 'credential_rejected'),
            ('qwen', {'code': 'Throttling.RateQuota'}, 'service_quota'),
            ('qwen', {'code': 'invalid_value', 'param': 'session.input_audio_transcription.model'}, 'service_model'),
            ('tencent', {'code': 4002}, 'credential_rejected'),
            ('tencent', {'code': 4004}, 'service_quota'),
            ('tencent', {'code': 6001}, 'service_region'),
            ('doubao', {'code': 45000001}, 'service_parameter'),
            ('doubao', {'code': 45000151}, 'audio_format'),
        ]
        for kind, error, expected in cases:
            with self.subTest(kind=kind, expected=expected):
                error['message'] = 'synthetic-private-key and auth-bearing URL'
                mapped = live.service_error(kind, {'error': error})
                self.assertEqual(mapped.code, expected)
                self.assertNotIn('synthetic-private-key', str(mapped))
        unknown = live.service_error('openai', {'error': {'message': 'synthetic-private-key'}})
        self.assertEqual(unknown.code, 'service')

    def test_expired_openai_key_fails_before_audio_and_identifies_session(self):
        output = io.StringIO()
        ws = FakeWebsocket('openai')
        ws.socket.put({'type': 'error', 'error': {'code': 'expired_secret_key',
                      'type': 'invalid_request_error', 'message': 'synthetic-private-key'}})
        bridge = live.Bridge(config('openai'), live.Output(output), ws)
        try:
            bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
            bridge.handle({'type': 'audio', 'id': 1, 'data': base64.b64encode(b'\x00\x00' * 3200).decode()})
            events = self.wait_event(output, 'end')
            failures = [e for e in events if e['type'] == 'error']
            self.assertEqual(len(failures), 1)
            self.assertEqual(failures[0]['code'], 'credential_expired')
            self.assertEqual(failures[0]['id'], 1)
            self.assertEqual(len(ws.socket.audio), 0)
            self.assertNotIn('synthetic-private-key', output.getvalue())
            self.assertFalse(any(e['type'] == 'final' for e in events))
        finally:
            bridge.shutdown()

    def test_http_handshake_error_is_classified_and_redacted(self):
        class HttpFailure(Exception):
            status_code = 401
            resp_body = json.dumps({'error': {'code': 'expired_secret_key', 'message': 'synthetic-private-key'}})
        output = io.StringIO()
        ws = FakeWebsocket('openai')
        def fail(*args, **kwargs):
            raise HttpFailure('synthetic-private-key in transport exception')
        ws.create_connection = fail
        bridge = live.Bridge(config('openai'), live.Output(output), ws)
        try:
            bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
            events = self.wait_event(output, 'end')
            self.assertTrue(any(e.get('code') == 'credential_expired' for e in events))
            self.assertNotIn('synthetic-private-key', output.getvalue())
            self.assertFalse(any(e['type'] == 'final' for e in events))
        finally:
            bridge.shutdown()

    def test_turn_order_and_revisions_are_not_duplicated(self):
        result = live.Transcript()
        result.reserve('first')
        result.reserve('second', 'first')
        result.update('second', '乙', 1)
        self.assertEqual(result.value(), ('乙', 0))
        result.update('first', '甲', 1)
        self.assertEqual(result.value(), ('甲乙', 2))
        result.update('second', '乙。', 2)
        self.assertEqual(result.value(), ('甲乙。', 3))

    def test_four_full_sessions_wait_for_final_and_close(self):
        pcm = struct.pack('<h', 123) * 3200
        for kind in live.DEFAULTS:
            with self.subTest(kind=kind):
                output = io.StringIO()
                ws = FakeWebsocket(kind)
                bridge = live.Bridge(config(kind), live.Output(output), ws)
                try:
                    bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
                    bridge.handle({'type': 'audio', 'id': 1, 'data': base64.b64encode(pcm).decode()})
                    bridge.handle({'type': 'stop', 'id': 1})
                    deadline = time.monotonic() + 3
                    while '"type":"end"' not in output.getvalue() and time.monotonic() < deadline:
                        time.sleep(.01)
                    events = [json.loads(line) for line in output.getvalue().splitlines()]
                    self.assertFalse(any(e['type'] == 'error' for e in events), events)
                    self.assertEqual([e['text'] for e in events if e['type'] == 'final'], ['你好世界'])
                    self.assertTrue(any(e['type'] == 'connected' for e in events))
                    self.assertEqual(events[-1], {'type': 'end', 'id': 1, 'cancelled': False})
                    self.assertEqual(len(ws.socket.audio), len(pcm) * 3 // 2 if kind == 'openai' else len(pcm))
                    self.assertEqual(ws.kwargs['redirect_limit'], 0)
                    self.assertTrue(ws.socket.closed)
                    if kind == 'openai':
                        session = ws.socket.config_event['session']
                        self.assertEqual(session['type'], 'transcription')
                        self.assertEqual(session['audio']['input']['format']['rate'], 24000)
                        self.assertIsNone(session['audio']['input']['turn_detection'])
                        self.assertEqual(session['audio']['input']['transcription']['model'], 'gpt-live-transcribe')
                    elif kind == 'qwen':
                        session = ws.socket.config_event['session']
                        self.assertEqual(session['sample_rate'], 16000)
                        self.assertEqual(session['turn_detection']['type'], 'server_vad')
                        self.assertIn('event_id', ws.socket.config_event)
                    elif kind == 'doubao':
                        self.assertEqual(ws.socket.config_event['audio']['rate'], 16000)
                        self.assertTrue(ws.socket.config_event['request']['show_utterances'])
                    else:
                        self.assertEqual(urlsplit(ws.url).path, '/asr/v2/12345678')
                finally:
                    bridge.shutdown()

    def test_cancel_closes_transport_and_next_session_can_start(self):
        for kind in live.DEFAULTS:
            with self.subTest(kind=kind):
                output = io.StringIO()
                ws = FakeWebsocket(kind)
                bridge = live.Bridge(config(kind), live.Output(output), ws)
                try:
                    bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
                    self.wait_event(output, 'connected')
                    bridge.handle({'type': 'cancel', 'id': 1})
                    events = self.wait_event(output, 'end')
                    self.assertEqual(events[-1], {'type': 'end', 'id': 1, 'cancelled': True})
                    self.assertFalse(any(e['type'] in ('error', 'final') for e in events))
                    self.assertTrue(ws.socket.closed)
                    ws.socket = FakeSocket(kind)
                    bridge.handle({'type': 'start', 'id': 2, 'sample_rate': 16000})
                    bridge.handle({'type': 'audio', 'id': 2, 'data': base64.b64encode(b'\x00\x00' * 3200).decode()})
                    bridge.handle({'type': 'stop', 'id': 2})
                    events = self.wait_event(output, 'end', 2)
                    self.assertTrue(any(e['type'] == 'final' and e['id'] == 2 for e in events))
                    self.assertFalse(any(e['type'] == 'error' for e in events))
                finally:
                    bridge.shutdown()

    def test_disconnect_and_service_errors_do_not_emit_success_or_sensitive_details(self):
        for kind in live.DEFAULTS:
            for mode in ('disconnect', 'service'):
                with self.subTest(kind=kind, mode=mode):
                    output = io.StringIO()
                    ws = FakeWebsocket(kind)
                    bridge = live.Bridge(config(kind), live.Output(output), ws)
                    try:
                        bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
                        self.wait_event(output, 'connected')
                        if mode == 'disconnect':
                            ws.socket.messages.put('')
                        elif kind == 'doubao':
                            ws.socket.messages.put(server_packet({'code': 45000001, 'message': 'synthetic-fixture'}))
                        elif kind == 'tencent':
                            ws.socket.put({'code': 4001, 'message': 'synthetic-fixture'})
                        else:
                            ws.socket.put({'type': 'error', 'error': {'message': 'synthetic-fixture'}})
                        events = self.wait_event(output, 'end')
                        self.assertTrue(any(e['type'] == 'error' for e in events))
                        self.assertFalse(any(e['type'] == 'final' for e in events))
                        self.assertNotIn('synthetic-fixture', output.getvalue())
                        self.assertTrue(ws.socket.closed)
                    finally:
                        bridge.shutdown()

    def test_missing_final_is_reported_instead_of_silently_truncating(self):
        output = io.StringIO()
        ws = FakeWebsocket('qwen')
        original = ws.socket.send
        def send(raw):
            if json.loads(raw)['type'] != 'session.finish':
                original(raw)
        ws.socket.send = send
        with patch.object(live, 'FINAL_TIMEOUT', .05):
            bridge = live.Bridge(config('qwen'), live.Output(output), ws)
            try:
                bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
                bridge.handle({'type': 'stop', 'id': 1})
                events = self.wait_event(output, 'end')
                self.assertTrue(any(e.get('code') == 'final_timeout' for e in events))
                self.assertFalse(any(e['type'] == 'final' for e in events))
                self.assertTrue(ws.socket.closed)
            finally:
                bridge.shutdown()

    def test_audio_backlog_fails_its_session_before_cancelled_end(self):
        output = io.StringIO()
        ws = FakeWebsocket('qwen')
        ws.socket.send = lambda raw: None  # Hold handshake so audio cannot drain.
        bridge = live.Bridge(config('qwen'), live.Output(output), ws)
        try:
            bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
            bridge.sessions[1].audio = queue.Queue(maxsize=1)
            audio = {'type': 'audio', 'id': 1, 'data': base64.b64encode(b'\x00\x00').decode()}
            bridge.handle(audio)
            bridge.handle(audio)
            events = self.wait_event(output, 'end')
            self.assertEqual(events[0]['type'], 'error')
            self.assertEqual(events[0]['id'], 1)
            self.assertEqual(events[0]['code'], 'audio_backlog')
            self.assertEqual(events[-1], {'type': 'end', 'id': 1, 'cancelled': True})
            self.assertFalse(any(e['type'] == 'final' for e in events))
            self.assertTrue(not hasattr(ws, 'kwargs') or ws.socket.closed)
        finally:
            bridge.shutdown()

    def test_pending_session_limit_rejects_only_overflow_with_scoped_error(self):
        output = io.StringIO()
        ws = FakeWebsocket('qwen')
        ws.socket.send = lambda raw: None
        with patch.object(live, 'MAX_SESSIONS', 2):
            bridge = live.Bridge(config('qwen'), live.Output(output), ws)
            try:
                for sid in (1, 2, 3):
                    bridge.handle({'type': 'start', 'id': sid, 'sample_rate': 16000})
                events = self.wait_event(output, 'end', 3)
                self.assertEqual(len(bridge.sessions), 2)
                self.assertLessEqual(bridge.pending.qsize(), 2)
                self.assertEqual(events[0]['type'], 'error')
                self.assertEqual(events[0]['id'], 3)
                self.assertEqual(events[0]['code'], 'audio_backlog')
                self.assertEqual(events[-1], {'type': 'end', 'id': 3, 'cancelled': True})
                self.assertFalse(any(e['id'] in (1, 2) and e['type'] == 'error' for e in events))
            finally:
                bridge.shutdown()

    def test_raw_transport_errors_are_redacted(self):
        ws = FakeWebsocket('openai')
        def fail(*args, **kwargs):
            raise RuntimeError('synthetic-fixture signed URL and request headers')
        ws.create_connection = fail
        output = io.StringIO()
        bridge = live.Bridge(config('openai'), live.Output(output), ws)
        try:
            bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
            deadline = time.monotonic() + 2
            while '"type":"end"' not in output.getvalue() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertIn('"code":"connection"', output.getvalue())
            self.assertNotIn('synthetic-fixture', output.getvalue())
            self.assertNotIn('request headers', output.getvalue())
        finally:
            bridge.shutdown()

    def test_prepare_is_opt_in_and_four_services_prepare_without_audio(self):
        for kind in live.DEFAULTS:
            ws = FreshWebsocket(kind)
            bridge = live.Bridge(config(kind), live.Output(io.StringIO()), ws)
            try:
                time.sleep(.02)
                self.assertEqual(ws.sockets, [], 'Bridge construction must not connect')
                bridge.handle({'type': 'prepare'})
                self.ready_standby(bridge)
                self.assertEqual(len(ws.sockets), 1)
                self.assertEqual(ws.sockets[0].audio, b'')
                bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
                self.wait_until(lambda: bridge.warmup.prepared is None)
                self.assertEqual(len(ws.sockets), 1, 'Every provider must claim its prepared socket')
                bridge.handle({'type': 'cancel', 'id': 1})
            finally:
                bridge.shutdown()

    def test_prepared_connection_is_audio_free_single_use_and_explicitly_refilled(self):
        output, ws = io.StringIO(), FreshWebsocket()
        bridge = live.Bridge(config('openai'), live.Output(output), ws)
        try:
            bridge.handle({'type': 'prepare'})
            prepared = self.ready_standby(bridge)
            for _ in range(20):
                bridge.handle({'type': 'prepare'})
            self.assertEqual(len(ws.sockets), 1)
            self.assertEqual(ws.sockets[0].audio, b'')
            self.assertEqual(output.getvalue(), '', 'Warmup must not claim a live session')
            bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
            self.wait_event(output, 'connected')
            self.assertIsNone(bridge.warmup.prepared)
            self.assertEqual(len(ws.sockets), 1, 'Start must claim rather than redial')
            for sid in (1, 2):
                if sid == 2:
                    bridge.handle({'type': 'prepare'})
                    second = self.ready_standby(bridge)
                    self.assertIsNot(second, prepared)
                    bridge.handle({'type': 'start', 'id': sid, 'sample_rate': 16000})
                bridge.handle({'type': 'audio', 'id': sid, 'data': base64.b64encode(b'\x00\x00' * 3200).decode()})
                bridge.handle({'type': 'stop', 'id': sid})
                self.wait_event(output, 'end', sid)
                self.assertTrue(ws.sockets[sid - 1].closed)
                self.assertEqual(len(ws.sockets), sid, 'Finishing must not implicitly redial')
            events = [json.loads(line) for line in output.getvalue().splitlines()]
            self.assertEqual([(e['id'], e['text']) for e in events if e['type'] == 'final'],
                             [(1, '你好世界'), (2, '你好世界')])
        finally:
            bridge.shutdown()

    def test_start_claims_inflight_prepare_without_second_connection(self):
        output, ws = io.StringIO(), FreshWebsocket()
        ws.gate = threading.Event()
        bridge = live.Bridge(config('openai'), live.Output(output), ws)
        try:
            bridge.handle({'type': 'prepare'})
            self.assertTrue(ws.entered.wait(1))
            bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
            self.wait_until(lambda: bridge.warmup.prepared is None)
            self.assertEqual(len(ws.sockets), 1)
            self.assertEqual(output.getvalue(), '')
            ws.gate.set()
            self.wait_event(output, 'connected')
            self.assertEqual(len(ws.sockets), 1)
            bridge.handle({'type': 'cancel', 'id': 1})
            self.wait_event(output, 'end')
            self.assertTrue(ws.sockets[0].closed)
        finally:
            ws.gate.set()
            bridge.shutdown()

    def test_cancel_claimed_inflight_prepare_closes_late_socket_without_session_leak(self):
        output, ws = io.StringIO(), FreshWebsocket()
        ws.gate = threading.Event()
        bridge = live.Bridge(config('openai'), live.Output(output), ws)
        try:
            bridge.handle({'type': 'prepare'})
            self.assertTrue(ws.entered.wait(1))
            bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
            self.wait_until(lambda: bridge.warmup.prepared is None)
            bridge.handle({'type': 'cancel', 'id': 1})
            events = self.wait_event(output, 'end')
            self.assertEqual(events, [{'type': 'end', 'id': 1, 'cancelled': True}])
            ws.gate.set()
            self.wait_until(lambda: ws.sockets[0].closed)
            bridge.handle({'type': 'start', 'id': 2, 'sample_rate': 16000})
            self.wait_event(output, 'connected', 2)
            bridge.handle({'type': 'cancel', 'id': 2})
            self.wait_event(output, 'end', 2)
            self.assertEqual(len(ws.sockets), 2)
            self.assertEqual(ws.sockets[0].audio, b'')
        finally:
            ws.gate.set()
            bridge.shutdown()

    def test_standby_lease_expires_and_start_reconnects(self):
        output, ws = io.StringIO(), FreshWebsocket()
        with patch.object(live, 'WARM_LEASE', .04):
            bridge = live.Bridge(config('openai'), live.Output(output), ws)
            try:
                bridge.handle({'type': 'prepare'})
                self.ready_standby(bridge)
                self.wait_until(lambda: ws.sockets[0].closed)
                self.assertEqual(len(ws.sockets), 1)
                bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
                self.wait_event(output, 'connected')
                self.assertEqual(len(ws.sockets), 2)
            finally:
                bridge.shutdown()

    def test_standby_absolute_age_is_bounded_while_lease_is_refreshed(self):
        output, ws = io.StringIO(), FreshWebsocket()
        with patch.object(live, 'WARM_MAX_AGE', .06):
            bridge = live.Bridge(config('openai'), live.Output(output), ws)
            try:
                bridge.handle({'type': 'prepare'})
                self.ready_standby(bridge)
                time.sleep(.07)
                bridge.handle({'type': 'prepare'})
                self.wait_until(lambda: len(ws.sockets) >= 2)
                self.assertTrue(ws.sockets[0].closed)
                self.assertTrue(all(not socket.audio for socket in ws.sockets))
            finally:
                bridge.shutdown()

    def test_dead_standby_falls_back_and_background_failures_are_rate_limited(self):
        output, ws = io.StringIO(), FreshWebsocket()
        bridge = live.Bridge(config('openai'), live.Output(output), ws)
        try:
            bridge.handle({'type': 'prepare'})
            first = self.ready_standby(bridge)
            ws.sockets[0].messages.put('')
            self.wait_until(lambda: first.adapter.finished.is_set())
            self.wait_until(lambda: first.closing.is_set())
            for _ in range(20):
                bridge.handle({'type': 'prepare'})
            time.sleep(.03)
            self.assertEqual(len(ws.sockets), 1, 'A dead standby must not retry every prepare')
            self.assertEqual(output.getvalue(), '')
            bridge.handle({'type': 'start', 'id': 1, 'sample_rate': 16000})
            self.wait_event(output, 'connected')
            self.assertEqual(len(ws.sockets), 2, 'An actual start bypasses background retry backoff')
        finally:
            bridge.shutdown()

    def test_failed_prepare_is_silent_and_shutdown_closes_idle_or_inflight_socket(self):
        output, ws = io.StringIO(), FreshWebsocket()
        ws.fail = True
        bridge = live.Bridge(config('openai'), live.Output(output), ws)
        try:
            bridge.handle({'type': 'prepare'})
            self.wait_until(lambda: bridge.warmup.retry_after > time.monotonic())
            for _ in range(20):
                bridge.handle({'type': 'prepare'})
            time.sleep(.03)
            self.assertEqual(len(ws.sockets), 1)
            self.assertEqual(output.getvalue(), '')
        finally:
            bridge.shutdown()
        for inflight in (False, True):
            with self.subTest(inflight=inflight):
                ws = FreshWebsocket()
                ws.gate = threading.Event() if inflight else None
                bridge = live.Bridge(config('openai'), live.Output(io.StringIO()), ws)
                bridge.handle({'type': 'prepare'})
                self.assertTrue(ws.entered.wait(1))
                if not inflight:
                    self.ready_standby(bridge)
                bridge.shutdown()
                if inflight:
                    ws.gate.set()
                self.wait_until(lambda: ws.sockets[0].closed)
                self.assertEqual(ws.sockets[0].audio, b'')


if __name__ == '__main__':
    unittest.main()
