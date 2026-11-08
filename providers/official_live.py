"""Official realtime ASR adapters; application protocol v1 on stdin/stdout.

Uses the existing websocket-client dependency. Configuration and Windows-stored
credentials arrive separately in child-process environment variables, never argv.
No raw HTTP errors, URLs, headers, credentials or audio are written to logs.
"""
import array
import base64
import gzip
import hashlib
import hmac
import json
import os
import queue
import secrets
import struct
import sys
import threading
import time
import uuid
from collections import OrderedDict
from urllib.parse import quote, urlencode, urlsplit

DEFAULTS = {
    'openai': ('wss://api.openai.com/v1/realtime?intent=transcription', 'gpt-live-transcribe'),
    'qwen': ('wss://dashscope.aliyuncs.com/api-ws/v1/realtime', 'qwen3-asr-flash-realtime'),
    'doubao': ('wss://openspeech.bytedance.com/api/v3/sauc/bigmodel_async', 'bigmodel'),
    'tencent': ('wss://asr.cloud.tencent.com/asr/v2/', '16k_zh'),
}
CAPABILITIES = {'interim': True, 'final': True, 'timestamps': False, 'languages': ['zh', 'en']}
FINAL_TIMEOUT = 20
MAX_SESSIONS = 8
# The part that is finishing plus the one that follows it.
MAX_ACTIVE = 2
WARM_LEASE = 15  # Refreshed by the engine's five-second Prepare, never indefinitely.
WARM_MAX_AGE = 120
WARM_SERVICE_MAX_AGE = {'doubao': 4, 'qwen': 30, 'tencent': 4}
WARM_RETRY_DELAY = 30


class ServiceError(Exception):
    """Only a safe predefined code may leave the adapter."""
    def __init__(self, code):
        self.code = code


def require(value, code='official_config_invalid'):
    if not value:
        raise ServiceError(code)


def clean(value):
    require(isinstance(value, str) and '\r' not in value and '\n' not in value)
    return value.strip()


def service_error(kind, event, status=None):
    """Map reviewed machine codes to local messages; never forward remote free text."""
    event = event if isinstance(event, dict) else {}
    error = event.get('error', event)
    error = error if isinstance(error, dict) else {}
    code = error.get('code')
    if kind == 'tencent':
        local = {4001: 'service_parameter', 4002: 'credential_rejected', 4003: 'credential_rejected',
                 4004: 'service_quota', 4005: 'service_quota', 4006: 'service_quota',
                 4007: 'audio_format', 4008: 'connection', 4009: 'connection',
                 4010: 'service_parameter', 6001: 'service_region'}.get(code)
        if local:
            return ServiceError(local)
    elif kind == 'doubao':
        local = {45000001: 'service_parameter', 45000002: 'audio_format',
                 45000081: 'connection', 45000151: 'audio_format'}.get(code)
        if local:
            return ServiceError(local)
    code = code.lower() if isinstance(code, str) else ''
    error_type = error.get('type')
    if code in {'expired_secret_key', 'token_expired', 'expired_token', 'expired_api_key'}:
        return ServiceError('credential_expired')
    if code in {'unsupported_country_region_territory', 'unsupported_country', 'unsupported_region'}:
        return ServiceError('service_region')
    if code in {'invalid_api_key', 'invalidapikey', 'authentication_error', 'invalid_token',
                'accessdenied', 'access_denied', 'permission_denied'} or error_type == 'authentication_error':
        return ServiceError('credential_rejected')
    if code in {'insufficient_quota', 'rate_limit_exceeded', 'arrearage', 'throttling.ratequota',
                'throttling.allocationquota', 'limit_requests', 'limit_tokens'} or error_type == 'rate_limit_error':
        return ServiceError('service_quota')
    if code in {'model_not_found', 'model_not_supported', 'unsupported_model', 'invalid_model'}:
        return ServiceError('service_model')
    if error.get('param') in {'model', 'session.audio.input.transcription.model',
                             'session.input_audio_transcription.model'}:
        return ServiceError('service_model')
    if code in {'invalid_value', 'unknown_parameter', 'missing_required_parameter', 'unsupported_value',
                'invalid_parameter', 'invalidparameter', 'invalid_event', 'input_audio_buffer_commit_empty'}:
        return ServiceError('service_parameter')
    if status in (401, 403):
        return ServiceError('credential_rejected')
    if status == 429:
        return ServiceError('service_quota')
    if status == 400:
        return ServiceError('service_parameter')
    return ServiceError('service')


