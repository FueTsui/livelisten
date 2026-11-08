"""Stream microphone blocks through the Doubao web speech transport."""
import http.cookiejar
import itertools
import json
import queue
import re
import threading
import time
from urllib.parse import urlencode
import urllib.request

import websocket

from .protocol import ProtocolError, decode_response, encode_request


HOME = 'https://www.doubao.com'
GATEWAY = 'wss://frontier-audio-web-ws.doubao.com/api/v2/sami/voicegenie'
# The application identifier is present in the public web client.
PUBLIC_APP_ID = 'GOqQpfo1fO7slHv8'
USER_AGENT = ('Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 '
              '(KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36')
PACKET_BYTES = 3200
FINAL_MODEL = 'bigasr-acllm-release-grpc-main'
LIVE_MODEL = 'bigasr-acllm-release-grpc-mlg-0'
STANDBY_REFRESH_SECONDS = 12
STANDBY_MAX_AGE = 18


class SpeechError(RuntimeError):
    pass


class Cancelled(SpeechError):
    pass


class ConnectionError(SpeechError):
    pass


def _ensure_running(cancelled):
    if cancelled.is_set():
        raise Cancelled('Dictation was cancelled.')


def _guest_context(timeout):
    opener = urllib.request.build_opener(
        urllib.request.HTTPCookieProcessor(http.cookiejar.CookieJar()))
    request = urllib.request.Request(HOME + '/chat/', headers={
        'User-Agent': USER_AGENT, 'Accept-Language': 'zh-CN,zh;q=0.9'})
    try:
        with opener.open(request, timeout=timeout) as response:
            page = response.read(10 * 1024 * 1024 + 1)
        if len(page) > 10 * 1024 * 1024:
            raise ValueError('oversized response')
        html = page.decode('utf-8')
        marker = re.search(r'window\._ROUTER_DATA\s*=\s*', html)
        if not marker:
            raise ValueError('missing public guest context')
        data, _ = json.JSONDecoder().raw_decode(html[marker.end():])
        layout = data['loaderData']['chat_layout']['chat_layout']
        device = str(layout['launchCore']['web_id'])
        if not device.isascii() or not device.isdecimal() or int(device) <= 0:
            raise ValueError('invalid guest identity')
        version, aid = str(layout['pc_version']), str(layout['aid'])
    except (OSError, ValueError, TypeError, KeyError) as error:
        raise SpeechError('The Doubao guest session could not be initialized.') from error
    return {
        'api_app_key': PUBLIC_APP_ID, 'namespace': 'VoiceGenie', 'version_code': '20800',
        'language': 'zh', 'device_platform': 'web', 'pkg_type': 'release_version',
        'pc_version': version, 'doubao_pc_version': version, 'region': 'CN', 'sys_region': 'CN',
        'samantha_web': '1', 'use-olympus-account': '1', 'doubao_device_platform': 'web',
        'aid': aid, 'real_aid': aid, 'device_id': device, 'web_platform': 'browser',
    }


def _connect(timeout, context=None):
    try:
        if context is None:
            context = _guest_context(timeout)
        return websocket.create_connection(
            GATEWAY + '?' + urlencode(context), origin=HOME, timeout=timeout,
            header={'User-Agent': USER_AGENT})
    except (OSError, SpeechError, websocket.WebSocketException) as error:
        raise ConnectionError('The Doubao speech gateway could not be reached.') from error


