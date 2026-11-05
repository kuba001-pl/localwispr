#!/usr/bin/env python3
"""Score paired real-speech negative controls; punctuation/case do not count in WER."""
import collections,json,pathlib,re,sys
p=pathlib.Path(sys.argv[1])
rows=[json.loads(line) for line in p.read_text().splitlines()]
def words(s): return re.findall(r"[^\W_]+(?:['’][^\W_]+)?",s.lower())
def distance(a,b):
 row=list(range(len(b)+1))
 for i,x in enumerate(a,1):
  nxt=[i]
  for j,y in enumerate(b,1): nxt.append(min(row[j]+1,nxt[j-1]+1,row[j-1]+(x!=y)))
  row=nxt
 return row[-1]
by_id=collections.defaultdict(dict);scores=collections.defaultdict(lambda:dict(words=0,errors=0,clips=0))
for r in rows:
 req=r['input']; mode=req['mode']; ref=words(req['reference']); hyp=words(r['result']['text']);e=distance(ref,hyp)
 scores[mode]['words']+=len(ref);scores[mode]['errors']+=e;scores[mode]['clips']+=1
 by_id[req['id']][mode]=(e,r['result']['text'],req)
counts=collections.Counter();changes=[]
for ident,pair in by_id.items():
 if set(pair)!={'off','distractor'}: raise ValueError(f'Missing pair {ident}')
 a,b=pair['off'],pair['distractor'];counts['improved' if b[0]<a[0] else 'regressed' if b[0]>a[0] else 'equal_errors']+=1
 if a[1]!=b[1]:changes.append(dict(id=ident,reference=a[2]['reference'],off=a[1],distractor=b[1],off_errors=a[0],distractor_errors=b[0]))
for s in scores.values():s['wer']=s['errors']/s['words']
result=dict(metrics=dict(scores),paired=dict(counts),changes=changes,limitation='73 clean read-English clips from one speaker; no general population accuracy claim')
p.with_suffix('.metrics.json').write_text(json.dumps(result,ensure_ascii=False,indent=2)+'\n')
print(json.dumps({k:v for k,v in result.items() if k!='changes'},indent=2))
