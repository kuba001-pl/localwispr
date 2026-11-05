#![allow(dead_code)]
#[path="frozen/src/config.rs"] mod config;
#[path="frozen/src/stt.rs"] mod stt;
#[path="frozen/src/insertion.rs"] mod insertion;
mod audio { pub const SAMPLE_RATE:u32=16000; }
use std::io::{BufRead,Write};
use serde_json::{Value,json};
#[tokio::main]
async fn main()->anyhow::Result<()> {
    let models=std::env::var("LOCALWISPR_BENCH_MODELS").map(std::path::PathBuf::from)
        .unwrap_or_else(|_|dirs::data_dir().unwrap().join("localwispr/models"));
    let stt=stt::SttWorker::spawn(models.join("ggml-large-v3-turbo.bin"),true)?;
    for line in std::io::stdin().lock().lines() {
        let v:Value=serde_json::from_str(&line?)?;
        let mut cfg=config::Transcription::default();
        cfg.vad=true;
        if v["accuracy"]=="Fast" { cfg.accuracy=config::Accuracy::Fast; }
        cfg.languages=serde_json::from_value(v["languages"].clone())?;
        let context=insertion::SurroundingText{
            before:v["before"].as_str().unwrap_or("").into(),
            after:v["after"].as_str().unwrap_or("").into(), is_replacement:false};
        let manual=v["manual"].as_str().unwrap_or("");
        cfg.initial_prompt=if v["condition"]=="off" {manual.into()}
          else if let Some(prompt)=v["override_prompt"].as_str() {prompt.into()}
          else {insertion::recognition_prompt(manual,Some(&context))};
        let bytes=std::fs::read(v["audio"].as_str().unwrap())?;
        let samples=bytes.chunks_exact(4).map(|b|f32::from_le_bytes(b.try_into().unwrap())).collect();
        stt.new_utterance();
        // Timing starts after loading audio; includes worker queue, VAD, language
        // detection, Whisper decode and the frozen STT filters. No capture/LLM.
        stt::set_normalize_long_prompt(v["normalize_long_prompt"].as_bool().unwrap_or(false));
        let _ = stt::take_confidence();
        let start=std::time::Instant::now();
        let transcript=stt.submit(samples,false,&cfg,Some(models.join("ggml-silero-v6.2.0.bin"))).await??;
        println!("{}",json!({"request":v,"text":transcript.text,"language":transcript.language,
            "stt_ms":transcript.ms,"submit_ms":start.elapsed().as_millis(),
            "audio_secs":transcript.audio_secs,"prompt":cfg.initial_prompt,"confidence":stt::take_confidence()}));
        std::io::stdout().flush()?;
    }
    Ok(())
}
