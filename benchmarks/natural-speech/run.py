#!/usr/bin/env python3
"""Adapt public natural-speech requests to the committed recognition harness."""
import argparse
import hashlib
import json
from pathlib import Path
import subprocess


def harness_request(row):
    if row['mode'] not in ('off', 'distractor') or row['context_on'] != (row['mode'] == 'distractor'):
        raise ValueError('Expected a matching off/distractor pair')
    if not row.get('vad', True):
        raise ValueError('The recognition harness always enables VAD')
    return dict(id=row['id'], condition=row['mode'], strategy='source', accuracy='Fast',
                languages=row['languages'], audio=row['audio'],
                before=row['context'] if row['context_on'] else '', after='',
                manual=row.get('manual', ''), stage='natural')


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('requests', type=Path)
    parser.add_argument('output', type=Path)
    parser.add_argument('--harness', type=Path, default=Path(__file__).resolve().parents[1] / 'recognition/recognition-harness')
    args = parser.parse_args()
    rows = [json.loads(line) for line in args.requests.read_text().splitlines()]
    requests = [harness_request(row) for row in rows]
    for row in rows:
        if hashlib.sha256(Path(row['audio']).read_bytes()).hexdigest() != row['sha256']:
            raise ValueError(f"Audio hash mismatch: {row['id']}")
    # Refuse to replace historical evidence. Model logs stay beside the output.
    with args.output.open('x') as output, args.output.with_suffix('.stderr.log').open('x') as log:
        worker = subprocess.Popen([str(args.harness.resolve())], stdin=subprocess.PIPE,
                                  stdout=subprocess.PIPE, stderr=log, text=True)
        try:
            for row, request in zip(rows, requests):
                worker.stdin.write(json.dumps(request) + '\n')
                worker.stdin.flush()
                response = worker.stdout.readline()
                if not response:
                    raise RuntimeError(f"Recognizer stopped at {row['id']}; see stderr log")
                result = json.loads(response)
                if result.pop('request') != request:
                    raise ValueError('Recognizer returned a mismatched request')
                wall_ms = result.pop('submit_ms')
                output.write(json.dumps(dict(input=row, result=result, wall_ms=wall_ms), ensure_ascii=False) + '\n')
                output.flush()
            worker.stdin.close()
            if worker.wait(timeout=60) != 0:
                raise RuntimeError('Recognizer exited unsuccessfully')
        finally:
            if worker.poll() is None:
                worker.kill()
                worker.wait()
    print(f'Wrote {len(rows)} outputs to {args.output}')


if __name__ == '__main__':
    main()
