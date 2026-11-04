from pathlib import Path
import shutil,json,importlib.util,hashlib,datetime
repo=Path('/home/kuba001/.t3/projects/localwispr');out=Path(__file__).resolve().parent
shutil.copytree(repo/'benchmarks/recognition/frozen',out/'frozen',dirs_exist_ok=True)
s=out/'frozen/src/stt.rs';text=s.read_text();marker='use crate::audio::SAMPLE_RATE;'
text=text.replace(marker,marker+'''
static CONFIDENCE: std::sync::OnceLock<std::sync::Mutex<serde_json::Value>> = std::sync::OnceLock::new();
pub fn take_confidence() -> serde_json::Value {
    CONFIDENCE.get_or_init(|| std::sync::Mutex::new(serde_json::Value::Null)).lock().unwrap().take()
}
''')
marker='    let mut text = String::new();\n    for seg in state.as_iter() {'
assert marker in text
text=text.replace(marker,'''    let confidence: Vec<_> = state.as_iter().map(|seg| {
        let tokens: Vec<_> = (0..seg.n_tokens()).map(|i| seg.get_token(i).unwrap()).map(|token| {
            let data = token.token_data();
            serde_json::json!({"id": token.token_id(), "p": token.token_probability(),
                "plog":data.plog,"bytes":token.to_bytes().unwrap_or(&[]),
                "text":token.to_str_lossy().unwrap_or_default(),
                "text_token":token.token_id()<context.token_eot()})
        }).collect();
        serde_json::json!({"text":seg.to_str_lossy().unwrap_or_default(),
            "start":seg.start_timestamp(),"end":seg.end_timestamp(),
            "no_speech":seg.no_speech_probability(),"tokens":tokens})
    }).collect();
    *CONFIDENCE.get_or_init(|| std::sync::Mutex::new(serde_json::Value::Null)).lock().unwrap() =
        serde_json::json!({"segments":confidence,"prompt_ids":prompt_tokens});
    let mut text = String::new();
    for seg in state.as_iter() {''')
s.write_text(text)
h=(repo/'benchmarks/recognition/harness.rs').read_text().replace('        let start=std::time::Instant::now();','        let _ = stt::take_confidence();\n        let start=std::time::Instant::now();').replace('"audio_secs":transcript.audio_secs,"prompt":cfg.initial_prompt','"audio_secs":transcript.audio_secs,"prompt":cfg.initial_prompt,"confidence":stt::take_confidence()')
(out/'harness.rs').write_text(h)
req=[json.loads(x) for x in (repo/'benchmarks/recognition/results/dev-requests.jsonl').read_text().splitlines() if json.loads(x)['strategy']=='source']
assert len(req)==288
(out/'requests.jsonl').write_text(''.join(json.dumps(v,ensure_ascii=False)+'\n' for v in req))
fixtures=[json.loads(x) for x in (repo/'benchmarks/recognition/corpus.jsonl').read_text().splitlines() if json.loads(x)['split']=='dev']
(out/'dev-corpus.jsonl').write_text(''.join(json.dumps(v,ensure_ascii=False)+'\n' for v in fixtures))
plan={'created':datetime.datetime.now(datetime.timezone.utc).isoformat(),'scope':'dev only; 93 speech and 3 acoustic; no heldout inference or per-case rule use','source':'benchmarks/recognition/frozen: original source, no parent silence/loop changes, equal across conditions','decode':'original Fast off/source relevant/source distractor; instrumentation only','families':['whole sentence baseline min or mean confidence conditional prompt decode','whole sentence protect all changed baseline words above confidence threshold','per-edit merge only prompt vocabulary replacements whose baseline confidence is low; preserve casing when same casefold token','optional candidate confidence floor / candidate-minus-baseline margin'],'thresholds':[0.1,0.2,0.3,0.4,0.5,0.6,0.7,0.8,0.9,0.95],'requirements':'zero dev lexical control false substitutions, no increase case flags, retain >=90% source relevant incremental name gain (>=29/42) and >=90% pooled incremental gain (>=44/84), pooled word errors <=792; inspect sensitivity and mixed/long/quiet buckets; candidate must be frozen before heldout','scoring':'references/target/decoys used by scorer only; never gate inputs'}
(out/'plan.json').write_text(json.dumps(plan,indent=2)+'\n')
spec=importlib.util.spec_from_file_location('buildbench',repo/'benchmarks/recognition/build.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);m.OUT=out;m.ROOT=repo;m.main()
(out/'source-hashes.json').write_text(json.dumps({str(p.relative_to(out)):hashlib.sha256(p.read_bytes()).hexdigest() for p in [out/'frozen/src/stt.rs',out/'harness.rs',out/'requests.jsonl',out/'plan.json']},indent=2)+'\n')
