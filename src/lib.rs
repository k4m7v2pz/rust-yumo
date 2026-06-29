//! # Yumo Audio
//!
//! Cross-platform audio mixing playback library.
//!
//! Rust port of the original C++/Win32 "Yumo Audio" library.
//! Supported platforms: Windows, Linux, macOS.
//!
//! ## Quick usage
//!
//! ```no_run
//! use yumo::AudioPool;
//!
//! let pool = AudioPool::instance();
//! pool.add_audio_file("test.wav", 1.0, None, None);
//! // ... enjoy ...
//! pool.stop_all();
//! ```

mod error;
pub mod wav;
pub mod resample;
mod pool;

pub use error::{Exception, ExceptionType};
pub use pool::{AudioPool, PreloadedAudio, PlayInstance, StandardWavInfo};
pub use wav::WavInfo;

pub(crate) const TARGET_SAMPLE_RATE: u32 = 44100;
pub(crate) const TARGET_CHANNELS: u16 = 2;
/// Audio chunk size in samples (frames * channels). ≈100ms at 44.1kHz stereo.
pub(crate) const AUDIO_CHUNK_SIZE: usize = (44100 * 2 * 100 / 1000) as usize; // 8820
#[allow(dead_code)]
pub(crate) const TARGET_BITS_PER_SAMPLE: u16 = 16;
