#!/usr/bin/env python3
"""Run the reader matrix on recovery-protected control/marked archive pairs."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import json
import os
from pathlib import Path
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from probe import commands, manifest, run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--archives", type=Path, required=True)
    parser.add_argument("--input", type=Path, required=True)
    parser.add_argument("--engines", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    parser.add_argument("--jobs", type=int, default=1)
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    expected = manifest(args.input)
    engines = json.loads(args.engines.read_text())

    def check(engine):
        results = []
        for version in ["rar30", "rar40"]:
            outcomes = {}
            for label in ["control", "marked"]:
                output = root / engine["name"] / version / label
                output.mkdir(parents=True)
                extracted = output / "extracted"
                extracted.mkdir()
                archive = (args.archives / version / label / "archive.rar").resolve()
                test, extract = commands(engine, archive, extracted)
                env = {**os.environ, **engine.get("env", {})}
                test_exit = run(test, root, env, output / "test.log")
                extract_exit = run(extract, root, env, output / "extract.log")
                outcomes[label] = {"test_exit": test_exit, "extract_exit": extract_exit,
                                   "contents_match": manifest(extracted) == expected}
            good = lambda row: row["test_exit"] == 0 and row["extract_exit"] == 0 and row["contents_match"]
            status = "PASS" if all(good(row) for row in outcomes.values()) else "REGRESSION" if good(outcomes["control"]) else "CONTROL_FAIL"
            results.append({"engine": engine["name"], "format": version, "status": status, **outcomes})
            print(engine["name"], version, status, flush=True)
        return results

    with ThreadPoolExecutor(max_workers=args.jobs) as pool:
        results = [row for group in pool.map(check, engines) for row in group]
    (root / "report.json").write_text(json.dumps({"engines": engines, "expected": expected, "results": results}, indent=2) + "\n")
    raise SystemExit(any(row["status"] == "REGRESSION" for row in results))


if __name__ == "__main__":
    main()
