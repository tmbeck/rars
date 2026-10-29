#!/usr/bin/env python3
"""Test the main-header candidate with original DOS readers, including RAR 1.402."""
import argparse
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import shutil
import struct
import subprocess

spec = importlib.util.spec_from_file_location('rar14_probe', Path(__file__).with_name('probe.py'))
probe = importlib.util.module_from_spec(spec)
spec.loader.exec_module(probe)


def main():
    parser = argparse.ArgumentParser()
    for arg in ['control', 'marked', 'engines', 'output']:
        parser.add_argument('--' + arg, type=Path, required=True)
    args = parser.parse_args()
    root = args.output.resolve()
    root.mkdir(parents=True, exist_ok=False)
    source = root / 'INPUT'
    source.mkdir()
    import random
    for name, data in {'TEXT.TXT': b'Repeated producer-marker interoperability text.\n' * 500,
                       'BINARY.BIN': random.Random(73).randbytes(12000),
                       'EMPTY.TXT': b'', 'CAFE.TXT': 'UTF-8 contents: caf\u00e9\n'.encode()}.items():
        (source / name).write_bytes(data)
    expected = probe.manifest(source)
    engines = json.loads(args.engines.read_text())
    for e in engines:
        shutil.copyfile(e['path'], root / (e['name'] + '.EXE'))
        e['sha256'] = hashlib.sha256(Path(e['path']).read_bytes()).hexdigest()
    batch = ['@echo off']
    cases = []
    def command(cmd, result, code=0):
        batch.extend([cmd, f'if errorlevel {code + 1} echo FAIL >> {result}',
                      f'if not errorlevel {code + 1} if errorlevel {code} echo PASS >> {result}'])
        if code:
            batch.append(f'if not errorlevel {code} echo FAIL >> {result}')
    for i, (case, options) in enumerate(probe.CASES.items()):
        wanted = {n: expected[n] for n in (['BINARY.BIN'] if 'volumes' in case else ['EMPTY.TXT'] if case == 'empty-file' else expected)}
        for variant, writer in [('control', args.control), ('marked', args.marked)]:
            name = f'A{i:02}{variant[0].upper()}'
            work = root / name
            work.mkdir()
            assert probe.run([str(writer.resolve()), 'a', '--format', 'rar14', *options,
                              str(work / 'ARC.RAR'), *wanted], source, os.environ.copy(), work / 'create.log') == 0
            for p in list(work.iterdir()):
                if p.name != 'create.log' and p.name != p.name.upper():
                    p.rename(p.with_name(p.name.upper()))
            cases.append({'case': case, 'variant': variant, 'directory': name, 'expected': wanted})
            batch.append(f'cd \\{name}')
            for e in engines:
                tag = e['name']
                (work / tag).mkdir()
                switch = '-o+' if e['unrar'] else '-y'
                command(f'\\{tag} t {switch} -pprobe-password ARC.RAR > {tag}T.LOG', f'{tag}T.OK')
                command(f'\\{tag} x {switch} -pprobe-password ARC.RAR {tag}\\ > {tag}X.LOG', f'{tag}X.OK')
                if case == 'comment' and not e['unrar']:
                    command(f'\\{tag} cw -y ARC.RAR {tag}.TXT > {tag}C.LOG', f'{tag}C.OK')
    for variant in ['C', 'M']:
        work = root / ('A00' + variant)
        data = bytearray((work / 'ARC.RAR').read_bytes())
        offset = struct.unpack_from('<H', data, 4)[0]
        packed = struct.unpack_from('<I', data, offset)[0]
        header = struct.unpack_from('<H', data, offset + 10)[0]
        assert packed > 10
        data[offset + header + packed // 2] ^= 0x40
        (work / 'BAD.RAR').write_bytes(data)
        batch.append(f'cd \\A00{variant}')
        for e in engines:
            tag = e['name']
            switch = '-o+' if e['unrar'] else '-y'
            command(f'\\{tag} t {switch} BAD.RAR > {tag}B.LOG', f'{tag}B.OK', 3)
        for i in [0, 3]:
            work = root / f'A{i:02}{variant}'
            shutil.copyfile(work / 'ARC.RAR', work / 'UPDATE.RAR')
            (work / 'ADD.TXT').write_bytes(b'Added by RAR 1.402.\n')
            (work / 'UPD').mkdir()
            batch.append(f'cd \\{work.name}')
            command('\\R140 a -y UPDATE.RAR ADD.TXT > UPDATE.LOG', 'UPDATE.OK')
            command('\\R140 t -y UPDATE.RAR > UPTEST.LOG', 'UPTEST.OK')
            command('\\R140 x -y UPDATE.RAR UPD\\ > UPX.LOG', 'UPX.OK')
            if i == 3:
                command('\\R140 cw -y UPDATE.RAR AFTER.TXT > AFTER.LOG', 'AFTER.OK')
    batch.append('exit')
    (root / 'GO.BAT').write_bytes(('\r\n'.join(batch) + '\r\n').encode())
    r = subprocess.run(['dosbox-x', '-silent', '-exit', '-nogui', '-nomenu', '-time-limit', '600',
                        '-c', 'config -set cpu cycles=max', '-c', f'mount c {root}', '-c', 'c:',
                        '-c', 'go.bat', '-c', 'exit'], env={**os.environ, 'SDL_VIDEODRIVER': 'dummy', 'SDL_AUDIODRIVER': 'dummy'},
                       stdout=subprocess.PIPE, stderr=subprocess.STDOUT, timeout=630)
    (root / 'dosbox.log').write_bytes(r.stdout)
    report = {'engines': engines, 'results': [], 'negative': [], 'updates': []}
    def ok(work, name):
        return (work / name).exists() and (work / name).read_text().strip() == 'PASS'
    for case in cases:
        work = root / case['directory']
        for e in engines:
            tag = e['name']
            actual = probe.manifest(work / tag)
            comment = case['case'] != 'comment' or e['unrar'] or (work / (tag + '.TXT')).exists() and (work / (tag + '.TXT')).read_bytes() == b'User comment survives independently'
            row = {**case, 'engine': tag, 'test_ok': ok(work, tag+'T.OK'), 'extract_ok': ok(work, tag+'X.OK'), 'contents_match': actual == case['expected'], 'comment_match': comment}
            row['pass'] = all(row[k] for k in ['test_ok','extract_ok','contents_match','comment_match'])
            report['results'].append(row)
            print(tag, case['case'], case['variant'], row['pass'], flush=True)
    for variant in ['C','M']:
        for e in engines:
            report['negative'].append({'variant':variant,'engine':e['name'],'crc_error':ok(root/('A00'+variant),e['name']+'B.OK')})
        for i in [0,3]:
            work = root / f'A{i:02}{variant}'
            data = (work/'UPDATE.RAR').read_bytes()
            report['updates'].append({'case':'comment' if i==3 else 'stored','variant':variant,'commands_ok':all(ok(work,n) for n in ['UPDATE.OK','UPTEST.OK','UPX.OK']), 'contents_match':probe.manifest(work/'UPD')=={**expected,'ADD.TXT':hashlib.sha256((work/'ADD.TXT').read_bytes()).hexdigest()},'comment_match':i!=3 or (work/'AFTER.TXT').exists() and (work/'AFTER.TXT').read_bytes()==b'User comment survives independently','marker_retained':b'\nmarker-schema=1\n' in data})
    (root/'report.json').write_text(json.dumps(report,indent=2)+'\n')


if __name__ == '__main__':
    main()
