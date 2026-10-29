#!/usr/bin/env python3
"""Independently inspect RAR 3/4 marker framing, updates and recovery repair."""
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
                assert data[offset + 24] in (20, 29) and data[offset + 25] == 0x30
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


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--rar", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root, matrix = args.output.resolve(), args.matrix.resolve()
    root.mkdir(parents=True, exist_ok=False)
    rar, env = str(args.rar.resolve()), os.environ.copy()
    evidence = {"framing": [], "maintenance": []}
    for path in sorted((matrix / "archives").rglob("*")):
        if not path.is_file() or path.name == "create.log":
            continue
        data = path.read_bytes()
        records = list(blocks(data))
        encrypted = bool(records[0]["flags"] & 0x80)
        marked = path.parent.name == "marked"
        if not encrypted:
            assert sum(r["name"] == b"RARS" for r in records) == int(marked), path
            if marked:
                assert records[1]["name"] == b"RARS", path
        assert (data.count(b"\nmarker-schema=1\n") == 1) == marked, path
        evidence["framing"].append({"archive": str(path.relative_to(matrix)),
                                    "encrypted_headers": encrypted,
                                    "markers": int(marked) if not encrypted else None})
    expected = manifest(matrix / "input")
    for version in ["rar30", "rar40"]:
        for label in ["control", "marked"]:
            update = root / version / "update" / label
            update.mkdir(parents=True)
            archive = update / "archive.rar"
            shutil.copyfile(matrix / "archives" / version / "comment" / label / "archive.rar", archive)
            (update / "added.txt").write_text("Added by RAR after archive creation.\n")
            assert run([rar, "a", "-y", str(archive), "added.txt"], update, env, update / "update.log") == 0
            assert run([rar, "cw", "-y", str(archive), "comment.txt"], update, env, update / "comment.log") == 0
            assert (update / "comment.txt").read_text() == "User comment survives independently"
            output = update / "extracted"
            output.mkdir()
            assert run([rar, "x", "-y", str(archive), str(output) + "/"], update, env, update / "extract.log") == 0
            assert manifest(output) == {**expected, "added.txt": manifest(update)["added.txt"]}
            evidence["maintenance"].append({"format": version, "operation": "update", "label": label,
                                            "marker_retained": has_marker(archive.read_bytes()),
                                            "contents_match": True, "comment_preserved": True})
            repair = root / version / "repair" / label
            repair.mkdir(parents=True)
            archive = repair / "damaged.rar"
            shutil.copyfile(matrix / "archives" / version / "stored" / label / "archive.rar", archive)
            assert run([rar, "rr10", "-y", str(archive)], repair, env, repair / "protect.log") == 0
            data = bytearray(archive.read_bytes())
            retained_before = has_marker(data)
            assert any(r["name"] == b"RR" for r in blocks(data))
            record = next(r for r in blocks(data) if r["kind"] == 0x74 and r["length"] > 10)
            data[record["data_offset"] + record["length"] // 2] ^= 0x40
            archive.write_bytes(data)
            assert run([rar, "t", "-y", str(archive)], repair, env, repair / "before.log") == 3
            assert run([rar, "r", "-y", str(archive)], repair, env, repair / "repair.log") == 0
            fixed = repair / "fixed.damaged.rar"
            assert run([rar, "t", "-y", str(fixed)], repair, env, repair / "after.log") == 0
            output = repair / "extracted"
            output.mkdir()
            assert run([rar, "x", "-y", str(fixed), str(output) + "/"], repair, env, repair / "extract.log") == 0
            assert manifest(output) == expected
            evidence["maintenance"].append({"format": version, "operation": "repair", "label": label,
                                            "marker_retained_when_adding_recovery": retained_before,
                                            "marker_retained_after_repair": has_marker(fixed.read_bytes()),
                                            "contents_match": True})
    (root / "report.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"Checked {len(evidence['framing'])} archives/volumes, updates and recovery repair.")


if __name__ == "__main__":
    main()
