"""Offline tests for the Doubao provider bridge, using a fake doubao module."""
import base64
import io
import json
import queue
import sys
import threading
import time
import types
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))

import doubao_live  # noqa: E402


class Collector(io.StringIO):
    def lines(self):
        return [json.loads(line) for line in self.getvalue().splitlines() if line]


def fake_module(behavior):
    module = types.SimpleNamespace()

    class SpeechError(RuntimeError):
        pass

    class Cancelled(SpeechError):
        pass

    class ConnectionError(SpeechError):
        pass

    class ConnectionWarmup:
        prepared = 0
        closed = False

        def prepare(self):
            ConnectionWarmup.prepared += 1

        def close(self):
            ConnectionWarmup.closed = True

    def decode_response(frame):
        return frame

    def transcribe(blocks, cancelled, preview, *, warmup=None, on_connected=None, on_final=None, timeout=12):
        return behavior(module, blocks, cancelled, preview, on_connected, on_final)

    module.SpeechError = SpeechError
    module.Cancelled = Cancelled
    module.ConnectionError = ConnectionError
    module.ConnectionWarmup = ConnectionWarmup
    module.decode_response = decode_response
    module.transcribe = transcribe
    return module


def response(*results):
    return {'event': 'ASRResponse', 'payload': json.dumps({'results': list(results)})}


def streaming(module, blocks, cancelled, preview, on_connected, on_final):
    received = b''.join(blocks)
    if cancelled.is_set():
        raise module.Cancelled('cancelled')
    on_connected()
    module.decode_response(response({'text': '你好', 'is_interim': True}))
    preview('你好')
    module.decode_response(response({'text': '你好，', 'is_interim': False}))
    preview('你好，')
    module.decode_response(response({'text': '世界', 'is_interim': True}))
    preview('你好，世界')
    text = '你好，世界。' if received else ''
    on_final(text)
    return text


def wait_for(collector, count, timeout=3):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        lines = collector.lines()
        if len(lines) >= count:
            return lines
        time.sleep(0.01)
    return collector.lines()


def audio(session, data=b'\x01\x00' * 160):
    return json.dumps({'type': 'audio', 'id': session, 'data': base64.b64encode(data).decode()})


