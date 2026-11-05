from pathlib import Path
import json,array,math,hashlib
out=Path(__file__).resolve().parent
fixtures=[json.loads(x) for x in (out/'dev-corpus.jsonl').read_text().splitlines()]
req=[json.loads(x) for x in (out/'requests.jsonl').read_text().splitlines()];rq={(r['id'],r['condition']):r for r in req}
en=sorted([f for f in fixtures if f['language']=='en' and f['category']=='name_absent'],key=lambda f:f['id'])[0]
pl=sorted([f for f in fixtures if f['language']=='pl' and f['category']=='name_absent'],key=lambda f:f['id'])[0]
def samples(f):
 a=array.array('f');a.frombytes(Path(rq[f['id'],'off']['audio']).read_bytes());return a
x=samples(en);y=samples(pl);cases=[]
for gain in [.1,.01]:cases.append((f'quiet-gain-{gain}',array.array('f',(v*gain for v in x)),en['reference']))
mix=array.array('f',(0.55*(x[i] if i<len(x) else 0)+.45*(y[i] if i<len(y) else 0) for i in range(max(len(x),len(y)))))
cases.append(('mixedspeech-dev-two-speaker',mix,None));requests=[];manifest=[]
for label,a,reference in cases:
 p=out/(label+'.f32');p.write_bytes(a.tobytes());manifest.append(dict(id=label,reference=reference,source_ids=[en['id'],pl['id']] if reference is None else [en['id']],samples=len(a),rms=math.sqrt(sum(v*v for v in a)/len(a)),sha256=hashlib.sha256(p.read_bytes()).hexdigest(),purpose='dev-derived acoustic stress; overlay has no unambiguous word reference and no WER score'))
 for cond in ['off','relevant','distractor']:
  r=dict(rq[en['id'],cond],id=label,audio=str(p),stage='stress',languages=['en','pl']);requests.append(r)
(out/'stress-requests.jsonl').write_text(''.join(json.dumps(r,ensure_ascii=False)+'\n' for r in requests));(out/'stress-manifest.json').write_text(json.dumps(manifest,indent=2,ensure_ascii=False)+'\n')
