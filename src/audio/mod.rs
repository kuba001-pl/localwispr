//! Microphone capture.
//!
//! The cpal callback only downmixes and pushes into a lock-free ring. A
//! dedicated thread drains it every few ms, resamples to 16 kHz, maintains the
//! pre-roll and recording buffers, and drives the spectrum analyser.

pub mod dsp;
pub mod resample;

use std::collections::VecDeque;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use crossbeam_channel::{Receiver, Sender, select, tick};
use tokio::sync::{mpsc, oneshot};

use crate::config;
use dsp::Analyzer;
use resample::Resampler;

pub const SAMPLE_RATE: u32 = 16_000;
/// Hard cap on a single recording (whisper handles long audio, memory doesn't care,
/// but a stuck key shouldn't record forever).
const MAX_RECORDING_SECS: usize = 600;
const LEVEL_INTERVAL: Duration = Duration::from_millis(16);

pub enum Cmd {
    Start,
    /// Finish the recording and return all 16 kHz samples.
    Stop(oneshot::Sender<Vec<f32>>),
    Cancel,
    /// Copy of the last `max_samples` of the current recording.
    Snapshot {
        max_samples: usize,
        reply: oneshot::Sender<(Vec<f32>, usize)>,
    },
    Shutdown,
}

pub struct AudioHandle {
    tx: Sender<Cmd>,
    thread: Option<std::thread::JoinHandle<()>>,
}

impl AudioHandle {
    pub fn spawn(
        cfg: config::Audio,
        bands: usize,
        levels: mpsc::UnboundedSender<Vec<f32>>,
    ) -> Result<Self> {
        let (tx, rx) = crossbeam_channel::unbounded();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let thread = std::thread::Builder::new()
            .name("audio".into())
            .spawn(move || audio_thread(cfg, bands, levels, rx, ready_tx))?;
        // Surface open errors (bad device, no PipeWire…) to the caller.
        match ready_rx.recv_timeout(Duration::from_secs(10)) {
            Ok(Ok(())) => Ok(Self {
                tx,
                thread: Some(thread),
            }),
            Ok(Err(e)) => Err(e),
            Err(_) => Err(anyhow!("audio thread did not start in time")),
        }
    }

    pub fn send(&self, cmd: Cmd) {
        let _ = self.tx.send(cmd);
    }

    /// Queue capture shutdown immediately; waiting for its buffer belongs to
    /// the processing task so a slow device cannot stall input handling.
    pub fn stop(&self) -> oneshot::Receiver<Vec<f32>> {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Stop(tx));
        rx
    }

    pub async fn snapshot(&self, max_samples: usize) -> (Vec<f32>, usize) {
        let (tx, rx) = oneshot::channel();
        self.send(Cmd::Snapshot {
            max_samples,
            reply: tx,
        });
        rx.await.unwrap_or_default()
    }

    #[cfg(test)]
    pub(crate) fn test_handle() -> (Self, Receiver<Cmd>) {
        let (tx, rx) = crossbeam_channel::unbounded();
        (Self { tx, thread: None }, rx)
    }
}

impl Drop for AudioHandle {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[derive(Debug, Clone)]
pub struct InputDevice {
    pub id: String,
    pub name: String,
    pub is_default: bool,
}

pub fn list_input_devices() -> Vec<InputDevice> {
    let host = cpal::default_host();
    let default_id = host
        .default_input_device()
        .and_then(|d| d.id().ok())
        .map(|i| i.to_string());
    let Ok(devices) = host.input_devices() else {
        return Vec::new();
    };
    devices
        .filter_map(|d| {
            let id = d.id().ok()?.to_string();
            let desc = d.description().ok();
            // PipeWire lists every sink as Duplex (recordable via its monitor);
            // a dictation mic picker should only offer real capture sources.
            if desc
                .as_ref()
                .is_some_and(|desc| desc.direction() != cpal::DeviceDirection::Input)
            {
                return None;
            }
            let name = desc
                .map(|desc| desc.name().to_owned())
                .unwrap_or_else(|| id.clone());
            Some(InputDevice {
                is_default: Some(&id) == default_id.as_ref(),
                id,
                name,
            })
        })
        .collect()
}

struct Capture {
    _stream: cpal::Stream,
    consumer: rtrb::Consumer<f32>,
    resampler: Resampler,
    failed: Arc<AtomicBool>,
    /// Set before the stream is dropped: PipeWire then reports the
    /// deliberate close as a device disconnect.
    closing: Arc<AtomicBool>,
    device_name: String,
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.closing.store(true, Ordering::Relaxed);
    }
}

