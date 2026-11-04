#!/usr/bin/env python3
"""Restore the EXACT pre-inference audio, including stochastic eSpeak variants."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess
import tempfile

OUT=Path(__file__).resolve().parent

def restore(destination):
    archive=OUT/'audio-fixtures.tar.zst'
    if not archive.exists():
        raise SystemExit('Canonical audio archive is not included in Git. See README.md for its availability and required hash; use validate.py --skip-audio for committed evidence.')
    manifest=json.loads((OUT/'audio-manifest.json').read_text())
    metadata=json.loads((OUT/'audio-archive.json').read_text())
    assert hashlib.sha256(archive.read_bytes()).hexdigest()==metadata['sha256']
    # Validate members before extraction, even though this is our own archive.
    members=subprocess.check_output(['tar','--zstd','-tf',str(archive)],text=True).splitlines()
    allowed={a['audio'] for a in manifest}|{'audio/'}
    assert set(members)==allowed,'Unexpected archive member'
    subprocess.run(['tar','--zstd','--warning=no-timestamp','--no-same-owner','-xf',str(archive),'-C',str(destination)],check=True)
    for a in manifest:
        assert hashlib.sha256((destination/a['audio']).read_bytes()).hexdigest()==a['sha256'],a['id']
    return len(manifest)

def main():
    p=argparse.ArgumentParser();p.add_argument('--verify-only',action='store_true');args=p.parse_args()
    if args.verify_only:
        with tempfile.TemporaryDirectory(prefix='localwispr-exact-audio-') as folder:
            n=restore(Path(folder))
        result={'fixtures':n,'matching':n,'method':'Lossless canonical archive extracted into a fresh temporary directory; all original float32 audio SHA256 hashes match.'}
        (OUT/'results/audio-archive-verification.json').write_text(json.dumps(result,indent=2)+'\n')
        print(f'PASS: {n} canonical audio fixtures restore exactly')
    else:
        print(f'Restored and verified {restore(OUT)} canonical audio fixtures')

if __name__=='__main__':main()
