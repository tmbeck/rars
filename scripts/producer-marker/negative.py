#!/usr/bin/env python3
"""Check that test commands reject corrupted file data despite silent GUI mode."""
import argparse
import json
import os
from pathlib import Path

from maintenance import blocks
from probe import commands, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--matrix", type=Path, required=True)
    parser.add_argument("--engines", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=False)
    results = []
    for engine in json.loads(args.engines.read_text()):
        if engine["family"] not in ("rar", "winrar"):
            raise ValueError("This check expects RAR/WinRAR test commands and CRC exit code 3")
        for label in ("control", "marked"):
            data = bytearray((args.matrix / "archives/stored" / label / "archive.rar").read_bytes())
            _, _, start, length = next(record for record in blocks(data)
                                      if record[0] == 2 and record[3] > 10)
            data[start + length // 2] ^= 0x40
            archive = output / f"{engine['name']}-{label}-damaged.rar"
            archive.write_bytes(data)
            test, _ = commands(engine, archive, output)
            env = os.environ.copy()
            env.update(engine.get("env", {}))
            code = run(test, output, env, output / f"{engine['name']}-{label}.log")
            if code != 3:
                raise RuntimeError(f"Expected CRC exit 3, got {code}: {engine['name']} {label}")
            results.append({"engine": engine["name"], "archive": label,
                            "test_exit": code, "expected_crc_error": True})
            print(f"{engine['name']} {label}: correctly rejected corrupt file (exit 3)", flush=True)
    (output / "report.json").write_text(json.dumps(results, indent=2) + "\n")


if __name__ == "__main__":
    main()
