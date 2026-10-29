#!/usr/bin/env python3
"""WinRAR 1.54 GUI extraction probe. Run in an isolated Xvfb and win32 prefix.

Requires wine32, libX11, libXtst and ImageMagick import. Uses fixed coordinates
for the original 1.54 beta GUI at 1280x1024 without a window manager. Extraction
hashes are checked automatically; inspect Test/comment/corruption screenshots
before claiming those checks pass. Never infer archive success from GUI exit.
"""
import argparse
import ctypes as C
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import time


class Input:
    def __init__(self):
        self.x = C.CDLL('libX11.so.6')
        self.t = C.CDLL('libXtst.so.6')
        self.x.XOpenDisplay.restype = C.c_void_p
        self.display = self.x.XOpenDisplay(None)
        if not self.display:
            raise RuntimeError('Run under xvfb-run at 1280x1024')
        self.x.XFlush.argtypes = [C.c_void_p]
        self.x.XKeysymToKeycode.argtypes = [C.c_void_p, C.c_ulong]
        self.x.XStringToKeysym.argtypes = [C.c_char_p]
        self.x.XStringToKeysym.restype = C.c_ulong
        self.t.XTestFakeKeyEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        self.t.XTestFakeButtonEvent.argtypes = [C.c_void_p, C.c_uint, C.c_int, C.c_ulong]
        self.t.XTestFakeMotionEvent.argtypes = [C.c_void_p, C.c_int, C.c_int, C.c_int, C.c_ulong]

    def keys(self, *actions):
        for action in actions:
            if action.startswith('click:'):
                x, y = map(int, action[6:].split(','))
                self.t.XTestFakeMotionEvent(self.display, -1, x, y, 0)
                self.t.XTestFakeButtonEvent(self.display, 1, 1, 0)
                self.t.XTestFakeButtonEvent(self.display, 1, 0, 0)
            else:
                codes = [self.x.XKeysymToKeycode(self.display, self.x.XStringToKeysym(k.encode()))
                         for k in action.split('+')]
                for code in codes:
                    self.t.XTestFakeKeyEvent(self.display, code, 1, 0)
                for code in reversed(codes):
                    self.t.XTestFakeKeyEvent(self.display, code, 0, 0)
            self.x.XFlush(self.display)
            time.sleep(.15)


def manifest(directory):
    return {str(p.relative_to(directory)): hashlib.sha256(p.read_bytes()).hexdigest()
            for p in sorted(directory.rglob('*')) if p.is_file()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('dos', 'exe', 'prefix', 'output'):
        parser.add_argument('--' + name, type=Path, required=True)
    args = parser.parse_args()
    root, corpus, prefix = args.output.resolve(), args.dos.resolve(), args.prefix.resolve()
    root.mkdir(parents=True, exist_ok=False)
    work = prefix / 'drive_c/R15GUI'
    work.mkdir(exist_ok=False)  # Refuse stale output from a previous run.
    env = {**os.environ, 'WINEPREFIX': str(prefix), 'WINEARCH': 'win32',
           'WINELOADER': '/usr/bin/wine32', 'WINEDEBUG': '-all'}
    ui = Input()
    cases = {row['directory']: row for row in json.loads((corpus / 'report.json').read_text())['results']}
    report = {'executable_sha256': hashlib.sha256(args.exe.read_bytes()).hexdigest(),
              'results': [], 'negative': [], 'screenshots_require_review': True}

    def shot(name):
        path = root / (name + '.png')
        subprocess.run(['import', '-window', 'root', str(path)], check=True)
        return {'file': path.name, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}

    for name, case in sorted(cases.items()):
        source, arc, out = corpus / name, work / name, work / (name + 'OUT')
        arc.mkdir()
        out.mkdir()
        for path in source.iterdir():
            if path.name == 'ARC.RAR' or (path.suffix.startswith('.R') and path.suffix[2:].isdigit()):
                shutil.copyfile(path, arc / path.name)
        volume = 'volumes' in case['case']
        password = 'password' in case['case']
        row = {'case': case['case'], 'variant': case['variant'],
               'archive_sha256': hashlib.sha256((arc / 'ARC.RAR').read_bytes()).hexdigest()}
        with (root / (name + '.log')).open('wb') as log:
            proc = subprocess.Popen(['/usr/bin/wine32', str(args.exe.resolve()),
                                     f'C:\\R15GUI\\{name}\\ARC.RAR'], cwd=out, env=env,
                                    stdout=log, stderr=log)
            try:
                time.sleep(3)
                if proc.poll() is not None:
                    raise RuntimeError('WinRAR exited before GUI interaction')
                if case['case'] == 'comment':
                    row['comment_screenshot'] = shot(name + '-comment')
                    ui.keys('Escape')
                for action, y in [('test', 58), ('extract', 78)]:
                    ui.keys('click:300,134', 'Shift_L+End', f'click:124,{y}')
                    time.sleep(1)
                    if volume:
                        ui.keys('Return')
                        time.sleep(1)
                    if password:
                        ui.keys(*list('probe'), 'minus', *list('password'), 'Return')
                        time.sleep(1)
                        if not volume:
                            ui.keys('click:557,655')  # Yes to all subsequent members.
                    time.sleep(1)
                    row[action + '_screenshot'] = shot(name + '-' + action)
                    ui.keys('Escape')
                row['actual'] = manifest(out)
                row['expected'] = case['expected']
                row['contents_match'] = row['actual'] == row['expected']
                ui.keys('Alt_L+F4')
                proc.wait(timeout=5)
            finally:
                if proc.poll() is None:
                    proc.terminate()
            report['results'].append(row)
            (root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')
            print(name, row['contents_match'], flush=True)
            if not row['contents_match']:
                raise RuntimeError(f'{name}: inspect screenshots before proceeding')
    # A deliberately corrupted stored member must produce an error dialog.
    for variant in ('C', 'M'):
        name = 'A00' + variant
        shutil.copyfile(corpus / name / 'BAD.RAR', work / name / 'BAD.RAR')
        with (root / (name + '-negative.log')).open('wb') as log:
            proc = subprocess.Popen(['/usr/bin/wine32', str(args.exe.resolve()),
                                     f'C:\\R15GUI\\{name}\\BAD.RAR'], cwd=work, env=env,
                                    stdout=log, stderr=log)
            try:
                time.sleep(3)
                ui.keys('click:300,134', 'Shift_L+End', 'click:124,58')
                time.sleep(2)
                report['negative'].append({'variant': variant,
                                           'screenshot': shot(name + '-negative')})
                ui.keys('Escape', 'Alt_L+F4')
                proc.wait(timeout=5)
            finally:
                if proc.poll() is None:
                    proc.terminate()
    (root / 'report.json').write_text(json.dumps(report, indent=2) + '\n')


if __name__ == '__main__':
    main()
