"""Live Listen speech provider for Doubao (protocol v1, JSON lines on stdio).

The app starts this script with ``python -B`` and passes the folder that contains
``doubao_asr`` in ``LIVE_LISTEN_DOUBAO_ROOT``. The app uses one process per audio channel.
Up to two sessions run side by side, so a new part connects while the one before it waits for
its final result; their messages still leave in start order.

stdout carries protocol messages only; everything else goes to stderr.
"""
import base64
import binascii
import importlib
import json
import os
import queue
import sys
import threading
import traceback
from pathlib import Path

PROTOCOL = 1
# Match the core's 90-second reconnect buffer plus live frames while connecting.
MAX_AUDIO_FRAMES = 4096
MAX_SESSIONS = 8
# The part that is finishing plus the one that follows it.
MAX_ACTIVE = 2
CAPABILITIES = {
    'interim': True,
    'final': True,
    'timestamps': False,
    'languages': ['zh'],
}


class Output:
    """Writes one ASCII JSON object per line."""

    def __init__(self, stream):
        self._stream = stream
        self._lock = threading.Lock()

    def send(self, **message):
        line = json.dumps(message, ensure_ascii=True, separators=(',', ':'))
        with self._lock:
            try:
                self._stream.write(line + '\n')
                self._stream.flush()
            except (OSError, ValueError):
                pass  # The app went away; the input loop ends on EOF.


class StableTracker:
    """Counts characters the service has finalized, per recognition stream.

    Doubao marks each result as interim or final. The module reports only joined preview
    text, so finals are counted by observing decoded responses before the module handles
    them. The module decodes a stream's responses and calls its preview on one thread (its
    reader, new for every stream), so a thread-local count keeps parallel sessions apart.
    Python string length equals the Unicode scalar count the protocol uses.
    """

    def __init__(self):
        self._local = threading.local()

    def reset(self):
        self._local.chars = 0

    @property
    def chars(self):
        return getattr(self._local, 'chars', 0)

    def observe(self, event):
        if not isinstance(event, dict) or event.get('event') != 'ASRResponse':
            return
        payload = event.get('payload', '{}')
        data = json.loads(payload) if isinstance(payload, str) else payload
        added = 0
        for item in (data or {}).get('results', []):
            text = item.get('text') if isinstance(item, dict) else None
            if isinstance(text, str) and item.get('is_interim') is False:
                added += len(text)
        if added:
            self._local.chars = self.chars + added


def tap(module, tracker):
    original = module.decode_response

    def decode_response(frame):
        event = original(frame)
        try:
            tracker.observe(event)
        except (AttributeError, TypeError, ValueError):
            pass  # The module validates the response itself.
        return event

    module.decode_response = decode_response


class SessionOrder:
    """Lets sessions run side by side while their messages keep start order.

    The app takes text of a newer session as the end of the one before it, so the messages of
    a session wait until every session started earlier has sent its last message.
    """

    def __init__(self, output):
        self._out = output
        self._lock = threading.Lock()
        self._order = []  # Unfinished session ids, oldest first.
        self._held = {}  # id -> [messages not sent yet, last message seen]

    def open(self, session_id):
        with self._lock:
            if session_id not in self._held:
                self._order.append(session_id)
                self._held[session_id] = [[], False]

    def send(self, session_id, message, last=False):
        with self._lock:
            entry = self._held.get(session_id)
            if entry is None:
                self._out.send(**message)
                return
            waiting = entry[0]
            if message.get('type') == 'text' and waiting and waiting[-1].get('type') == 'text':
                waiting[-1] = message  # Each preview carries the whole text; the newest is enough.
            else:
                waiting.append(message)
            entry[1] = entry[1] or last
            while self._order:
                head = self._held[self._order[0]]
                for held in head[0]:
                    self._out.send(**held)
                head[0].clear()
                if not head[1]:
                    break
                del self._held[self._order.pop(0)]


class Session:
    def __init__(self, session_id):
        self.id = session_id
        self.audio = queue.Queue(maxsize=MAX_AUDIO_FRAMES)
        self.cancelled = threading.Event()
        self.closed = False

    def blocks(self):
        while not self.cancelled.is_set():
            if self.closed and self.audio.empty():
                return
            try:
                block = self.audio.get(timeout=.05)
            except queue.Empty:
                if self.closed:
                    return
                continue
            if block is None:
                return
            if not self.cancelled.is_set():
                yield block

    def close(self):
        if not self.closed:
            self.closed = True
            try:
                self.audio.put_nowait(None)  # Wake an idle reader without adding a polling delay.
            except queue.Full:
                pass  # The reader exits after draining the final audio block.

    def cancel(self):
        self.cancelled.set()
        self.close()
        # Queued cancelled sessions can sit behind a slow handshake. Release their
        # audio immediately, and never feed it to the next recognition request.
        while True:
            try:
                self.audio.get_nowait()
            except queue.Empty:
                return


