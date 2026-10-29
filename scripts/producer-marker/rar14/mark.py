#!/usr/bin/env python3
"""Experimental RAR 1.4 main-header extension; no production writer changes."""
import struct
import zlib

DATA = b'rars/0.10.2\nmarker-schema=1\n'
RECORD = b'RARS' + struct.pack('<H', len(DATA)) + DATA + struct.pack('<I', zlib.crc32(DATA))


def mark(data):
    assert data.startswith(b'RE~^')
    size = struct.unpack_from('<H', data, 4)[0]
    assert 7 <= size <= len(data)
    assert not data[6] & 0x20, 'Do not modify authenticity-bearing archives'
    assert size + len(RECORD) <= 65535
    result = data[:4] + struct.pack('<H', size + len(RECORD)) + data[6:size] + RECORD + data[size:]
    assert result[6:size] == data[6:size]
    assert result[size + len(RECORD):] == data[size:]
    return result
