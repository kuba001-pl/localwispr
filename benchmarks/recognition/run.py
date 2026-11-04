#!/usr/bin/env python3
"""One serial GPU worker; every stage freezes its requests before execution."""
import argparse
import datetime
import hashlib
import json
from pathlib import Path
import re
import subprocess

OUT=Path(__file__).resolve().parent
MANUAL='ordinary vocabulary '*280+'Manual note: keep the wording natural.'

def alternative(strategy,before,after,manual):
    # Generic context-only strategies: no corpus brand list, references, target
    # annotations, ASR output or substitutions are consulted here.
    if strategy=='prose320':
        prompt=before[-160:]+' '+after[:160]
    else:
        tokens=lambda s:re.findall(r"[\w.+#@$/:-]+",s,flags=re.UNICODE)
        left=tokens(before);right=tokens(after);nearest=[]
        for i in range(max(len(left),len(right))):
            for w in ([left[-i-1]] if i<len(left) else [])+([right[i]] if i<len(right) else []):
                w=w.strip('.,:-')
                if not w or w in nearest:continue
                if strategy=='case16' and not any(c.isupper() or c.isdigit() for c in w):continue
                nearest.append(w)
        limit=16 if strategy=='case16' else 12
        selected=[];chars=0
        for word in nearest:
            if len(selected)<limit and chars+len(word)+2<=160:
                selected.append(word);chars+=len(word)+2
        prompt=', '.join(reversed(selected))
    return prompt+('\n'+manual if manual else '')

def requests(stage,rows):
    req=[]
    for r in rows:
        if stage=='dev_manual' and not (r['split']=='dev' and r['long_hint']):continue
        if stage=='dev' and r['split']!='dev':continue
        if stage=='heldout' and r['split']!='heldout':continue
        if stage=='balanced' and not r['balanced_subset']:continue
        if stage=='repeat' and not r['repeat_subset']:continue
        strategies=['source','case16','prose320','short12'] if stage=='dev' else ['source']
        conditions=['off','relevant','distractor']
        # Counterbalance context ordering by frozen id; repeated trials reverse.
        shift=int(hashlib.sha256(r['id'].encode()).hexdigest()[:8],16)%3
        conditions=conditions[shift:]+conditions[:shift]
        if stage=='repeat':conditions=list(reversed(conditions))
        for strategy in strategies:
            for cond in conditions:
                if cond=='off' and strategy!='source':continue
                before=r['context_before'] if cond=='relevant' else r['distractor_before']
                after=r['context_after'] if cond=='relevant' else r['distractor_after']
                manual=MANUAL if r['long_hint'] and stage!='dev_manual' else ''
                v={'id':r['id'],'condition':cond,'strategy':strategy,
                   'accuracy':'Balanced' if stage=='balanced' else 'Fast',
                   'languages':r['languages'],'audio':str(OUT/'audio'/(r['id']+'.f32')),
                   'before':before if cond!='off' else '', 'after':after if cond!='off' else '',
                   'manual':manual,'stage':stage}
                if strategy!='source':v['override_prompt']=alternative(strategy,before,after,manual)
                req.append(v)
    return req

def main():
    p=argparse.ArgumentParser();p.add_argument('stage',choices=['dev','heldout','balanced','repeat','dev_manual']);args=p.parse_args()
    plan=json.loads((OUT/'plan.json').read_text())
    assert hashlib.sha256((OUT/'corpus.jsonl').read_bytes()).hexdigest()==plan['freeze_sha256']
    for file,sha in json.loads((OUT/'frozen/source-hashes.json').read_text()).items():
        assert hashlib.sha256((OUT/'frozen'/file).read_bytes()).hexdigest()==sha,file
    if args.stage!='dev':
        assert (OUT/'results/dev-decision.json').exists(),'Analyze and lock dev decision before heldout/secondary runs'
    if args.stage=='dev_manual':
        assert (OUT/'dev-manual-plan.json').exists(),'Freeze supplemental ablation plan before inference'
    result=OUT/'results'/(args.stage+'.jsonl')
    if result.exists():raise SystemExit(f'Refusing to overwrite {result}')
    rows=[json.loads(s) for s in (OUT/'corpus.jsonl').read_text().splitlines()]
    req=requests(args.stage,rows)
    reqpath=OUT/'results'/(args.stage+'-requests.jsonl')
    reqpath.write_text(''.join(json.dumps(v,ensure_ascii=False)+'\n' for v in req))
    meta={'started_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
          'requests_sha256':hashlib.sha256(reqpath.read_bytes()).hexdigest(),'requests':len(req),
          'corpus_sha256':plan['freeze_sha256'],'binary_sha256':hashlib.sha256((OUT/'recognition-harness').read_bytes()).hexdigest(),
          'tool_hashes':{p.name:hashlib.sha256(p.read_bytes()).hexdigest() for p in OUT.glob('*.py')}}
    meta_path=OUT/'results'/(args.stage+'-run.json');meta_path.write_text(json.dumps(meta,indent=2)+'\n')
    with (OUT/'results'/(args.stage+'.stderr.log')).open('w') as err,result.open('w') as f:
        worker=subprocess.Popen([str(OUT/'recognition-harness')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=err,text=True)
        try:
            for i,v in enumerate(req):
                worker.stdin.write(json.dumps(v,ensure_ascii=False)+'\n');worker.stdin.flush()
                line=worker.stdout.readline()
                if not line:raise RuntimeError(f'Worker exited {worker.poll()}, request {i}; see stderr')
                row=json.loads(line);assert row['request']==v
                f.write(line);f.flush()
                if i%25==0 or i+1==len(req):
                    print(f'{args.stage} {i+1}/{len(req)} {v["id"]} {v["strategy"]}/{v["condition"]}',flush=True)
            worker.stdin.close();assert worker.wait(timeout=60)==0
        finally:
            if worker.poll() is None:worker.kill();worker.wait()
    meta.update(completed_utc=datetime.datetime.now(datetime.timezone.utc).isoformat(),
                results_sha256=hashlib.sha256(result.read_bytes()).hexdigest())
    meta_path.write_text(json.dumps(meta,indent=2)+'\n')

if __name__=='__main__':main()
