#!/usr/bin/env python3
"""Inspect the RAR 1.5 candidate record framing."""
import argparse
import json
import os
from pathlib import Path
import shutil
import struct
import sys
import zlib

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from probe import manifest, run


def blocks(data):
    assert data[:7] == b"Rar!\x1a\x07\x00"
    offset = 7
    while offset < len(data):
        crc, kind, flags, size = struct.unpack_from("<HBHH", data, offset)
        end = offset + size
        assert size >= 7 and end <= len(data)
        # Old-style comments include their payload in HEAD_SIZE, but only the
        # fixed 13-byte header contributes to HEAD_CRC.
        crc_end = offset + 13 if kind == 0x75 else end
        assert crc_end <= end
        assert zlib.crc32(data[offset + 2:crc_end]) & 0xffff == crc
        length = struct.unpack_from("<I", data, offset + 7)[0] if flags & 0x8000 else 0
        name = None
        if kind in (0x74, 0x7a):
            name_length = struct.unpack_from("<H", data, offset + 26)[0]
            name_at = offset + (40 if flags & 0x100 else 32)
            name = bytes(data[name_at:name_at + name_length])
            assert name_at + name_length <= end
            if flags & 0x100:
                length += struct.unpack_from("<I", data, offset + 32)[0] << 32
            if kind == 0x7a and name == b"RARS":
                assert flags == 0xc000 and size == 36
                assert data[offset + 15] == 0  # host OS
                assert data[offset + 20:offset + 24] == bytes(4)  # timestamp
                assert data[offset + 24] == 15 and data[offset + 25] == 0x30
                assert data[offset + 28:offset + 32] == bytes(4)  # attributes
                assert struct.unpack_from("<I", data, offset + 11)[0] == length
                assert struct.unpack_from("<I", data, offset + 16)[0] == zlib.crc32(data[end:end + length])
                assert data[end:end + length].startswith(b"rars/")
                assert data[end:end + length].endswith(b"\nmarker-schema=1\n")
        assert end + length <= len(data)
        yield {"kind": kind, "flags": flags, "name": name,
               "offset": offset, "data_offset": end, "length": length}
        if kind == 0x73 and flags & 0x80:
            return  # Later headers require the password; the payload stays public.
        offset = end + length


def has_marker(data):
    return any(block["name"] == b"RARS" for block in blocks(data))