class Bridge:
    def __init__(self, doubao, output):
        self.doubao = doubao
        self.out = output
        self.tracker = StableTracker()
        tap(doubao, self.tracker)
        self.warmup = doubao.ConnectionWarmup()
        self.order = SessionOrder(output)
        self.sessions = {}
        self.pending = queue.Queue(maxsize=MAX_SESSIONS + 1)  # Shutdown sentinel has a slot.
        self.slots = threading.Semaphore(MAX_ACTIVE)
        self.running = []
        self.worker = threading.Thread(target=self._run, name='doubao-sessions', daemon=True)
        self.worker.start()

    def handle(self, message):
        """Applies one app message; returns False when the provider should exit."""
        kind = message.get('type')
        if kind == 'shutdown':
            return False
        if kind == 'prepare':
            self.warmup.prepare()
            return True
        if kind not in ('start', 'audio', 'stop', 'cancel'):
            return True  # Unknown messages are ignored for forward compatibility.
        session_id = int(message['id'])
        if kind == 'start':
            if session_id not in self.sessions:
                if len(self.sessions) >= MAX_SESSIONS:
                    self.out.send(type='error', id=session_id, code='audio_backlog', message='Speech session queue is full')
                    self.out.send(type='end', id=session_id, cancelled=True)
                    return True
                session = Session(session_id)
                self.sessions[session_id] = session
                self.order.open(session_id)
                self.pending.put_nowait(session)
            return True
        session = self.sessions.get(session_id)
        if session is None:
            return True
        if kind == 'audio':
            if not session.closed:
                data = base64.b64decode(message['data'], validate=True)
                if data:
                    try:
                        session.audio.put_nowait(data)
                    except queue.Full:
                        self.order.send(session_id, {'type': 'error', 'id': session_id, 'code': 'audio_backlog',
                                                     'message': 'Speech audio queue is full'})
                        session.cancel()
        elif kind == 'stop':
            session.close()
        else:
            session.cancel()
        return True

    def shutdown(self):
        for session in list(self.sessions.values()):
            session.cancel()
        self.pending.put(None)
        self.warmup.close()
        self.worker.join(timeout=3)

    def _run(self):
        """Starts queued sessions in arrival order, at most MAX_ACTIVE at a time."""
        while True:
            session = self.pending.get()
            if session is None:
                break
            self.slots.acquire()
            thread = threading.Thread(target=self._session, args=(session,),
                                      name='doubao-session-%d' % session.id, daemon=True)
            self.running = [t for t in self.running if t.is_alive()] + [thread]
            thread.start()
        for thread in self.running:
            thread.join(timeout=3)

    def _session(self, session):
        try:
            self._transcribe(session)
        finally:
            self.sessions.pop(session.id, None)
            self.slots.release()

    def _transcribe(self, session):
        sid = session.id
        doubao = self.doubao

        def send(last=False, **message):
            self.order.send(sid, dict(message, id=sid), last)

        if session.cancelled.is_set():
            send(type='end', cancelled=True, last=True)
            return
        self.tracker.reset()

        def preview(text):
            if isinstance(text, str) and not session.cancelled.is_set():
                send(type='text', text=text, stable=min(self.tracker.chars, len(text)))

        def on_connected():
            if not session.cancelled.is_set():
                send(type='connected')

        def on_final(text):
            if not session.cancelled.is_set():
                send(type='final', text=text if isinstance(text, str) else '')

        try:
            doubao.transcribe(session.blocks(), session.cancelled, preview, warmup=self.warmup,
                              on_connected=on_connected, on_final=on_final)
        except doubao.Cancelled:
            send(type='end', cancelled=True, last=True)
            return
        except doubao.ConnectionError as error:
            send(type='error', code='connection', message=str(error), last=True)
            return
        except doubao.SpeechError as error:
            send(type='error', code='service', message=str(error), last=True)
            return
        except Exception as error:  # Keep the provider alive for the next session.
            traceback.print_exc(file=sys.stderr)
            send(type='error', code='internal', message=type(error).__name__, last=True)
            return
        finally:
            session.close()
        send(type='end', cancelled=False, last=True)


def load_module():
    root = os.environ.get('LIVE_LISTEN_DOUBAO_ROOT') or str(Path(__file__).resolve().parents[2])
    if root not in sys.path:
        sys.path.insert(0, root)
    return importlib.import_module('doubao_asr.doubao')


def serve(doubao, lines, output):
    output.send(type='hello', protocol=PROTOCOL, name='Doubao', capabilities=CAPABILITIES)
    bridge = Bridge(doubao, output)
    try:
        for raw in lines:
            try:
                message = json.loads(raw.decode('utf-8') if isinstance(raw, bytes) else raw)
            except (UnicodeDecodeError, ValueError):
                output.send(type='error', code='bad_message', message='invalid JSON line')
                continue
            if not isinstance(message, dict):
                continue
            try:
                if not bridge.handle(message):
                    break
            except (KeyError, TypeError, ValueError, binascii.Error):
                output.send(type='error', code='bad_message', message='invalid message fields')
    finally:
        bridge.shutdown()


def main():
    output = Output(sys.stdout)
    sys.stdout = sys.stderr  # Stray prints must not corrupt the protocol stream.
    try:
        doubao = load_module()
    except ImportError as error:
        name = getattr(error, 'name', '') or ''
        code = 'module_missing' if name.startswith('doubao_asr') else 'dependency_missing'
        output.send(type='error', code=code, message=name or 'import failed')
        return 3
    serve(doubao, sys.stdin.buffer, output)
    return 0


if __name__ == '__main__':
    sys.exit(main())
