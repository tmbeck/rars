#!/usr/bin/env python3
"""Compare a producer-marker prototype with its unmodified writer.

Requires explicit binaries and an engine configuration; never installs software
or patches the working tree. Outputs archives, command logs and JSON evidence.
"""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import subprocess


CASES = {
    "stored": ["--store"],
    "compressed": [],
    "solid": ["--solid"],
    "comment": ["--comment", "User comment survives independently"],
    "quick-open": ["--quick-open"],
    "recovery": ["--recovery-percent", "10"],
    "password": ["--password", "probe-password"],
    "encrypted-headers": ["--password", "probe-password", "--encrypt-headers"],
    "combined": ["--password", "probe-password", "--encrypt-headers",
                 "--solid", "--recovery-percent", "10",
                 "--comment", "User comment survives independently"],
    "volumes": ["--volume-size", "4k"],
    "solid-volumes": ["--solid", "--volume-size", "4k"],
    "encrypted-volumes": ["--password", "probe-password", "--encrypt-headers",
                          "--volume-size", "4k"],
    "recovery-volumes": ["--recovery-percent", "10", "--volume-size", "4k"],
    "empty-file": ["--store"],
}


def manifest(directory):
    return {str(p.relative_to(directory)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(directory.rglob("*")) if p.is_file()}


def run(command, cwd, env, log):
    try:
        result = subprocess.run(command, cwd=cwd, env=env, stdin=subprocess.DEVNULL,
                                stdout=subprocess.PIPE, stderr=subprocess.STDOUT,
                                timeout=60)
        output = result.stdout.decode("utf-8", errors="replace")
        code = result.returncode
    except subprocess.TimeoutExpired as exc:
        output = (exc.stdout or b"").decode("utf-8", errors="replace") + "\nTIMEOUT"
        code = 124
    log.write_text(json.dumps(command) + "\n" + output)
    return code


def windows(path):
    return "Z:" + str(path).replace("/", "\\")


def commands(engine, archive, output):
    prefix = engine["command"]
    family = engine["family"]
    if engine.get("windows"):
        archive, output = windows(archive), windows(output) + "\\"
    else:
        archive, output = str(archive), str(output) + "/"
    if family == "rar":
        return (prefix + ["t", "-y", "-pprobe-password", archive],
                prefix + ["x", "-y", "-pprobe-password", archive, output])
    if family == "winrar":
        switches = ["-ibck", "-inul", "-y", "-pprobe-password"]
        return (prefix + ["t", *switches, archive],
                prefix + ["x", *switches, archive, output])
    if family == "7zip":
        return (prefix + ["t", "-y", "-pprobe-password", archive],
                prefix + ["x", "-y", "-pprobe-password", "-o" + output, archive])
    if family == "bandizip":
        return (prefix + ["t", "-y", "-p:probe-password", archive],
                prefix + ["x", "-y", "-p:probe-password", "-o:" + output, archive])
    if family == "libarchive":
        return (prefix + ["-tf", archive],
                prefix + ["--passphrase", "probe-password", "-xf", archive, "-C", output])
    if family == "unar":
        return (engine["list_command"] + ["-p", "probe-password", archive],
                prefix + ["-q", "-f", "-D", "-p", "probe-password", "-o", output, archive])
    if family == "rars":
        return (prefix + ["test", "-p", "probe-password", archive],
                prefix + ["x", "-p", "probe-password", archive, output])
    raise ValueError(f"Unknown engine family: {family}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--control", required=True, type=Path)
    parser.add_argument("--marked", required=True, type=Path)
    parser.add_argument("--engines", required=True, type=Path)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--reuse-archives", action="store_true")
    parser.add_argument("--resume", action="store_true", help="Resume completed engine/case pairs")
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=True)
    source = root / "input"
    source.mkdir(exist_ok=True)
    (source / "text.txt").write_bytes(b"Repeated producer-marker interoperability text.\n" * 500)
    (source / "binary.bin").write_bytes(random.Random(73).randbytes(12000))
    (source / "empty.txt").write_bytes(b"")
    (source / "unicode-caf\u00e9.txt").write_text("Unicode filename and contents: caf\u00e9\n")
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
    for case, options in CASES.items():
        wanted = {"empty.txt": expected["empty.txt"]} if case == "empty-file" else expected
        completed = {r["engine"] for r in report["results"] if r["case"] == case}
        if completed and report["cases"][case] != {"options": options, "expected": wanted}:
            raise RuntimeError(f"Cannot resume with changed case: {case}")
        if completed == {engine["name"] for engine in engines}:
            continue
        report["cases"][case] = {"options": options, "expected": wanted}
        archives = {}
        for label, writer in [("control", args.control), ("marked", args.marked)]:
            directory = root / "archives" / case / label
            directory.mkdir(parents=True, exist_ok=True)
            archive = directory / "archive.rar"
            if not args.reuse_archives and not (args.resume and list(directory.glob("*.rar"))):
                if list(directory.glob("*.rar")):
                    raise RuntimeError(f"Archive directory must be empty: {directory}")
                command = [str(writer.resolve()), "a", *options, str(archive), *wanted]
                if run(command, source, os.environ.copy(), directory / "create.log") != 0:
                    raise RuntimeError(f"Creation failed: {directory / 'create.log'}")
            candidates = sorted(directory.glob("*.rar"))
            if not candidates:
                raise RuntimeError(f"No archives in {directory}")
            archives[label] = candidates[0]
            for path in candidates:
                data = path.read_bytes()
                assert data.startswith(b"Rar!\x1a\x07\x01\x00"), path
                # Prototype uses a plain stored payload even with encrypted headers.
                # Ensure every marked volume actually contains the producer record.
                assert (b"\nmarker-schema=1\n" in data) == (label == "marked"), path
        for engine in engines:
            if engine["name"] in completed:
                continue
            outcomes = {}
            for label, archive in archives.items():
                destination = root / "extracted" / engine["name"] / case / label
                destination.mkdir(parents=True, exist_ok=False)
                logs = root / "logs" / engine["name"] / case / label
                logs.mkdir(parents=True, exist_ok=True)
                env = os.environ.copy()
                env.update(engine.get("env", {}))
                test, extract = commands(engine, archive, destination)
                test_code = run(test, root, env, logs / "test.log")
                extract_code = run(extract, root, env, logs / "extract.log")
                actual = manifest(destination)
                outcomes[label] = {"test_exit": test_code, "extract_exit": extract_code,
                                   "contents_match": actual == wanted, "manifest": actual}
            good = lambda r: r["test_exit"] == 0 and r["extract_exit"] == 0 and r["contents_match"]
            control, marked = good(outcomes["control"]), good(outcomes["marked"])
            status = "PASS" if control and marked else "REGRESSION" if control else "CONTROL_FAIL"
            row = {"engine": engine["name"], "case": case, "status": status, **outcomes}
            report["results"].append(row)
            print(f"{engine['name']:20} {case:20} {status}", flush=True)
            (root / "report.json").write_text(json.dumps(report, indent=2) + "\n")
    raise SystemExit(any(row["status"] == "REGRESSION" for row in report["results"]))


if __name__ == "__main__":
    main()
