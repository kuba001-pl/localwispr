#!/usr/bin/env python3
"""Render the already-computed results. Does not run or tune recognition."""
import collections
import html
import json
from pathlib import Path
from analyze import aggregate

OUT=Path(__file__).resolve().parent

def pct(x):return '—' if x is None else f'{x*100:.2f}%'
def esc(s):return str(s).replace('|','\\|').replace('\n',' ')
def table(headers,rows):
    return '\n'.join(['| '+' | '.join(headers)+' |','| '+' | '.join(['---']*len(headers))+' |']+
                     ['| '+' | '.join(esc(v) for v in row)+' |' for row in rows])+'\n'

def main():
    metrics=json.loads((OUT/'results/metrics.json').read_text())
    decision=json.loads((OUT/'results/dev-decision.json').read_text())
    plan=json.loads((OUT/'plan.json').read_text())
    corpus=[json.loads(s) for s in (OUT/'corpus.jsonl').read_text().splitlines()]
    scored=[json.loads(s) for s in (OUT/'results/scored.jsonl').read_text().splitlines()]
    g=metrics['groups'];p=metrics['paired'];lines=[]
    def add(s=''):lines.append(s+'\n')
    add('# Expanded synthetic recognition benchmark')
    base=g['Fast/all/off'];rel=g['Fast/all/relevant'];dist=g['Fast/all/distractor']
    add(f'Frozen working-tree recognition source; installed Whisper large-v3-turbo on CUDA. **270 unique sentences**, **42 product/company names**, eight additional acoustic fixtures. This is synthetic eSpeak evidence, not a real-dictation accuracy estimate. No production edits, target corrections, LLM cleanup, private recordings or downloads.')
    add(f'The source context helper changed micro WER from **{pct(base["wer"])}** without document context to **{pct(rel["wer"])}** with relevant context and **{pct(dist["wer"])}** with distractor context. Case-insensitive literal target recall was **{base["target_literal_ci"]}/126**, **{rel["target_literal_ci"]}/126**, and **{dist["target_literal_ci"]}/126** respectively. Read the paired harm and control results below before interpreting aggregate gains.')
    add('## Frozen design and provenance')
    add(f'* Corpus SHA256: `{plan["freeze_sha256"]}`. Inputs and split are in `corpus.jsonl` and `plan.json`; each run records request, binary and tool hashes.')
    add('* Development: 93 speech sentences + 3 acoustic fixtures. Held out: 177 speech sentences + 5 acoustic fixtures. Split was frozen before inference. Names and related language templates overlap across sets; this is a held-out-sentence test, not a held-out-brand test.')
    add('* Speech: 126 product sentences (42 English, 42 Polish, 42 mixed), 84 ordinary-word/near-homophone controls (28 per language), 60 name-absent sentences (20 per language). All 270 references are unique. Related templates across translations limit diversity.')
    add('* Available installed eSpeak voices/variants: US English, standard English, Scottish English, US feminine variant, Polish, Polish feminine and masculine variants. Speeds: 135/160/185 WPM, 90 sentences per speed. Polish voices mechanically pronounce mixed/English text. Voice/language assignments are partially confounded, so voice buckets are descriptive.')
    add('* Every primary fixture: context off / source relevant / source distractor. “Relevant” for an ordinary control deliberately mentions its paired brand, testing misleading context. Names-absent controls receive irrelevant brands in both context-on conditions. Context lengths vary with repeated ordinary prose; 21 speech sentences additionally receive the same long manual hint in all conditions. Context-off still preserves manual and multilingual hints.')
    add('* The source prompt prioritizes uppercase/identifier vocabulary, admits at most 40 hints / 320 characters, and puts a manual hint last. Source token budgeting may remove the context portion when that manual hint is long. Requests retain full configured prompts; the model sees the bounded prompt from the frozen STT implementation.')
    add('* Fast: all 278 fixtures × 3 = 834 primary decodes. Default/Balanced: preselected 78 speech + 8 acoustic fixtures × 3 = 258 decodes. Stability: 24 speech sentences × 3 = 72 repeated decodes, excluded from primary totals. Development alternatives: 96 fixtures × 3 alternatives × 2 context conditions = 576 extra decodes. Original plan: 1740 requests. A separately frozen post-hoc dev-only manual-hint ablation adds 21 requests. **1761 total requests; 270 unique speech sentences.**')
    add('* GPU: RTX 3080. Source and dirty working tree are preserved in `frozen/`; dependency/model/audio hashes are in `build-command.json`, `model-hashes.json`, `audio-manifest.json`. The parent may change current production helpers without changing this baseline.')
    add('## Primary recognition scores')
    add('WER/CER are micro averages over speech only, with Unicode case-folding, punctuation removal, single word spaces and preserved diacritics. Exact name recall uses literal official spelling and word boundaries. Casing is a separate column; no hardcoded recognition corrections are applied. “Case only” control flags are ambiguous orthographic changes, not evidence of a semantic brand intention.')
    rows=[]
    for split in ['all','dev','heldout']:
        for cond in ['off','relevant','distractor']:
            a=g[f'Fast/{split}/{cond}']
            rows.append([split,cond,a['unique_sentences'],f'{a["word_errors"]}/{a["words"]} ({pct(a["wer"])})',
                         pct(a['cer']),f'{a["target_literal_ci"]}/{a["targets"]}',f'{a["target_exact_case"]}/{a["targets"]}'])
    add(table(['Set','Context','Unique speech','Word errors / words (WER)','CER','Exact name, CI','Exact name + case'],rows))
    add('## Paired improvement and harm')
    rows=[]
    for split in ['all','heldout']:
        for cond in ['relevant','distractor']:
            a=p[f'Fast/{split}/{cond}']
            rows.append([split,cond,a['pairs'],a['wer_improved'],a['wer_regressed'],a['wer_equal'],
                         a['exact_name_improved'],a['exact_name_regressed'],a['new_lexical_false_brand_clips'],a['new_case_false_brand_clips']])
    add(table(['Set','Context','Pairs','WER better','WER worse','WER equal','Name better','Name worse','New lexical-brand harms','New case flags'],rows))
    add('Counts compare each sentence to its own context-off result. WER equality can hide different outputs; `output_changed` and full harm examples are in `results/metrics.json`. These are descriptive comparisons, without a population-level significance claim.')
    add('## Ordinary-word / near-homophone and absent-name controls')
    rows=[]
    for cond in ['off','relevant','distractor']:
        a=g[f'Fast/all/{cond}']
        rows.append([cond,f'{a["control_lexical_false_clips"]}/{a["ordinary_control_clips"]} ({pct(a["control_lexical_false_rate"])})',
                     f'{a["control_case_only_false_clips"]}/{a["ordinary_control_clips"]} ({pct(a["control_case_only_false_rate"])})',
                     f'{a["ordinary_preserved"]}/{a["ordinary_anchors"]}',a['any_lexical_false_brand_clips'],
                     f'{a["acoustic_nonempty"]}/{a["acoustic_clips"]}'])
    add(table(['Context','Paired decoy lexical false clips','Paired decoy case-only clips','Ordinary anchors retained','Any lexical false-brand clips, all speech','Nonempty acoustic'],rows))
    add('Lexical false-brand clips require a brand token sequence absent from the reference. Case-only flags require a reference occurrence without official capitalization, e.g. notion → Notion, cursor → Cursor, linear → Linear, anthropic → Anthropic; these do not affect normalized WER. Repetition of a correctly cased target is an expansion failure, not a false-brand flag. Near-homophones such as quadrant → Qdrant or cerebral → Cerebras count lexically. Multiword / compound spacing is not silently repaired. Some Polish controls translate the ordinary word and therefore have no literal ordinary-anchor annotation; they remain in the 84-clip false-substitution denominator.')
    add('## Default/Balanced versus Fast on the identical frozen subset')
    rows=[]
    for cond,a in metrics['accuracy_comparison'].items():
        for accuracy,key in [('Fast','fast'),('Balanced','balanced')]:
            s=a[key]
            rows.append([cond,accuracy,s['unique_sentences'],pct(s['wer']),pct(s['cer']),f'{s["target_literal_ci"]}/{s["targets"]}',s['latency_p50_ms'],s['latency_p95_ms']])
    add(table(['Context','Accuracy','Speech','WER','CER','Exact name, CI','Submit p50 ms','Submit p95 ms'],rows))
    add(table(['Context','Balanced improves sentence WER','Balanced regresses','Equal'],
              [[c,a['balanced_improved'],a['balanced_regressed'],a['balanced_equal']] for c,a in metrics['accuracy_comparison'].items()]))
    add('Balanced uses the actual default config, including fallback temperatures; Fast disables fallback re-decodes. This is not a comparison to the separate Best/beam-search setting. Acoustic fixtures enter latency summaries but not WER/CER. These batches ran at different times with uncontrolled background CPU/GPU load; the observed p95 difference is not evidence that Balanced is inherently faster.')
    add('## Latency and stability')
    rows=[]
    for cond in ['off','relevant','distractor']:
        a=g[f'Fast/all/{cond}'];rows.append([cond,a['latency_p50_ms'],a['latency_p95_ms'],a['latency_max_ms'],f'{a["summed_submit_ms"]/1000:.2f}'])
    add(table(['Context','Submit p50 ms','Submit p95 ms','Max ms','Sum seconds'],rows))
    st=metrics['stability']
    add(f'Timing starts after file loading and excludes synthesis, capture, model loading/warmup and cleanup. It includes worker submission, VAD, language detection, decoding and STT filters. Desktop GPU activity is uncontrolled. Reverse-order repetition of {st["unique_sentences"]} sentences ({st["repeat_decodes"]} decodes) changed {st["output_changed"]} raw outputs and {st["word_errors_changed"]} normalized word-error counts. Repeats are not additional unique examples.')
    add('## Failure buckets')
    rows=[]
    for lang in ['en','pl','mixed']:
        for cond in ['off','relevant','distractor']:
            a=metrics['buckets'][f'language/{lang}/{cond}'];rows.append([lang,cond,pct(a['wer']),pct(a['cer']),f'{a["target_literal_ci"]}/{a["targets"]}'])
    add(table(['Language','Context','WER','CER','Exact name, CI'],rows))
    rows=[]
    for long_hint in ['False','True']:
        for cond in ['off','relevant','distractor']:
            a=metrics['buckets'][f'long_hint/{long_hint}/{cond}'];rows.append([long_hint,cond,a['unique_sentences'],pct(a['wer']),f'{a["target_literal_ci"]}/{a["targets"]}'])
    add(table(['Long manual hint','Context','Unique speech','WER','Exact name, CI'],rows))
    add('The long-hint bucket is deliberately extreme: 280 repetitions of “ordinary vocabulary” plus a fixed natural-language suffix. WER can exceed 100% because insertions and repeated sentences count as errors. These cases stay in the primary aggregate; removing them would hide a measured failure. Bucket comparisons alone are confounded by sentence composition; the matched dev-only ablation below addresses that limitation for seven sentences.')
    counts={k:len(v) for k,v in metrics['failure_buckets'].items()}
    add('Across all three primary context conditions, overlapping failure-record counts (not unique-sentence counts): '+', '.join(f'{k}: {v}' for k,v in counts.items())+'. Full records include references, hypotheses, speed, voice, language and context condition in `results/metrics.json`; voice/speed/category aggregate buckets are there too.')
    add('Largest held-out regressions, selected mechanically by normalized word-error increase:')
    examples=[]
    for cond in ['relevant','distractor']:
        for a in p[f'Fast/heldout/{cond}']['harms'][:4]:
            examples.append([cond,a['id'],a['reference'],a['off'],a['context'],a['error_delta']])
    add(table(['Context','ID','Reference','Off output','Context output','Added errors'],examples))
    name_rows=[]
    for name,a in sorted(metrics['per_name'].items(),key=lambda pair:(pair[1]['relevant']['target_literal_ci'],pair[0])):
        name_rows.append([name]+[f'{a[c]["target_literal_ci"]}/{a[c]["targets"]}' for c in ['off','relevant','distractor']])
    add('Per-name exact spelling recall (three distinct sentences per name; English, Polish and mixed):')
    add(table(['Name','Off','Relevant','Distractor'],name_rows))
    case_examples=[r for r in metrics['failure_buckets']['ordinary_brand_case'] if r['split']=='heldout' and r['condition']!='off'][:6]
    add('Held-out case-only ordinary-word flags (in frozen corpus order; normalized WER may remain unchanged):')
    add(table(['ID','Context','Reference','Output','Case flags'],
              [[r['id'],r['condition'],r['reference'],r['hypothesis'],', '.join(r['case_only_false_brands'])] for r in case_examples]))
    add('## Development-only general prompt experiments')
    add('These alternatives were specified before inference and evaluated only on the 93 dev sentences + 3 acoustic fixtures. They change only Whisper prompt construction; all use the same frozen recognizer and no output correction. Each score pools one relevant and one distractor decode per dev fixture (186 speech decodes, 93 unique speech sentences). The off condition is shared, not rerun for every alternative.')
    add('* `source`: the original 40-word / 320-character scored-vocabulary helper.')
    add('* `case16`: at most 16 distinct nearby tokens / 160 characters containing uppercase letters or digits, ordered by proximity, without a target/name whitelist.')
    add('* `prose320`: last 160 characters before the cursor plus first 160 after, preserving ordinary prose.')
    add('* `short12`: at most 12 distinct nearest tokens / 160 characters, regardless of case, without source stopword or product lists.')
    rows=[]
    for strategy,a in decision['metrics_relevant_plus_distractor'].items():
        rows.append([strategy,a['word_errors'],pct(a['wer']),pct(a['cer']),f'{a["target_literal_ci"]}/{a["targets"]}',
                     f'{a["control_lexical_false_clips"]}/{a["ordinary_control_clips"]}',
                     f'{a["control_case_only_false_clips"]}/{a["ordinary_control_clips"]}'])
    add(table(['Strategy','Pooled word errors','WER','CER','Name CI','Decoy lexical false','Decoy case-only'],rows))
    winner=decision['selected_strategy']
    add(f'The predeclared selection rule picked **`{winner}`** on dev (fewest pooled word errors, then fewest lexical false-brand clips, then source). Decision locked at `{decision["locked_utc"]}` before any held-out inference. **No alternative was run on held-out data and no production helper was changed.**')
    add(f'Recommendation: treat `{winner}` as a development-supported candidate for a fresh validation corpus. Bound nearby vocabulary and test both relevance and misleading ordinary-word contexts. Capitalization alone cannot disambiguate a brand from an ordinary word. The evidence does not justify automatic target corrections or claims of broad name robustness. Longer manual hints need a separate policy decision because suffix-prioritized token budgeting can suppress document vocabulary.')
    add('## Supplemental matched manual-hint ablation — development only')
    add('After observing catastrophic repetition in dev long-hint cases, `dev-manual-plan.json` froze a follow-up on the seven existing dev sentences with long manual hints. It removes only the manual hint, rerunning the same three conditions with the same audio/source/model (21 requests). This is post-hoc diagnosis, not a new strategy-selection contest. No corresponding held-out ablation was run and no recognizer/source setting was tuned.')
    rows=[]
    for cond,a in metrics['manual_ablation'].items():
        old=a['original_long'];new=a['without_manual']
        rows.append([cond,old['unique_sentences'],pct(old['wer']),pct(new['wer']),
                     a['improved'],a['regressed'],a['equal'],f'{old["target_literal_ci"]}/{old["targets"]} → {new["target_literal_ci"]}/{new["targets"]}'])
    add(table(['Context','Unique dev speech','With long hint WER','Without manual WER','Improved','Regressed','Equal','Name CI'],rows))
    add('Examples and raw predictions are in `results/metrics.json` under `manual_ablation` and `results/dev_manual.jsonl`. This small diagnosis does not establish how arbitrary natural long hints behave. A future experiment should compare bounded, deduplicated explicit hints on a fresh corpus, with manual information retained rather than blindly deleted.')
    add('## Reproduction and practical limits')
    add('Run commands are in `README.md`. Source is compiled directly against existing installed release Rust/native dependencies to avoid rebuilding CUDA. `validate.py` checks every audio hash, frozen source, unique references, disjoint sets, request counts and scoring edge cases. `run.py` refuses to overwrite stage evidence and writes input hashes before opening the model. Raw JSONL retains prompts, original predictions, detected language and timing. `analyze.py` is standard-library code with explicit scoring definitions.')
    add('Exact audio reproduction uses the canonical `audio-fixtures.tar.zst` lossless archive (about 62 MiB), with its hash in `audio-archive.json`. `restore_audio.py --verify-only` extracts into a fresh temporary directory and verified all 278 original SHA256 hashes. Fresh eSpeak synthesis is **not** bit-identical: a separate isolated process matched 172/278 assets; all 106 feminine `f3` variants differed, consistent with that variant’s breath-noise configuration. The failed regeneration check and mismatch list are preserved in `results/audio-reproduction.json` and `.log`. No original audio or inference result was overwritten. Reproduction commands restore the actual pre-inference waveforms; resynthesis is a separate diagnostic.')
    add('The experiment uses one Whisper model and one synthetic engine family, with short scripted sentences and artificial mixed-language pronunciation. Controls are deliberately difficult and are not prevalence weighted. Hints are fabricated and often contain the target verbatim; this tests prompt influence, not automated relevance detection. Eight acoustic fixtures and 24 repeated sentences are limited stress checks. Human-speaker recordings, accents, microphones, natural disfluencies, inflection, real documents and independent unseen brands remain untested. Scores can reveal regressions; they cannot establish real-world accuracy or safety from the absence of a failure in a bucket.')
    (OUT/'REPORT.md').write_text('\n'.join(lines))
    # Compact, standalone comparison chart; the host can render this HTML
    # without a server. It intentionally shows both benefit and control harm.
    chart=[]
    for cond,label in [('off','Context off'),('relevant','Relevant context'),('distractor','Distractor context')]:
        a=g['Fast/heldout/'+cond]
        chart.append(f'<div class="row"><span>{label}</span><div class="track"><div class="bar" style="width:{a["wer"]*100:.2f}%"></div></div><strong>{pct(a["wer"])}</strong></div>')
    chart_html='''<!doctype html><html><head><meta charset="utf-8"><style>
    body{font-family:var(--font-sans);color:var(--foreground)}h3{font-size:16px;margin:8px 0 16px}p{font-size:12px;color:var(--muted-foreground);line-height:1.5}.row{display:grid;grid-template-columns:125px 1fr 65px;gap:10px;align-items:center;margin:16px 0;font-size:13px}.track{background:var(--secondary);height:20px;border-radius:3px}.bar{background:var(--chart-1);height:20px;border-radius:3px}strong{text-align:right;font-size:12px}table{font-size:12px;width:100%;border-collapse:collapse}td,th{padding:7px;text-align:left;border-bottom:1px solid var(--border)}
    </style></head><body><h3>Held-out synthetic sentences: word error rate ↓</h3>'''+''.join(chart)+f'<p>177 unique held-out speech sentences · Whisper large-v3-turbo / Fast · eSpeak NG synthetic audio · same frozen source in every condition</p><table><tr><th>All 84 ordinary controls</th><th>Off</th><th>Relevant</th><th>Distractor</th></tr><tr><td>Decoy lexical substitutions</td>'+''.join(f'<td>{g["Fast/all/"+c]["control_lexical_false_clips"]}/84</td>' for c in ['off','relevant','distractor'])+'</tr><tr><td>Decoy case-only brand flags</td>'+''.join(f'<td>{g["Fast/all/"+c]["control_case_only_false_clips"]}/84</td>' for c in ['off','relevant','distractor'])+'</tr></table><p>Case-only flags are ambiguous orthographic changes. Synthetic scores do not estimate real-speaker accuracy. Full evidence and limits: REPORT.md.</p></body></html>'
    (OUT/'comparison.html').write_text(chart_html)
    print('Wrote REPORT.md and comparison.html')

if __name__=='__main__':main()
