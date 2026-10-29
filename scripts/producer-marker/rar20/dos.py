#!/usr/bin/env python3
"""Paired marker tests with original DOS RAR/UnRAR 2.50 under DOSBox-X."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import random
import shutil
import subprocess
import struct

import importlib.util

spec = importlib.util.spec_from_file_location("rar20_probe", Path(__file__).with_name("probe.py"))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)
CASES, manifest, run = probe.CASES, probe.manifest, probe.run


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('control', 'marked', 'tools', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    source = root / 'INPUT'
    source.mkdir()
    (source / 'TEXT.TXT').write_bytes(b'Repeated producer-marker interoperability text.\n' * 500)
    (source / 'BINARY.BIN').write_bytes(random.Random(73).randbytes(12000))
    (source / 'EMPTY.TXT').write_bytes(b'')
    (source / 'CAFE.TXT').write_bytes('UTF-8 contents: caf\u00e9\n'.encode())
    expected = manifest(source)
    for exe in ('RAR.EXE', 'UNRAR.EXE'):
        shutil.copyfile(args.tools / exe, root / exe)
    batch = ['@echo off']
    cases = []
    def command(text, result, expected_exit=0):
        batch.append(text)
        batch.append(f'if errorlevel {expected_exit + 1} echo FAIL >> {result}')
        batch.append(f'if not errorlevel {expected_exit + 1} if errorlevel {expected_exit} echo PASS >> {result}')
        if expected_exit:
            batch.append(f'if not errorlevel {expected_exit} echo FAIL >> {result}')
    for i, (case, options) in enumerate(CASES.items()):
        wanted = {n: expected[n] for n in (['BINARY.BIN'] if 'volumes' in case else ['EMPTY.TXT'] if case == 'empty-file' else expected)}
        for label, writer in [('control', args.control), ('marked', args.marked)]:
            name = f'A{i:02}{label[0].upper()}'
            work = root / name
            work.mkdir()
            archive = work / 'ARC.RAR'
            assert run([str(writer.resolve()), 'a', '--format', 'rar20', *options, str(archive), *wanted], source, os.environ.copy(), work / 'create.log') == 0
            for path in list(work.iterdir()):
                if path.name != 'create.log' and path.name != path.name.upper():
                    path.rename(path.with_name(path.name.upper()))
            row = {'case': case, 'variant': label, 'directory': name, 'expected': wanted}
            cases.append(row)
            batch.append(f'cd \\{name}')
            for exe, prefix in [('RAR', 'R'), ('UNRAR', 'U')]:
                (work / prefix).mkdir()
                command(f'\\{exe} t -y -pprobe-password ARC.RAR > {prefix}TEST.LOG', f'{prefix}TEST.OK')
                command(f'\\{exe} x -y -pprobe-password ARC.RAR {prefix}\\ > {prefix}EXTR.LOG', f'{prefix}EXTR.OK')
            if case == 'comment':
                command('\\RAR cw -y ARC.RAR COMMENT.TXT > COMMENT.LOG', 'COMMENT.OK')
    for label in ('control', 'marked'):
        work = root / f'A00{label[0].upper()}'
        data = bytearray((work / 'ARC.RAR').read_bytes())
        offset = 7
        while True:
            kind, flags, size = struct.unpack_from('<BHH', data, offset + 2)
            length = struct.unpack_from('<I', data, offset + 7)[0] if flags & 0x8000 else 0
            if kind == 0x74 and length > 10:
                data[offset + size + length // 2] ^= 0x40
                break
            offset += size + length
        (work / 'BAD.RAR').write_bytes(data)
        batch.append(f'cd \\{work.name}')
        for exe, prefix in [('RAR', 'R'), ('UNRAR', 'U')]:
            command(f'\\{exe} t -y BAD.RAR > {prefix}BAD.LOG', f'{prefix}BAD.OK', 3)
        work = root / f'A03{label[0].upper()}'
        shutil.copyfile(work / 'ARC.RAR', work / 'UPDATE.RAR')
        (work / 'ADD.TXT').write_bytes(b'Added by RAR 2.50.\n')
        (work / 'UPD').mkdir()
        batch.append(f'cd \\{work.name}')
        command('\\RAR a -y UPDATE.RAR ADD.TXT > UPDATE.LOG', 'UPDATE.OK')
        command('\\RAR cw -y UPDATE.RAR AFTER.TXT > AFTER.LOG', 'AFTER.OK')
        command('\\RAR x -y UPDATE.RAR UPD\\ > UPDOUT.LOG', 'UPDOUT.OK')
    batch.append('exit')
    (root / 'GO.BAT').write_bytes(('\r\n'.join(batch) + '\r\n').encode())
    env = {**os.environ, 'SDL_VIDEODRIVER': 'dummy', 'SDL_AUDIODRIVER': 'dummy'}
    invocation = ['dosbox-x', '-silent', '-exit', '-nogui', '-nomenu', '-time-limit', '300',
                  '-c', 'config -set cpu cycles=max', '-c', f'mount c {root}', '-c', 'c:', '-c', 'go.bat', '-c', 'exit']
    result = subprocess.run(invocation, env=env, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=330)
    (root / 'dosbox.log').write_bytes(result.stdout)
    report = {'control_sha256': hashlib.sha256(args.control.read_bytes()).hexdigest(),
              'marked_sha256': hashlib.sha256(args.marked.read_bytes()).hexdigest(),
              'executables': {exe: hashlib.sha256((args.tools / exe).read_bytes()).hexdigest() for exe in ('RAR.EXE', 'UNRAR.EXE')},
              'results': []}
    for case in cases:
        work = root / case['directory']
        for exe, prefix in [('RAR', 'R'), ('UNRAR', 'U')]:
            test = (work / f'{prefix}TEST.OK').read_text().strip() == 'PASS'
            extract = (work / f'{prefix}EXTR.OK').read_text().strip() == 'PASS'
            actual = manifest(work / prefix)
            comment = case['case'] != 'comment' or (work / 'COMMENT.TXT').read_bytes() == b'User comment survives independently'
            row = {**case, 'engine': exe, 'test_ok': test, 'extract_ok': extract, 'contents_match': actual == case['expected'], 'comment_match': comment}
            row['pass'] = test and extract and row['contents_match'] and comment
            if not row['contents_match']:
                row['actual'] = actual
            report['results'].append(row)
            print(exe, case['case'], case['variant'], row['pass'], flush=True)
    report['negative'] = []
    report['updates'] = []
    for label in ('control', 'marked'):
        work = root / f'A00{label[0].upper()}'
        for exe, prefix in [('RAR', 'R'), ('UNRAR', 'U')]:
            assert (work / f'{prefix}BAD.OK').read_text().strip() == 'PASS'
            report['negative'].append({'engine': exe, 'variant': label, 'test_exit': 3})
        work = root / f'A03{label[0].upper()}'
        assert all((work / name).read_text().strip() == 'PASS' for name in ('UPDATE.OK', 'AFTER.OK', 'UPDOUT.OK'))
        assert (work / 'AFTER.TXT').read_bytes() == b'User comment survives independently'
        assert manifest(work / 'UPD') == {**expected, 'ADD.TXT': hashlib.sha256((work / 'ADD.TXT').read_bytes()).hexdigest()}
        report['updates'].append({'engine': 'RAR', 'variant': label, 'comment_preserved': True, 'contents_match': True,
                                  'marker_retained': b'\nmarker-schema=1\n' in (work / 'UPDATE.RAR').read_bytes()})
    (root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
    raise SystemExit(any(not r['pass'] for r in report['results']))


if __name__ == '__main__':
    main()