fn resolve_device(device_id: &str) -> Result<cpal::Device> {
    let host = cpal::default_host();
    let device = if device_id.is_empty() {
        None
    } else {
        match device_id.parse::<cpal::DeviceId>() {
            Ok(id) => {
                let d = host.device_by_id(&id);
                if d.is_none() {
                    tracing::warn!("microphone `{device_id}` not found; using the default");
                }
                d
            }
            Err(e) => {
                tracing::warn!("invalid device id `{device_id}`: {e}");
                None
            }
        }
    };
    device
        .or_else(|| host.default_input_device())
        .ok_or_else(|| anyhow!("no microphone found"))
}

fn open_capture(device_id: &str, keep_warm: bool, recording: bool) -> Result<Capture> {
    let t0 = Instant::now();
    let device = resolve_device(device_id)?;
    let device_name = device
        .description()
        .map(|d| d.name().to_owned())
        .unwrap_or_else(|_| "unknown".into());

    // 2 s of headroom; the drain thread runs every few ms.
    let (producer, consumer) = rtrb::RingBuffer::<f32>::new(SAMPLE_RATE as usize * 6);
    let failed = Arc::new(AtomicBool::new(false));
    let closing = Arc::new(AtomicBool::new(false));
    let flags = (failed.clone(), closing.clone());

    // Preferred: let PipeWire deliver 16 kHz mono f32 directly (its resampler
    // is excellent and this saves a stage).
    let direct = cpal::StreamConfig {
        channels: 1,
        sample_rate: SAMPLE_RATE,
        buffer_size: cpal::BufferSize::Default,
    };
    let producer = Arc::new(std::sync::Mutex::new(producer));
    let attempt = build_stream::<f32>(&device, &direct, producer.clone(), flags.clone());
    let (stream, rate) = match attempt {
        Ok(s) => (s, SAMPLE_RATE),
        Err(e) => {
            tracing::debug!("16 kHz mono not available ({e}); using native format");
            let supported = device
                .default_input_config()
                .context("querying input format")?;
            let cfg: cpal::StreamConfig = supported.config();
            let s = match supported.sample_format() {
                cpal::SampleFormat::F32 => {
                    build_stream::<f32>(&device, &cfg, producer, flags.clone())
                }
                cpal::SampleFormat::I16 => {
                    build_stream::<i16>(&device, &cfg, producer, flags.clone())
                }
                cpal::SampleFormat::I32 => {
                    build_stream::<i32>(&device, &cfg, producer, flags.clone())
                }
                cpal::SampleFormat::U16 => {
                    build_stream::<u16>(&device, &cfg, producer, flags.clone())
                }
                f => return Err(anyhow!("unsupported sample format {f:?}")),
            }
            .context("opening microphone")?;
            (s, cfg.sample_rate)
        }
    };
    start_capture(&stream, keep_warm, recording)?;
    if keep_warm {
        tracing::info!("microphone: {device_name} @ {rate} Hz (kept open)");
    } else {
        tracing::debug!("microphone opened in {} ms", t0.elapsed().as_millis());
    }
    Ok(Capture {
        _stream: stream,
        consumer,
        resampler: Resampler::new(rate, SAMPLE_RATE),
        failed,
        closing,
        device_name,
    })
}

fn start_capture(stream: &impl StreamTrait, keep_warm: bool, recording: bool) -> Result<()> {
    // Newly built streams are stopped. An idle cold microphone must stay that
    // way on initial open AND after recovery from a device disconnect.
    if keep_warm || recording {
        stream.play().context("starting microphone")?;
    }
    Ok(())
}

