//! MP3 decoding via [`minimp3`] (pure Rust, no platform ACM dependency).
//!
//! Follows the upstream C++ MP3 support (`6e39179`), which used the Windows
//! ACM MP3 decoder. Here the decode target is the same: raw PCM, which the
//! caller feeds into [`crate::resample::convert_pcm_to_standard`] to obtain
//! the canonical 44.1kHz stereo `i16` representation.

use crate::error::{Exception, ExceptionType, Result};
use minimp3::{Decoder, Error as Mp3Error, Frame};
use std::fs::File;
use std::io::{Read, Seek};
use std::path::Path;

/// Decoded MP3 payload: interleaved `i16` PCM plus per-file format info.
///
/// MP3 frames may carry different sample rates / channel counts; the values of
/// the first frame are taken as the file-level format (the same assumption the
/// upstream ACM pipeline makes).
#[derive(Debug, Clone)]
pub struct DecodedMp3 {
    pub sample_rate: u32,
    pub channels: u16,
    /// Interleaved i16 samples (frame-major, channel-minor).
    pub data: Vec<i16>,
}

/// Decode an MP3 file into raw interleaved PCM.
pub fn decode_mp3<P: AsRef<Path>>(path: P) -> Result<DecodedMp3> {
    let file = File::open(path).map_err(|e| {
        Exception::new(
            ExceptionType::FileOpenError,
            format!("MP3文件打开失败: {e}"),
        )
    })?;
    decode_mp3_reader(file)
}

/// Decode MP3 data from any `Read + Seek` source (file, `&[u8]` via
/// `Cursor`, ...) into raw interleaved PCM.
pub fn decode_mp3_reader<R: Read + Seek>(mut reader: R) -> Result<DecodedMp3> {
    let mut dec = Decoder::new(&mut reader);

    let mut out = DecodedMp3 {
        sample_rate: 44100,
        channels: 2,
        data: Vec::new(),
    };
    let mut first = true;

    loop {
        match dec.next_frame() {
            Ok(Frame {
                data,
                sample_rate,
                channels,
                ..
            }) => {
                if first {
                    out.sample_rate = sample_rate.max(0) as u32;
                    out.channels = channels.max(0) as u16;
                    first = false;
                }
                out.data.extend_from_slice(&data);
            }
            Err(Mp3Error::Eof) => break,
            Err(e) => {
                return Err(Exception::new(
                    ExceptionType::DecodeError,
                    format!("MP3解码失败: {e}"),
                ))
            }
        }
    }

    if out.data.is_empty() {
        return Err(Exception::new(
            ExceptionType::DecodeError,
            "MP3解码结果为空，文件可能已损坏",
        ));
    }
    Ok(out)
}

/// Decode an MP3 file directly into the canonical `StandardWavInfo` form
/// (44.1kHz stereo interleaved i16).
pub fn decode_mp3_to_standard<P: AsRef<Path>>(path: P) -> Result<crate::resample::StandardWavInfo> {
    let pcm = decode_mp3(path)?;
    crate::resample::convert_pcm_to_standard(pcm.sample_rate, pcm.channels, &pcm.data)
}
