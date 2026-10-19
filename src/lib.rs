//! # Yumo Audio
//!
//! Cross-platform audio mixing playback library.
//!
//! Rust port of the original C++/Win32 "Yumo Audio" library, following the
//! upstream API evolution up to v0.2.0: free functions + [`AudioInstance`]
//! handles, a global [`AudioSign`] control, a playback-finished callback, and
//! MP3 support.
//! Supported platforms: Windows, Linux, macOS.
//!
//! ## Quick usage
//!
//! ```no_run
//! use yumo::{add_audio_file, preload_audio, add_audio, AudioInstance};
//! use std::sync::atomic::Ordering;
//! use std::sync::Arc;
//!
//! let ready = Arc::new(yumo::ReadySign::new(false));
//! let id = preload_audio("test.wav", Some(&ready)).unwrap();
//! while !ready.load(Ordering::SeqCst) {
//!     std::thread::sleep(std::time::Duration::from_millis(10));
//! }
//! let inst: AudioInstance = add_audio(id, 1.0).unwrap();
//! inst.set_volume(0.5).unwrap();
//! yumo::global.set_stop(true); // suspend all playback
//! ```

mod error;
pub mod mp3;
pub mod resample;
pub mod wav;
mod pool;

pub use error::{Exception, ExceptionType};
pub use mp3::{decode_mp3, decode_mp3_to_standard, DecodedMp3};
pub use pool::{
    add_audio, add_audio_file, get_playing_count, get_preloaded_count, global, preload_audio,
    regain, register_playback_finished_callback, remove, remove_preloaded_audio, reset_all,
    unregister_playback_finished_callback, AudioInstance, AudioSign, PreloadedAudio, ReadySign,
    StandardWavInfo, SwitchSign, VolumeSign,
};
pub use wav::WavInfo;

pub(crate) const TARGET_SAMPLE_RATE: u32 = 44100;
pub(crate) const TARGET_CHANNELS: u16 = 2;
/// Audio chunk size in samples (frames * channels). ≈100ms at 44.1kHz stereo.
pub(crate) const AUDIO_CHUNK_SIZE: usize = (44100 * 2 * 100 / 1000) as usize; // 8820
#[allow(dead_code)]
pub(crate) const TARGET_BITS_PER_SAMPLE: u16 = 16;
