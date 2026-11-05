#!/usr/bin/env python3
"""Reuse installed release Rust/native dependencies; never builds production."""
import hashlib
import json
from pathlib import Path
import subprocess

OUT=Path(__file__).resolve().parent
ROOT=OUT.parents[1]

def main():
    fingerprints=ROOT/'target/release/.fingerprint'
    candidates=sorted(fingerprints.glob('localwispr-*/bin-localwispr.json'))
    assert candidates, 'Build localwispr --release first (CUDA default)'
    app=json.loads(candidates[-1].read_text())
    args=['rustc','--edition=2024','--crate-name','recognition_benchmark','-C','opt-level=1',
          '-C','panic=abort','-L','dependency='+str(ROOT/'target/release/deps')]
    required={'anyhow','serde','serde_json','toml','dirs','crossbeam_channel','tokio','whisper_rs','tracing'}
    resolved={}
    for _,name,_,n in app['deps']:
        if name not in required: continue
        matches=[]
        for p in fingerprints.glob('*/lib-'+name):
            if int.from_bytes(bytes.fromhex(p.read_text().strip()),'little')==n:
                lib=ROOT/'target/release/deps'/('lib'+name+'-'+p.parent.name.rsplit('-',1)[1]+'.rlib')
                if lib.exists(): matches.append(lib)
        assert len(matches)==1,(name,matches)
        args+=['--extern',name+'='+str(matches[0])]
        resolved[name]={'path':str(matches[0]),'sha256':hashlib.sha256(matches[0].read_bytes()).hexdigest()}
    assert set(resolved)==required,required-set(resolved)
    search=set()
    for p in (ROOT/'target/release/build').glob('whisper-rs-sys-*/output'):
        for line in p.read_text().splitlines():
            if line.startswith('cargo:rustc-link-search='): search.add(line.split('=',1)[1])
    for value in sorted(search): args+=['-L',value]
    args+=['-L','native=/opt/cuda/lib64','-C','link-arg=-Wl,-rpath,/opt/cuda/lib64',str(OUT/'harness.rs'),'-o',str(OUT/'recognition-harness')]
    (OUT/'build-command.json').write_text(json.dumps({'command':args,'selected_fingerprint':str(candidates[-1]),'dependencies':resolved},indent=2)+'\n')
    with (OUT/'results/build.log').open('w') as log: subprocess.run(args,stdout=log,stderr=subprocess.STDOUT,check=True)
    print('Built frozen recognition harness')

if __name__=='__main__': main()