class ConnectionWarmup:
    """Keep a short-lived standby connection while dictation is enabled."""

    def __init__(self):
        self._changed = threading.Condition()
        self._next = None
        self._visitor = None
        self._pending = False
        self._closed = False

    def prepare(self):
        with self._changed:
            if self._closed or self._pending or self._usable(STANDBY_MAX_AGE - STANDBY_REFRESH_SECONDS + 1):
                return
            self._pending = True
        threading.Thread(target=self._prepare, daemon=True).start()

    def _usable(self, reserve=0):
        return (self._next is not None and self._next[1].connected
                and self._next[0] > time.monotonic() + reserve)

    def _prepare(self):
        socket = None
        discard = None
        try:
            socket = self.connect(12)
        except SpeechError:
            pass
        finally:
            with self._changed:
                if socket is not None and not self._closed:
                    discard = self._next
                    self._next = time.monotonic() + STANDBY_MAX_AGE, socket
                    socket = None
                self._pending = False
                self._changed.notify_all()
            if discard is not None:
                discard[1].shutdown()
            if socket is not None:
                socket.shutdown()

    def connect(self, timeout):
        with self._changed:
            visitor = self._visitor
        if visitor is None or visitor[0] <= time.monotonic():
            try:
                context = _guest_context(timeout)
            except SpeechError as error:
                raise ConnectionError(str(error)) from error
            with self._changed:
                if self._closed:
                    raise Cancelled('Dictation was disabled.')
                self._visitor = time.monotonic() + 120, context
        else:
            context = visitor[1]
        return _connect(timeout, context)

    def take(self, timeout, cancelled):
        deadline = time.monotonic() + timeout
        with self._changed:
            while self._pending and not self._usable() and not self._closed:
                _ensure_running(cancelled)
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    raise ConnectionError('The standby connection did not respond in time.')
                self._changed.wait(min(remaining, 0.1))
            _ensure_running(cancelled)
            if self._closed:
                raise Cancelled('Dictation was disabled.')
            usable = self._usable()
            entry, self._next = self._next, None
        if usable:
            return entry[1], True
        if entry is not None:
            entry[1].shutdown()
        return self.connect(timeout), False

    def close(self):
        with self._changed:
            self._closed = True
            entry, self._next = self._next, None
            self._visitor = None
            self._changed.notify_all()
        if entry is not None:
            entry[1].shutdown()


def _packets(blocks):
    pending = bytearray()
    for block in blocks:
        if not isinstance(block, bytes) or len(block) % 2:
            raise SpeechError('Invalid PCM16 microphone block.')
        pending.extend(block)
        while len(pending) >= PACKET_BYTES:
            yield bytes(pending[:PACKET_BYTES])
            del pending[:PACKET_BYTES]
    if pending:
        yield bytes(pending)


def transcribe(blocks, cancelled, preview, *, timeout=12, warmup=None,
               on_connected=None, on_final=None):
    """Transmit microphone blocks as they arrive and return only the final text."""
    _ensure_running(cancelled)
    packets = iter(_packets(blocks))
    first = next(packets, None)
    _ensure_running(cancelled)
    if first is None:
        if on_final is not None:
            on_final('')
        return ''
    socket, prepared = warmup.take(timeout, cancelled) if warmup is not None else (_connect(timeout), False)
    try:
        return _stream(socket, first, packets, cancelled, preview, timeout, on_connected, on_final)
    except ConnectionError:
        if not prepared:
            raise
        # The server can expire a standby connection before any audio is sent.
        _ensure_running(cancelled)
        socket = warmup.connect(timeout)
        return _stream(socket, first, packets, cancelled, preview, timeout, on_connected, on_final)


