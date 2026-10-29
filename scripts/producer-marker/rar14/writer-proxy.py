#!/usr/bin/env python3
"""Run an unmodified writer, then append the candidate to each main header."""
import os
from pathlib import Path
import subprocess
import sys
from mark import mark

writer = Path(os.environ.get('RARS_CONTROL', 'target/producer-marker-rar14/rars-control'))
if not writer.is_absolute():
    writer = Path(__file__).resolve().parents[3] / writer
args = sys.argv[1:]
assert args[0] == 'a' and args[args.index('--format') + 1] == 'rar14'
result = subprocess.run([str(writer), *args])
if result.returncode:
    sys.exit(result.returncode)
archive = next(Path(v) for v in args if Path(v).is_absolute() and v.lower().endswith('.rar'))
for path in archive.parent.glob(archive.stem + '.*'):
    if path.suffix.lower() == '.rar' or (path.suffix.lower().startswith('.r') and path.suffix[2:].isdigit()):
        path.write_bytes(mark(path.read_bytes()))
