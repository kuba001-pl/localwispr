#!/usr/bin/env python3
"""Transparent standard-library scoring; never alters recognition output."""
import argparse
from collections import Counter, defaultdict
import datetime
import hashlib
import json
import math
from pathlib import Path
import re
import statistics

OUT=Path(__file__).resolve().parent

def tokens(text):return re.findall(r'[^\W_]+',text.casefold(),flags=re.UNICODE)
def norm(text):return ' '.join(tokens(text))
def distance(a,b):
    prev=list(range(len(b)+1))
    for i,x in enumerate(a,1):
        row=[i]
        for j,y in enumerate(b,1):row.append(min(row[-1]+1,prev[j]+1,prev[j-1]+(x!=y)))
        prev=row
    return prev[-1]
def count_literal(text,term,case=True):
    return len(re.findall(r'(?<!\w)'+re.escape(term)+r'(?!\w)',text,flags=0 if case else re.IGNORECASE))
def count_tokens(text,term):
    hay=tokens(text);needle=tokens(term)
    return sum(hay[i:i+len(needle)]==needle for i in range(len(hay)-len(needle)+1))
def quantile(data,q):
    return sorted(data)[max(0,math.ceil(q*len(data))-1)] if data else None

def score(row,fixture,names):
    ref=fixture['reference'];hyp=row['text'];rt=tokens(ref);ht=tokens(hyp)
    lexical=[];case_only=[]
    for name in names:
        # A lowercase ordinary word shares spelling with some brands. Its
        # capitalization is explicitly a separate, ambiguous failure bucket.
        if count_tokens(ref,name)==0 and count_tokens(hyp,name)>0:lexical.append(name)
        if (count_literal(hyp,name)>count_literal(ref,name)
                and count_literal(ref,name)<count_tokens(ref,name)):
            case_only.append(name)
    targets=fixture['targets']
    details=dict(id=fixture['id'],reference=ref,hypothesis=hyp,category=fixture['category'],
                 split=fixture['split'],language=fixture['language'],voice=fixture.get('voice','none'),
                 speed=fixture.get('speed',0),long_hint=fixture['long_hint'],
                 stage=row['request']['stage'],accuracy=row['request']['accuracy'],
                 strategy=row['request']['strategy'],condition=row['request']['condition'],
                 reference_words=len(rt),word_errors=distance(rt,ht),reference_chars=len(norm(ref)),
                 char_errors=distance(norm(ref),norm(hyp)),
                 targets=len(targets),target_literal_ci=sum(count_literal(hyp,t,False)>0 for t in targets),
                 target_exact_case=sum(count_literal(hyp,t)>0 for t in targets),
                 target_token_ci=sum(count_tokens(hyp,t)>0 for t in targets),
                 ordinary_anchors=len(fixture['ordinary']),
                 ordinary_preserved=sum(count_tokens(hyp,t)>0 for t in fixture['ordinary']),
                 lexical_false_brands=lexical,case_only_false_brands=case_only,
                 decoy_lexical_false=[n for n in lexical if n in fixture['decoys']],
                 decoy_case_only_false=[n for n in case_only if n in fixture['decoys']],
                 submit_ms=row['submit_ms'],stt_ms=row['stt_ms'],audio_secs=row['audio_secs'],
                 nonempty=bool(hyp.strip()))
    return details

def aggregate(rows):
    speech=[r for r in rows if r['category']!='acoustic']
    acoustic=[r for r in rows if r['category']=='acoustic']
    controls=[r for r in speech if r['category']=='ordinary_control']
    def total(key):return sum(r[key] for r in speech)
    words=total('reference_words');chars=total('reference_chars');targets=total('targets')
    def div(a,b):return a/b if b else None
    return dict(clips=len(rows),unique_sentences=len({r['id'] for r in speech}),
                words=words,word_errors=total('word_errors'),wer=div(total('word_errors'),words),
                chars=chars,char_errors=total('char_errors'),cer=div(total('char_errors'),chars),
                targets=targets,target_literal_ci=total('target_literal_ci'),target_exact_case=total('target_exact_case'),
                target_token_ci=total('target_token_ci'),target_accuracy_ci=div(total('target_literal_ci'),targets),
                target_accuracy_exact_case=div(total('target_exact_case'),targets),
                ordinary_control_clips=len(controls),
                control_lexical_false_clips=sum(bool(r['decoy_lexical_false']) for r in controls),
                control_case_only_false_clips=sum(bool(r['decoy_case_only_false']) for r in controls),
                control_lexical_false_rate=div(sum(bool(r['decoy_lexical_false']) for r in controls),len(controls)),
                control_case_only_false_rate=div(sum(bool(r['decoy_case_only_false']) for r in controls),len(controls)),
                ordinary_anchors=total('ordinary_anchors'),ordinary_preserved=total('ordinary_preserved'),
                any_lexical_false_brand_clips=sum(bool(r['lexical_false_brands']) for r in speech),
                acoustic_clips=len(acoustic),acoustic_nonempty=sum(r['nonempty'] for r in acoustic),
                latency_p50_ms=statistics.median([r['submit_ms'] for r in rows]) if rows else None,
                latency_p95_ms=quantile([r['submit_ms'] for r in rows],.95),
                latency_mean_ms=statistics.mean([r['submit_ms'] for r in rows]) if rows else None,
                latency_max_ms=max([r['submit_ms'] for r in rows],default=None),
                summed_submit_ms=sum(r['submit_ms'] for r in rows),
                audio_seconds=sum(r['audio_secs'] for r in rows))