fn build_stream<T>(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    producer: Arc<std::sync::Mutex<rtrb::Producer<f32>>>,
    (failed, closing): (Arc<AtomicBool>, Arc<AtomicBool>),
) -> Result<cpal::Stream>
where
    T: cpal::SizedSample + Send + 'static,
    f32: cpal::FromSample<T>,
{
    use cpal::Sample;
    let channels = cfg.channels.max(1) as usize;
    // The mutex is only ever touched by this callback (uncontended, no syscalls);
    // it exists so the producer can be shared between fallback attempts.
    let stream = device.build_input_stream::<T, _, _>(
        *cfg,
        move |data: &[T], _| {
            let Ok(mut prod) = producer.try_lock() else {
                return;
            };
            let frames = (data.len() / channels).min(prod.slots());
            let Ok(mut chunk) = prod.write_chunk_uninit(frames) else {
                return;
            };
            let (a, b) = chunk.as_mut_slices();
            let mut frame_iter = data.chunks_exact(channels);
            for slot in a.iter_mut().chain(b.iter_mut()) {
                let Some(frame) = frame_iter.next() else {
                    break;
                };
                let sum: f32 = frame.iter().map(|s| f32::from_sample(*s)).sum();
                slot.write(sum / channels as f32);
            }
            let n = a.len() + b.len();
            unsafe { chunk.commit(n) };
        },
        move |err| {
            if closing.load(Ordering::Relaxed) {
                tracing::debug!("audio stream closed: {err}");
                return;
            }
            tracing::error!("audio stream error: {err}");
            failed.store(true, Ordering::Relaxed);
        },
        None,
    )?;
    Ok(stream)
}

fn audio_thread(
    cfg: config::Audio,
    bands: usize,
    levels: mpsc::UnboundedSender<Vec<f32>>,
    rx: Receiver<Cmd>,
    ready: std::sync::mpsc::Sender<Result<()>>,
) {
    // A cold microphone has no stream at all while idle: a paused PipeWire
    // stream still counts as recording for the desktop's privacy indicator.
    let opened = if cfg.keep_warm {
        open_capture(&cfg.device, true, false).map(Some)
    } else {
        resolve_device(&cfg.device).map(|d| {
            let name = d
                .description()
                .map(|d| d.name().to_owned())
                .unwrap_or_default();
            tracing::info!("microphone: {name} (opened only while dictating)");
            None
        })
    };
    let mut capture = match opened {
        Ok(c) => {
            let _ = ready.send(Ok(()));
            c
        }
        Err(e) => {
            let _ = ready.send(Err(e));
            return;
        }
    };

    let preroll_len = (cfg.preroll_ms as usize * SAMPLE_RATE as usize / 1000).max(1);
    let mut preroll: VecDeque<f32> = VecDeque::with_capacity(preroll_len + 1024);
    let mut recording: Option<Vec<f32>> = None;
    let mut analyzer = Analyzer::new(SAMPLE_RATE, bands);
    let mut raw = Vec::with_capacity(8192);
    let mut resampled = Vec::with_capacity(8192);
    let mut last_level = Instant::now();
    let mut last_reopen_try = Instant::now();
    let ticker = tick(Duration::from_millis(4));

    loop {
        select! {
            recv(rx) -> msg => {
                let Ok(cmd) = msg else { return };
                match cmd {
                    Cmd::Start => {
                        if !cfg.keep_warm {
                            capture = open_capture(&cfg.device, false, true)
                                .inspect_err(|e| tracing::error!("microphone: {e:#}"))
                                .ok();
                        }
                        let mut buf = Vec::with_capacity(SAMPLE_RATE as usize * 30);
                        buf.extend(preroll.drain(..));
                        analyzer.reset();
                        recording = Some(buf);
                        last_level = Instant::now();
                    }
                    Cmd::Stop(reply) => {
                        // Flush whatever arrived since the last tick.
                        if let Some(c) = capture.as_mut() {
                            drain(c, &mut raw, &mut resampled);
                            if let Some(rec) = recording.as_mut() {
                                rec.extend_from_slice(&resampled);
                            }
                        }
                        let _ = reply.send(recording.take().unwrap_or_default());
                        close_if_cold(&mut capture, &cfg);
                    }
                    Cmd::Cancel => {
                        recording = None;
                        close_if_cold(&mut capture, &cfg);
                    }
                    Cmd::Snapshot { max_samples, reply } => {
                        let _ = reply.send(recording_snapshot(recording.as_deref(), max_samples));
                    }
                    Cmd::Shutdown => return,
                }
            }
            recv(ticker) -> _ => {
                let Some(c) = capture.as_mut() else {
                    if (cfg.keep_warm || recording.is_some())
                        && last_reopen_try.elapsed() > Duration::from_secs(1)
                    {
                        last_reopen_try = Instant::now();
                        capture = open_capture(&cfg.device, cfg.keep_warm, recording.is_some()).ok();
                    }
                    continue;
                };
                if c.failed.load(Ordering::Relaxed) {
                    tracing::warn!("microphone `{}` lost; reopening", c.device_name);
                    capture = None;
                    continue;
                }
                drain(c, &mut raw, &mut resampled);
                if resampled.is_empty() {
                    continue;
                }
                match recording.as_mut() {
                    Some(rec) => {
                        if rec.len() < MAX_RECORDING_SECS * SAMPLE_RATE as usize {
                            rec.extend_from_slice(&resampled);
                        }
                        analyzer.push(&resampled);
                        let now = Instant::now();
                        let dt = now - last_level;
                        if dt >= LEVEL_INTERVAL {
                            last_level = now;
                            let bands = analyzer.compute(dt.as_secs_f32()).to_vec();
                            let _ = levels.send(bands);
                        }
                    }
                    None if cfg.keep_warm => {
                        preroll.extend(resampled.iter().copied());
                        let excess = preroll.len().saturating_sub(preroll_len);
                        preroll.drain(..excess);
                    }
                    None => {}
                }
            }
        }
    }
}

