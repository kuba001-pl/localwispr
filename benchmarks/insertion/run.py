#!/usr/bin/env python3
"""Run the original independent frozen insertion cases against actual repository code."""
import hashlib,json,pathlib,subprocess,tempfile
here=pathlib.Path(__file__).resolve().parent
source=here.parent.parent/'src/insertion.rs'
cases=json.loads((here/'cases.json').read_text())
def rust(s):
 # Raw Rust strings need no JSON/Unicode escaping; select delimiter collision-free.
 n=1
 while '"'+'#'*n in s:n+=1
 return 'r'+'#'*n+'"'+s+'"'+'#'*n
def boolean(v):return str(v).lower()
code=['#[allow(dead_code)]',f'#[path={rust(str(source))}] mod insertion;',f'#[allow(dead_code)] #[path={rust(str(source.with_name("fence.rs")))}] mod fence;', 'fn main() { let mut passed=0;']
for c in cases:
 context='None' if not c['context'] else 'Some(&insertion::SurroundingText{before:'+rust(c['before'])+'.into(),after:'+rust(c['after'])+'.into(),is_replacement:'+boolean(c['is_replacement'])+'})'
 code += ['{',f'let actual=insertion::format({rust(c["text"])},{context},{boolean(c["append_space"])});',f'let expected={rust(c["expected"])};',f'if actual==expected {{ passed+=1; }} else {{ println!("FAIL {{}} expected={{:?}} actual={{:?}}",{rust(c["id"])},expected,actual); }}','}']
code += [f'println!("Passed {{}}/{len(cases)} frozen cases",passed);',f'if passed!={len(cases)} {{ std::process::exit(1); }}','}']
print('Cases SHA256:',hashlib.sha256((here/'cases.json').read_bytes()).hexdigest(),flush=True)
with tempfile.TemporaryDirectory(prefix='localwispr-insertion-') as folder:
 p=pathlib.Path(folder);(p/'main.rs').write_text('\n'.join(code))
 subprocess.run(['rustc','--edition=2024',str(p/'main.rs'),'-o',str(p/'check')],check=True)
 raise SystemExit(subprocess.run([str(p/'check')]).returncode)
