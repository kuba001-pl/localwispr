//! Spectrum analyser driving the HUD visualiser.
//!
//! Log-spaced bands over the speech range, pink-tilt compensated (so high
//! bands move too), with automatic gain and attack/release smoothing.

use std::sync::Arc;

use realfft::{RealFftPlanner, RealToComplex, num_complex::Complex};

pub const FFT_SIZE: usize = 1024;
const F_LO: f32 = 90.0;
const F_HI: f32 = 6500.0;
/// dB boost per octave above F_LO (speech energy falls ~6 dB/oct).
const TILT_DB_PER_OCT: f32 = 4.5;
/// Display range below the tracked peak.
const RANGE_DB: f32 = 42.0;
/// The tracked peak never drops below this, so silence stays flat.
const PEAK_FLOOR_DB: f32 = -38.0;

pub struct Analyzer {
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    ring: Vec<f32>,
    ring_pos: usize,
    input: Vec<f32>,
    spectrum: Vec<Complex<f32>>,
    scratch: Vec<Complex<f32>>,
    edges: Vec<(usize, usize)>,
    tilt: Vec<f32>,
    smoothed: Vec<f32>,
    peak_db: f32,
}

impl Analyzer {
    pub fn new(sample_rate: u32, bands: usize) -> Self {
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(FFT_SIZE);
        let window = (0..FFT_SIZE)
            .map(|i| {
                0.5 - 0.5 * (2.0 * std::f32::consts::PI * i as f32 / (FFT_SIZE - 1) as f32).cos()
            })
            .collect();
        let bin_hz = sample_rate as f32 / FFT_SIZE as f32;
        let hi = F_HI.min(sample_rate as f32 * 0.45);
        let mut edges = Vec::with_capacity(bands);
        let mut tilt = Vec::with_capacity(bands);
        let mut prev_end = 1;
        for b in 0..bands {
            let f0 = F_LO * (hi / F_LO).powf(b as f32 / bands as f32);
            let f1 = F_LO * (hi / F_LO).powf((b + 1) as f32 / bands as f32);
            let start = ((f0 / bin_hz) as usize).max(prev_end);
            let end = ((f1 / bin_hz).ceil() as usize).max(start + 1);
            prev_end = end;
            edges.push((start, end));
            let centre = (f0 * f1).sqrt();
            tilt.push(TILT_DB_PER_OCT * (centre / F_LO).log2());
        }
        Self {
            scratch: fft.make_scratch_vec(),
            spectrum: fft.make_output_vec(),
            input: fft.make_input_vec(),
            fft,
            window,
            ring: vec![0.0; FFT_SIZE],
            ring_pos: 0,
            edges,
            tilt,
            smoothed: vec![0.0; bands],
            peak_db: PEAK_FLOOR_DB,
        }
    }

    pub fn push(&mut self, samples: &[f32]) {
        for &s in samples {
            self.ring[self.ring_pos] = s;
            self.ring_pos = (self.ring_pos + 1) % FFT_SIZE;
        }
    }

    pub fn reset(&mut self) {
        self.ring.fill(0.0);
        self.smoothed.fill(0.0);
        self.peak_db = PEAK_FLOOR_DB;
    }

    /// Computes band levels in `0..=1` from the most recent `FFT_SIZE` samples.
    /// `dt` is the time since the previous call, for frame-rate independent smoothing.
    pub fn compute(&mut self, dt: f32) -> &[f32] {
        for i in 0..FFT_SIZE {
            let s = self.ring[(self.ring_pos + i) % FFT_SIZE];
            self.input[i] = s * self.window[i];
        }
        if self
            .fft
            .process_with_scratch(&mut self.input, &mut self.spectrum, &mut self.scratch)
            .is_err()
        {
            return &self.smoothed;
        }

        let norm = 4.0 / (FFT_SIZE as f32 * FFT_SIZE as f32);
        let mut frame_peak = f32::MIN;
        let mut levels_db = [0f32; 64];
        let n = self.edges.len().min(levels_db.len());
        for (b, &(lo, hi)) in self.edges.iter().take(n).enumerate() {
            let hi = hi.min(self.spectrum.len());
            let lo = lo.min(hi.saturating_sub(1));
            let power: f32 = self.spectrum[lo..hi]
                .iter()
                .map(|c| c.norm_sqr())
                .sum::<f32>()
                / (hi - lo).max(1) as f32;
            let db = 10.0 * (power * norm + 1e-12).log10() + self.tilt[b];
            levels_db[b] = db;
            frame_peak = frame_peak.max(db);
        }

        // AGC: fast rise, slow (≈6 dB/s) decay.
        self.peak_db = if frame_peak > self.peak_db {
            frame_peak
        } else {
            (self.peak_db - 6.0 * dt).max(PEAK_FLOOR_DB)
        };

        let attack = 1.0 - (-dt / 0.025).exp();
        let release = 1.0 - (-dt / 0.14).exp();
        for (b, &level_db) in levels_db.iter().take(n).enumerate() {
            let target = ((level_db - (self.peak_db - RANGE_DB)) / RANGE_DB).clamp(0.0, 1.0);
            // Slight curve so mid-level speech fills the bars.
            let target = target.powf(1.4);
            let cur = &mut self.smoothed[b];
            let k = if target > *cur { attack } else { release };
            *cur += (target - *cur) * k;
        }
        &self.smoothed
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn silence_is_flat_and_tone_lights_its_band() {
        let mut a = Analyzer::new(16_000, 16);
        a.push(&vec![0.0; FFT_SIZE]);
        for _ in 0..10 {
            a.compute(0.016);
        }
        assert!(a.compute(0.016).iter().all(|&v| v < 0.01));

        let tone: Vec<f32> = (0..FFT_SIZE)
            .map(|i| 0.3 * (2.0 * std::f32::consts::PI * 1000.0 * i as f32 / 16_000.0).sin())
            .collect();
        a.push(&tone);
        let mut levels = Vec::new();
        for _ in 0..20 {
            levels = a.compute(0.016).to_vec();
        }
        let (max_band, _) = levels
            .iter()
            .enumerate()
            .max_by(|x, y| x.1.total_cmp(y.1))
            .unwrap();
        let (lo, hi) = a.edges[max_band];
        let bin = 1000.0 / (16_000.0 / FFT_SIZE as f32);
        assert!(
            (lo as f32 - 1.0..=hi as f32 + 1.0).contains(&bin),
            "band {max_band}"
        );
        assert!(levels[max_band] > 0.8);
    }
}