def transport_error(kind, error):
    if isinstance(error, ServiceError):
        return error
    status = getattr(error, 'status_code', None)
    if status is not None:
        body = getattr(error, 'resp_body', None)
        event = {}
        if isinstance(body, (str, bytes)) and len(body) <= 65536:
            try:
                event = json.loads(body)
            except (ValueError, UnicodeError):
                pass
        return service_error(kind, event, status)
    return ServiceError('connection')


class Config:
    def __init__(self, config, credential):
        self.kind = config.get('providerType', '')
        require(self.kind in DEFAULTS)
        endpoint, model = DEFAULTS[self.kind]
        self.endpoint = clean(config.get('endpoint') or endpoint)
        self.model = clean(config.get('model') or model)
        self.language = clean(config.get('language', 'zh'))
        self.app_id = clean(config.get('appId', ''))
        self.resource_id = clean(config.get('resourceId') or 'volc.bigasr.sauc.duration')
        require(isinstance(credential, dict), 'credential_invalid')
        self.credential = {k: clean(v) for k, v in credential.items() if isinstance(v, str)}
        # Only the reviewed official endpoints may receive stored credentials.
        url = urlsplit(self.endpoint)
        require(url.scheme == 'wss' and not url.username and not url.password
                and not url.fragment and url.port in (None, 443))
        allowed = {
            'openai': {'api.openai.com'},
            'qwen': {'dashscope.aliyuncs.com', 'dashscope-intl.aliyuncs.com'},
            'doubao': {'openspeech.bytedance.com'},
            'tencent': {'asr.cloud.tencent.com'},
        }
        require(url.hostname in allowed[self.kind])
        if self.kind == 'openai':
            require(url.path == '/v1/realtime' and url.query in ('', 'intent=transcription'))
            self.endpoint = 'wss://api.openai.com/v1/realtime?intent=transcription'
            require(self.credential.get('apiKey'), 'credential_missing')
        elif self.kind == 'qwen':
            require(url.path == '/api-ws/v1/realtime' and not url.query)
            require(self.model.startswith('qwen3-asr-flash-realtime'))
            require(self.credential.get('apiKey'), 'credential_missing')
        elif self.kind == 'doubao':
            require(url.path in ('/api/v3/sauc/bigmodel', '/api/v3/sauc/bigmodel_async') and not url.query)
            require(self.credential.get('apiKey') or (self.app_id and self.credential.get('accessKey')),
                    'credential_missing')
        else:
            require(self.app_id.isdecimal())
            require(url.path in ('/asr/v2/', '/asr/v2/' + self.app_id) and not url.query)
            require(self.model.startswith('16k_') or self.model == 'Hy-ASR-3.0-preview')
            require(self.credential.get('secretId') and self.credential.get('secretKey'), 'credential_missing')


class Output:
    def __init__(self, stream):
        self.stream = stream
        self.lock = threading.Lock()

    def send(self, **message):
        with self.lock:
            self.stream.write(json.dumps(message, ensure_ascii=True, separators=(',', ':')) + '\n')
            self.stream.flush()


