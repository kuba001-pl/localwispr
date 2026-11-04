#!/usr/bin/env python3
"""Validate frozen inputs and sensitive scoring edge cases without inference."""
import argparse
import hashlib
import json
from pathlib import Path
from analyze import count_literal, count_tokens, distance, tokens, score
from run import requests, alternative

OUT=Path(__file__).resolve().parent

def main():
    parser=argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--skip-audio',action='store_true',help='Validate committed evidence only; do not claim waveform verification')
    args=parser.parse_args()
    rows=[json.loads(s) for s in (OUT/'corpus.jsonl').read_text().splitlines()]
    speech=[r for r in rows if r['reference']]
    plan=json.loads((OUT/'plan.json').read_text())
    assert len(speech)==270==len({r['reference'].casefold() for r in speech})
    assert len(rows)==278==len({r['id'] for r in rows})
    assert hashlib.sha256((OUT/'corpus.jsonl').read_bytes()).hexdigest()==plan['freeze_sha256']
    assets=json.loads((OUT/'audio-manifest.json').read_text())
    assert {r['id'] for r in assets}=={r['id'] for r in rows}
    if not args.skip_audio:
        for a in assets:
            path=OUT/a['audio']
            assert path.exists(), 'Canonical audio missing: see README.md; use --skip-audio to validate committed evidence only'
            assert hashlib.sha256(path.read_bytes()).hexdigest()==a['sha256'],path
    for file,sha in json.loads((OUT/'frozen/source-hashes.json').read_text()).items():
        assert hashlib.sha256((OUT/'frozen'/file).read_bytes()).hexdigest()==sha,file
    dev=requests('dev',rows);held=requests('heldout',rows)
    assert not ({r['id'] for r in dev}&{r['id'] for r in held})
    assert all(r['strategy']=='source' for r in held)
    assert len(dev)==96*9 and len(held)==182*3
    assert len(requests('balanced',rows))==86*3
    assert len(requests('repeat',rows))==24*3
    assert tokens('Żółć, TEST—A 10.5!')==['żółć','test','a','10','5']
    assert distance(['keep','this'],['keep','all','this'])==1
    assert distance(['do','not','delete'],['delete'])==2
    assert count_literal('a notion remains','Notion')==0
    assert count_literal('a Notion remains','Notion')==1
    assert count_literal('an emotion notionally','Notion',False)==0
    assert count_literal('Notion and notion','Notion',False)==2
    assert count_tokens('eleven labs','ElevenLabs')==0
    assert count_tokens('elevenlabs','ElevenLabs')==1
    assert count_tokens('Hugging, Face','Hugging Face')==1
    assert count_literal('Hugging, Face','Hugging Face',False)==0
    assert count_tokens('the cursor moved','Cursor')==1
    fixture=next(r for r in rows if r['targets']==['GitHub'])
    request={'stage':'test','accuracy':'Fast','strategy':'source','condition':'off'}
    repeated=score({'text':fixture['reference']+' '+fixture['reference'],'request':request,
                    'submit_ms':1,'stt_ms':1,'audio_secs':1},fixture,['GitHub'])
    assert repeated['lexical_false_brands']==[] and repeated['case_only_false_brands']==[]
    ordinary=next(r for r in rows if r['id']=='control-00-en')
    changed=score({'text':ordinary['reference'].replace('notion','Notion'),'request':request,
                  'submit_ms':1,'stt_ms':1,'audio_secs':1},ordinary,['Notion'])
    assert changed['lexical_false_brands']==[] and changed['case_only_false_brands']==['Notion']
    substituted=score({'text':ordinary['reference'].replace('notion','Qdrant'),'request':request,
                      'submit_ms':1,'stt_ms':1,'audio_secs':1},ordinary,['Qdrant'])
    assert substituted['lexical_false_brands']==['Qdrant']
    prompt=alternative('case16','Ordinary prose Qdrant here',' Cursor next','')
    assert 'Qdrant' in prompt and 'Cursor' in prompt and 'prose' not in prompt and len(prompt)<=160
    for stage in ['dev','heldout','balanced','repeat','dev_manual']:
        metadata=OUT/'results'/(stage+'-run.json')
        if not metadata.exists():continue
        meta=json.loads(metadata.read_text())
        if 'completed_utc' not in meta:continue
        raw=OUT/'results'/(stage+'.jsonl');reqpath=OUT/'results'/(stage+'-requests.jsonl')
        assert hashlib.sha256(raw.read_bytes()).hexdigest()==meta['results_sha256']
        assert hashlib.sha256(reqpath.read_bytes()).hexdigest()==meta['requests_sha256']
        recorded=[json.loads(s) for s in raw.read_text().splitlines()]
        expected=[json.loads(s) for s in reqpath.read_text().splitlines()]
        assert [r['request'] for r in recorded]==expected
        assert len(recorded)==meta['requests']
        keys=[(r['request']['id'],r['request']['strategy'],r['request']['condition']) for r in recorded]
        assert len(keys)==len(set(keys))
        if stage=='heldout':
            locked=json.loads((OUT/'results/dev-decision.json').read_text())
            assert locked['locked_utc']<meta['started_utc']
            assert all(r['request']['strategy']=='source' for r in recorded)
    audio='audio not checked (--skip-audio)' if args.skip_audio else '278 waveform hashes checked'
    print(f'PASS: 270 unique sentences, {audio}, frozen source/corpus, disjoint split, preplanned run counts, scoring edge cases')

if __name__=='__main__':main()
