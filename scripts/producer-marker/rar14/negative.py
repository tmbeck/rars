#!/usr/bin/env python3
"""Ensure RAR/WinRAR test mode detects corrupt RAR 1.4 member data."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path

import sys
import struct
sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from probe import commands, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--engines", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=1)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)

    def check(engine):
        assert engine["family"] in ("rar", "winrar")
        results = []
        for version in ["rar14"]:
            for label in ["control", "marked"]:
                data = bytearray((args.matrix / "archives" / version / "stored" / label / "archive.rar").read_bytes())
                offset = struct.unpack_from("<H", data, 4)[0]
                packed = struct.unpack_from("<I", data, offset)[0]
                head = struct.unpack_from("<H", data, offset + 10)[0]
                assert packed > 10
                data[offset + head + packed // 2] ^= 0x40
                name = f"{engine['name']}-{version}-{label}"
                archive = output / f"{name}.rar"
                archive.write_bytes(data)
                test, _ = commands(engine, archive, output)
                code = run(test, output, {**os.environ, **engine.get("env", {})}, output / f"{name}.log")
                assert code == 3, (name, code)
                results.append({"engine": engine["name"], "format": version,
                                "label": label, "test_exit": code})
                print(f"{name}: correctly rejected corruption (exit 3)", flush=True)
        return results

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        results = [row for group in pool.map(check, json.loads(args.engines.read_text())) for row in group]
    (output / "report.json").write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
