#!/usr/bin/env python3
"""Generate public synthetic audio using the already installed eSpeak library."""
import array
import ctypes as C
import hashlib
import json
import math
from pathlib import Path
import random
import subprocess
import wave

OUT=Path(__file__).resolve().parent
PREFIX=Path('/usr/share/steam/compatibilitytools.d/proton-cachyos-slr/files')
VOICE_ALIASES={'en-gb':'en','en-sc':'en-gb-scotland'}

def sha(path): return hashlib.sha256(path.read_bytes()).hexdigest()

def main():
    if (OUT/'audio-manifest.json').exists():
        raise SystemExit('Refusing to overwrite canonical fixtures. Restore exact audio with restore_audio.py; use an isolated directory for fresh synthesis.')
    rows=[json.loads(s) for s in (OUT/'corpus.jsonl').read_text().splitlines()]
    (OUT/'audio').mkdir(exist_ok=True)
    libpath=PREFIX/'lib/x86_64-linux-gnu/libespeak-ng.so'
    lib=C.CDLL(str(libpath))
    lib.espeak_Initialize.argtypes=[C.c_int,C.c_int,C.c_char_p,C.c_int]
    rate=lib.espeak_Initialize(2,0,str(PREFIX/'share').encode(),0)
    assert rate>0
    CALLBACK=C.CFUNCTYPE(C.c_int,C.POINTER(C.c_short),C.c_int,C.c_void_p)
    chunks=[]
    @CALLBACK
    def callback(samples,n,events):
        if samples and n: chunks.append(C.string_at(samples,n*2))
        return 0
    lib.espeak_SetSynthCallback(callback)
    lib.espeak_SetVoiceByName.argtypes=[C.c_char_p]
    lib.espeak_Synth.argtypes=[C.c_void_p,C.c_size_t,C.c_uint,C.c_int,C.c_uint,C.c_uint,C.c_void_p,C.c_void_p]
    version=C.c_char_p()
    lib.espeak_Info.restype=C.c_char_p
    lib.espeak_Info.argtypes=[C.POINTER(C.c_char_p)]
    info={'engine':'eSpeak NG synthetic speech','version':lib.espeak_Info(C.byref(version)).decode(),
          'library':str(libpath),'library_sha256':sha(libpath),'source_rate':rate,
          'output':'mono float32 little endian 16kHz; ffmpeg default resampling + 300ms end silence',
          'voices':sorted({r['voice'] for r in rows if 'voice' in r}),
          'voice_alias_resolution':VOICE_ALIASES,
          'speeds_wpm':[135,160,185], 'private_audio':False,'downloads':False,
          'warning':'Polish eSpeak pronounces English brand names and mixed prose artificially. Voice variants share the same synthesis family.'}
    assets=[]
    for i,r in enumerate(rows):
        raw=OUT/'audio'/(r['id']+'.f32')
        if r['category']=='acoustic':
            rng=random.Random(4917+i)
            kind=r['noise']; values=[]; smooth=0.
            for j in range(16000*4):
                white=rng.uniform(-1,1)
                smooth=.985*smooth+.015*white
                t=j/16000
                value={'silence':0.,'white_quiet':white*.002,'white_loud':white*.08,
                       'pink':smooth*.35,'tone':math.sin(2*math.pi*440*t)*.03,
                       'hum':math.sin(2*math.pi*50*t)*.04+white*.001,
                       'clicks':.2 if j%4000<8 else 0.,
                       'fade_noise':white*.05*(.5+.5*math.sin(2*math.pi*.4*t))}[kind]
                values.append(value)
            data=array.array('f',values)
            if __import__('sys').byteorder!='little':data.byteswap()
            raw.write_bytes(data.tobytes())
        else:
            chunks.clear()
            voice=VOICE_ALIASES.get(r['voice'],r['voice'])
            assert lib.espeak_SetVoiceByName(voice.encode())==0,voice
            assert lib.espeak_SetParameter(1,r['speed'],0)==0
            data=r['reference'].encode(); buf=C.create_string_buffer(data)
            assert lib.espeak_Synth(buf,len(data)+1,0,1,0,1,None,None)==0
            lib.espeak_Synchronize()
            wav=raw.with_suffix('.wav')
            with wave.open(str(wav),'wb') as w:
                w.setnchannels(1);w.setsampwidth(2);w.setframerate(rate);w.writeframes(b''.join(chunks))
            subprocess.run(['ffmpeg','-v','error','-y','-i',str(wav),'-af','apad=pad_dur=0.3',
                            '-ar','16000','-ac','1','-f','f32le',str(raw)],check=True)
            wav.unlink()
        assets.append({'id':r['id'],'audio':str(raw.relative_to(OUT)),'sha256':sha(raw),'seconds':raw.stat().st_size/64000})
    (OUT/'audio-manifest.json').write_text(json.dumps(assets,indent=2)+'\n')
    (OUT/'tts-info.json').write_text(json.dumps(info,indent=2)+'\n')
    models=Path.home()/'.local/share/localwispr/models'
    (OUT/'model-hashes.json').write_text(json.dumps({n:sha(models/n) for n in ['ggml-large-v3-turbo.bin','ggml-silero-v6.2.0.bin']},indent=2)+'\n')
    print(f'Generated {len(assets)} fixtures, {sum(r["seconds"] for r in assets):.1f}s; voices {info["voices"]}')

if __name__=='__main__':main()
