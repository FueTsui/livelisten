"""The observed VoiceGenie protobuf envelope; no generated/vendor code."""
import json


class ProtocolError(ValueError):
    pass


def varint(value):
    if not isinstance(value, int) or not 0 <= value < 2**64:
        raise ProtocolError('整数必须在 uint64 范围内')
    result = bytearray()
    while value > 127:
        result.append((value & 127) | 128)
        value >>= 7
    result.append(value)
    return bytes(result)


def _field(number, value):
    data = value.encode('utf-8') if isinstance(value, str) else value
    return varint(number << 3 | 2) + varint(len(data)) + data


def encode_request(event, appkey, *, task_id='', payload=None, audio=None, sequence=None):
    packet = _field(2, appkey) + _field(3, 'VoiceGenie') + _field(5, event)
    packet += _field(6, json.dumps(payload if payload is not None else {}, ensure_ascii=False))
    if task_id:
        packet += _field(8, task_id)
    if audio is not None:
        packet += _field(7, audio)
    if sequence is not None:
        packet += varint(9 << 3) + varint(sequence)
    return packet


def decode_response(data):
    if isinstance(data, str):
        try:
            result = json.loads(data)
        except ValueError as error:
            raise ProtocolError('响应不是有效 JSON') from error
        if not isinstance(result, dict):
            raise ProtocolError('JSON 响应必须为对象')
        return result
    if not isinstance(data, (bytes, bytearray)):
        raise ProtocolError('响应必须为文本或二进制帧')
    offset = 0

    def read_varint():
        nonlocal offset
        value = 0
        for index in range(10):
            if offset >= len(data):
                raise ProtocolError('响应中的 varint 被截断')
            byte = data[offset]
            offset += 1
            if index == 9 and byte > 1:
                raise ProtocolError('响应中的 varint 超出 uint64')
            value |= (byte & 127) << (index * 7)
            if not byte & 128:
                return value
        raise ProtocolError('无效 varint')

    names = {1: 'task_id', 2: 'message_id', 3: 'namespace', 4: 'event',
             5: 'status_code', 6: 'status_text', 7: 'payload', 8: 'data',
             9: 'seq_id', 10: 'session_id', 11: 'log_id'}
    integer_fields = {5, 9}
    result = {}
    while offset < len(data):
        tag = read_varint()
        number, wire = tag >> 3, tag & 7
        if not number:
            raise ProtocolError('无效 protobuf 字段编号')
        if wire == 0:
            value = read_varint()
        elif wire in (1, 2, 5):
            length = read_varint() if wire == 2 else (8 if wire == 1 else 4)
            end = offset + length
            if end > len(data):
                raise ProtocolError('响应字段被截断')
            value = data[offset:end]
            offset = end
        else:
            raise ProtocolError(f'不支持的 protobuf wire type: {wire}')
        if number not in names:
            continue
        expected_wire = 0 if number in integer_fields else 2
        if wire != expected_wire:
            raise ProtocolError('已知响应字段的 wire type 不符')
        if wire == 2 and number != 8:
            try:
                value = value.decode('utf-8')
            except UnicodeDecodeError as error:
                raise ProtocolError('响应字符串不是 UTF-8') from error
        result[names[number]] = value
    return result
