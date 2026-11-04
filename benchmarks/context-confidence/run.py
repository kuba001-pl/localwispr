from pathlib import Path
import subprocess,json,hashlib,datetime,sys
out=Path(__file__).resolve().parent
stage=sys.argv[1] if len(sys.argv)>1 else 'dev'
req=out/('requests.jsonl' if stage=='dev' else stage+'-requests.jsonl');dest=out/'results'/(stage+'.jsonl')
assert not dest.exists(),dest
meta={'start':datetime.datetime.now(datetime.timezone.utc).isoformat(),'requests_sha256':hashlib.sha256(req.read_bytes()).hexdigest(),'binary_sha256':hashlib.sha256((out/'recognition-harness').read_bytes()).hexdigest()}
(out/'results'/(stage+'-run.json')).write_text(json.dumps(meta,indent=2))
with req.open() as f,(out/'results'/(stage+'.stderr.log')).open('w') as err,dest.open('w') as result:
 p=subprocess.Popen([str(out/'recognition-harness')],stdin=subprocess.PIPE,stdout=subprocess.PIPE,stderr=err,text=True)
 for i,line in enumerate(f):
  p.stdin.write(line);p.stdin.flush();s=p.stdout.readline();assert s,(p.poll(),i);row=json.loads(s);assert row['request']==json.loads(line);result.write(s);result.flush()
  if i%25==0:print(stage,i+1,flush=True)
 p.stdin.close();assert p.wait()==0
meta['end']=datetime.datetime.now(datetime.timezone.utc).isoformat();meta['result_sha256']=hashlib.sha256(dest.read_bytes()).hexdigest();(out/'results'/(stage+'-run.json')).write_text(json.dumps(meta,indent=2));print('done',i+1,flush=True)
