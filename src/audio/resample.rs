//! Streaming rational-ratio polyphase resampler (windowed sinc).
//!
//! Only used when the audio backend cannot deliver 16 kHz directly. 48 taps at
//! 16 kHz output is ~0.8 M MAC/s — negligible.

const HALF_TAPS: usize = 24;
const TAPS: usize = HALF_TAPS * 2;

pub struct Resampler {
    up: u64,
    down: u64,
    /// `up` phases × `TAPS` coefficients.
    table: Vec<f32>,
    buf: Vec<f32>,
    /// Absolute input index of `buf[0]`.
    base: u64,
    /// Output position in units of 1/up input samples.
    t: u64,
    passthrough: bool,
}

fn gcd(a: u64, b: u64) -> u64 {
    if b == 0 { a } else { gcd(b, a % b) }
}

impl Resampler {
    pub fn new(in_rate: u32, out_rate: u32) -> Self {
        let g = gcd(in_rate as u64, out_rate as u64);
        let up = out_rate as u64 / g;
        let down = in_rate as u64 / g;
        let passthrough = in_rate == out_rate;
        // Cutoff relative to the input Nyquist, with a little guard band.
        let fc = (out_rate as f64 / in_rate as f64).min(1.0) * 0.92;
        let mut table = Vec::with_capacity(if passthrough { 0 } else { up as usize * TAPS });
        if !passthrough {
            for p in 0..up {
                let frac = p as f64 / up as f64;
                let start = table.len();
                let mut sum = 0.0f64;
                for j in 0..TAPS {
                    // Distance (in input samples) from the output instant to tap j.
                    let d = frac + (HALF_TAPS - 1) as f64 - j as f64;
                    let x = std::f64::consts::PI * fc * d;
                    let sinc = if x.abs() < 1e-9 { 1.0 } else { x.sin() / x };
                    let w = blackman_harris((d + HALF_TAPS as f64) / (2 * HALF_TAPS) as f64);
                    let h = sinc * w;
                    sum += h;
                    table.push(h as f32);
                }
                // Unity DC gain per phase.
                for c in &mut table[start..] {
                    *c = (*c as f64 / sum) as f32;
                }
            }
        }
        Self {
            up,
            down,
            table,
            // Zero history so the first outputs have a full window.
            buf: vec![0.0; HALF_TAPS - 1],
            base: 0,
            t: (HALF_TAPS as u64 - 1) * up,
            passthrough,
        }
    }

    pub fn process(&mut self, input: &[f32], out: &mut Vec<f32>) {
        if self.passthrough {
            out.extend_from_slice(input);
            return;
        }
        self.buf.extend_from_slice(input);
        let avail_end = self.base + self.buf.len() as u64;
        loop {
            let idx = self.t / self.up;
            let phase = (self.t % self.up) as usize;
            if idx + HALF_TAPS as u64 + 1 > avail_end {
                break;
            }
            let start = (idx + 1 - HALF_TAPS as u64 - self.base) as usize;
            let x = &self.buf[start..start + TAPS];
            let h = &self.table[phase * TAPS..(phase + 1) * TAPS];
            let acc: f32 = x.iter().zip(h).map(|(a, b)| a * b).sum();
            out.push(acc);
            self.t += self.down;
        }
        // Drop input no longer needed by any future output.
        let keep_from = (self.t / self.up + 1).saturating_sub(HALF_TAPS as u64);
        if keep_from > self.base {
            let drop = ((keep_from - self.base) as usize).min(self.buf.len());
            self.buf.drain(..drop);
            self.base += drop as u64;
        }
    }
}

fn blackman_harris(x: f64) -> f64 {
    use std::f64::consts::PI;
    let x = x.clamp(0.0, 1.0);
    0.35875 - 0.48829 * (2.0 * PI * x).cos() + 0.14128 * (4.0 * PI * x).cos()
        - 0.01168 * (6.0 * PI * x).cos()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(rate: u32, freq: f32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / rate as f32).sin())
            .collect()
    }

    fn rms(x: &[f32]) -> f32 {
        (x.iter().map(|v| v * v).sum::<f32>() / x.len() as f32).sqrt()
    }

    #[test]
    fn length_and_passband() {
        for in_rate in [48_000, 44_100, 32_000, 22_050] {
            let input = tone(in_rate, 440.0, 1.0);
            let mut r = Resampler::new(in_rate, 16_000);
            let mut out = Vec::new();
            // Feed in odd-sized chunks to exercise streaming.
            for chunk in input.chunks(331) {
                r.process(chunk, &mut out);
            }
            let expected = 16_000.0;
            assert!(
                (out.len() as f32 - expected).abs() < 40.0,
                "{in_rate}: {}",
                out.len()
            );
            let level = rms(&out[1000..out.len() - 1000]);
            assert!(
                (level - std::f32::consts::FRAC_1_SQRT_2).abs() < 0.02,
                "{in_rate}: {level}"
            );
        }
    }

    #[test]
    fn stopband_attenuation() {
        // 12 kHz cannot be represented at 16 kHz and must be filtered out.
        let input = tone(48_000, 12_000.0, 0.5);
        let mut r = Resampler::new(48_000, 16_000);
        let mut out = Vec::new();
        r.process(&input, &mut out);
        assert!(rms(&out[500..]) < 0.01, "aliasing: {}", rms(&out[500..]));
    }
}
