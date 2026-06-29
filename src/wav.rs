//! Minimal PCM WAV parser.
//!
//! Equivalent to the original C++ `loadWav` / `ValidatePcmFormat`.
//! Only PCM-format WAV files are supported (8/16/24/32-bit, mono or stereo,
//! sample rate 8kHz..192kHz). Extra chunks (`fact`, `LIST`, ...) are skipped.

use crate::error::{Exception, ExceptionType, Result};
use std::fs::File;
use std::io::{BufReader, Read, Seek, SeekFrom};
use std::path::Path;

/// Mirrors the original C++ `WavInfo` struct.
#[derive(Debug)]
pub struct WavInfo {
    pub sample_rate: u32,
    pub channels: u16,
    pub bits_per_sample: u16,
    /// Raw PCM bytes (exactly `data_size` bytes).
    pub pcm_data: Vec<u8>,
    pub valid: bool,
}

impl WavInfo {
    /// Block alignment in bytes (= channels * bits_per_sample / 8).
    pub fn block_align(&self) -> u16 {
        self.channels * (self.bits_per_sample / 8)
    }

    /// Number of full audio frames contained in `pcm_data`.
    pub fn frame_count(&self) -> usize {
        let b = self.block_align() as usize;
        if b == 0 { 0 } else { self.pcm_data.len() / b }
    }
}

/// Four-CC reader: read exactly 4 bytes and return them as a `[u8; 4]`.
fn read_fourcc<R: Read>(r: &mut R) -> Result<[u8; 4]> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).map_err(|_| {
        Exception::new(ExceptionType::FileReadError, "WAV 文件读取失败，可能是文件损坏或其他错误")
    })?;
    Ok(buf)
}

fn read_u32_le<R: Read>(r: &mut R) -> Result<u32> {
    let mut buf = [0u8; 4];
    r.read_exact(&mut buf).map_err(|_| {
        Exception::new(ExceptionType::FileReadError, "WAV 文件读取失败，可能是文件损坏或其他错误")
    })?;
    Ok(u32::from_le_bytes(buf))
}

fn read_u16_le<R: Read>(r: &mut R) -> Result<u16> {
    let mut buf = [0u8; 2];
    r.read_exact(&mut buf).map_err(|_| {
        Exception::new(ExceptionType::FileReadError, "WAV 文件读取失败，可能是文件损坏或其他错误")
    })?;
    Ok(u16::from_le_bytes(buf))
}

fn eq_fourcc(buf: &[u8; 4], s: &[u8; 4]) -> bool {
    buf == s
}

/// Validate a PCM WAV format descriptor (mirrors `ValidatePcmFormat`).
pub fn validate_pcm_format(sample_rate: u32, channels: u16, bits: u16) -> Result<()> {
    if channels != 1 && channels != 2 {
        return Err(Exception::new(ExceptionType::InvalidInput, "仅支持单声道或立体声"));
    }
    if !matches!(bits, 8 | 16 | 24 | 32) {
        return Err(Exception::new(ExceptionType::InvalidInput, "仅支持 8/16/24/32 位深度"));
    }
    if sample_rate < 8000 || sample_rate > 192000 {
        return Err(Exception::new(ExceptionType::InvalidInput, "采样率超出合理范围"));
    }
    Ok(())
}

/// Load a WAV file from disk. Mirrors the original C++ `loadWav`.
pub fn load_wav<P: AsRef<Path>>(filename: P) -> Result<WavInfo> {
    let path = filename.as_ref();
    let file = File::open(path).map_err(|e| {
        let kind = if e.kind() == std::io::ErrorKind::NotFound {
            ExceptionType::FileOpenError
        } else {
            ExceptionType::FileOpenError
        };
        Exception::new(
            kind,
            format!("WAV文件打开失败，可能是文件不存在或其他错误: {e}"),
        )
    })?;
    let mut r = BufReader::new(file);

    // RIFF header
    let riff = read_fourcc(&mut r)?;
    let _file_len = read_u32_le(&mut r)?;
    let wave = read_fourcc(&mut r)?;

    const RIFF_ID: [u8; 4] = *b"RIFF";
    const WAVE_ID: [u8; 4] = *b"WAVE";
    if !eq_fourcc(&riff, &RIFF_ID) || !eq_fourcc(&wave, &WAVE_ID) {
        return Err(Exception::new(
            ExceptionType::InvalidInput,
            "WAV文件的RIFF或WAVE标记存在问题，可能不是合法的WAV文件",
        ));
    }

    let mut info = WavInfo {
        sample_rate: 0,
        channels: 0,
        bits_per_sample: 0,
        pcm_data: Vec::new(),
        valid: false,
    };

    const FMT_ID: [u8; 4] = *b"fmt ";
    const DATA_ID: [u8; 4] = *b"data";
    const MINIMUM_FMT_SIZE: u32 = 16;

    loop {
        let chunk_id = read_fourcc(&mut r)?;
        let chunk_size = read_u32_le(&mut r)?;

        if eq_fourcc(&chunk_id, &FMT_ID) {
            if chunk_size < MINIMUM_FMT_SIZE {
                return Err(Exception::new(
                    ExceptionType::InvalidInput,
                    "WAV文件的格式块大小异常，无法解析音频格式信息",
                ));
            }
            // 16-byte basic PCM header
            let _format_tag = read_u16_le(&mut r)?;
            info.channels = read_u16_le(&mut r)?;
            info.sample_rate = read_u32_le(&mut r)?;
            let _byte_rate = read_u32_le(&mut r)?;
            let _block_align = read_u16_le(&mut r)?;
            info.bits_per_sample = read_u16_le(&mut r)?;

            // Skip remaining bytes of the fmt chunk (cbSize + any extension)
            let consumed: u32 = 16;
            if chunk_size > consumed {
                let to_skip = chunk_size - consumed;
                r.seek(SeekFrom::Current(to_skip as i64)).map_err(|_| {
                    Exception::new(ExceptionType::FileReadError, "WAV 文件读取失败，可能是文件损坏或其他错误")
                })?;
            }
            validate_pcm_format(info.sample_rate, info.channels, info.bits_per_sample)?;
        } else if eq_fourcc(&chunk_id, &DATA_ID) {
            info.pcm_data = vec![0u8; chunk_size as usize];
            r.read_exact(&mut info.pcm_data).map_err(|_| {
                Exception::new(ExceptionType::FileReadError, "WAV文件读取失败，可能是文件损坏或其他错误")
            })?;
            info.valid = true;
            break;
        } else {
            // Unknown chunk: skip
            r.seek(SeekFrom::Current(chunk_size as i64)).map_err(|_| {
                Exception::new(ExceptionType::FileReadError, "WAV文件读取失败，可能是文件损坏或其他错误")
            })?;
        }
    }

    Ok(info)
}