def paired(rows,condition):
    off={r['id']:r for r in rows if r['condition']=='off'}
    pairs=[(off[r['id']],r) for r in rows if r['condition']==condition and r['id'] in off]
    speech=[(a,b) for a,b in pairs if a['category']!='acoustic']
    return dict(pairs=len(speech),wer_improved=sum(b['word_errors']<a['word_errors'] for a,b in speech),
                wer_regressed=sum(b['word_errors']>a['word_errors'] for a,b in speech),
                wer_equal=sum(b['word_errors']==a['word_errors'] for a,b in speech),
                output_changed=sum(a['hypothesis']!=b['hypothesis'] for a,b in speech),
                exact_name_improved=sum(b['target_literal_ci']>a['target_literal_ci'] for a,b in speech),
                exact_name_regressed=sum(b['target_literal_ci']<a['target_literal_ci'] for a,b in speech),
                new_lexical_false_brand_clips=sum(bool(set(b['lexical_false_brands'])-set(a['lexical_false_brands'])) for a,b in speech),
                new_case_false_brand_clips=sum(bool(set(b['case_only_false_brands'])-set(a['case_only_false_brands'])) for a,b in speech),
                word_errors_delta=sum(b['word_errors']-a['word_errors'] for a,b in speech),
                harms=[dict(id=b['id'],reference=b['reference'],off=a['hypothesis'],context=b['hypothesis'],
                            error_delta=b['word_errors']-a['word_errors'],new_false_brands=list(set(b['lexical_false_brands'])-set(a['lexical_false_brands'])))
                       for a,b in sorted(speech,key=lambda p:p[1]['word_errors']-p[0]['word_errors'],reverse=True)
                       if b['word_errors']>a['word_errors'] or set(b['lexical_false_brands'])-set(a['lexical_false_brands'])])

def load(stages):
    fixtures={r['id']:r for r in map(json.loads,(OUT/'corpus.jsonl').read_text().splitlines())}
    names=sorted({t for r in fixtures.values() for t in r['targets']})
    rows=[]
    for stage in stages:
        path=OUT/'results'/(stage+'.jsonl')
        if path.exists():
            for line in path.read_text().splitlines():
                raw=json.loads(line);rows.append(score(raw,fixtures[raw['request']['id']],names))
    return fixtures,rows

def dev():
    fixtures,rows=load(['dev'])
    expected=sum(r['split']=='dev' for r in fixtures.values())*9
    assert len(rows)==expected,(len(rows),expected)
    metrics={}
    for strategy in ['source','case16','prose320','short12']:
        relevant=[r for r in rows if r['strategy']==strategy and r['condition']!='off']
        metrics[strategy]=aggregate(relevant)
    winner=min(metrics,key=lambda s:(metrics[s]['word_errors'],metrics[s]['control_lexical_false_clips'],s!='source',s))
    decision={'locked_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
              'dev_results_sha256':hashlib.sha256((OUT/'results/dev.jsonl').read_bytes()).hexdigest(),
              'selected_strategy':winner,'metrics_relevant_plus_distractor':metrics,
              'heldout_policy':'Only preplanned source strategy evaluated on heldout; alternatives remain dev-only. Winner is a recommendation, not a production change.'}
    dest=OUT/'results/dev-decision.json'
    if dest.exists():
        existing=json.loads(dest.read_text());assert existing['selected_strategy']==winner
    else:dest.write_text(json.dumps(decision,indent=2)+'\n')
    print(json.dumps(decision,indent=2))

