#!/usr/bin/env python3
"""Compare old-format archive updates, retaining baseline failures as evidence."""
import argparse
import json
import os
from pathlib import Path
import shutil
import sys
sys.path.insert(0,str(Path(__file__).resolve().parents[1]))
from probe import commands, manifest, run, windows


def main():
    p=argparse.ArgumentParser()
    for n in ['matrix','engines','output']:p.add_argument('--'+n,type=Path,required=True)
    a=p.parse_args();root=a.output.resolve();root.mkdir(parents=True,exist_ok=False)
    rows=[]
    for e in json.loads(a.engines.read_text()):
        for case in ['stored','comment']:
            for label in ['control','marked']:
                work=root/e['name']/case/label;work.mkdir(parents=True)
                arc=work/'archive.rar';shutil.copyfile(a.matrix/'archives/rar14'/case/label/'archive.rar',arc)
                env={**os.environ,**e.get('env',{})};conv=windows if e.get('windows') else str
                def call(args,log):return run(e['command']+args,work,env,work/log)
                row={'engine':e['name'],'case':case,'variant':label}
                if case=='comment':
                    row['before_exit']=call(['cw','-y',conv(arc),'before.txt'],'before.log')
                    row['before_comment']=(work/'before.txt').read_text(errors='replace') if (work/'before.txt').exists() else None
                (work/'added.txt').write_bytes(b'Added by reference RAR.\n')
                row['update_exit']=call(['a','-y',conv(arc),'added.txt'],'update.log')
                if case=='comment':
                    row['after_exit']=call(['cw','-y',conv(arc),'after.txt'],'after.log')
                    row['after_comment']=(work/'after.txt').read_text(errors='replace') if (work/'after.txt').exists() else None
                out=work/'out';out.mkdir();test,extract=commands(e,arc,out)
                row['test_exit']=run(test,work,env,work/'test.log');row['extract_exit']=run(extract,work,env,work/'extract.log')
                row['contents_match']=manifest(out)=={**manifest(a.matrix/'input'),'added.txt':manifest(work)['added.txt']}
                row['marker_retained']=b'\nmarker-schema=1\n' in arc.read_bytes()
                rows.append(row);(root/'report.json').write_text(json.dumps(rows,indent=2)+'\n');print(row,flush=True)


if __name__=='__main__':main()
