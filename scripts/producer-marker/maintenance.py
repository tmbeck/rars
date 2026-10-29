#!/usr/bin/env python3
"""Verify marker framing, then exercise RAR update and real recovery repair."""
import argparse
import json
import os
from pathlib import Path
import shutil
import struct
import zlib

from probe import manifest, run


def vint(data, offset):
    value = 0
    for shift in range(0, 70, 7):
        byte = data[offset]
        offset += 1
        value |= (byte & 127) << shift
        if byte < 128:
            return value, offset
    raise ValueError("Oversized vint")


def blocks(data):
    assert data[:8] == b"Rar!\x1a\x07\x01\x00"
    offset = 8
    while offset < len(data):
        size, cursor = vint(data, offset + 4)
        end = cursor + size
        assert end <= len(data)
        assert zlib.crc32(data[offset + 4:end]) == struct.unpack_from("<I", data, offset)[0]
        kind, cursor = vint(data, cursor)
        flags, cursor = vint(data, cursor)
        extra = length = 0
        if flags & 1:
            extra, cursor = vint(data, cursor)
        if flags & 2:
            length, cursor = vint(data, cursor)
        name = None
        if kind in (2, 3):
            file_flags, q = vint(data, cursor)
            unpacked, q = vint(data, q)
            _, q = vint(data, q)  # attributes
            if file_flags & 2:
                q += 4
            crc = None
            if file_flags & 4:
                crc = struct.unpack_from("<I", data, q)[0]
                q += 4
            compression, q = vint(data, q)
            _, q = vint(data, q)  # host OS
            name_length, q = vint(data, q)
            name = bytes(data[q:q + name_length])
            assert q + name_length <= end - extra
            if kind == 3 and name == b"RARS":
                assert flags == 6 and extra == 0
                assert compression == 0 and unpacked == length
                assert crc == zlib.crc32(data[end:end + length])
                assert data[end:end + length].startswith(b"rars/")
        assert end + length <= len(data)
        yield kind, name, end, length
        if kind == 4:  # Remaining headers are encrypted; external readers check them.
            return
        offset = end + length


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--rar", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root, matrix = args.output.resolve(), args.matrix.resolve()
    root.mkdir(parents=True, exist_ok=False)
    rar = str(args.rar.resolve())
    env = os.environ.copy()
    evidence = {"framing": [], "maintenance": []}
    for path in sorted((matrix / "archives").rglob("*.rar")):
        records = list(blocks(path.read_bytes()))
        encrypted = records[0][0] == 4
        count = sum(kind == 3 and name == b"RARS" for kind, name, _, _ in records)
        marked = path.parent.name == "marked"
        if not encrypted:
            assert count == int(marked), path
            if marked:
                assert records[0][0] == 1 and records[1][1] == b"RARS", path
        evidence["framing"].append({"archive": str(path.relative_to(matrix)),
                                    "encrypted_headers": encrypted,
                                    "markers": count if not encrypted else None})
    expected = manifest(matrix / "input")
    for label in ("control", "marked"):
        update = root / "update" / label
        update.mkdir(parents=True)
        archive = update / "archive.rar"
        shutil.copyfile(matrix / "archives/comment" / label / "archive.rar", archive)
        (update / "added.txt").write_text("Added by RAR after archive creation.\n")
        assert run([rar, "a", "-y", str(archive), "added.txt"], update, env,
                   update / "update.log") == 0
        assert run([rar, "cw", "-y", str(archive), "comment.txt"], update, env,
                   update / "comment.log") == 0
        assert (update / "comment.txt").read_text() == "User comment survives independently"
        output = update / "extracted"
        output.mkdir()
        assert run([rar, "x", "-y", str(archive), str(output) + "/"], update, env,
                   update / "extract.log") == 0
        assert manifest(output) == {**expected, "added.txt": manifest(update)["added.txt"]}
        retained = any(name == b"RARS" for _, name, _, _ in blocks(archive.read_bytes()))
        evidence["maintenance"].append({"operation": "update", "label": label,
                                        "marker_retained": retained, "contents_match": True,
                                        "comment_preserved": True})

        repair = root / "repair" / label
        repair.mkdir(parents=True)
        data = bytearray((matrix / "archives/recovery" / label / "archive.rar").read_bytes())
        _, _, start, length = next(record for record in blocks(data)
                                  if record[0] == 2 and record[3] > 10)
        data[start + length // 2] ^= 0x40
        damaged = repair / "damaged.rar"
        damaged.write_bytes(data)
        assert run([rar, "t", "-y", str(damaged)], repair, env, repair / "before.log") == 3
        assert run([rar, "r", "-y", str(damaged)], repair, env, repair / "repair.log") == 0
        fixed = repair / "fixed.damaged.rar"
        assert run([rar, "t", "-y", str(fixed)], repair, env, repair / "after.log") == 0
        output = repair / "extracted"
        output.mkdir()
        assert run([rar, "x", "-y", str(fixed), str(output) + "/"], repair, env,
                   repair / "extract.log") == 0
        assert manifest(output) == expected
        retained = any(name == b"RARS" for _, name, _, _ in blocks(fixed.read_bytes()))
        assert retained == (label == "marked")
        evidence["maintenance"].append({"operation": "repair", "label": label,
                                        "marker_retained": retained, "contents_match": True})
    (root / "report.json").write_text(json.dumps(evidence, indent=2) + "\n")
    print(f"Verified {len(evidence['framing'])} archive/volume headers, update and repair.")


if __name__ == "__main__":
    main()