def all_results():
    fixtures,rows=load(['dev','heldout','balanced','repeat','dev_manual'])
    primary=[r for r in rows if r['strategy']=='source' and r['stage'] in ['dev','heldout']]
    assert len(primary)==len(fixtures)*3,(len(primary),len(fixtures)*3)
    groups={};pairs={}
    for accuracy,data in [('Fast',primary),('Balanced',[r for r in rows if r['stage']=='balanced'])]:
        for split in ['all','dev','heldout']:
            selected=[r for r in data if split=='all' or r['split']==split]
            for condition in ['off','relevant','distractor']:
                key=f'{accuracy}/{split}/{condition}'
                groups[key]=aggregate([r for r in selected if r['condition']==condition])
                if condition!='off':pairs[key]=paired(selected,condition)
    buckets={}
    for field in ['category','language','voice','speed','long_hint']:
        for value in sorted({str(r[field]) for r in primary}):
            for cond in ['off','relevant','distractor']:
                group=[r for r in primary if str(r[field])==value and r['condition']==cond]
                buckets[f'{field}/{value}/{cond}']=aggregate(group)
    # A fair accuracy comparison restricts Fast to precisely the Balanced ids.
    balanced_ids={r['id'] for r in rows if r['stage']=='balanced'}
    accuracy_comparison={}
    for condition in ['off','relevant','distractor']:
        fast={r['id']:r for r in primary if r['id'] in balanced_ids and r['condition']==condition}
        balanced=[r for r in rows if r['stage']=='balanced' and r['condition']==condition]
        speech=[r for r in balanced if r['category']!='acoustic']
        accuracy_comparison[condition]={'fast':aggregate(list(fast.values())),'balanced':aggregate(balanced),
            'balanced_improved':sum(r['word_errors']<fast[r['id']]['word_errors'] for r in speech),
            'balanced_regressed':sum(r['word_errors']>fast[r['id']]['word_errors'] for r in speech),
            'balanced_equal':sum(r['word_errors']==fast[r['id']]['word_errors'] for r in speech)}
    first={(r['id'],r['condition']):r for r in primary}
    repeated=[r for r in rows if r['stage']=='repeat']
    stability={'repeat_decodes':len(repeated),'unique_sentences':len({r['id'] for r in repeated}),
               'output_changed':sum(r['hypothesis']!=first[r['id'],r['condition']]['hypothesis'] for r in repeated),
               'word_errors_changed':sum(r['word_errors']!=first[r['id'],r['condition']]['word_errors'] for r in repeated)}
    per_name={}
    for name in sorted({t for f in fixtures.values() for t in f['targets']}):
        per_name[name]={cond:aggregate([r for r in primary if name in fixtures[r['id']]['targets'] and r['condition']==cond])
                        for cond in ['off','relevant','distractor']}
    manual_ablation={}
    for cond in ['off','relevant','distractor']:
        without=[r for r in rows if r['stage']=='dev_manual' and r['condition']==cond]
        ids={r['id'] for r in without}
        original=[r for r in primary if r['id'] in ids and r['condition']==cond]
        lookup={r['id']:r for r in original}
        manual_ablation[cond]={'original_long':aggregate(original),'without_manual':aggregate(without),
            'improved':sum(r['word_errors']<lookup[r['id']]['word_errors'] for r in without),
            'regressed':sum(r['word_errors']>lookup[r['id']]['word_errors'] for r in without),
            'equal':sum(r['word_errors']==lookup[r['id']]['word_errors'] for r in without),
            'examples':[dict(id=r['id'],reference=r['reference'],long=lookup[r['id']]['hypothesis'],without=r['hypothesis']) for r in without
                        if r['word_errors']!=lookup[r['id']]['word_errors']]}
    metrics={'groups':groups,'paired':pairs,'buckets':buckets,'per_name':per_name,'manual_ablation':manual_ablation,'accuracy_comparison':accuracy_comparison,'stability':stability,
             'failure_buckets':{'target_spelling_miss':[r for r in primary if r['targets']>r['target_literal_ci']],
                               'target_case_only_miss':[r for r in primary if r['target_literal_ci']>r['target_exact_case']],
                               'lexical_false_brand':[r for r in primary if r['lexical_false_brands']],
                               'ordinary_brand_case':[r for r in primary if r['case_only_false_brands']],
                               'ordinary_anchor_loss':[r for r in primary if r['ordinary_anchors']>r['ordinary_preserved']],
                               'acoustic_hallucination':[r for r in primary if r['category']=='acoustic' and r['nonempty']],
                               'empty_speech':[r for r in primary if r['category']!='acoustic' and not r['nonempty']]}}
    metrics['failure_buckets']['large_expansion']=[r for r in primary if r['reference_words'] and
        len(tokens(r['hypothesis']))>=3*r['reference_words']]
    (OUT/'results/metrics.json').write_text(json.dumps(metrics,ensure_ascii=False,indent=2)+'\n')
    (OUT/'results/scored.jsonl').write_text(''.join(json.dumps(r,ensure_ascii=False)+'\n' for r in rows))
    print(json.dumps({'groups':groups,'accuracy_comparison':accuracy_comparison,'stability':stability},indent=2))

if __name__=='__main__':
    parser=argparse.ArgumentParser();parser.add_argument('stage',choices=['dev','all']);args=parser.parse_args()
    dev() if args.stage=='dev' else all_results()
