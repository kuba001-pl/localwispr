//! Speech-to-text worker (whisper.cpp via whisper-rs).
//!
//! A single thread owns the model and one decoding state. Live previews and
//! the final pass share it; a preview in flight is aborted the moment the
//! final pass is requested, so release-to-text latency is never blocked by a
//! stale preview.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use tokio::sync::oneshot;
use whisper_rs::{
    FullParams, SamplingStrategy, WhisperContext, WhisperContextParameters, WhisperVadContext,
    WhisperVadContextParams,
};

use crate::audio::SAMPLE_RATE;
static NORMALIZE_LONG_PROMPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);
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
static CONFIDENCE: std::sync::OnceLock<std::sync::Mutex<serde_json::Value>> = std::sync::OnceLock::new();
pub fn take_confidence() -> serde_json::Value {
    CONFIDENCE.get_or_init(|| std::sync::Mutex::new(serde_json::Value::Null)).lock().unwrap().take()
}

use crate::config::{Accuracy, Transcription};

#[derive(Debug, Clone)]
pub struct Transcript {
    pub text: String,
    pub language: String,
    pub audio_secs: f32,
    pub ms: u64,
}

struct Job {
    id: u64,
    audio: Vec<f32>,
    partial: bool,
    cfg: Transcription,
    vad: Option<std::path::PathBuf>,
    reply: oneshot::Sender<Result<Transcript>>,
}

enum Msg {
    Job(Job),
    NewUtterance,
}

pub struct SttWorker {
    tx: Option<crossbeam_channel::Sender<Msg>>,
    thread: Option<std::thread::JoinHandle<()>>,
    next_id: AtomicU64,
    /// Jobs with an id below this value abort as soon as possible.
    abort_below: Arc<AtomicU64>,
    pub model_path: std::path::PathBuf,
}

impl SttWorker {
    /// Loads the model (blocking, ~1 s for large-v3-turbo) and warms up the GPU.
    pub fn spawn(model_path: std::path::PathBuf, use_gpu: bool) -> Result<Self> {
        let t0 = Instant::now();
        let path_str = model_path
            .to_str()
            .ok_or_else(|| anyhow!("non-UTF-8 model path"))?;
        let mut cparams = WhisperContextParameters::default();
        cparams.use_gpu(use_gpu).flash_attn(use_gpu);
        let ctx = WhisperContext::new_with_params(path_str, cparams)
            .with_context(|| format!("loading whisper model {}", model_path.display()))?;
        let mut state = ctx.create_state().context("creating whisper state")?;

        // Warm-up: the first GPU run initialises cuBLAS and kernels (~0.5 s).
        {
            let mut p = FullParams::new(SamplingStrategy::Greedy { best_of: 1 });
            p.set_language(Some("en"));
            quiet(&mut p);
            let _ = state.full(p, &vec![0.0; SAMPLE_RATE as usize]);
        }
        tracing::info!(
            "whisper model {} ready in {} ms (gpu: {use_gpu})",
            model_path.file_name().unwrap_or_default().to_string_lossy(),
            t0.elapsed().as_millis()
        );

        let (tx, rx) = crossbeam_channel::unbounded::<Msg>();
        let abort_below = Arc::new(AtomicU64::new(0));
        let abort = abort_below.clone();
        let thread = std::thread::Builder::new()
            .name("stt".into())
            .spawn(move || {
                // Keep the model on this thread for decoding and token budgeting.
                let mut lang_cache: Option<(String, f32)> = None;
                let mut vad: Option<(std::path::PathBuf, WhisperVadContext)> = None;
                for msg in rx {
                    match msg {
                        Msg::NewUtterance => lang_cache = None,
                        Msg::Job(job) => {
                            if abort.load(Ordering::Relaxed) > job.id {
                                let _ = job.reply.send(Err(anyhow!("aborted")));
                                continue;
                            }
                            let t0 = Instant::now();
                            let speech = if job.partial {
                                None
                            } else {
                                speech_only(&job, &mut vad)
                            };
                            let res = match speech {
                                Some(s) if s.is_empty() => Ok(Transcript {
                                    text: String::new(),
                                    language: String::new(),
                                    audio_secs: job.audio.len() as f32 / SAMPLE_RATE as f32,
                                    ms: t0.elapsed().as_millis() as u64,
                                }),
                                Some(s) => transcribe(
                                    &ctx,
                                    &mut state,
                                    &job,
                                    &s,
                                    &mut lang_cache,
                                    abort.clone(),
                                )
                                .map(|mut t| {
                                    t.audio_secs = job.audio.len() as f32 / SAMPLE_RATE as f32;
                                    t.ms = t0.elapsed().as_millis() as u64;
                                    t
                                }),
                                None => transcribe(
                                    &ctx,
                                    &mut state,
                                    &job,
                                    &job.audio,
                                    &mut lang_cache,
                                    abort.clone(),
                                ),
                            };
                            let _ = job.reply.send(res);
                        }
                    }
                }
            })?;
        Ok(Self {
            tx: Some(tx),
            thread: Some(thread),
            next_id: AtomicU64::new(1),
            abort_below,
            model_path,
        })
    }