/// Return both the bounded window and the total capture progress. Window
/// length stops growing during long recordings even as its contents change.
fn recording_snapshot(recording: Option<&[f32]>, max_samples: usize) -> (Vec<f32>, usize) {
    match recording {
        Some(samples) => (
            samples[samples.len().saturating_sub(max_samples)..].to_vec(),
            samples.len(),
        ),
        None => (Vec::new(), 0),
    }
}

fn close_if_cold(capture: &mut Option<Capture>, cfg: &config::Audio) {
    if !cfg.keep_warm {
        *capture = None;
    }
}

fn drain(c: &mut Capture, raw: &mut Vec<f32>, out: &mut Vec<f32>) {
    raw.clear();
    out.clear();
    let n = c.consumer.slots();
    if n == 0 {
        return;
    }
    if let Ok(chunk) = c.consumer.read_chunk(n) {
        let (a, b) = chunk.as_slices();
        raw.extend_from_slice(a);
        raw.extend_from_slice(b);
        chunk.commit_all();
    }
    c.resampler.process(raw, out);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    #[derive(Default)]
    struct TestStream {
        starts: Cell<usize>,
    }

    impl StreamTrait for TestStream {
        fn play(&self) -> std::result::Result<(), cpal::Error> {
            self.starts.set(self.starts.get() + 1);
            Ok(())
        }

        fn pause(&self) -> std::result::Result<(), cpal::Error> {
            Ok(())
        }

        fn buffer_size(&self) -> std::result::Result<cpal::FrameCount, cpal::Error> {
            Ok(0)
        }

        fn now(&self) -> cpal::StreamInstant {
            unreachable!("capture startup does not query stream time")
        }
    }

    #[test]
    fn cold_idle_reopens_never_start_the_microphone() {
        // Exercise repeated initial/recovery opens with the same startup path.
        for _ in 0..3 {
            let replacement = TestStream::default();
            start_capture(&replacement, false, false).unwrap();
            assert_eq!(replacement.starts.get(), 0);
        }
    }

    #[test]
    fn replacement_streams_resume_recording_and_warm_preroll() {
        for (keep_warm, recording) in [(false, true), (true, false), (true, true)] {
            let replacement = TestStream::default();
            start_capture(&replacement, keep_warm, recording).unwrap();
            assert_eq!(replacement.starts.get(), 1);
        }
    }

    #[test]
    fn rolling_snapshots_report_capture_progress_after_the_window_is_full() {
        let window = 28 * SAMPLE_RATE as usize;
        let mut samples = vec![1.0; 30 * SAMPLE_RATE as usize];
        let (before, captured_before) = recording_snapshot(Some(&samples), window);
        samples.extend(vec![2.0; SAMPLE_RATE as usize]);
        let (after, captured_after) = recording_snapshot(Some(&samples), window);
        assert_eq!(before.len(), window);
        assert_eq!(after.len(), window);
        assert_eq!(captured_after - captured_before, SAMPLE_RATE as usize);
        assert_eq!(before.last(), Some(&1.0));
        assert_eq!(after.last(), Some(&2.0));
        assert_eq!(recording_snapshot(None, window), (Vec::new(), 0));
        assert_eq!(
            recording_snapshot(Some(&samples), 0),
            (Vec::new(), samples.len())
        );
    }
}
