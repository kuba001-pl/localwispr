//! Minimal WAV reader (PCM 16/24/32-bit, IEEE float) → mono 16 kHz.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::audio::{SAMPLE_RATE, resample::Resampler};

pub fn read_mono_16k(path: &Path) -> Result<Vec<f32>> {
    let bytes = std::fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    let (rate, samples) = decode(&bytes)?;
    let capacity = samples
        .len()
        .checked_mul(SAMPLE_RATE as usize)
        .and_then(|n| (n / rate as usize).checked_add(SAMPLE_RATE as usize / 50 + 64))
        .context("WAV resampled length overflow")?;
    let mut out = Vec::new();
    out.try_reserve(capacity)
        .context("WAV audio too large to resample")?;
    let mut r = Resampler::new(rate, SAMPLE_RATE);
    r.process(&samples, &mut out);
    // Flush the filter tail.
    let silence = [0.0; 1024];
    let mut remaining = rate as usize / 50;
    while remaining > 0 {
        let count = remaining.min(silence.len());
        r.process(&silence[..count], &mut out);
        remaining -= count;
    }
    Ok(out)
}

fn decode(b: &[u8]) -> Result<(u32, Vec<f32>)> {
    if b.len() < 12 || &b[0..4] != b"RIFF" || &b[8..12] != b"WAVE" {
        bail!("not a RIFF/WAVE file");
    }
    let u16_at = |i: usize| u16::from_le_bytes([b[i], b[i + 1]]);
    let u32_at = |i: usize| u32::from_le_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);

    let mut pos = 12;
    let mut fmt: Option<(u16, u16, u32, u16)> = None;
    while pos + 8 <= b.len() {
        let id = &b[pos..pos + 4];
        let len = u32_at(pos + 4) as usize;
        let body = pos + 8;
        let end = body
            .checked_add(len)
            .filter(|end| *end <= b.len())
            .context("truncated WAV chunk")?;
        match id {
            b"fmt " if len >= 16 => {
                let mut format = u16_at(body);
                if format == 0xFFFE {
                    if len < 40 || u16_at(body + 16) < 22 {
                        bail!("short WAV extensible fmt chunk");
                    }
                    // Only PCM/IEEE-float GUIDs in the standard namespace.
                    if b[body + 26..body + 40]
                        != [0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]
                    {
                        bail!("unsupported WAV extensible sub-format GUID");
                    }
                    format = u16_at(body + 24);
                }
                if u16_at(body + 2) == 0 || u32_at(body + 4) == 0 {
                    bail!("zero WAV channels or sample rate");
                }
                let bits = u16_at(body + 14);
                let expected_align = u16_at(body + 2) as usize * (bits as usize / 8);
                if bits == 0 || bits % 8 != 0 || expected_align != u16_at(body + 12) as usize {
                    bail!("invalid WAV block alignment");
                }
                fmt = Some((
                    format,
                    u16_at(body + 2),
                    u32_at(body + 4),
                    u16_at(body + 14),
                ));
            }
            b"fmt " => bail!("short WAV fmt chunk"),
            b"data" => {
                let (format, channels, rate, bits) = fmt.context("data chunk before fmt")?;
                if channels == 0 || rate == 0 {
                    bail!("zero WAV channels or sample rate");
                }
                let ch = channels as usize;
                if bits == 0 || bits % 8 != 0 || (end - body) % (ch * (bits as usize / 8)) != 0 {
                    bail!("incomplete WAV frame");
                }
                let data = &b[body..end];
                let frames: Vec<f32> = match (format, bits) {
                    (1, 16) => data
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|s| i16::from_le_bytes([s[0], s[1]]) as f32 / 32768.0)
                        .collect(),
                    (1, 24) => data
                        .as_chunks::<3>()
                        .0
                        .iter()
                        .map(|s| {
                            (i32::from_le_bytes([0, s[0], s[1], s[2]]) >> 8) as f32 / 8_388_608.0
                        })
                        .collect(),
                    (1, 32) => data
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|s| {
                            i32::from_le_bytes([s[0], s[1], s[2], s[3]]) as f32 / 2_147_483_648.0
                        })
                        .collect(),
                    (3, 32) => data
                        .as_chunks::<4>()
                        .0
                        .iter()
                        .map(|s| f32::from_le_bytes([s[0], s[1], s[2], s[3]]))
                        .collect(),
                    _ => bail!("unsupported WAV encoding (format {format}, {bits} bit)"),
                };
                let mono: Vec<f32> = frames
                    .chunks_exact(ch)
                    .map(|f| f.iter().map(|v| v / ch as f32).sum())
                    .collect();
                if mono.iter().any(|v| !v.is_finite()) {
                    bail!("non-finite WAV sample");
                }
                return Ok((rate, mono));
            }
            _ => {}
        }
        pos = body + len + (len & 1);
    }
    bail!("no audio data in WAV file")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn wav(format: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
        let align = channels.saturating_mul(bits / 8);
        let mut b = b"RIFF\0\0\0\0WAVEfmt ".to_vec();
        b.extend(16u32.to_le_bytes());
        b.extend(format.to_le_bytes());
        b.extend(channels.to_le_bytes());
        b.extend(rate.to_le_bytes());
        b.extend(rate.saturating_mul(align as u32).to_le_bytes());
        b.extend(align.to_le_bytes());
        b.extend(bits.to_le_bytes());
        b.extend(b"data");
        b.extend((data.len() as u32).to_le_bytes());
        b.extend(data);
        b
    }

    #[test]
    fn all_truncated_headers_and_bodies_return_errors() {
        let b = wav(1, 1, 16000, 16, &[0, 0]);
        for end in 0..b.len() {
            assert!(
                decode(&b[..end]).is_err(),
                "accepted truncated WAV at {end}"
            );
        }
        assert!(decode(&b).is_ok());
        let mut oversized = b.clone();
        oversized[16..20].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(decode(&oversized).is_err());
    }

    #[test]
    fn rejects_zero_metadata_incomplete_frames_and_extensible_headers() {
        for b in [
            wav(1, 0, 16000, 16, &[]),
            wav(1, 1, 0, 16, &[]),
            wav(1, 2, 16000, 16, &[0, 0]),
            wav(1, 1, 16000, 0, &[]),
            wav(0xfffe, 1, 16000, 16, &[0, 0]),
        ] {
            assert!(decode(&b).is_err());
        }
        let mut b = wav(1, 1, 16000, 16, &[0, 0]);
        b[32..34].copy_from_slice(&4u16.to_le_bytes());
        assert!(decode(&b).is_err());
        assert!(decode(&wav(3, 1, 16000, 32, &f32::NAN.to_le_bytes())).is_err());
    }

    #[test]
    fn accepts_complete_pcm_extensible_format() {
        let mut b = wav(0xfffe, 1, 16000, 16, &16384i16.to_le_bytes());
        b[16..20].copy_from_slice(&40u32.to_le_bytes());
        let mut extension = Vec::new();
        extension.extend(22u16.to_le_bytes());
        extension.extend(16u16.to_le_bytes());
        extension.extend(0u32.to_le_bytes());
        extension.extend([1, 0, 0, 0, 0, 0, 16, 0, 128, 0, 0, 170, 0, 56, 155, 113]);
        b.splice(36..36, extension);
        assert_eq!(decode(&b).unwrap().1, vec![0.5]);
    }

    #[test]
    fn handles_odd_unknown_chunks_and_pcm24_sign() {
        let mut b = wav(1, 1, 16000, 24, &[0, 0, 128, 255, 255, 127]);
        b.splice(12..12, *b"JUNK\x01\0\0\0x\0");
        let (_, mono) = decode(&b).unwrap();
        assert_eq!(mono[0], -1.0);
        assert!(mono[1] > 0.999);
    }

    #[test]
    fn decodes_pcm16_stereo() {
        let mut b = Vec::new();
        b.extend(b"RIFF\0\0\0\0WAVEfmt ");
        b.extend(16u32.to_le_bytes());
        b.extend(1u16.to_le_bytes()); // PCM
        b.extend(2u16.to_le_bytes()); // stereo
        b.extend(16_000u32.to_le_bytes());
        b.extend((16_000u32 * 4).to_le_bytes());
        b.extend(4u16.to_le_bytes());
        b.extend(16u16.to_le_bytes());
        b.extend(b"data");
        b.extend(8u32.to_le_bytes());
        for s in [16384i16, 0, -16384, 0] {
            b.extend(s.to_le_bytes());
        }
        let (rate, mono) = decode(&b).unwrap();
        assert_eq!(rate, 16_000);
        assert_eq!(mono, vec![0.25, -0.25]);
    }
}
