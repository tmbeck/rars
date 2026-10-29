#!/usr/bin/env python3
"""Prove the candidate changes only the main-header length and added extension."""
import argparse
import json
from pathlib import Path
import struct
from mark import RECORD


def unmark(data):
    assert data.startswith(b'RE~^')
    end = struct.unpack_from('<H',data,4)[0]
    assert data[end-len(RECORD):end] == RECORD
    return data[:4] + struct.pack('<H',end-len(RECORD)) + data[6:end-len(RECORD)] + data[end:]


def main():
    p=argparse.ArgumentParser()
    p.add_argument('matrix',type=Path)
    args=p.parse_args()
    checked=[]
    for marked in sorted((args.matrix/'archives').glob('rar14/*/marked/*')):
        if marked.name=='create.log':continue
        control=marked.parent.parent/'control'/marked.name
        assert unmark(marked.read_bytes())==control.read_bytes(),marked
        checked.append(str(marked.relative_to(args.matrix)))
    assert checked
    print(json.dumps({'files_checked':len(checked),'files':checked},indent=2))


if __name__=='__main__':main()
