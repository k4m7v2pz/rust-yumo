//! Resampling & PCM format conversion.
//!
//! Replaces the original C++ implementation that delegated to the Windows ACM
//! (`acmStreamOpen` / `acmStreamConvert`). Here we use the pure-Rust `rubato`
//! crate for high-quality resampling, plus hand-written bit-depth and channel
//! conversions, to produce the library's canonical format:
//! **44.1kHz, stereo, interleaved `i16` PCM** (`StandardWavInfo`).

use crate::error::{Exception, ExceptionType, Result};
use crate::wav::WavInfo;
use crate::{TARGET_CHANNELS, TARGET_SAMPLE_RATE};
use rubato::{Resampler, SincFixedIn, SincInterpolationParameters, SincInterpolationType, WindowFunction};

/// Canonical audio representation: interleaved i16 PCM, 44.1kHz, stereo.
pub type StandardWavInfo = Vec<i16>;

/// Decode raw PCM bytes of arbitrary bit depth into a planar `f64` buffer,
/// one `Vec` per channel. Returns `(planar_frames, channels)`.
fn decode_to_planar_f64(wav: &WavInfo) -> Result<(Vec<Vec<f64>>, u16)> {
    let channels = wav.channels as usize;
    let bits = wav.bits_per_sample as usize;
    let bytes_per_sample = bits / 8;
    let block_align = wav.block_align() as usize;
    if block_align == 0 || bytes_per_sample == 0 {
        return Err(Exception::new(ExceptionType::InvalidFormat, "block align 为零"));
    }
    let frames = wav.pcm_data.len() / block_align;

    let mut planar: Vec<Vec<f64>> = (0..channels).map(|_| Vec::with_capacity(frames)).collect();
    let scale = |x: i64| (x as f64) / ((1i64 << (bits - 1)) as f64);

    for f in 0..frames {
        for ch in 0..channels {
            let off = f * block_align + ch * bytes_per_sample;
            let sample: i64 = match bits {
                8 => {
                    // WAV 8-bit is unsigned, midpoint 128
                    (wav.pcm_data[off] as i64) - 128
                }
                16 => {
                    let b = [wav.pcm_data[off], wav.pcm_data[off + 1]];
                    i16::from_le_bytes(b) as i64
                }
                24 => {
                    let b = [
                        wav.pcm_data[off],
                        wav.pcm_data[off + 1],
                        wav.pcm_data[off + 2],
                    ];
                    // sign-extend
                    let v = (b[0] as i32) | ((b[1] as i32) << 8) | ((b[2] as i32) << 16);
                    let v = if (v & 0x800000) != 0 { v | !0xFFFFFF } else { v };
                    v as i64
                }
                32 => {
                    let mut b = [0u8; 4];
                    b.copy_from_slice(&wav.pcm_data[off..off + 4]);
                    i32::from_le_bytes(b) as i64
                }
                _ => return Err(Exception::new(ExceptionType::InvalidFormat, "不支持的位深度")),
            };
            planar[ch].push(scale(sample));
        }
    }
    Ok((planar, channels as u16))
}

/// Convert a decoded WAV into the canonical `StandardWavInfo` form
/// (44.1kHz stereo interleaved i16).
///
/// Mirrors the original C++ `convertToStandard`.
pub fn convert_to_standard(wav: &WavInfo) -> Result<StandardWavInfo> {
    if !wav.valid {
        return Err(Exception::new(ExceptionType::InvalidInput, "WAV文件格式无效"));
    }

    let (planar, src_channels) = decode_to_planar_f64(wav)?;
    convert_planar_to_standard(wav.sample_rate, src_channels, planar)
}

