from pathlib import Path
import shutil,json,hashlib
out=Path(__file__).resolve().parent
shutil.copytree(out/'frozen',out/'manual-source/frozen',dirs_exist_ok=True);shutil.copy2(out/'harness.rs',out/'manual-source/harness.rs');shutil.copy2(out/'recognition-harness',out/'manual-recognition-harness');shutil.copy2(out/'build-command.json',out/'manual-build-command.json')
s=(out/'setup.py').read_text().split('req=[')[0]
# Reconstruct the exact initial instrumentation from the original saved setup,
# into a separate archive. Do not rerun inference or overwrite plans/results.
archive=out/'confidence-source';archive.mkdir(exist_ok=True)
exec(compile(s,str(out/'setup.py'),'exec'),{'__file__':str(archive/'setup.py')})
original=json.loads((out/'source-hashes.json').read_text())
assert hashlib.sha256((archive/'frozen/src/stt.rs').read_bytes()).hexdigest()==original['frozen/src/stt.rs']
assert hashlib.sha256((archive/'harness.rs').read_bytes()).hexdigest()==original['harness.rs']
# Natural parity uses original public-clip predictions, never references in rules.
new={(r['request']['id'],r['request']['condition']):r for r in map(json.loads,(out/'results/natural.jsonl').read_text().splitlines())}
old=[json.loads(x) for x in Path('/tmp/localwispr-natural-validation/before.jsonl').read_text().splitlines()]
differences=[]
for r in old:
 condition='off' if r['input']['mode']=='off' else 'distractor';n=new[r['input']['id'],condition]
 if n['text']!=r['result']['text']:differences.append(dict(id=r['input']['id'],condition=condition,old=r['result']['text'],new=n['text']))
(out/'results/natural-parity.json').write_text(json.dumps(differences,ensure_ascii=False,indent=2)+'\n')
print('Natural output differences',len(differences),'off',sum(r['condition']=='off' for r in differences))