    fn send(&self, msg: Msg) {
        if let Some(tx) = &self.tx {
            let _ = tx.send(msg);
        }
    }

    pub fn new_utterance(&self) {
        self.send(Msg::NewUtterance);
    }

    /// Aborts every job submitted so far (e.g. on cancel).
    pub fn abort_pending(&self) {
        self.abort_below
            .fetch_max(self.next_id.load(Ordering::Relaxed), Ordering::Relaxed);
    }

    pub fn submit(
        &self,
        audio: Vec<f32>,
        partial: bool,
        cfg: &Transcription,
        vad: Option<std::path::PathBuf>,
    ) -> oneshot::Receiver<Result<Transcript>> {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        if !partial {
            // Final pass preempts any preview still running.
            self.abort_below.fetch_max(id, Ordering::Relaxed);
        }
        let (reply, rx) = oneshot::channel();
        self.send(Msg::Job(Job {
            id,
            audio,
            partial,
            cfg: cfg.clone(),
            vad,
            reply,
        }));
        rx
    }
}

impl Drop for SttWorker {
    /// Frees the model on its own thread *before* returning, so GPU memory is
    /// released deterministically (model switches) and CUDA is never torn
    /// down underneath a live context (process exit).
    fn drop(&mut self) {
        self.abort_pending();
        self.tx = None;
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Runs Silero VAD once and returns only the speech (segments joined by short
/// silences). `Some(empty)` = no speech at all — the case where Whisper would
/// otherwise hallucinate ("you", "Thank you"). `None` = VAD unavailable/off.
/// Replaces whisper.cpp's built-in VAD so audio is scanned once, not twice,
/// and the decoder only sees speech.
fn speech_only(
    job: &Job,
    cache: &mut Option<(std::path::PathBuf, WhisperVadContext)>,
) -> Option<Vec<f32>> {
    if !job.cfg.vad {
        return None;
    }
    let path = job.vad.as_ref()?;
    if cache.as_ref().is_none_or(|(p, _)| p != path) {
        let mut params = WhisperVadContextParams::new();
        params.set_n_threads(2);
        params.set_use_gpu(false);
        *cache = path
            .to_str()
            .and_then(|p| WhisperVadContext::new(p, params).ok())
            .map(|c| (path.clone(), c));
    }
    let (_, vad) = cache.as_mut()?;
    let t = Instant::now();
    let mut vparams = whisper_rs::WhisperVadParams::new();
    vparams.set_threshold(0.5);
    vparams.set_min_speech_duration(150);
    vparams.set_min_silence_duration(500);
    vparams.set_speech_pad(200);
    let segs = vad.segments_from_samples(vparams, &job.audio).ok()?;
    let out = join_speech_segments(
        &job.audio,
        (0..segs.num_segments()).filter_map(|i| {
            Some((
                segs.get_segment_start_timestamp(i)?,
                segs.get_segment_end_timestamp(i)?,
            ))
        }),
    );
    tracing::debug!(
        "vad: {:.1}s speech of {:.1}s in {} ms",
        out.len() as f32 / SAMPLE_RATE as f32,
        job.audio.len() as f32 / SAMPLE_RATE as f32,
        t.elapsed().as_millis()
    );
    Some(out)
}

/// Silero segment timestamps are centiseconds. Keep a short gap for word
/// boundaries, without forwarding long silent spans to Whisper's state API.
fn join_speech_segments(audio: &[f32], segments: impl IntoIterator<Item = (f32, f32)>) -> Vec<f32> {
    let gap = [0.0; SAMPLE_RATE as usize * 3 / 20];
    let mut out = Vec::with_capacity(audio.len());
    for (start, end) in segments {
        if !start.is_finite() || !end.is_finite() || start < 0.0 || end <= start {
            continue;
        }
        let a = ((start as f64 / 100.0) * SAMPLE_RATE as f64) as usize;
        let b = (((end as f64 / 100.0) * SAMPLE_RATE as f64) as usize).min(audio.len());
        if a < b {
            if !out.is_empty() {
                out.extend_from_slice(&gap);
            }
            out.extend_from_slice(&audio[a..b]);
        }
    }
    out
}

struct AbortCtx {
    flag: Arc<AtomicU64>,
    id: u64,
}

impl AbortCtx {
    fn check(&self) -> Result<()> {
        if self.flag.load(Ordering::Relaxed) > self.id {
            bail!("aborted");
        }
        Ok(())
    }
}

unsafe extern "C" fn abort_trampoline(user_data: *mut std::ffi::c_void) -> bool {
    // SAFETY: points at an `AbortCtx` on the caller's stack that lives until
    // `WhisperState::full` returns.
    let ctx = unsafe { &*(user_data as *const AbortCtx) };
    ctx.flag.load(Ordering::Relaxed) > ctx.id
}

fn quiet(p: &mut FullParams<'_, '_>) {
    p.set_print_progress(false);
    p.set_print_realtime(false);
    p.set_print_special(false);
    p.set_print_timestamps(false);
}

fn rms(x: &[f32]) -> f32 {
    if x.is_empty() {
        return 0.0;
    }
    (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
}

fn transcribe(
    context: &WhisperContext,
    state: &mut whisper_rs::WhisperState,
    job: &Job,
    input: &[f32],
    lang_cache: &mut Option<(String, f32)>,
    abort: Arc<AtomicU64>,
) -> Result<Transcript> {
    let t0 = Instant::now();
    let cfg = &job.cfg;
    let audio_secs = input.len() as f32 / SAMPLE_RATE as f32;
    let threads = cfg.threads as usize;

    // Previews of near-silence are pointless and are where Whisper hallucinates.
    if job.partial && rms(input) < 0.004 {
        return Ok(Transcript {
            text: String::new(),
            language: String::new(),
            audio_secs,
            ms: 0,
        });
    }

    // whisper.cpp wants ≥1 s; pad short clips with silence.
    let mut audio = std::borrow::Cow::Borrowed(input);
    let min_len = SAMPLE_RATE as usize + SAMPLE_RATE as usize / 10;
    if audio.len() < min_len {
        let mut padded = input.to_vec();
        padded.resize(min_len, 0.0);
        audio = std::borrow::Cow::Owned(padded);
    }

    let abort_ctx = AbortCtx {
        flag: abort.clone(),
        id: job.id,
    };
    abort_ctx.check()?;
    let language = choose_language(
        state,
        cfg,
        &audio,
        audio_secs,
        job.partial,
        lang_cache,
        threads,
        &abort_ctx,
    )?;
    abort_ctx.check()?;

    let strategy = match (cfg.accuracy, job.partial) {
        (Accuracy::Best, false) => SamplingStrategy::BeamSearch {
            beam_size: 5,
            patience: -1.0,
        },
        _ => SamplingStrategy::Greedy { best_of: 1 },
    };
    let mut p = FullParams::new(strategy);
    quiet(&mut p);
    p.set_n_threads(threads as i32);
    p.set_translate(false);
    p.set_no_context(true);
    // Code-switching: in no-timestamp mode Whisper tends to end the
    // transcript where the other language starts, or translate that part.
    let switch_hint = code_switch_hint(&cfg.languages);
    p.set_no_timestamps(switch_hint.is_none());
    p.set_suppress_blank(true);
    p.set_suppress_nst(true);
    p.set_no_speech_thold(0.6);
    p.set_language(Some(language.as_str()));
    if job.partial || cfg.accuracy == Accuracy::Fast {
        p.set_temperature_inc(0.0); // no fallback re-decodes
    }
    if job.partial {
        // Previews use the full 30 s encoder context, like the final pass.
        // Shrinking `audio_ctx` to the clip length looked like a cheap speedup
        // (~40–100 ms on an RTX 3080), but Whisper was never trained on it:
        // large-v3-turbo then loops, hallucinates and returns empty text for
        // whole seconds, so the HUD showed gibberish or stalled.
        //
        // Bound runaway decodes anyway; real speech stays well under ~8
        // tokens/s even in Polish.
        p.set_max_tokens((audio_secs * 8.0) as i32 + 16);
    }
    let prompt_tokens = bounded_prompt_tokens(context, cfg.initial_prompt.trim(), switch_hint)?;
    if !prompt_tokens.is_empty() {
        p.set_tokens(&prompt_tokens);
    }
    // whisper-rs 0.16's `set_abort_callback_safe` casts its double-boxed
    // closure back to the wrong type (reads garbage → random aborts), so use
    // the raw API with a context that outlives the call.
    unsafe {
        p.set_abort_callback(Some(abort_trampoline));
        p.set_abort_callback_user_data(&abort_ctx as *const AbortCtx as *mut std::ffi::c_void);
    }

    let res = state.full(p, &audio);
    if abort.load(Ordering::Relaxed) > job.id {
        bail!("aborted");
    }
    res.map_err(|e| anyhow!("whisper: {e}"))?;

    let confidence: Vec<_> = state.as_iter().map(|seg| {
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
    for seg in state.as_iter() {
        if seg.no_speech_probability() > 0.85 {
            continue;
        }
        if let Ok(s) = seg.to_str_lossy() {
            text.push_str(&s);
        }
    }
    let mut text = filter_hallucinations(&text);
    let (collapsed, looped) = collapse_loops(&text);
    if looped && job.partial {
        text.clear(); // keep showing the previous preview rather than a loop
    } else {
        text = collapsed;
    }
    if !text.chars().any(char::is_alphanumeric) {
        text.clear(); // "." / "…" on noise
    }

    Ok(Transcript {
        text,
        language,
        audio_secs,
        ms: t0.elapsed().as_millis() as u64,
    })
}

/// A mixed-language sentence passed as Whisper's prompt when both languages
/// are allowed. It shows the decoder that switching mid-sentence is expected,
/// so it keeps each language as spoken instead of dropping or translating it.
/// Only pairs that were measured are listed; a generic concatenation of one
/// phrase per language did not work as well.
const CODE_SWITCH_HINTS: &[(&str, &str, &str)] = &[(
    "pl",
    "en",
    "Dobra, zrobię deploy na staging, okay? Potem check the logs i fix tego buga.",
)];

fn bounded_prompt_tokens(
    context: &WhisperContext,
    configured: &str,
    switch_hint: Option<&str>,
) -> Result<Vec<whisper_rs::WhisperTokenId>> {
    // Native Whisper uses at most half the text context, minus the previous-
    // text marker. Reserve the bilingual hint first, then retain the suffix
    // containing nearby vocabulary and the user's highest-priority manual hint.
    let budget = (context.n_text_ctx() / 2).saturating_sub(1).max(0) as usize;
    let hint = switch_hint.unwrap_or("");
    let mut tokens = context.tokenize(hint, hint.len().saturating_add(1))?;
    tokens.truncate(budget);
    if !configured.is_empty() && tokens.len() < budget {
        let text = if hint.is_empty() {
            configured.to_owned()
        } else {
            format!(" {configured}")
        };
        let mut configured_tokens = context.tokenize(&text, text.len().saturating_add(1))?;
        if NORMALIZE_LONG_PROMPT.load(Ordering::Relaxed) && configured_tokens.len()>budget-tokens.len() {
            let normalized=collapse_exact_prompt_phrases(&text);
            configured_tokens=context.tokenize(&normalized, normalized.len().saturating_add(1))?;
        }
        let retained = configured_tokens.len().min(budget - tokens.len());
        tokens.extend_from_slice(&configured_tokens[configured_tokens.len() - retained..]);
    }
    Ok(tokens)
}

fn code_switch_hint(languages: &[String]) -> Option<&'static str> {
    let allowed = |code: &str| languages.iter().any(|l| l == code);
    CODE_SWITCH_HINTS
        .iter()
        .find(|(a, b, _)| allowed(a) && allowed(b))
        .map(|(_, _, hint)| *hint)
}

#[allow(clippy::too_many_arguments)]
fn choose_language(
    state: &mut whisper_rs::WhisperState,
    cfg: &Transcription,
    audio: &[f32],
    audio_secs: f32,
    partial: bool,
    cache: &mut Option<(String, f32)>,
    threads: usize,
    abort: &AbortCtx,
) -> Result<String> {
    if cfg.languages.len() == 1 {
        return Ok(cfg.languages[0].clone());
    }
    if let Some((lang, detected_on)) = cache.as_ref() {
        // A detection on ≥3 s is trusted for the rest of the utterance. Below
        // that, previews re-detect once they reach 3 s, so the final pass —
        // the latency-critical one — normally finds a trusted cached answer.
        let trusted = *detected_on >= 3.0 || audio_secs < *detected_on + 1.0;
        if trusted || (partial && audio_secs < 3.0) {
            return Ok(lang.clone());
        }
    }
    if partial && audio_secs < 1.0 {
        return Ok(cfg
            .languages
            .first()
            .cloned()
            .unwrap_or_else(|| "en".into()));
    }

    // Detection only needs a short sample. Bound mel work and check between
    // stages; the patched detector passes our callback into encoder/decoder.
    let detection_audio = &audio[..audio.len().min(SAMPLE_RATE as usize * 3)];
    abort.check()?;
    state
        .pcm_to_mel(detection_audio, threads)
        .map_err(|e| anyhow!("mel: {e}"))?;
    abort.check()?;
    // SAFETY: `abort` remains alive for this synchronous call.
    let detection = unsafe {
        state.lang_detect_with_abort(
            0,
            threads,
            Some(abort_trampoline),
            abort as *const AbortCtx as *mut std::ffi::c_void,
        )
    };
    abort.check()?;
    let (_, probs) = detection.map_err(|e| anyhow!("lang detect: {e}"))?;
    let best = if cfg.languages.is_empty() {
        probs
            .iter()
            .enumerate()
            .max_by(|a, b| a.1.total_cmp(b.1))
            .and_then(|(i, _)| whisper_rs::get_lang_str(i as i32))
            .unwrap_or("en")
            .to_owned()
    } else {
        cfg.languages
            .iter()
            .filter_map(|l| {
                let id = whisper_rs::get_lang_id(l)?;
                Some((l, probs.get(id as usize).copied().unwrap_or(0.0)))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(l, _)| l.clone())
            .unwrap_or_else(|| cfg.languages[0].clone())
    };
    tracing::debug!(
        "detected language {best} (p {:.2}) on {audio_secs:.1}s",
        whisper_rs::get_lang_id(&best)
            .and_then(|id| probs.get(id as usize))
            .copied()
            .unwrap_or(0.0)
    );
    *cache = Some((best.clone(), audio_secs));
    Ok(best)
}

/// Phrases Whisper emits on silence/noise (learned from subtitle training data).
const HALLUCINATIONS: &[&str] = &[
    "napisy stworzone przez społeczność amara.org",
    "napisy wykonane przez społeczność amara.org",
    "subtitles by the amara.org community",
    "thank you for watching",
    "thanks for watching",
    "dziękuję za obejrzenie",
    "dzięki za obejrzenie",
    "transcribed by",
    "napisy by",
];

/// Byte spans of words, so deterministic edits never rebuild whitespace.
pub(crate) fn word_spans(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut spans = Vec::new();
    let mut start = None;
    for (i, c) in text.char_indices() {
        if c.is_whitespace() {
            if let Some(start) = start.take() {
                spans.push(start..i);
            }
        } else {
            start.get_or_insert(i);
        }
    }
    if let Some(start) = start {
        spans.push(start..text.len());
    }
    spans
}

/// Explicit code and indented lines are opaque to speech cleanup. Unclosed
/// backticks/fences are protected too: previews often end inside a code span.
fn code_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = Vec::new();
    let mut fence: Option<(char, usize)> = None;
    let mut offset = 0;
    for line in text.split_inclusive('\n') {
        let body = line.trim_start_matches([' ', '\t']);
        let marker = body.chars().next().unwrap_or(' ');
        let run = body.chars().take_while(|c| *c == marker).count();
        let boundary = matches!(marker, '`' | '~') && run >= 3;
        let in_fence = fence.is_some();
        if let Some((c, n)) = fence {
            if boundary && marker == c && run >= n {
                fence = None;
            }
        } else if boundary {
            fence = Some((marker, run));
        }
        let code_statement = (["let ", "const ", "var "]
            .iter()
            .any(|prefix| body.starts_with(prefix))
            && body.contains('='))
            || (["fn ", "pub fn "]
                .iter()
                .any(|prefix| body.starts_with(prefix))
                && body.contains('('));
        let command = [
            "cargo test",
            "cargo run",
            "cargo build",
            "git diff",
            "git status",
            "npm run",
        ]
        .iter()
        .any(|prefix| {
            body.trim_end().strip_prefix(prefix).is_some_and(|args| {
                (args.is_empty() || args.starts_with(char::is_whitespace))
                    && args
                        .split_whitespace()
                        .all(|arg| arg.starts_with('-') || arg.contains('/'))
            })
        });
        if in_fence
            || boundary
            || line.starts_with("    ")
            || line.starts_with('\t')
            || code_statement
            || command
        {
            ranges.push(offset..offset + line.trim_end_matches(['\r', '\n']).len());
        }
        offset += line.len();
    }
    let mut i = 0;
    while i < text.len() {
        if let Some(range) = ranges.iter().find(|r| r.contains(&i)) {
            i = range.end;
            continue;
        }
        if text.as_bytes()[i] == b'`' {
            let n = text[i..].bytes().take_while(|b| *b == b'`').count();
            let delimiter = "`".repeat(n);
            let end = text[i + n..]
                .find(&delimiter)
                .map_or(text.len(), |end| i + n + end + n);
            ranges.push(i..end);
            i = end;
        } else {
            i += text[i..].chars().next().unwrap().len_utf8();
        }
    }
    ranges.sort_by_key(|r| r.start);
    ranges
}

pub(crate) fn overlaps(a: &std::ops::Range<usize>, b: &std::ops::Range<usize>) -> bool {
    a.start < b.end && b.start < a.end
}

/// Exact technical fragments. Ordinary punctuation/case in prose stays editable.
pub(crate) fn literal_ranges(text: &str) -> Vec<std::ops::Range<usize>> {
    let mut ranges = code_ranges(text);
    let spans = word_spans(text);
    for (i, span) in spans.iter().enumerate() {
        let token = &text[span.clone()];
        let left_trimmed = token.trim_start_matches([',', '.', '!', '?', ':', ';']);
        let core = left_trimmed.trim_end_matches([',', '.', '!', '?', ':', ';']);
        if ((core.starts_with('[') && core.ends_with(']'))
            || (core.starts_with('(') && core.ends_with(')')))
            || core.contains('_')
            || core.contains("::")
            || core.starts_with("--")
            || matches!(core, "em" | "er" | "EM" | "ER" | "mm" | "MM")
            || (matches!(core, "um" | "hm")
                && i > 0
                && text[spans[i - 1].clone()]
                    .chars()
                    .last()
                    .is_some_and(|c| c.is_ascii_digit()))
        {
            let start = span.start + token.len() - left_trimmed.len();
            ranges.push(start..start + core.len());
        }
    }
    ranges.sort_by_key(|r| r.start);
    // Code lines already include their inner tokens.
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for range in ranges {
        if let Some(prev) = merged.last_mut().filter(|r| r.end >= range.start) {
            prev.end = prev.end.max(range.end);
        } else {
            merged.push(range);
        }
    }
    merged
}

/// Remove only the selected spans and redundant adjacent horizontal spaces.
/// Newlines and the indentation following them always belong to the source.
fn removal_span(text: &str, span: &std::ops::Range<usize>) -> std::ops::Range<usize> {
    let mut range = span.clone();
    let after = &text[range.end..];
    range.end += after.len() - after.trim_start_matches([' ', '\t']).len();
    if range.end == span.end {
        let before = &text[..range.start];
        let line = before.rsplit('\n').next().unwrap_or(before);
        if line.chars().any(|c| !c.is_whitespace()) {
            range.start -= line.len() - line.trim_end_matches([' ', '\t']).len();
        }
    }
    range
}

pub(crate) fn removal_spans(
    text: &str,
    spans: &[std::ops::Range<usize>],
) -> Vec<std::ops::Range<usize>> {
    let mut sorted = spans.to_vec();
    sorted.sort_by_key(|r| r.start);
    let mut merged: Vec<std::ops::Range<usize>> = Vec::new();
    for span in sorted {
        if let Some(prev) = merged.last_mut().filter(|r| {
            r.end >= span.start
                || text[r.end..span.start]
                    .chars()
                    .all(|c| matches!(c, ' ' | '\t'))
        }) {
            prev.end = prev.end.max(span.end);
        } else {
            merged.push(span);
        }
    }
    merged.iter().map(|r| removal_span(text, r)).collect()
}

pub(crate) fn remove_spans(text: &str, spans: &[std::ops::Range<usize>]) -> String {
    let edits = removal_spans(text, spans);
    let mut out = String::new();
    let mut cursor = 0;
    for range in edits {
        if range.start > cursor {
            out.push_str(&text[cursor..range.start]);
        }
        cursor = cursor.max(range.end);
    }
    out.push_str(&text[cursor..]);
    out
}

pub fn filter_hallucinations(text: &str) -> String {
    let protected = code_ranges(text);
    let mut removed = Vec::new();
    // Match within original UTF-8 spans instead of using offsets from a
    // lowercased copy (case folding can change individual byte lengths).
    let spans = word_spans(text);
    for (i, span) in spans.iter().enumerate() {
        if protected.iter().any(|r| overlaps(r, span)) {
            continue;
        }
        let token = text[span.clone()].trim_end_matches(['.', '!', ',', '?']);
        let annotation = token
            .strip_prefix('[')
            .and_then(|s| s.strip_suffix(']'))
            .or_else(|| token.strip_prefix('(').and_then(|s| s.strip_suffix(')')));
        if annotation.is_some_and(|s| {
            matches!(
                s.to_lowercase().as_str(),
                "music"
                    | "applause"
                    | "laughter"
                    | "laughing"
                    | "silence"
                    | "muzyka"
                    | "śmiech"
                    | "oklaski"
                    | "cisza"
            )
        }) {
            removed.push(span.clone());
            continue;
        }
        for phrase in HALLUCINATIONS {
            let count = phrase.split_whitespace().count();
            if let Some(last) = spans.get(i + count - 1) {
                let range = span.start..last.end;
                if protected.iter().any(|r| overlaps(r, &range)) {
                    continue;
                }
                let candidate = text[range.clone()]
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
                if candidate
                    .trim_end_matches(['.', '!', ',', '?'])
                    .to_lowercase()
                    == *phrase
                {
                    removed.push(range);
                }
            }
        }
    }
    let out = remove_spans(text, &removed);
    // Retain the historical tidying of spaces in plain, single-line speech.
    // Structured or literal input keeps its original whitespace exactly.
    if !text.contains(['\n', '\r', '\t']) && literal_ranges(text).is_empty() {
        out.split_whitespace().collect::<Vec<_>>().join(" ")
    } else {
        out
    }
}

/// Collapses decoder loops without reflowing unaffected text or touching code.
pub fn collapse_loops(text: &str) -> (String, bool) {
    let spans = word_spans(text);
    let protected = literal_ranges(text);
    let opaque: Vec<bool> = spans
        .iter()
        .map(|s| protected.iter().any(|r| overlaps(s, r)))
        .collect();
    let words: Vec<String> = spans
        .iter()
        .enumerate()
        .map(|(i, span)| {
            let word = &text[span.clone()];
            if opaque[i] {
                word.to_owned()
            } else {
                collapse_word(word).0
            }
        })
        .collect();
    let keys: Vec<String> = words
        .iter()
        .map(|word| {
            word.trim_matches(|c: char| c.is_ascii_punctuation())
                .to_lowercase()
        })
        .collect();
    let mut edits = Vec::new();
    let mut i = 0;
    'scan: while i < spans.len() {
        if !opaque[i] {
            for n in 1..=4 {
                let mut reps = 1;
                while i + (reps + 1) * n <= spans.len() {
                    let end = i + (reps + 1) * n;
                    if opaque[i..end].iter().any(|p| *p)
                        || text[spans[i].start..spans[end - 1].end].contains(['\n', '\r'])
                        || keys[i + reps * n..end] != keys[i..i + n]
                    {
                        break;
                    }
                    reps += 1;
                }
                if reps >= 4 {
                    let last = i + reps * n - 1;
                    let mut end = spans[last].end;
                    if text[end..].chars().all(|c| matches!(c, ' ' | '\t')) {
                        end = text.len();
                    }
                    edits.push((spans[i + n - 1].end..end, String::new()));
                    for j in i..i + n {
                        if words[j] != text[spans[j].clone()] {
                            edits.push((spans[j].clone(), words[j].clone()));
                        }
                    }
                    i = last + 1;
                    continue 'scan;
                }
            }
        }
        if words[i] != text[spans[i].clone()] {
            edits.push((spans[i].clone(), words[i].clone()));
        }
        i += 1;
    }
    let looped = !edits.is_empty();
    edits.sort_by_key(|(range, _)| range.start);
    let mut out = String::new();
    let mut cursor = 0;
    for (range, replacement) in edits {
        out.push_str(&text[cursor..range.start]);
        out.push_str(&replacement);
        cursor = range.end;
    }
    out.push_str(&text[cursor..]);
    (out, looped)
}

/// Character-level loops inside one "word" ("f-i-i-i-i-i-i", "theiszisziszisz").
fn collapse_word(word: &str) -> (String, bool) {
    let chars: Vec<char> = word.chars().collect();
    if chars.len() < 12 {
        return (word.to_owned(), false);
    }
    for p in 2..=6 {
        for start in 0..chars.len().saturating_sub(4 * p - 1) {
            let unit = &chars[start..start + p];
            if !unit.iter().any(|c| c.is_alphabetic()) {
                continue;
            }
            let mut reps = 1;
            while start + (reps + 1) * p <= chars.len()
                && chars[start + reps * p..start + (reps + 1) * p] == *unit
            {
                reps += 1;
            }
            if reps >= 4 {
                let mut out: String = chars[..start + p].iter().collect();
                out.extend(&chars[start + reps * p..]);
                return (collapse_word(&out).0, true);
            }
        }
    }
    (word.to_owned(), false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn collapses_decoder_loops() {
        let to = format!("and so my fellow Americans, ask not {}", "to, ".repeat(40));
        assert_eq!(
            collapse_loops(&to),
            ("and so my fellow Americans, ask not to,".into(), true)
        );
        let dots = format!("what your ccc {}", ". ".repeat(50));
        assert_eq!(collapse_loops(&dots), ("what your ccc .".into(), true));
        let of = "of ".repeat(30);
        assert_eq!(collapse_loops(&of), ("of".into(), true));
        let isz = format!("of the{}", "isz".repeat(40));
        assert_eq!(collapse_loops(&isz), ("of theisz".into(), true));
        let fi = format!("And so, my, f{}", "-i".repeat(40));
        assert_eq!(collapse_loops(&fi), ("And so, my, f-i".into(), true));
        let phrase = "ask what you ".repeat(5);
        assert_eq!(collapse_loops(&phrase), ("ask what you".into(), true));
    }

    #[test]
    fn keeps_natural_repetition() {
        for t in [
            "And so my fellow Americans, ask not what your country can do for you.",
            "Nie, nie, nie, tak nie będzie.",
            "very very very good",
            "Konstantynopolitańczykowianeczka najprawdopodobniej przyjdzie",
            "Numer 100000000000 i 1111111111111.",
            "It, it. It.",
        ] {
            assert_eq!(collapse_loops(t), (t.to_owned(), false), "{t}");
        }
    }

    #[test]
    fn filters_only_known_annotations_and_preserves_structure_and_literals() {
        for text in [
            "First paragraph.\n\nSecond paragraph.\n",
            "Use [value] and (x), then [--flag] and (HTTP).",
            "```rust\nfn main() {\n    let um = [1, 2];\n}\n```",
            "Use `[Music]` and `thank you for watching` literally.",
            "alpha\n\n    beta\n\tcommand",
            "Źródło ẞẞ é İİ\n[value] (x)",
            "Use ,x_ą. and (żółć).",
        ] {
            assert_eq!(filter_hallucinations(text), text, "{text:?}");
            assert_eq!(collapse_loops(text), (text.into(), false), "{text:?}");
        }
        assert_eq!(filter_hallucinations("[Music] Hello. (śmiech)"), "Hello.");
        assert_eq!(
            filter_hallucinations("[Music]\n\nHello. (śmiech)"),
            "\n\nHello."
        );
        assert_eq!(
            filter_hallucinations("ẞẞ é thank you for watching İİ"),
            "ẞẞ é İİ"
        );
        assert_eq!(
            collapse_loops("alpha\n\n  to to to to beta"),
            ("alpha\n\n  to beta".into(), true)
        );
        for literal in [
            "`um um um um`",
            "    um um um um",
            "[x] [x] [x] [x]",
            "alpha\nalpha\nalpha\nalpha",
        ] {
            assert_eq!(collapse_loops(literal), (literal.into(), false));
        }
        assert_eq!(
            collapse_loops("alpha  beta\n\n  gamma"),
            ("alpha  beta\n\n  gamma".into(), false)
        );
    }

    /// Optional native regression for a real model, separate from ordinary unit tests.
    #[test]
    #[ignore = "requires LOCALWISPR_TEST_WHISPER_MODEL and native model inference"]
    fn language_detection_can_abort_inside_encoder_then_reuse_state() {
        use std::sync::atomic::AtomicUsize;
        unsafe extern "C" fn stop_after_polling(data: *mut std::ffi::c_void) -> bool {
            let polls = unsafe { &*(data as *const AtomicUsize) };
            polls.fetch_add(1, Ordering::Relaxed) >= 2
        }
        let path = std::env::var("LOCALWISPR_TEST_WHISPER_MODEL").expect("model path");
        let mut params = WhisperContextParameters::default();
        params.use_gpu(false);
        let context = WhisperContext::new_with_params(&path, params).unwrap();
        let mut state = context.create_state().unwrap();
        let audio = vec![0.0; SAMPLE_RATE as usize * 3];
        state.pcm_to_mel(&audio, 2).unwrap();
        let polls = AtomicUsize::new(0);
        let result = unsafe {
            state.lang_detect_with_abort(
                0,
                2,
                Some(stop_after_polling),
                &polls as *const AtomicUsize as *mut std::ffi::c_void,
            )
        };
        assert!(result.is_err());
        assert!(polls.load(Ordering::Relaxed) >= 3);
        // A subsequent call must not retain a dangling callback or partial graph.
        assert!(state.lang_detect(0, 2).is_ok());
    }

    #[test]
    #[ignore = "requires LOCALWISPR_TEST_WHISPER_MODEL for native tokenization"]
    fn context_prompt_budget_preserves_language_nearby_and_manual_hints() {
        let path = std::env::var("LOCALWISPR_TEST_WHISPER_MODEL").expect("model path");
        let mut params = WhisperContextParameters::default();
        params.use_gpu(false);
        let context = WhisperContext::new_with_params(&path, params).unwrap();
        let versions = (0..35)
            .map(|i| format!("v{}.{}.{}", i % 10, i / 10, i % 7))
            .collect::<Vec<_>>();
        let nearby = crate::insertion::SurroundingText {
            before: format!("{} PostgreSQL", versions.join(" ")),
            after: versions
                .iter()
                .rev()
                .map(String::as_str)
                .collect::<Vec<_>>()
                .join(" "),
            ..crate::insertion::SurroundingText::default()
        };
        let configured = crate::insertion::recognition_prompt("Manual", Some(&nearby));
        let language_hint = CODE_SWITCH_HINTS[0].2;
        let tokens = bounded_prompt_tokens(&context, &configured, Some(language_hint)).unwrap();
        let budget = context.n_text_ctx() as usize / 2 - 1;
        assert!(tokens.len() <= budget);
        let language_tokens = context
            .tokenize(language_hint, language_hint.len() + 1)
            .unwrap();
        assert!(tokens.starts_with(&language_tokens));
        let retained: String = tokens
            .iter()
            .map(|token| context.token_to_str_lossy(*token).unwrap().to_string())
            .collect();
        assert!(retained.ends_with("PostgreSQL\nManual"), "{retained}");
        assert!(
            bounded_prompt_tokens(&context, "", None)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn raw_abort_callback_uses_the_owned_context_and_job_threshold() {
        let flag = Arc::new(AtomicU64::new(5));
        let context = AbortCtx {
            flag: flag.clone(),
            id: 5,
        };
        let ptr = &context as *const AbortCtx as *mut std::ffi::c_void;
        assert!(!unsafe { abort_trampoline(ptr) });
        assert!(context.check().is_ok());
        flag.store(6, Ordering::Relaxed);
        assert!(unsafe { abort_trampoline(ptr) });
        assert!(context.check().is_err());
    }

    #[test]
    fn vad_segments_trim_long_silences_and_join_speech_with_short_gap() {
        let rate = SAMPLE_RATE as usize;
        let mut audio = vec![0.0; rate * 20];
        audio[rate..2 * rate].fill(0.25);
        audio[10 * rate..11 * rate].fill(-0.5);
        let speech = join_speech_segments(&audio, [(100.0, 200.0), (1000.0, 1100.0)]);
        assert_eq!(speech.len(), 2 * rate + rate * 3 / 20);
        assert!(speech[..rate].iter().all(|v| *v == 0.25));
        assert!(speech[speech.len() - rate..].iter().all(|v| *v == -0.5));
        assert!(join_speech_segments(&audio, []).is_empty());
        assert!(join_speech_segments(&audio, [(f32::NAN, 2.0), (4000.0, 5000.0)]).is_empty());
    }

    #[test]
    fn code_switch_hint_needs_both_languages_of_a_pair() {
        let langs = |l: &[&str]| l.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert!(code_switch_hint(&langs(&["pl", "en"])).is_some());
        assert!(code_switch_hint(&langs(&["en", "de", "pl"])).is_some());
        assert!(code_switch_hint(&langs(&["pl"])).is_none());
        assert!(code_switch_hint(&langs(&["en", "de"])).is_none());
        assert!(code_switch_hint(&[]).is_none());
    }

    #[test]
    fn strips_known_hallucinations() {
        assert_eq!(
            filter_hallucinations(" Napisy stworzone przez społeczność Amara.org"),
            ""
        );
        assert_eq!(
            filter_hallucinations("Hello there. Thank you for watching!"),
            "Hello there."
        );
        assert_eq!(filter_hallucinations("[Music] Cześć"), "Cześć");
        assert_eq!(filter_hallucinations("  Dzień   dobry  "), "Dzień dobry");
        // Lowercasing changes byte lengths per character while keeping the
        // total equal; the match must not be cut inside a character.
        filter_hallucinations("ẞẞ é thank you for watching İİ");
    }
}
