#!/usr/bin/env python3
"""Run retained independent cleanup/STT assertions against current source, without models."""
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[1]


def main():
    fixtures = [json.loads(line)['case'] for line in (HERE / 'cleanup-frozen-results.jsonl').read_text().splitlines()]
    with tempfile.TemporaryDirectory(prefix='localwispr-verification-') as folder:
        work = Path(folder)
        (work / 'Cargo.toml').write_text('''[package]
name = "localwispr-independent-text-verification"
version = "0.0.0"
edition = "2024"
[dependencies]
anyhow = "1"
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "fs", "io-util", "process"] }
tracing = "0.1"
serde = { version = "1", features = ["derive"] }
serde_json = "1"
[[bin]]
name = "review"
path = "main.rs"
''')
        config = (ROOT / 'src/config.rs').read_text()
        start = config.index('pub struct Cleanup {')
        end = config.index('#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]\n#[serde(rename_all = "kebab-case")]\npub enum PasteChord', start)
        (work / 'config.rs').write_text('use serde::{Serialize, Deserialize};\n#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]\n' + config[start:end])
        source = (ROOT / 'src/stt.rs').read_text()
        start = source.index('const HALLUCINATIONS:')
        end = source.index('\n#[cfg(test)]', start)
        signal = source[source.index('fn has_audio_signal('):source.index('\nfn transcribe(')]
        tests_end = source.index('    /// Optional native regression', end)
        (work / 'stt.rs').write_text(signal + source[start:end] +
            '\npub fn review_signal(samples: &[f32]) -> bool { has_audio_signal(samples) }\n' + source[end:tests_end] + '}\n')
        for name in ['cleanup.rs', 'llm_proto.rs', 'fence.rs']:
            shutil.copyfile(ROOT / 'src' / name, work / name)
        shutil.copyfile(HERE / 'review_regressions.rs', work / 'review_regressions.rs')
        (work / 'main.rs').write_text('''#![allow(dead_code)]
mod config; mod stt; mod cleanup; mod llm_proto; mod fence;
#[cfg(test)] mod review_regressions;
use std::io::BufRead;
fn main() {
    for line in std::io::stdin().lock().lines() {
        let case: serde_json::Value = serde_json::from_str(&line.unwrap()).unwrap();
        let raw = case["raw"].as_str().unwrap();
        let cfg = config::Cleanup::default();
        let actual = match case["op"].as_str().unwrap() {
            "guard" => serde_json::json!(cleanup::check(raw, case["candidate"].as_str().unwrap(), &cfg).is_ok()),
            "fallback" => serde_json::json!(cleanup::fallback(raw, &cfg)),
            other => panic!("unsupported operation: {other}"),
        };
        assert_eq!(actual, case["expected"], "case {}", case["id"]);
    }
}
''')
        env = dict(os.environ)
        env['CARGO_TARGET_DIR'] = str(ROOT / 'target/independent-text-verification')
        subprocess.run(['cargo', 'test', '--offline', '--quiet', '--manifest-path', str(work / 'Cargo.toml')], env=env, check=True)
        subprocess.run(['cargo', 'run', '--offline', '--quiet', '--manifest-path', str(work / 'Cargo.toml')],
                       input=''.join(json.dumps(row, ensure_ascii=False) + '\n' for row in fixtures), text=True, env=env, check=True)
    print(f'PASS: {len(fixtures)} unchanged cleanup expectations and independent Rust regressions against current source')
    for name in ['cleanup.rs', 'stt.rs']:
        print(name, hashlib.sha256((ROOT / 'src' / name).read_bytes()).hexdigest())


if __name__ == '__main__':
    main()
