from pathlib import Path
import json,re,math,difflib,sys,importlib.util,statistics,itertools
OUT=Path(__file__).resolve().parent;REPO=Path('/home/kuba001/.t3/projects/localwispr')
spec=importlib.util.spec_from_file_location('score',REPO/'benchmarks/recognition/analyze.py');score=importlib.util.module_from_spec(spec);spec.loader.exec_module(score)
fixtures={r['id']:r for r in map(json.loads,(OUT/'dev-corpus.jsonl').read_text().splitlines())}
names=sorted({n for f in fixtures.values() for n in f['targets']+f['decoys']})
WORD=re.compile(r'[^\W_]+',re.UNICODE)
def wordspans(s):return list(WORD.finditer(s))
def words(s):return [m.group().casefold() for m in wordspans(s)]
def geo(v):return math.exp(statistics.mean(math.log(max(p,1e-10)) for p in v)) if v else 1.
def enrich(row):
 data=bytearray();spans=[];segmeans=[]
 for seg in (row.get('confidence') or {}).get('segments',[]):
  if seg['no_speech']>.85:continue
  probs=[t['p'] for t in seg['tokens'] if t['text_token']]
  segmeans.append(geo(probs))
  for t in seg['tokens']:
   if not t['text_token']:continue
   start=len(data);data.extend(t['bytes']);spans.append((start,len(data),t['p']))
 raw=data.decode('utf-8',errors='replace');rawwords=wordspans(raw);cw=[]
 for m in rawwords:
  a=len(raw[:m.start()].encode());b=len(raw[:m.end()].encode());p=[p for x,y,p in spans if x<b and y>a]
  cw.append({'min':min(p,default=1.),'geo':geo(p)})
 ws=words(row['text']);mapped=[{'min':1.,'geo':1.} for _ in ws]
 for a,b,n in difflib.SequenceMatcher(None,words(raw),ws,autojunk=False).get_matching_blocks():
  for k in range(n):mapped[b+k]=cw[a+k]
 row['_w']=ws;row['_p']=mapped;row['_span']=wordspans(row['text']);row['_sentence_geo']=geo([t['p'] for seg in (row.get('confidence') or {}).get('segments',[]) for t in seg['tokens'] if t['text_token']]);row['_min']=min([p['min'] for p in mapped],default=1.)
 row['_segment_min']=min(segmeans,default=1.)
 return row

def edited_output(a,b,kind,t,metric='min',vocab=False,floor=0.,margin=-1.,protect_case=True):
 if kind=='off':return a['text'],False
 if kind=='source':return b['text'],False
 if kind.startswith('conditional'):
  field={'conditional_min':'_min','conditional_geo':'_sentence_geo','conditional_segment':'_segment_min'}[kind]
  decode=a[field]<t
  return (b['text'] if decode else a['text']),decode
 # Diff gates are simulated two-pass strategies: all context-on decodes paid.
 ops=difflib.SequenceMatcher(None,a['_w'],b['_w'],autojunk=False).get_opcodes()
 hint=set(words(b['prompt']));replacements=[];accept=[]
 for op,i,j,k,l in ops:
  if op=='equal':continue
  base=[p[metric] for p in a['_p'][i:j]];candidate=[p[metric] for p in b['_p'][k:l]]
  # Insertions/deletions cannot be shown acoustically superior by token confidence.
  ok=op=='replace' and bool(base) and bool(candidate)
  ok=ok and max(base)<t and min(candidate)>=floor and geo(candidate)-geo(base)>=margin
  if vocab:ok=ok and all(w in hint for w in b['_w'][k:l])
  accept.append(ok)
  if ok:
   bs=a['_span'];cs=b['_span'];replacements.append((bs[i].start(),bs[j-1].end(),b['text'][cs[k].start():cs[l-1].end()]))
 if kind=='whole':return (b['text'] if accept and all(accept) else a['text']),True
 text=a['text']
 for x,y,s in reversed(replacements):text=text[:x]+s+text[y:]
 return text,True

def summarize(rows):
 agg=score.aggregate(rows);keep=['unique_sentences','words','word_errors','wer','target_literal_ci','target_exact_case','targets','control_lexical_false_clips','control_case_only_false_clips','ordinary_control_clips','any_lexical_false_brand_clips','acoustic_nonempty','acoustic_clips','latency_p50_ms','latency_p95_ms','summed_submit_ms']
 return {k:agg[k] for k in keep}