class BridgeTests(unittest.TestCase):
    def test_audio_is_streamed_before_stop_and_stop_keeps_final_order(self):
        def live(module, blocks, cancelled, preview, on_connected, on_final):
            packets = iter(blocks)
            self.assertEqual(next(packets), b'\x01\x00' * 160)
            on_connected()
            preview('实时')
            list(packets)
            on_final('实时。')

        out = Collector()
        bridge = doubao_live.Bridge(fake_module(live), doubao_live.Output(out))
        try:
            bridge.handle({'type': 'start', 'id': 1})
            bridge.handle(json.loads(audio(1)))
            got = wait_for(out, 2)
            self.assertEqual([m['type'] for m in got], ['connected', 'text'])
            self.assertFalse(bridge.sessions[1].closed)
            bridge.handle({'type': 'stop', 'id': 1})
            got = wait_for(out, 4)
            self.assertEqual([m['type'] for m in got], ['connected', 'text', 'final', 'end'])
        finally:
            bridge.shutdown()
        self.assertFalse(bridge.worker.is_alive())

    def test_audio_backlog_is_scoped_cancelled_and_releases_buffer(self):
        entered, release = threading.Event(), threading.Event()

        def slow(module, blocks, cancelled, preview, on_connected, on_final):
            entered.set()
            release.wait(2)
            return streaming(module, blocks, cancelled, preview, on_connected, on_final)

        out = Collector()
        bridge = doubao_live.Bridge(fake_module(slow), doubao_live.Output(out))
        try:
            bridge.handle({'type': 'start', 'id': 3})
            self.assertTrue(entered.wait(1))
            session = bridge.sessions[3]
            session.audio = queue.Queue(maxsize=2)
            for _ in range(3):
                bridge.handle(json.loads(audio(3)))
            self.assertEqual(out.lines()[0]['code'], 'audio_backlog')
            self.assertEqual(out.lines()[0]['id'], 3)
            self.assertTrue(session.cancelled.is_set())
            self.assertTrue(session.audio.empty())
            release.set()
            got = wait_for(out, 2)
            self.assertEqual([m['type'] for m in got], ['error', 'end'])
            self.assertTrue(got[-1]['cancelled'])
        finally:
            release.set()
            bridge.shutdown()

    def test_pending_sessions_are_bounded_and_overflow_does_not_block_controls(self):
        entered, release = threading.Event(), threading.Event()

        def slow(module, blocks, cancelled, preview, on_connected, on_final):
            entered.set()
            release.wait(2)
            if cancelled.is_set():
                raise module.Cancelled('cancelled')

        out = Collector()
        bridge = doubao_live.Bridge(fake_module(slow), doubao_live.Output(out))
        try:
            bridge.handle({'type': 'start', 'id': 1})
            self.assertTrue(entered.wait(1))
            for sid in range(2, doubao_live.MAX_SESSIONS + 2):
                bridge.handle({'type': 'start', 'id': sid})
            self.assertEqual(len(bridge.sessions), doubao_live.MAX_SESSIONS)
            self.assertLessEqual(bridge.pending.qsize(), doubao_live.MAX_SESSIONS)
            got = out.lines()
            self.assertEqual(got[0]['code'], 'audio_backlog')
            self.assertEqual(got[0]['id'], doubao_live.MAX_SESSIONS + 1)
            for sid in list(bridge.sessions):
                bridge.handle({'type': 'cancel', 'id': sid})
            release.set()
        finally:
            release.set()
            bridge.shutdown()
        self.assertFalse(bridge.worker.is_alive())
        self.assertFalse(bridge.sessions)

    def test_cancel_empty_reader_wakes_and_full_stop_preserves_all_audio(self):
        session = doubao_live.Session(1)
        result = []
        consumer = threading.Thread(target=lambda: result.extend(session.blocks()))
        consumer.start()
        session.cancel()
        consumer.join(1)
        self.assertFalse(consumer.is_alive())
        self.assertEqual(result, [])
        session = doubao_live.Session(2)
        session.audio = queue.Queue(maxsize=2)
        session.audio.put(b'one')
        session.audio.put(b'two')
        session.close()  # A full audio queue must not block the control reader.
        self.assertEqual(list(session.blocks()), [b'one', b'two'])

    def run_lines(self, behavior, lines, expect):
        out = Collector()
        module = fake_module(behavior)

        def feed():
            # Keep stdin open until the expected replies arrive; EOF cancels open sessions.
            yield from lines
            wait_for(out, expect)

        thread = threading.Thread(target=doubao_live.serve,
                                  args=(module, feed(), doubao_live.Output(out)))
        thread.start()
        got = wait_for(out, expect)
        thread.join(timeout=5)
        return got, module

    def test_session_reports_stable_prefix_final_and_end(self):
        lines = [json.dumps({'type': 'start', 'id': 7, 'sample_rate': 16000, 'language': 'zh-CN'}),
                 audio(7), audio(7), json.dumps({'type': 'stop', 'id': 7})]
        got, _ = self.run_lines(streaming, lines, 7)
        self.assertEqual(got[0]['type'], 'hello')
        self.assertEqual(got[0]['protocol'], 1)
        kinds = [m['type'] for m in got[1:]]
        self.assertEqual(kinds, ['connected', 'text', 'text', 'text', 'final', 'end'])
        texts = [(m['text'], m['stable']) for m in got if m['type'] == 'text']
        self.assertEqual(texts, [('你好', 0), ('你好，', 3), ('你好，世界', 3)])
        self.assertEqual(got[5]['text'], '你好，世界。')
        self.assertEqual(got[6], {'type': 'end', 'id': 7, 'cancelled': False})

    def test_cancel_before_audio_ends_cancelled(self):
        lines = [json.dumps({'type': 'start', 'id': 1}), json.dumps({'type': 'cancel', 'id': 1})]
        got, _ = self.run_lines(streaming, lines, 2)
        self.assertIn({'type': 'end', 'id': 1, 'cancelled': True}, got)
        self.assertNotIn('final', [m['type'] for m in got])

    def test_connection_failure_is_reported_with_its_code(self):
        def failing(module, blocks, cancelled, preview, on_connected, on_final):
            list(blocks)
            raise module.ConnectionError('gateway unreachable')

        lines = [json.dumps({'type': 'start', 'id': 3}), audio(3), json.dumps({'type': 'stop', 'id': 3})]
        got, _ = self.run_lines(failing, lines, 2)
        self.assertEqual(got[1]['type'], 'error')
        self.assertEqual(got[1]['id'], 3)
        self.assertEqual(got[1]['code'], 'connection')

    def test_bad_lines_are_reported_and_ignored(self):
        lines = ['not json', json.dumps({'type': 'audio', 'id': 'x', 'data': '%%%'}),
                 json.dumps({'type': 'later_feature'}), json.dumps({'type': 'prepare'})]
        got, module = self.run_lines(streaming, lines, 3)
        codes = [m.get('code') for m in got if m['type'] == 'error']
        self.assertEqual(codes, ['bad_message', 'bad_message'])
        self.assertEqual(module.ConnectionWarmup.prepared, 1)
        self.assertTrue(module.ConnectionWarmup.closed)

    def test_next_part_connects_while_the_previous_waits_and_messages_keep_order(self):
        first, second = b'\x01\x00' * 160, b'\x02\x00' * 160
        release, second_running = threading.Event(), threading.Event()

        def parts(module, blocks, cancelled, preview, on_connected, on_final):
            blocks = iter(blocks)
            head = next(blocks)
            list(blocks)
            on_connected()
            if head == first:
                module.decode_response(response({'text': '一', 'is_interim': False}))
                preview('一')
                self.assertTrue(release.wait(2), 'the first part waits for its final')
                on_final('一。')
            else:
                second_running.set()
                module.decode_response(response({'text': '二二', 'is_interim': False}))
                preview('二二')
                module.decode_response(response({'text': '三', 'is_interim': True}))
                preview('二二三')
                on_final('二二三。')

        out = Collector()
        bridge = doubao_live.Bridge(fake_module(parts), doubao_live.Output(out))
        try:
            for sid, data in ((1, first), (2, second)):
                bridge.handle({'type': 'start', 'id': sid})
                bridge.handle(json.loads(audio(sid, data)))
                bridge.handle({'type': 'stop', 'id': sid})
            self.assertTrue(second_running.wait(2), 'the next part runs before the previous final')
            time.sleep(.1)
            self.assertEqual({m['id'] for m in out.lines()}, {1}, 'nothing of part 2 before part 1 ends')
            release.set()
            got = wait_for(out, 8)
        finally:
            release.set()
            bridge.shutdown()
        self.assertEqual([(m['id'], m['type']) for m in got],
                         [(1, 'connected'), (1, 'text'), (1, 'final'), (1, 'end'),
                          (2, 'connected'), (2, 'text'), (2, 'final'), (2, 'end')])
        texts = [(m['id'], m['text'], m['stable']) for m in got if m['type'] == 'text']
        self.assertEqual(texts, [(1, '一', 1), (2, '二二三', 2)], 'held previews collapse; counts stay per part')
        self.assertFalse(bridge.worker.is_alive())

    def test_at_most_two_sessions_run_at_once(self):
        entered, release = queue.Queue(), threading.Event()

        def blocking(module, blocks, cancelled, preview, on_connected, on_final):
            entered.put(threading.current_thread().name)
            release.wait(2)
            raise module.Cancelled('cancelled')

        out = Collector()
        bridge = doubao_live.Bridge(fake_module(blocking), doubao_live.Output(out))
        try:
            for sid in (1, 2, 3):
                bridge.handle({'type': 'start', 'id': sid})
            started = [entered.get(timeout=1), entered.get(timeout=1)]
            self.assertEqual(sorted(started), ['doubao-session-1', 'doubao-session-2'])
            with self.assertRaises(queue.Empty):
                entered.get(timeout=.2)
            release.set()
            self.assertEqual(entered.get(timeout=1), 'doubao-session-3')
            got = wait_for(out, 3)
        finally:
            release.set()
            bridge.shutdown()
        self.assertEqual([m['id'] for m in got if m['type'] == 'end'], [1, 2, 3])

    def test_sessions_run_in_order(self):
        lines = []
        for sid in (1, 2):
            lines += [json.dumps({'type': 'start', 'id': sid}), audio(sid), json.dumps({'type': 'stop', 'id': sid})]
        got, _ = self.run_lines(streaming, lines, 13)
        ends = [m['id'] for m in got if m['type'] == 'end']
        self.assertEqual(ends, [1, 2])


class TrackerTests(unittest.TestCase):
    def test_counts_only_final_results(self):
        tracker = doubao_live.StableTracker()
        tracker.observe(response({'text': '一二', 'is_interim': True}))
        tracker.observe(response({'text': '一二三', 'is_interim': False}, {'text': '四', 'is_interim': True}))
        tracker.observe({'event': 'TaskStarted'})
        self.assertEqual(tracker.chars, 3)
        tracker.reset()
        self.assertEqual(tracker.chars, 0)


if __name__ == '__main__':
    unittest.main()