def _stream(socket, first, packets, cancelled, preview, timeout, on_connected, on_final):
    incoming = queue.Queue()
    reader_stopped = threading.Event()
    finals = []
    task_id = ''
    session_started = False
    recognized = False

    def send(event, **kwargs):
        socket.send_binary(encode_request(event, PUBLIC_APP_ID, task_id=task_id, **kwargs))

    def reader():
        while not reader_stopped.is_set():
            try:
                frame = socket.recv()
                if not frame:
                    raise SpeechError('The speech gateway closed the connection.')
                event = decode_response(frame)
                kind = event.get('event')
                if event.get('status_code') not in (None, 20000000) or kind in ('TaskFailed', 'SessionFailed'):
                    raise SpeechError('The Doubao speech gateway rejected the session.')
                if kind == 'ASRResponse':
                    process_response(event)
                    continue
                incoming.put(event)
                if kind == 'SessionFinished':
                    return
            except websocket.WebSocketTimeoutException:
                continue
            except SpeechError as error:
                incoming.put(error)
                return
            except (OSError, websocket.WebSocketException, ProtocolError):
                incoming.put(SpeechError('The speech gateway returned an invalid response.'))
                return

    def process_response(event):
        try:
            payload = event.get('payload', '{}')
            data = json.loads(payload) if isinstance(payload, str) else payload
            for item in data.get('results', []):
                text = item['text']
                if not isinstance(text, str):
                    raise TypeError
                if item.get('is_interim') is False:
                    finals.append(text)
                    preview(''.join(finals))
                elif item.get('is_interim') is True:
                    preview(''.join(finals) + text)
        except (AttributeError, TypeError, KeyError, ValueError) as error:
            raise SpeechError('The speech gateway returned invalid transcript data.') from error

    def wait_for(kind):
        deadline = time.monotonic() + timeout
        while True:
            _ensure_running(cancelled)
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise SpeechError('The speech gateway did not respond in time.')
            try:
                event = incoming.get(timeout=min(remaining, 0.1))
            except queue.Empty:
                continue
            if isinstance(event, SpeechError):
                raise event
            if event.get('event') == kind:
                return event

    worker = threading.Thread(target=reader, daemon=True)
    worker.start()
    try:
        _ensure_running(cancelled)
        send('StartTask')
        task_id = wait_for('TaskStarted').get('task_id')
        if not isinstance(task_id, str) or not task_id:
            raise SpeechError('The speech gateway did not create a task.')
        send('StartSession', payload={
            'business': 1,
            'request_type': 3,
            'enable_audio_input': True,
            'asr': {
                'model': FINAL_MODEL,
                'lang': 'zh',
                'enable_vad': True,
                'enable_punctuation': True,
                'enable_itn': True,
                'audio_src': 1,
                'audio_info': {'channel': 1, 'format': 'pcm', 'sample_rate': 16000},
                'extra': {
                    'use_bigasr_audio_emb': True,
                    'enable_shot_audio_without_vad': True,
                    'enable_asr_twopass': True,
                    'minor_model': LIVE_MODEL,
                    'nonstream_model': FINAL_MODEL,
                },
            },
        })
        wait_for('SessionStarted')
        session_started = True
        if on_connected is not None:
            on_connected()
        for sequence, packet in enumerate(itertools.chain((first,), packets), 1):
            _ensure_running(cancelled)
            while True:
                try:
                    event = incoming.get_nowait()
                except queue.Empty:
                    break
                if isinstance(event, SpeechError):
                    raise event
            send('TaskRequest', audio=packet, sequence=sequence)
            # Bound catch-up bursts when the guest handshake buffered microphone audio.
            if cancelled.wait(0.025):
                _ensure_running(cancelled)
        _ensure_running(cancelled)
        send('EndASR')
        wait_for('ASREnded')
        recognized = True
        text = ''.join(finals)
        if on_final is not None:
            on_final(text)
        try:
            send('FinishSession')
            wait_for('SessionFinished')
        except (SpeechError, OSError, websocket.WebSocketException):
            pass  # The final result is already available.
        return text
    except Cancelled:
        raise
    except (SpeechError, OSError, websocket.WebSocketException, ProtocolError) as error:
        if not session_started:
            raise ConnectionError('The Doubao speech session could not be started.') from error
        if isinstance(error, SpeechError):
            raise
        raise SpeechError('The Doubao speech connection was interrupted.') from error
    finally:
        if session_started and not recognized:
            try:
                send('CancelSession')
            except (OSError, websocket.WebSocketException):
                pass
        reader_stopped.set()
        try:
            socket.close(timeout=1)
        finally:
            worker.join(timeout=2)
