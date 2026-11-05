from pathlib import Path
import json,importlib.util,sys,datetime,hashlib
out=Path(__file__).resolve().parent;repo=Path('/home/kuba001/.t3/projects/localwispr')
spec=importlib.util.spec_from_file_location('s',repo/'benchmarks/recognition/analyze.py');s=importlib.util.module_from_spec(spec);spec.loader.exec_module(s)
stage=sys.argv[1] if len(sys.argv)>1 else 'manual'
fixtures={r['id']:r for r in map(json.loads,(repo/'benchmarks/recognition/corpus.jsonl').read_text().splitlines())};names=sorted({t for f in fixtures.values() for t in f['targets']})
new=[json.loads(x) for x in (out/'results'/(stage+'.jsonl')).read_text().splitlines()]
old=[json.loads(x) for x in (out/'results/dev.jsonl' if stage=='manual' else repo/'benchmarks/recognition/results/heldout.jsonl').read_text().splitlines() if json.loads(x)['request']['strategy']=='source']
base={(r['request']['id'],r['request']['condition']):r for r in old}
final={}
for x in map(json.loads,Path('/tmp/localwispr-final-recognition/results/primary.jsonl').read_text().splitlines()):final[x['request']['id'],x['request']['condition']]=x
result={}
for cond in ['off','relevant','distractor']:
 pairs=[(base[r['request']['id'],cond],r) for r in new if r['request']['condition']==cond]
 before=[s.score(a,fixtures[a['request']['id']],names) for a,b in pairs];after=[s.score(b,fixtures[b['request']['id']],names) for a,b in pairs];parent=[s.score(final[a['request']['id'],cond],fixtures[a['request']['id']],names) for a,b in pairs]
 result[cond]={'old':s.aggregate(before),'new':s.aggregate(after),'parent_final':s.aggregate(parent),'improved':sum(b['word_errors']<a['word_errors'] for a,b in zip(before,after)),'regressed':sum(b['word_errors']>a['word_errors'] for a,b in zip(before,after)),'equal':sum(b['word_errors']==a['word_errors'] for a,b in zip(before,after)),'regressed_vs_parent':sum(b['word_errors']>a['word_errors'] for a,b in zip(parent,after)),'examples':[dict(id=b['id'],reference=b['reference'],old=a['hypothesis'],new=b['hypothesis'],old_errors=a['word_errors'],new_errors=b['word_errors'],parent_errors=p['word_errors']) for a,b,p in zip(before,after,parent)]}
 print(cond,'errors',result[cond]['old']['word_errors'],'->',result[cond]['new']['word_errors'],'parent',result[cond]['parent_final']['word_errors'],'improved/regressed',result[cond]['improved'],result[cond]['regressed'],'names',result[cond]['old']['target_literal_ci'],'->',result[cond]['new']['target_literal_ci'])
(out/'results'/(stage+'-metrics.json')).write_text(json.dumps(result,indent=2,ensure_ascii=False)+'\n')
qualifies=all(r['regressed']==0 and r['new']['word_errors']<r['old']['word_errors'] and r['new']['target_literal_ci']>=r['old']['target_literal_ci'] for r in result.values())
print('qualifies',qualifies)
if stage=='manual':
 decision={'frozen_at':datetime.datetime.now(datetime.timezone.utc).isoformat(),'qualifies':qualifies,'policy_sha256':hashlib.sha256((out/'manual-plan.json').read_bytes()).hexdigest(),'dev_results_sha256':hashlib.sha256((out/'results/manual.jsonl').read_bytes()).hexdigest(),'heldout':'once-only same exact rule on existing14 long-hint speech fixtures, all3 contexts; no retuning' if qualifies else 'not run'}
 (out/'manual-decision.json').write_text(json.dumps(decision,indent=2)+'\n')
 if qualifies:
  req=[]
  for r in map(json.loads,(repo/'benchmarks/recognition/results/heldout-requests.jsonl').read_text().splitlines()):
   if r['manual'] and r['strategy']=='source':r['normalize_long_prompt']=True;r['stage']='manual_heldout_validation';req.append(r)
  assert len(req)==42
  (out/'manual-heldout-requests.jsonl').write_text(''.join(json.dumps(r,ensure_ascii=False)+'\n' for r in req))
