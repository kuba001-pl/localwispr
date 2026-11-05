#!/usr/bin/env python3
"""Download public LibriSpeech dummy validation audio and freeze distractor controls.

This tiny clean-read-English subset has ONE speaker, not broad voice coverage.
Source: hf-internal-testing/librispeech_asr_dummy, clean/validation; LibriSpeech CC BY 4.0.
No audio or model binaries belong in git. Output defaults to /tmp.
"""
import concurrent.futures, hashlib, json, pathlib, subprocess, sys, urllib.request
root = pathlib.Path(sys.argv[1] if len(sys.argv)>1 else '/tmp/localwispr-natural-validation')
root.mkdir(parents=True, exist_ok=True)
url='https://datasets-server.huggingface.co/rows?dataset=hf-internal-testing/librispeech_asr_dummy&config=clean&split=validation&offset=0&length=100'
if not (root/'metadata.json').exists():
    with urllib.request.urlopen(url, timeout=30) as response:
        (root/'metadata.json').write_bytes(response.read())
rows=json.loads((root/'metadata.json').read_text())['rows']
# Fixed before decoding; no transcript-derived hints. Deliberately confusable
# everyday words/brands and difficult technical vocabulary are irrelevant here.
distractors='Linear Notion Cursor Apple Amazon Mercury Stripe Square Zoom Slack Teams Qdrant Cerebras Anthropic Vercel Supabase ElevenLabs OpenTelemetry ClickHouse Tailscale'
def prepare(entry):
    row=entry['row']; name=row['id']; flac=root/(name+'.flac'); pcm=root/(name+'.f32')
    if not flac.exists():
        with urllib.request.urlopen(row['audio'][0]['src'],timeout=45) as response:
            flac.write_bytes(response.read())
    subprocess.run(['ffmpeg','-v','error','-y','-i',str(flac),'-ar','16000','-ac','1','-f','f32le',str(pcm)],check=True)
    base=dict(id=name,reference=row['text'],audio=str(pcm),sha256=hashlib.sha256(pcm.read_bytes()).hexdigest(),languages=['en'],vad=True,manual='',terms=[],synthetic=False,speaker=row['speaker_id'],chapter=row['chapter_id'])
    return [dict(base,mode=mode,context_on=enabled,context=distractors) for mode,enabled in [('off',False),('distractor',True)]]
with concurrent.futures.ThreadPoolExecutor(max_workers=4) as pool:
    prepared=list(pool.map(prepare,rows))
(root/'requests.jsonl').write_text(''.join(json.dumps(r)+'\n' for pair in prepared for r in pair))
(root/'provenance.json').write_text(json.dumps(dict(source=url,license='CC BY 4.0',clips=len(rows),speakers=len({x['row']['speaker_id'] for x in rows}),requests_sha256=hashlib.sha256((root/'requests.jsonl').read_bytes()).hexdigest()),indent=2)+'\n')
print(f'Frozen {len(rows)} unique human recordings, {len(prepared)*2} paired requests in {root}')
