#!/usr/bin/env python3
"""Compare RAR 1.4 producer prototypes with the same unmodified writer."""
import argparse
from concurrent.futures import ThreadPoolExecutor
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import sys

sys.path.insert(0, str(Path(__file__).resolve().parents[1]))
from probe import commands, manifest, run, windows


CASES = {
    "stored": ["--store"],
    "compressed": [],
    "solid": ["--solid"],
    "comment": ["--comment", "User comment survives independently"],
    "password-stored": ["--store", "--password", "probe-password"],
    "password-compressed": ["--password", "probe-password"],
    "stored-volumes": ["--store", "--volume-size", "4k"],
    "compressed-volumes": ["--volume-size", "4k"],
    "solid-volumes": ["--solid", "--volume-size", "4k"],
    "empty-file": ["--store"],
}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--control", required=True, type=Path)
    parser.add_argument("--marked", required=True, type=Path)
    parser.add_argument("--engines", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--resume", action="store_true")
    parser.add_argument("--ascii", action="store_true", help="Use ASCII filenames for older Windows readers")
    parser.add_argument("--jobs", type=int, default=1, help="Independent reader processes per case")
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    source = root / "input"
    source.mkdir(exist_ok=True)
    (source / "text.txt").write_bytes(b"Repeated producer-marker interoperability text.\n" * 500)
    (source / "binary.bin").write_bytes(random.Random(73).randbytes(12000))
    (source / "empty.txt").write_bytes(b"")
    unicode_name = "unicode-cafe.txt" if args.ascii else "unicode-caf\u00e9.txt"
    (source / unicode_name).write_text("Unicode filename and contents: caf\u00e9\n")
    expected = manifest(source)
    engines = json.loads(args.engines.read_text())
    report = {"control_sha256": hashlib.sha256(args.control.read_bytes()).hexdigest(),
              "marked_sha256": hashlib.sha256(args.marked.read_bytes()).hexdigest(),
              "engines": engines, "cases": {}, "results": []}
    if args.resume and (root / "report.json").exists():
        previous = json.loads((root / "report.json").read_text())
        for key in ("control_sha256", "marked_sha256", "engines"):
            if previous[key] != report[key]:
                raise RuntimeError(f"Cannot resume with changed {key}")
        report = previous
    for version in ["rar14"]:
        for case, options in CASES.items():
            key = f"{version}/{case}"
            names = ["binary.bin"] if "volumes" in case else ["empty.txt"] if case == "empty-file" else expected
            wanted = {name: expected[name] for name in names}
            definition = {"options": options, "expected": wanted}
            completed = {r["engine"] for r in report["results"] if r["case"] == key}
            if completed and report["cases"][key] != definition:
                raise RuntimeError(f"Cannot resume changed case {key}")
            if completed == {engine["name"] for engine in engines}:
                continue
            report["cases"][key] = definition
            archives = {}
            for label, writer in [("control", args.control), ("marked", args.marked)]:
                directory = root / "archives" / key / label
                directory.mkdir(parents=True, exist_ok=True)
                archive = directory / "archive.rar"
                existing = sorted(directory.glob("*.rar"))
                if not (args.resume and existing):
                    if existing:
                        raise RuntimeError(f"Archive directory must be empty: {directory}")
                    command = [str(writer.resolve()), "a", "--format", version, *options, str(archive), *wanted]
                    if run(command, source, os.environ.copy(), directory / "create.log") != 0:
                        raise RuntimeError(f"Creation failed: {directory / 'create.log'}")
                candidates = sorted(directory.glob("*.rar"))
                if not candidates:
                    raise RuntimeError(f"No archive in {directory}")
                archives[label] = candidates[0]
                # Legacy continuation volumes can have .r00, .r01, ... names.
                for path in directory.iterdir():
                    if path.name == "create.log":
                        continue
                    data = path.read_bytes()
                    assert data.startswith(b"RE~^"), path
                    assert (b"\nmarker-schema=1\n" in data) == (label == "marked"), path
            def check_engine(engine):
                outcomes = {}
                for label, archive in archives.items():
                    destination = root / "extracted" / engine["name"] / key / label
                    if args.resume and destination.exists():
                        shutil.rmtree(destination)
                    destination.mkdir(parents=True, exist_ok=False)
                    logs = root / "logs" / engine["name"] / key / label
                    logs.mkdir(parents=True, exist_ok=True)
                    env = {**os.environ, **engine.get("env", {})}
                    test, extract = commands(engine, archive, destination)
                    test_code = run(test, root, env, logs / "test.log")
                    extract_code = run(extract, root, env, logs / "extract.log")
                    comment_match = True
                    if case == "comment" and engine["family"] == "rar" and "unrar" in Path(engine["command"][-1]).name.lower():
                        comment_match = "User comment survives independently" in (logs / "test.log").read_text()
                    elif case == "comment" and engine["family"] == "rar":
                        comment_path = logs / "comment.txt"
                        convert = windows if engine.get("windows") else str
                        code = run(engine["command"] + ["cw", "-y", convert(archive), convert(comment_path)],
                                   root, env, logs / "comment.log")
                        comment_match = code == 0 and comment_path.exists() and comment_path.read_bytes() == b"User comment survives independently"
                    actual = manifest(destination)
                    outcomes[label] = {"test_exit": test_code, "extract_exit": extract_code,
                                       "contents_match": actual == wanted, "comment_match": comment_match, "manifest": actual}
                good = lambda r: r["test_exit"] == 0 and r["extract_exit"] == 0 and r["contents_match"] and r["comment_match"]
                control, marked = good(outcomes["control"]), good(outcomes["marked"])
                status = "PASS" if control and marked else "REGRESSION" if control else "CONTROL_FAIL"
                row = {"engine": engine["name"], "case": key, "status": status, **outcomes}
                print(f"{engine['name']:20} {key:40} {status}", flush=True)
                return row
            pending = [engine for engine in engines if engine["name"] not in completed]
            with ThreadPoolExecutor(max_workers=args.jobs) as pool:
                for row in pool.map(check_engine, pending):
                    report["results"].append(row)
                    (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    raise SystemExit(any(row["status"] == "REGRESSION" for row in report["results"]))


if __name__ == "__main__":
    main()