/// Convert arbitrary interleaved i16 PCM (as produced by the MP3 decoder) into
/// the canonical `StandardWavInfo` form (44.1kHz stereo interleaved i16).
pub fn convert_pcm_to_standard(
    sample_rate: u32,
    channels: u16,
    interleaved: &[i16],
) -> Result<StandardWavInfo> {
    if channels == 0 {
        return Err(Exception::new(ExceptionType::InvalidData, "声道数为零"));
    }
    if sample_rate == 0 {
        return Err(Exception::new(ExceptionType::InvalidData, "采样率为零"));
    }
    if interleaved.len() % channels as usize != 0 {
        return Err(Exception::new(
            ExceptionType::InvalidData,
            "交织PCM数据长度与声道数不匹配",
        ));
    }

    let frames = interleaved.len() / channels as usize;
    let mut planar: Vec<Vec<f64>> = (0..channels as usize).map(|_| Vec::with_capacity(frames)).collect();
    for f in 0..frames {
        for ch in 0..channels as usize {
            let s = interleaved[f * channels as usize + ch];
            planar[ch].push(s as f64 / 32768.0);
        }
    }
    convert_planar_to_standard(sample_rate, channels, planar)
}

/// Shared tail of both conversion paths: channel mapping, resampling to
/// `TARGET_SAMPLE_RATE` and interleaving into `i16`.
fn convert_planar_to_standard(
    src_rate: u32,
    src_channels: u16,
    mut planar: Vec<Vec<f64>>,
) -> Result<StandardWavInfo> {
    // --- Channel conversion to TARGET_CHANNELS ---
    if src_channels != TARGET_CHANNELS {
        if src_channels == 1 && TARGET_CHANNELS == 2 {
            // mono -> stereo: duplicate
            let mono = std::mem::take(&mut planar[0]);
            planar = vec![mono.clone(), mono];
        } else if src_channels == 2 && TARGET_CHANNELS == 1 {
            // stereo -> mono: average, then we'll upmix again below
            let l = std::mem::take(&mut planar[0]);
            let r = std::mem::take(&mut planar[1]);
            let mono: Vec<f64> = l.iter().zip(r.iter()).map(|(a, b)| (a + b) * 0.5).collect();
            planar = vec![mono.clone(), mono];
        } else {
            // Generic: take first TARGET_CHANNELS, or pad with silence.
            let mut next: Vec<Vec<f64>> = (0..TARGET_CHANNELS).map(|_| Vec::new()).collect();
            let frames = planar[0].len();
            for ch in 0..TARGET_CHANNELS as usize {
                if ch < planar.len() {
                    next[ch] = std::mem::take(&mut planar[ch]);
                } else {
                    next[ch] = vec![0.0; frames];
                }
            }
            planar = next;
        }
    }

    // --- Resampling to TARGET_SAMPLE_RATE ---
    if src_rate != TARGET_SAMPLE_RATE {
        let ratio = TARGET_SAMPLE_RATE as f64 / src_rate as f64;
        let chunk_in = planar[0].len();
        // Pad with enough input frames for the resampler's filter latency.
        let params = SincInterpolationParameters {
            sinc_len: 256,
            f_cutoff: 0.95,
            interpolation: SincInterpolationType::Linear,
            oversampling_factor: 160,
            window: WindowFunction::BlackmanHarris2,
        };
        let mut resampler = SincFixedIn::<f64>::new(
            ratio,
            2.0,
            params,
            chunk_in,
            planar.len(),
        )
        .map_err(|e| Exception::new(ExceptionType::UnknownError, format!("重采样器初始化失败: {e}")))?;

        // One-shot: feed the entire input, then drain with partial (None).
        let processed = resampler
            .process(&planar, None)
            .map_err(|e| Exception::new(ExceptionType::UnknownError, format!("重采样失败: {e}")))?;
        let mut out_channels: Vec<Vec<f64>> = processed;

        // Drain any delayed frames remaining inside the resampler.
        if let Ok(tail) = resampler.process_partial::<Vec<f64>>(None, None) {
            for ch in 0..out_channels.len().min(tail.len()) {
                out_channels[ch].extend_from_slice(&tail[ch]);
            }
        }
        planar = out_channels;
    }

    // --- Interleave + clip to i16 ---
    let frames = planar[0].len();
    let channels = planar.len();
    let mut out: Vec<i16> = Vec::with_capacity(frames * channels);
    const MAX_I16: f64 = 32767.0;
    const MIN_I16: f64 = -32768.0;
    for f in 0..frames {
        for ch in 0..channels {
            let v = planar[ch][f] * MAX_I16;
            let v = v.max(MIN_I16).min(MAX_I16);
            out.push(v.round() as i16);
        }
    }
    Ok(out)
}
