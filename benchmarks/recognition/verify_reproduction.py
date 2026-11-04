#!/usr/bin/env python3
"""Regenerate all synthetic audio in isolation; compare frozen asset hashes."""
import datetime
import json
from pathlib import Path
import shutil
import tempfile
import synthesize

OUT=Path(__file__).resolve().parent

def main():
    frozen=json.loads((OUT/'audio-manifest.json').read_text())
    with tempfile.TemporaryDirectory(prefix='localwispr-audio-reproduction-') as folder:
        temp=Path(folder)
        shutil.copy(OUT/'corpus.jsonl',temp/'corpus.jsonl')
        synthesize.OUT=temp
        synthesize.main()
        actual=json.loads((temp/'audio-manifest.json').read_text())
        expected={r['id']:r['sha256'] for r in frozen}
        generated={r['id']:r['sha256'] for r in actual}
        mismatch=[ident for ident in expected if expected[ident]!=generated.get(ident)]
        result={'checked_utc':datetime.datetime.now(datetime.timezone.utc).isoformat(),
                'fixtures':len(expected),'matching':len(expected)-len(mismatch),'mismatches':mismatch,
                'method':'Fresh isolated synthesis process, same frozen corpus/order/library/ffmpeg; exact float32 audio SHA256 comparison. No ASR inference.'}
        (OUT/'results/audio-reproduction.json').write_text(json.dumps(result,indent=2)+'\n')
        assert expected==generated,result
        print('PASS: all 278 regenerated synthetic audio fixtures match frozen SHA256 hashes')

if __name__=='__main__':main()
