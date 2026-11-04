from pathlib import Path
import json,datetime,importlib.util,hashlib
out=Path(__file__).resolve().parent;repo=Path('/home/kuba001/.t3/projects/localwispr')
s=out/'frozen/src/stt.rs';text=s.read_text()
marker='static CONFIDENCE:'
text=text.replace(marker,'''static NORMALIZE_LONG_PROMPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
pub fn set_normalize_long_prompt(enabled:bool) { NORMALIZE_LONG_PROMPT.store(enabled, Ordering::Relaxed); }
// Only exact, case-sensitive, whitespace-token phrase repeats; no spelling or case rewriting.
fn collapse_exact_prompt_phrases(text: &str) -> String {
    let mut spans=Vec::new(); let mut start=None;
    for (i,c) in text.char_indices() {
        if c.is_whitespace() { if let Some(s)=start.take() { spans.push(s..i); } }
        else if start.is_none() {start=Some(i);}
    }
    if let Some(s)=start {spans.push(s..text.len());}
    let mut removals=Vec::new(); let mut i=0;
    'scan: while i<spans.len() {
        for width in 1..=32.min((spans.len()-i)/4) {
            let mut reps=1;
            while i+(reps+1)*width<=spans.len() &&
                (0..width).all(|k| text[spans[i+k].clone()]==text[spans[i+reps*width+k].clone()]) { reps+=1; }
            if reps>=4 {let end=i+reps*width;removals.push(spans[i+width-1].end..spans[end-1].end);i=end;continue 'scan;}
        }
        i+=1;
    }
    let mut result=String::new();let mut cursor=0;
    for r in removals {result.push_str(&text[cursor..r.start]);cursor=r.end;}
    result.push_str(&text[cursor..]);result
}
static CONFIDENCE:''')
old='''        let configured_tokens = context.tokenize(&text, text.len().saturating_add(1))?;
        let retained = configured_tokens.len().min(budget - tokens.len());'''
new='''        let mut configured_tokens = context.tokenize(&text, text.len().saturating_add(1))?;
        if NORMALIZE_LONG_PROMPT.load(Ordering::Relaxed) && configured_tokens.len()>budget-tokens.len() {
            let normalized=collapse_exact_prompt_phrases(&text);
            configured_tokens=context.tokenize(&normalized, normalized.len().saturating_add(1))?;
        }
        let retained = configured_tokens.len().min(budget - tokens.len());'''
assert old in text;text=text.replace(old,new);s.write_text(text)
h=out/'harness.rs';text=h.read_text().replace('        let _ = stt::take_confidence();','        stt::set_normalize_long_prompt(v["normalize_long_prompt"].as_bool().unwrap_or(false));\n        let _ = stt::take_confidence();');h.write_text(text)
req=[json.loads(x) for x in (out/'requests.jsonl').read_text().splitlines() if json.loads(x)['manual']]
for r in req:r['normalize_long_prompt']=True;r['stage']='dev_manual_normalized'
assert len(req)==21
(out/'manual-requests.jsonl').write_text(''.join(json.dumps(r,ensure_ascii=False)+'\n' for r in req))
plan={'created':datetime.datetime.now(datetime.timezone.utc).isoformat(),'policy':'only when original configured prompt token length exceeds native available budget after multilingual reservation: collapse exact adjacent case-sensitive whitespace token phrase repeats >=4 repetitions, widths1..32, left to right shortest width first; retain first copy, surrounding exact text and unique suffix; no collapse_word, spelling/case/punctuation normalization; then original suffix token-budget rule','scope':'dev7 original long_manual fixtures all3 conditions; original frozen recognizer identical in both, only prompt normalization differs','selection':'useful only dev no sentence WER regressions, improves total errors, no target recall loss; freeze same policy then once-only heldout long hints if qualifies; no threshold tuning','references':'scoring only'}
(out/'manual-plan.json').write_text(json.dumps(plan,indent=2)+'\n')
spec=importlib.util.spec_from_file_location('buildbench',repo/'benchmarks/recognition/build.py');m=importlib.util.module_from_spec(spec);spec.loader.exec_module(m);m.OUT=out;m.ROOT=repo;m.main()
(out/'manual-source-hashes.json').write_text(json.dumps({str(p.relative_to(out)):hashlib.sha256(p.read_bytes()).hexdigest() for p in [s,h,out/'manual-plan.json',out/'manual-requests.jsonl']},indent=2)+'\n')
