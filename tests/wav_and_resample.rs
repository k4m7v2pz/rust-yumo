//! Non-audio unit tests for the yumo library.
//!
//! These cover WAV parsing and the resampling/format-conversion pipeline
//! without opening any audio device, so they run on any CI host.

use std::io::Write;

fn write_wav(path: &std::path::Path, sample_rate: u32, channels: u16, bits: u16, frames: usize, fill: impl Fn(usize, usize) -> i32) {
    let bytes_per_sample = bits as usize / 8;
    let block_align = channels as usize * bytes_per_sample;
    let data_len = frames * block_align;
    let mut data: Vec<u8> = Vec::with_capacity(data_len);
    for f in 0..frames {
        for ch in 0..channels as usize {
            let v = fill(f, ch);
            match bits {
                8 => data.push((v + 128) as u8), // unsigned
                16 => data.extend_from_slice(&(v as i16).to_le_bytes()),
                24 => {
                    let u = (v as i32) & 0xFFFFFF;
                    data.push((u & 0xFF) as u8);
                    data.push(((u >> 8) & 0xFF) as u8);
                    data.push(((u >> 16) & 0xFF) as u8);
                }
                32 => data.extend_from_slice(&v.to_le_bytes()),
                _ => panic!("unsupported bits {bits}"),
            }
        }
    }
    let mut f = std::fs::File::create(path).unwrap();
    f.write_all(b"RIFF").unwrap();
    f.write_all(&(36 + data_len as u32).to_le_bytes()).unwrap();
    f.write_all(b"WAVE").unwrap();
    f.write_all(b"fmt ").unwrap();
    f.write_all(&16u32.to_le_bytes()).unwrap(); // fmt chunk size
    f.write_all(&1u16.to_le_bytes()).unwrap(); // PCM
    f.write_all(&channels.to_le_bytes()).unwrap();
    f.write_all(&sample_rate.to_le_bytes()).unwrap();
    let byte_rate = sample_rate * block_align as u32;
    f.write_all(&byte_rate.to_le_bytes()).unwrap();
    f.write_all(&(block_align as u16).to_le_bytes()).unwrap();
    f.write_all(&bits.to_le_bytes()).unwrap();
    f.write_all(b"data").unwrap();
    f.write_all(&(data_len as u32).to_le_bytes()).unwrap();
    f.write_all(&data).unwrap();
}

#[test]
fn loads_16bit_stereo_44100() {
    let dir = tempdir();
    let p = dir.join("a.wav");
    write_wav(&p, 44100, 2, 16, 100, |_f, _ch| 1234);
    let w = yumo::wav::load_wav(&p).unwrap();
    assert!(w.valid);
    assert_eq!(w.sample_rate, 44100);
    assert_eq!(w.channels, 2);
    assert_eq!(w.bits_per_sample, 16);
    assert_eq!(w.pcm_data.len(), 100 * 2 * 2);
    assert_eq!(w.frame_count(), 100);
}

#[test]
fn rejects_non_wav() {
    let dir = tempdir();
    let p = dir.join("not.wav");
    std::fs::write(&p, b"XXXX****WXYZ").unwrap();
    let err = yumo::wav::load_wav(&p).unwrap_err();
    assert_eq!(err.kind(), yumo::ExceptionType::InvalidInput);
}

#[test]
fn validates_bad_channels() {
    assert!(yumo::wav::validate_pcm_format(44100, 3, 16).is_err());
    assert!(yumo::wav::validate_pcm_format(44100, 2, 16).is_ok());
}

#[test]
fn validates_bad_bits() {
    assert!(yumo::wav::validate_pcm_format(44100, 2, 12).is_err());
    assert!(yumo::wav::validate_pcm_format(44100, 2, 24).is_ok());
}

#[test]
fn validates_bad_rate() {
    assert!(yumo::wav::validate_pcm_format(100, 2, 16).is_err());
    assert!(yumo::wav::validate_pcm_format(200000, 2, 16).is_err());
    assert!(yumo::wav::validate_pcm_format(48000, 2, 16).is_ok());
}

#[test]
fn converts_16bit_stereo_44100_passthrough() {
    // Same canonical format -> no resampling, only interleaving.
    let frames = 200;
    let dir = tempdir();
    let p = dir.join("s.wav");
    write_wav(&p, 44100, 2, 16, frames, |f, ch| {
        // L=+10000, R=-10000, constant
        if ch == 0 { 10000 } else { -10000 }
    });
    let w = yumo::wav::load_wav(&p).unwrap();
    let std = yumo::resample::convert_to_standard(&w).unwrap();
    assert_eq!(std.len(), frames * 2);
    // interleaved: L,R,L,R...
    assert_eq!(std[0], 10000);
    assert_eq!(std[1], -10000);
    assert_eq!(std[2], 10000);
    assert_eq!(std[3], -10000);
}

#[test]
fn converts_mono_to_stereo() {
    let frames = 100;
    let dir = tempdir();
    let p = dir.join("m.wav");
    write_wav(&p, 44100, 1, 16, frames, |_f, _ch| 5000);
    let w = yumo::wav::load_wav(&p).unwrap();
    let std = yumo::resample::convert_to_standard(&w).unwrap();
    // Stereo output: every mono frame -> two samples
    assert_eq!(std.len(), frames * 2);
    for i in 0..frames {
        assert_eq!(std[2 * i], 5000);
        assert_eq!(std[2 * i + 1], 5000);
    }
}

#[test]
fn resamples_22050_to_44100() {
    // Mono 22050 -> stereo 44100. Output length should roughly double.
    let frames = 1000;
    let dir = tempdir();
    let p = dir.join("r.wav");
    write_wav(&p, 22050, 1, 16, frames, |f, _ch| ((f as f32 / 10.0).sin() * 8000.0) as i32);
    let w = yumo::wav::load_wav(&p).unwrap();
    let std = yumo::resample::convert_to_standard(&w).unwrap();
    // Allow some slack for the resampler's latency padding.
    let expected_frames = frames * 2; // 22050 -> 44100 doubles frame count
    let out_frames = std.len() / 2;
    assert!(out_frames >= expected_frames, "out_frames={out_frames} expected>={expected_frames}");
}

#[test]
fn handles_8bit_unsigned() {
    let frames = 50;
    let dir = tempdir();
    let p = dir.join("8.wav");
    // 8-bit: midpoint 128 == silence. Fill with 128 -> decode to 0.
    write_wav(&p, 44100, 1, 8, frames, |_f, _ch| 0);
    let w = yumo::wav::load_wav(&p).unwrap();
    let std = yumo::resample::convert_to_standard(&w).unwrap();
    for s in &std {
        assert!((-50..=50).contains(s), "expected near-silence, got {s}");
    }
}

fn tempdir() -> std::path::PathBuf {
    let mut d = std::env::temp_dir();
    d.push(format!("yumo-test-{}", std::process::id()));
    d.push(format!("{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().subsec_nanos()));
    std::fs::create_dir_all(&d).unwrap();
    d
}
