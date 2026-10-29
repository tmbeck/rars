#!/usr/bin/env python3
"""Verify repair with the producer inside the protected prefix, not discarded.

Uses a RAR 3.93 RR header as a template. First reproduces its original payload
byte-for-byte, then computes sector tags and XOR parity for a marked prefix.
This is an investigation fixture generator, not production recovery output.
"""
import argparse
import json
import os
from pathlib import Path
import struct
import zlib

from maintenance import blocks, has_marker
from probe import manifest, run


def parity(prefix, count):
    tags = bytearray()
    shards = [bytearray(512) for _ in range(count)]
    for index, offset in enumerate(range(0, len(prefix), 512)):
        sector = prefix[offset:offset + 512].ljust(512, b"\0")
        tags += struct.pack("<H", ~zlib.crc32(sector) & 0xffff)
        shard = shards[index % count]
        for pos, byte in enumerate(sector):
            shard[pos] ^= byte
    return tags + b"".join(shards)


def header_crc(data, offset, size):
    struct.pack_into("<H", data, offset, zlib.crc32(data[offset + 2:offset + size]) & 0xffff)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--maintenance", type=Path, required=True)
    parser.add_argument("--rar", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    expected = manifest(args.matrix / "input")
    evidence = []
    for version in ["rar30", "rar40"]:
        reference = (args.maintenance / version / "repair/control/fixed.damaged.rar").read_bytes()
        rr = next(r for r in blocks(reference) if r["name"] == b"RR")
        header = bytearray(reference[rr["offset"]:rr["data_offset"]])
        assert header[34:42] == b"Protect+"
        count, sectors = struct.unpack_from("<IQ", header, 42)
        assert sectors == (rr["offset"] + 511) // 512
        assert parity(reference[:rr["offset"]], count) == reference[rr["data_offset"]:rr["data_offset"] + rr["length"]]
        for label in ["control", "marked"]:
            source = (args.matrix / "archives" / version / "stored" / label / "archive.rar").read_bytes()
            records = list(blocks(source))
            end = next((r["offset"] for r in records if r["kind"] == 0x7b), len(source))
            prefix = bytearray(source[:end])
            flags = struct.unpack_from("<H", prefix, 10)[0]
            struct.pack_into("<H", prefix, 10, flags | 0x40)
            header_crc(prefix, 7, struct.unpack_from("<H", prefix, 12)[0])
            payload = parity(prefix, count)
            struct.pack_into("<II", header, 7, len(payload), len(payload))
            struct.pack_into("<I", header, 16, zlib.crc32(payload))
            struct.pack_into("<Q", header, 46, (len(prefix) + 511) // 512)
            header_crc(header, 0, len(header))
            # Retain RAR's ENDARC framing after the generated recovery payload.
            data = prefix + header + payload + reference[rr["data_offset"] + rr["length"]:]
            directory = root / version / label
            directory.mkdir(parents=True)
            archive = directory / "archive.rar"
            archive.write_bytes(data)
            assert has_marker(data) == (label == "marked")
            rar, env = str(args.rar.resolve()), os.environ.copy()
            assert run([rar, "t", "-y", str(archive)], directory, env, directory / "test.log") == 0
            member = next(r for r in blocks(data) if r["kind"] == 0x74 and r["length"] > 10)
            data[member["data_offset"] + member["length"] // 2] ^= 0x40
            if label == "marked":
                producer = next(r for r in records if r["name"] == b"RARS")
                data[producer["data_offset"]] ^= 0x20
            damaged = directory / "damaged.rar"
            damaged.write_bytes(data)
            assert run([rar, "t", "-y", str(damaged)], directory, env, directory / "before.log") == 3
            assert run([rar, "r", "-y", str(damaged)], directory, env, directory / "repair.log") == 0
            fixed = directory / "fixed.damaged.rar"
            assert has_marker(fixed.read_bytes()) == (label == "marked")
            assert fixed.read_bytes()[:len(prefix)] == prefix
            assert run([rar, "t", "-y", str(fixed)], directory, env, directory / "after.log") == 0
            output = directory / "extracted"
            output.mkdir()
            assert run([rar, "x", "-y", str(fixed), str(output) + "/"], directory, env, directory / "extract.log") == 0
            assert manifest(output) == expected
            evidence.append({"format": version, "label": label, "marker_inside_protected_prefix": label == "marked",
                             "reference_parity_reproduced": True, "corruption_exit": 3,
                             "producer_payload_corrupted_and_restored": label == "marked",
                             "repair_exit": 0, "marker_retained": has_marker(fixed.read_bytes()), "contents_match": True})
    (root / "report.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print("Reference parity reproduced; all four damaged archives repaired with expected marker retention.")


if __name__ == "__main__":
    main()