def evaluate(data,params):
 groups={};outputs=[]
 for row in data:groups.setdefault(row['request']['id'],{})[row['request']['condition']]=row
 for g in groups.values():
  a=g['off']
  for cond in ['relevant','distractor']:
   b=g[cond];out,decode=edited_output(a,b,**params);row=dict(b,text=out)
   row['submit_ms']=a['submit_ms']+(b['submit_ms'] if decode else 0) if params['kind']!='source' else b['submit_ms']
   sc=score.score(row,fixtures[b['request']['id']],names);sc['decode_extra']=decode;sc['text']=out;sc['baseline_errors']=score.distance(score.tokens(fixtures[b['request']['id']]['reference']),score.tokens(a['text']));sc['source_errors']=score.distance(score.tokens(fixtures[b['request']['id']]['reference']),score.tokens(b['text']));outputs.append(sc)
 res={cond:summarize([r for r in outputs if cond=='pooled' or r['condition']==cond]) for cond in ['relevant','distractor','pooled']}
 res.update(params=params,extra_decodes=sum(r['decode_extra'] for r in outputs),speech_pairs_worse_off=sum(r['word_errors']>r['baseline_errors'] for r in outputs if r['category']!='acoustic'),speech_pairs_worse_source=sum(r['word_errors']>r['source_errors'] for r in outputs if r['category']!='acoustic'))
 return res,outputs

def main():
 data=[enrich(json.loads(x)) for x in (OUT/'results/dev.jsonl').read_text().splitlines()];assert len(data)==288
 old=[json.loads(x) for x in (REPO/'benchmarks/recognition/results/dev.jsonl').read_text().splitlines() if json.loads(x)['request']['strategy']=='source'];base={(r['request']['id'],r['request']['condition']):r for r in old};changes=[{'id':r['request']['id'],'condition':r['request']['condition'],'old':base[r['request']['id'],r['request']['condition']]['text'],'new':r['text']} for r in data if r['text']!=base[r['request']['id'],r['request']['condition']]['text']];(OUT/'results/instrumentation-parity.json').write_text(json.dumps(changes,indent=2,ensure_ascii=False))
 configs=[{'kind':k,'t':1.} for k in ['off','source']]
 for kind,t in itertools.product(['conditional_min','conditional_geo','conditional_segment'],[.1,.2,.3,.4,.5,.6,.7,.8,.9,.95]):configs.append(dict(kind=kind,t=t))
 for kind,t,metric,vocab in itertools.product(['whole','merge'],[.1,.2,.3,.4,.5,.6,.7,.8,.9,.95],['min','geo'],[False,True]):configs.append(dict(kind=kind,t=t,metric=metric,vocab=vocab))
 for kind,t,floor,margin in itertools.product(['whole','merge'],[.3,.5,.7,.9],[.3,.5,.7],[0.,.1]):configs.append(dict(kind=kind,t=t,metric='min',vocab=True,floor=floor,margin=margin))
 results=[]
 for config in configs:
  res,_=evaluate(data,config);results.append(res)
 qualifiers=[r for r in results if r['relevant']['target_literal_ci']>=29 and r['pooled']['target_literal_ci']>=44 and r['pooled']['control_lexical_false_clips']==0 and r['pooled']['control_case_only_false_clips']<=8 and r['pooled']['word_errors']<=792]
 (OUT/'results/grid.json').write_text(json.dumps(results,indent=2))
 (OUT/'results/qualifiers.json').write_text(json.dumps(qualifiers,indent=2))
 print('parity differences',len(changes),'configs',len(results),'qualifiers',len(qualifiers))
 for r in results[:2]+sorted(qualifiers,key=lambda r:(r['pooled']['word_errors'],r['pooled']['control_case_only_false_clips']))[:10]:print(json.dumps(r))
 # Diagnostic changes: references stay in scorer/report only.
 diags=[]
 groups={}
 for row in data:groups.setdefault(row['request']['id'],{})[row['request']['condition']]=row
 for g in groups.values():
  a=g['off']
  for cond in ['relevant','distractor']:
   b=g[cond]
   if a['text']==b['text']:continue
   edits=[]
   for op,i,j,k,l in difflib.SequenceMatcher(None,a['_w'],b['_w'],autojunk=False).get_opcodes():
    if op=='equal':continue
    edits.append(dict(op=op,baseline=a['_w'][i:j],candidate=b['_w'][k:l],base_conf=a['_p'][i:j],candidate_conf=b['_p'][k:l]))
   sc=score.score(b,fixtures[b['request']['id']],names)
   diags.append(dict(id=b['request']['id'],condition=cond,reference=fixtures[b['request']['id']]['reference'],off=a['text'],source=b['text'],source_geo=b['_sentence_geo'],off_geo=a['_sentence_geo'],false=sc['decoy_lexical_false'],case=sc['decoy_case_only_false'],edits=edits))
 (OUT/'results/edit-diagnostics.json').write_text(json.dumps(diags,ensure_ascii=False,indent=2))
if __name__=='__main__':main()