class Resample24k:
    """Streaming linear PCM16 16-to-24 kHz conversion, invariant to packet boundaries.

    Output j lies at ``position + 2j`` in thirds of an input sample. Outputs three apart share
    the same fraction and step two input samples, so each of the three phases is computed
    from slices in one pass instead of sample by sample.
    """
    def __init__(self):
        self.samples = []
        self.position = 0
        self.input_count = 0
        self.output_count = 0

    def feed(self, pcm, final=False):
        require(len(pcm) % 2 == 0, 'audio_format')
        incoming = array.array('h')
        incoming.frombytes(pcm)
        if sys.byteorder != 'little':
            incoming.byteswap()
        self.input_count += len(incoming)
        self.samples.extend(incoming)
        if final and self.samples:
            self.samples.append(self.samples[-1])
        samples = self.samples
        # Each output needs the sample after its position; never exceed 1.5 outputs per input.
        count = max(0, min((3 * (len(samples) - 1) - self.position + 1) // 2,
                           self.input_count * 3 // 2 - self.output_count))
        values = [0] * count
        for phase in range(min(3, count)):
            index, fraction = divmod(self.position + 2 * phase, 3)
            n = len(range(phase, count, 3))
            left = samples[index:index + 2 * n:2]
            if fraction == 0:
                values[phase::3] = left
            else:
                right = samples[index + 1:index + 2 * n:2]
                values[phase::3] = [round((a * (3 - fraction) + b * fraction) / 3) for a, b in zip(left, right)]
        out = array.array('h', values)
        self.position += 2 * count
        self.output_count += count
        consumed = self.position // 3
        self.samples = self.samples[consumed:]
        self.position -= consumed * 3
        if sys.byteorder != 'little':
            out.byteswap()
        return out.tobytes()


class Transcript:
    def __init__(self):
        self.items = OrderedDict()

    def reserve(self, key, previous=None):
        if key in self.items:
            return
        self.items[key] = ('', 0)
        if previous in self.items:
            entries = list(self.items.items())
            current = entries.pop()
            entries.insert(next(i for i, (k, _) in enumerate(entries) if k == previous) + 1, current)
            self.items = OrderedDict(entries)

    def update(self, key, text, stable=0):
        self.reserve(key)
        self.items[key] = (text, max(0, min(len(text), stable)))
        return self.value()

    def value(self):
        text = ''.join(item[0] for item in self.items.values())
        stable = 0
        for part, count in self.items.values():
            stable += count
            if not part or count != len(part):
                break
        return text, stable


def doubao_packet(payload, audio=False, final=False):
    data = gzip.compress(payload)
    return bytes((0x11, (0x20 if audio else 0x10) | (2 if final else 0),
                  0x01 if audio else 0x11, 0)) + struct.pack('>I', len(data)) + data


def doubao_response(frame):
    require(isinstance(frame, bytes) and len(frame) >= 8, 'protocol')
    require(frame[0] >> 4 == 1, 'protocol')
    offset = (frame[0] & 15) * 4
    kind, flags = frame[1] >> 4, frame[1] & 15
    require(offset >= 4 and kind in (9, 15), 'protocol')
    if kind == 15:
        require(len(frame) >= offset + 8, 'protocol')
        raise service_error('doubao', {'code': struct.unpack_from('>I', frame, offset)[0]})
    if flags & 1:
        offset += 4
    require(len(frame) >= offset + 4, 'protocol')
    length = struct.unpack_from('>I', frame, offset)[0]
    payload = frame[offset + 4:]
    require(len(payload) == length and length <= (1 << 22), 'protocol')
    if frame[2] & 15 == 1:
        payload = gzip.decompress(payload)
    require(len(payload) <= (1 << 23), 'protocol')
    return json.loads(payload), bool(flags & 2)


def tencent_url(config, now=None, nonce=None, voice_id=None):
    now = int(time.time()) if now is None else now
    params = {
        'engine_model_type': config.model, 'expired': now + 3600,
        'needvad': 1, 'nonce': secrets.randbelow(2**31 - 1) + 1 if nonce is None else nonce,
        'secretid': config.credential['secretId'], 'timestamp': now,
        'voice_format': 1, 'voice_id': voice_id or str(uuid.uuid4()),
    }
    path = 'asr.cloud.tencent.com/asr/v2/' + config.app_id
    canonical = path + '?' + '&'.join(f'{key}={params[key]}' for key in sorted(params))
    signature = base64.b64encode(hmac.new(config.credential['secretKey'].encode(), canonical.encode(), hashlib.sha1).digest()).decode()
    return 'wss://' + path + '?' + urlencode(sorted(params.items()), quote_via=quote) + '&signature=' + quote(signature, safe='')


class Adapter:
    def __init__(self, config, emit):
        self.config = config
        self.emit = emit
        self.transcript = Transcript()
        self.resampler = Resample24k()
        self.connected = threading.Event()
        self.finished = threading.Event()
        self.stopped = threading.Event()
        self.error = None
        self.socket = None
        self.sent_samples = 0

    def connection(self):
        c = self.config
        if c.kind in ('openai', 'qwen'):
            endpoint = c.endpoint if c.kind == 'openai' else c.endpoint + '?model=' + quote(c.model, safe='')
            return endpoint, {'Authorization': 'Bearer ' + c.credential['apiKey']}
        if c.kind == 'tencent':
            return tencent_url(c), {}
        request_id = str(uuid.uuid4())
        headers = {'X-Api-Resource-Id': c.resource_id, 'X-Api-Connect-Id': request_id,
                   'X-Api-Request-Id': request_id}
        if c.credential.get('apiKey'):
            headers['X-Api-Key'] = c.credential['apiKey']
        else:
            headers.update({'X-Api-App-Key': c.app_id, 'X-Api-Access-Key': c.credential['accessKey']})
        return c.endpoint, headers

    def json(self, message):
        if self.config.kind == 'qwen':
            message['event_id'] = 'event_' + uuid.uuid4().hex
        self.socket.send(json.dumps(message, separators=(',', ':')))

    def begin(self):
        c = self.config
        if c.kind == 'openai':
            transcription = {'model': c.model}
            if c.language:
                if c.model == 'gpt-live-transcribe':
                    transcription['languages'] = [c.language]
                else:
                    transcription['language'] = c.language
            self.json({'type': 'session.update', 'session': {'type': 'transcription', 'audio': {'input': {
                'format': {'type': 'audio/pcm', 'rate': 24000},
                'transcription': transcription, 'turn_detection': None}}}})
        elif c.kind == 'qwen':
            self.json({'type': 'session.update', 'session': {
                'input_audio_format': 'pcm', 'sample_rate': 16000,
                'input_audio_transcription': {'language': c.language} if c.language else {},
                'turn_detection': {'type': 'server_vad', 'threshold': 0.0, 'silence_duration_ms': 800}}})
        elif c.kind == 'doubao':
            request = {'user': {'uid': 'live-listen'},
                       'audio': {'format': 'pcm', 'rate': 16000, 'bits': 16, 'channel': 1},
                       'request': {'model_name': c.model, 'enable_itn': True, 'enable_punc': True,
                                   'show_utterances': True, 'enable_nonstream': True}}
            self.socket.send_binary(doubao_packet(json.dumps(request).encode()))

    def audio(self, data):
        require(len(data) % 2 == 0, 'audio_format')
        self.sent_samples += len(data) // 2
        kind = self.config.kind
        if kind == 'openai':
            data = self.resampler.feed(data)
        if kind in ('openai', 'qwen'):
            if data:
                self.json({'type': 'input_audio_buffer.append', 'audio': base64.b64encode(data).decode()})
        elif kind == 'doubao':
            self.socket.send_binary(doubao_packet(data, audio=True))
        else:
            self.socket.send_binary(data)

    def stop(self):
        self.stopped.set()
        kind = self.config.kind
        if kind == 'openai':
            tail = self.resampler.feed(b'', final=True)
            if tail:
                self.json({'type': 'input_audio_buffer.append', 'audio': base64.b64encode(tail).decode()})
            # OpenAI commits require at least 100 ms of audio.
            if self.sent_samples < 1600:
                self.finished.set()
            else:
                self.json({'type': 'input_audio_buffer.commit'})
        elif kind == 'qwen':
            self.json({'type': 'session.finish'})
        elif kind == 'doubao':
            self.socket.send_binary(doubao_packet(b'', audio=True, final=True))
        else:
            self.json({'type': 'end'})

    def result(self, key, text, stable):
        text, stable = self.transcript.update(key, text, stable)
        self.emit(type='text', text=text, stable=stable)

    def receive(self, raw):
        kind = self.config.kind
        if kind == 'doubao':
            event, final = doubao_response(raw)
            if event.get('code', 0) not in (0, 20000000):
                raise service_error(kind, event)
            self.connected.set()
            result = event.get('result') or {}
            if isinstance(result, list):
                result = result[0] if result else {}
            text = result.get('text', '')
            prefix = ''
            for utterance in result.get('utterances', []):
                if not utterance.get('definite'):
                    break
                prefix += utterance.get('text', '')
            stable = len(prefix) if text.startswith(prefix) else 0
            self.result('all', text, len(text) if final else stable)
            if final:
                self.finished.set()
            return
        event = json.loads(raw)
        if kind == 'tencent':
            if event.get('code', 0) != 0:
                raise service_error(kind, event)
            self.connected.set()
            result = event.get('result')
            if result:
                text = result.get('voice_text_str', '')
                index = result.get('index', 0)
                self.result(index, text, len(text) if result.get('slice_type') == 2 else 0)
            if event.get('final') == 1:
                self.finished.set()
            return
        event_type = event.get('type', '')
        if event_type in ('error', 'conversation.item.input_audio_transcription.failed'):
            raise service_error(kind, event)
        if event_type == 'session.updated':
            self.connected.set()
        elif event_type in ('input_audio_buffer.committed', 'conversation.item.created', 'conversation.item.added'):
            key = event.get('item_id') or event.get('item', {}).get('id')
            if key:
                self.transcript.reserve(key, event.get('previous_item_id'))
        elif event_type == 'conversation.item.input_audio_transcription.delta':
            key = event.get('item_id', 'all')
            text = self.transcript.items.get(key, ('', 0))[0] + event.get('delta', '')
            self.result(key, text, 0)
        elif event_type == 'conversation.item.input_audio_transcription.text':
            text = event.get('text', '')
            self.result(event.get('item_id', 'all'), text + event.get('stash', ''), len(text))
        elif event_type == 'conversation.item.input_audio_transcription.completed':
            text = event.get('transcript', '')
            self.result(event.get('item_id', 'all'), text, len(text))
            if kind == 'openai' and self.stopped.is_set():
                self.finished.set()
        elif event_type == 'session.finished':
            self.finished.set()


class SessionOrder:
    """Lets sessions run side by side while their messages keep start order.

    The app takes text of a newer session as the end of the one before it, so the messages of
    a session wait until every session started earlier has sent its last message.
    """
    def __init__(self, output):
        self.out = output
        self.lock = threading.Lock()
        self.order = []  # Unfinished session ids, oldest first.
        self.held = {}  # id -> [messages not sent yet, last message seen]

    def open(self, sid):
        with self.lock:
            if sid not in self.held:
                self.order.append(sid)
                self.held[sid] = [[], False]

    def send(self, sid, message, last=False):
        with self.lock:
            entry = self.held.get(sid)
            if entry is None:
                self.out.send(**message)
                return
            waiting = entry[0]
            if message.get('type') == 'text' and waiting and waiting[-1].get('type') == 'text':
                waiting[-1] = message  # Each text carries the whole transcript; the newest is enough.
            else:
                waiting.append(message)
            entry[1] = entry[1] or last
            while self.order:
                head = self.held[self.order[0]]
                for held in head[0]:
                    self.out.send(**held)
                head[0].clear()
                if not head[1]:
                    break
                del self.held[self.order.pop(0)]


class Session:
    def __init__(self, sid):
        self.id = sid
        self.audio = queue.Queue(maxsize=4096)
        self.cancelled = threading.Event()
        self.closed = False

    def close(self):
        self.closed = True


class Connection:
    """One configured socket, consumed by exactly one recognition session."""
    def __init__(self, config, websocket):
        self.config, self.websocket = config, websocket
        self.adapter = Adapter(config, lambda **_: None)
        self.created = time.monotonic()
        self.opened_at = None
        self.ready = threading.Event()
        self.closing = threading.Event()
        self.receiver = None

    def open(self, cancelled=None):
        adapter = self.adapter
        try:
            endpoint, headers = adapter.connection()
            adapter.socket = self.websocket.create_connection(endpoint, header=headers, timeout=10,
                                                              redirect_limit=0, enable_multithread=True)
            self.opened_at = time.monotonic()
            if self.closing.is_set():
                return
            adapter.socket.settimeout(.25)
            receiver = threading.Thread(target=self.read, daemon=True)
            receiver.start()
            # close() can run from the warmup owner while this thread is opening.
            # Publish only a started thread, otherwise join() can raise mid-shutdown.
            self.receiver = receiver
            if self.closing.is_set():
                return
            adapter.begin()
            deadline = time.monotonic() + 12
            while not adapter.connected.wait(.05):
                if self.closing.is_set() or (cancelled is not None and cancelled.is_set()):
                    self.close()
                    return
                require(not adapter.finished.is_set(), adapter.error or 'connection')
                require(time.monotonic() < deadline, 'connection')
            require(not adapter.finished.is_set(), adapter.error or 'connection')
        except Exception as error:
            adapter.error = transport_error(self.config.kind, error).code
            adapter.finished.set()
        finally:
            self.ready.set()
            if self.closing.is_set() or adapter.error:
                self.close()

    def read(self):
        adapter = self.adapter
        while not self.closing.is_set() and not adapter.finished.is_set():
            try:
                raw = adapter.socket.recv()
                if raw in ('', b''):
                    raise ServiceError('connection')
                adapter.receive(raw)
            except self.websocket.WebSocketTimeoutException:
                continue
            except Exception as error:
                adapter.error = transport_error(self.config.kind, error).code
                adapter.finished.set()
                return

    def close(self):
        self.closing.set()
        if self.adapter.socket is not None:
            try:
                self.adapter.socket.close(timeout=0)
            except Exception:
                pass
        if self.receiver is not None and self.receiver is not threading.current_thread():
            self.receiver.join(timeout=1)


class ConnectionWarmup:
    """One unused, audio-free connection with a renewable lease and bounded age."""
    def __init__(self, config, websocket):
        self.config, self.websocket = config, websocket
        self.condition = threading.Condition()
        self.prepared = None
        self.wanted_until = 0
        self.retry_after = 0
        self.closed = False
        self.worker = threading.Thread(target=self.run, daemon=True)
        self.worker.start()

    def prepare(self):
        with self.condition:
            if not self.closed:
                self.wanted_until = time.monotonic() + WARM_LEASE
                self.condition.notify()

    def expired(self, connection, now):
        maximum = min(WARM_MAX_AGE, WARM_SERVICE_MAX_AGE.get(self.config.kind, WARM_MAX_AGE))
        return now - (connection.opened_at or connection.created) >= maximum

    def take(self):
        with self.condition:
            now = time.monotonic()
            connection = self.prepared
            lease_expired = now >= self.wanted_until
            self.prepared = None
            self.wanted_until = 0  # Refill only after another explicit Prepare.
            self.condition.notify()
        if connection is not None:
            if (lease_expired or connection.closing.is_set() or connection.adapter.finished.is_set()
                    or self.expired(connection, now) or connection.adapter.sent_samples
                    or any(text for text, _ in connection.adapter.transcript.items.values())):
                connection.close()
                return None
        return connection

    def run(self):
        while True:
            discard = opening = None
            with self.condition:
                if self.closed:
                    return
                now = time.monotonic()
                current = self.prepared
                if current is not None and (now >= self.wanted_until
                        or self.expired(current, now) or current.adapter.finished.is_set()):
                    discard, self.prepared = current, None
                    if current.adapter.error:
                        self.retry_after = now + WARM_RETRY_DELAY
                if self.prepared is None and now < self.wanted_until and now >= self.retry_after:
                    opening = self.prepared = Connection(self.config, self.websocket)
                if discard is None and opening is None:
                    self.condition.wait(.1)
                    continue
            if discard is not None:
                discard.close()
            if opening is not None:
                # This single worker also owns in-flight preparation. take() may claim
                # it while opening; the session then waits on ready instead of redialing.
                opening.open()

    def close(self):
        with self.condition:
            self.closed = True
            connection, self.prepared = self.prepared, None
            self.wanted_until = 0
            self.condition.notify()
        if connection is not None:
            connection.close()
        self.worker.join(timeout=1)


class Bridge:
    def __init__(self, config, output, websocket):
        self.config, self.out, self.websocket = config, output, websocket
        self.order = SessionOrder(output)
        self.sessions = {}
        self.warmup = ConnectionWarmup(config, websocket)
        self.pending = queue.Queue(maxsize=MAX_SESSIONS + 1)  # Includes shutdown sentinel.
        self.slots = threading.Semaphore(MAX_ACTIVE)
        self.running = []
        self.worker = threading.Thread(target=self.run, daemon=True)
        self.worker.start()

    def handle(self, message):
        kind = message.get('type')
        if kind == 'shutdown':
            return False
        if kind == 'prepare':
            if self.warmup is not None:
                self.warmup.prepare()
            return True
        if kind not in ('start', 'audio', 'stop', 'cancel'):
            return True
        sid = int(message['id'])
        if kind == 'start':
            require(message.get('sample_rate') == 16000, 'audio_format')
            if sid not in self.sessions:
                if len(self.sessions) >= MAX_SESSIONS:
                    self.out.send(type='error', id=sid, code='audio_backlog', message='Speech session queue is full')
                    self.out.send(type='end', id=sid, cancelled=True)
                    return True
                session = self.sessions[sid] = Session(sid)
                self.order.open(sid)
                self.pending.put_nowait(session)
            return True
        session = self.sessions.get(sid)
        if session is None:
            return True
        if kind == 'audio' and not session.closed:
            data = base64.b64decode(message['data'], validate=True)
            require(len(data) % 2 == 0, 'audio_format')
            try:
                session.audio.put_nowait(data)
            except queue.Full:
                # The engine must fail this session before it receives a cancelled End.
                # An unscoped error only shows a notification and leaves capture active.
                self.order.send(sid, {'type': 'error', 'id': sid, 'code': 'audio_backlog',
                                      'message': 'Speech audio queue is full'})
                session.cancelled.set()
                session.close()
        elif kind == 'stop':
            session.close()
        elif kind == 'cancel':
            session.cancelled.set()
            session.close()
        return True

    def run(self):
        """Starts queued sessions in arrival order, at most MAX_ACTIVE at a time."""
        while True:
            session = self.pending.get()
            if session is None:
                break
            self.slots.acquire()
            thread = threading.Thread(target=self.session, args=(session,), daemon=True)
            self.running = [t for t in self.running if t.is_alive()] + [thread]
            thread.start()
        for thread in self.running:
            thread.join(timeout=2)

    def session(self, session):
        try:
            self.transcribe(session)
        finally:
            self.sessions.pop(session.id, None)
            self.slots.release()

    def transcribe(self, session):
        sid = session.id
        connection = None

        def emit(last=False, **message):
            self.order.send(sid, dict(message, id=sid), last)

        try:
            if session.cancelled.is_set():
                return
            connection = self.warmup.take() if self.warmup is not None else None
            if connection is None:
                connection = Connection(self.config, self.websocket)
                connection.adapter.emit = emit
                connection.open(session.cancelled)
            else:
                connection.adapter.emit = emit
            adapter = connection.adapter
            while not connection.ready.wait(.05):
                if session.cancelled.is_set():
                    return
            if session.cancelled.is_set():
                return
            require(adapter.connected.is_set() and not adapter.finished.is_set(), adapter.error or 'connection')
            emit(type='connected')
            buffered = bytearray()
            packet_size = 6400 if self.config.kind == 'tencent' else 3200 if self.config.kind == 'doubao' else 1280
            started = time.monotonic()
            sent_bytes = 0

            def send(data):
                nonlocal sent_bytes
                if self.config.kind == 'tencent':
                    wait = started + sent_bytes / 32000 - time.monotonic()
                    if wait > 0 and session.cancelled.wait(wait):
                        return
                adapter.audio(data)
                sent_bytes += len(data)

            while not session.cancelled.is_set():
                require(not adapter.finished.is_set(), adapter.error or 'connection')
                try:
                    buffered.extend(session.audio.get(timeout=.05))
                except queue.Empty:
                    if session.closed:
                        break
                while len(buffered) >= packet_size:
                    send(bytes(buffered[:packet_size]))
                    del buffered[:packet_size]
            if session.cancelled.is_set():
                return
            if buffered:
                send(bytes(buffered))
            adapter.stop()
            deadline = time.monotonic() + FINAL_TIMEOUT
            while not adapter.finished.wait(.05):
                if session.cancelled.is_set():
                    return
                require(time.monotonic() < deadline, 'final_timeout')
            require(not adapter.error, adapter.error or 'connection')
            emit(type='final', text=adapter.transcript.value()[0])
        except Exception as error:
            if not session.cancelled.is_set():
                code = transport_error(self.config.kind, error).code
                emit(type='error', code=code, message='Official speech service request failed')
        finally:
            if connection is not None:
                connection.close()
            emit(type='end', cancelled=session.cancelled.is_set(), last=True)

    def shutdown(self):
        for session in list(self.sessions.values()):
            session.cancelled.set()
            session.close()
        self.pending.put(None)
        if self.warmup is not None:
            self.warmup.close()
        self.worker.join(timeout=2)


def main():
    output = Output(sys.stdout)
    try:
        raw_config = json.loads(os.environ.pop('LIVE_LISTEN_OFFICIAL_CONFIG', '{}'))
        raw_credential = json.loads(os.environ.pop('LIVE_LISTEN_OFFICIAL_CREDENTIAL', '{}'))
        config = Config(raw_config, raw_credential)
        try:
            import websocket
        except ImportError:
            raise ServiceError('websocket_missing') from None
        output.send(type='hello', protocol=1, name=config.kind, capabilities=CAPABILITIES)
        bridge = Bridge(config, output, websocket)
    except Exception as error:
        output.send(type='error', code=error.code if isinstance(error, ServiceError) else 'credential_invalid',
                    message='Official speech service configuration is incomplete')
        return 1
    try:
        for line in sys.stdin:
            try:
                if not bridge.handle(json.loads(line)):
                    break
            except Exception as error:
                output.send(type='error', code=error.code if isinstance(error, ServiceError) else 'protocol',
                            message='Invalid speech protocol message')
    finally:
        bridge.shutdown()
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
