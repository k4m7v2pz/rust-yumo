//! Cross-platform audio mixing pool.
//!
//! Follows the upstream C++ API evolution (up to v0.2.0):
//!   * the `AudioPool` singleton has been demoted to an internal engine;
//!   * the public surface is a set of free functions (`preload_audio`,
//!     `add_audio`, ...) plus the [`AudioInstance`] handle, mirroring the
//!     upstream `audioInstance` proxy class;
//!   * global state is controlled through the [`global`] [`AudioSign`]
//!     (mute / stop / volume), matching upstream `yumo::global`;
//!   * a playback-finished callback is invoked *outside* the pool lock,
//!     following the upstream deadlock fix (`38d5433`).
//!
//! Instead of Win32 `waveOut*` + ACM, this implementation uses:
//!   * `cpal` for cross-platform audio output (Win/Linux/macOS),
//!   * a dedicated mixer thread that owns the non-`Send` cpal `Stream` and
//!     pulls mixed chunks from the shared pool state,
//!   * `rubato` for resampling (see [`crate::resample`]).
//!
//! The cpal `Stream` type is **not** `Send`/`Sync` on all platforms (notably
//! macOS CoreAudio). To keep the engine `Send + Sync`, the stream is owned by
//! a dedicated audio thread; the pool communicates with it via an atomic flag
//! (`playing`) plus the shared `PoolInner`.

use crate::error::{Exception, ExceptionType, Result};
use crate::mp3::decode_mp3_to_standard;
use crate::resample::convert_to_standard;
use crate::wav::load_wav;
use crate::{AUDIO_CHUNK_SIZE, TARGET_CHANNELS, TARGET_SAMPLE_RATE};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SampleRate, Stream, StreamConfig};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

/// Canonical audio representation: interleaved i16 PCM, 44.1kHz, stereo.
pub type StandardWavInfo = Vec<i16>;

/// Atomic "load / ready" flag (mirrors upstream `readySign`).
pub type ReadySign = AtomicBool;
/// Atomic boolean switch (mirrors upstream `switchSign`).
pub type SwitchSign = AtomicBool;
/// Atomic float-as-bits volume (mirrors upstream `volumeSign`).
pub type VolumeSign = AtomicU32;

/// A preloaded (and resampled) audio asset, shareable between many playback
/// instances.
#[derive(Debug, Default)]
pub struct PreloadedAudio {
    /// Resampled audio data (44.1kHz, stereo, 16-bit interleaved).
    pub data: StandardWavInfo,
    /// Marked for removal: cleaned up once no live instances reference it.
    pub marked_for_removal: AtomicBool,
}

/// Global audio control signals (mirrors upstream `yumo::audioSign` +
/// `yumo::global`). All fields are lock-free atomics; values can be read or
/// assigned from any thread.
pub struct AudioSign {
    mute: SwitchSign,
    stop: SwitchSign,
    volume: VolumeSign,
}

impl AudioSign {
    pub const fn new() -> Self {
        Self {
            mute: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
        }
    }

    /// Global mute: skipped in mix but positions keep advancing.
    pub fn set_mute(&self, muted: bool) {
        self.mute.store(muted, Ordering::SeqCst);
    }
    pub fn muted(&self) -> bool {
        self.mute.load(Ordering::SeqCst)
    }

    /// Global stop (suspend): positions do NOT advance.
    pub fn set_stop(&self, stopped: bool) {
        self.stop.store(stopped, Ordering::SeqCst);
    }
    pub fn stopped(&self) -> bool {
        self.stop.load(Ordering::SeqCst)
    }

    /// Global volume, clamped to [0.0, 1.0].
    pub fn set_volume(&self, volume: f32) {
        self.volume
            .store(volume.clamp(0.0, 1.0).to_bits(), Ordering::SeqCst);
    }
    pub fn volume(&self) -> f32 {
        f32::from_bits(self.volume.load(Ordering::SeqCst))
    }
}

/// The global audio control signal instance (mirrors upstream `yumo::global`).
#[allow(non_upper_case_globals)]
pub static global: AudioSign = AudioSign::new();

/// Playback-finished callback: invoked with the recycled instance id.
pub type PlaybackFinishedCallback = dyn Fn(usize) + Send + Sync;

/// One playing copy of a [`PreloadedAudio`]. Tracks an independent position.
/// Internal: users operate on [`AudioInstance`] handles instead.
#[derive(Debug)]
struct PlayInstance {
    /// Index into the preloaded vector.
    source: usize,
    position: usize,
    volume: f32,
    active: bool,
    /// Paused: position is NOT advanced.
    stopped: bool,
    /// Muted: skipped in mix but position IS advanced.
    muted: bool,
}

/// Shared, lock-protected pool state. Accessed by both client threads and the
/// audio mixer thread.
struct PoolInner {
    preloaded: Vec<Option<Arc<PreloadedAudio>>>,
    instances: HashMap<usize, PlayInstance>,
    free_ids: VecDeque<usize>,
    next_id: usize,
    /// Registered playback-finished callback (invoked outside the lock).
    callback: Option<Arc<PlaybackFinishedCallback>>,
}

impl Default for PoolInner {
    fn default() -> Self {
        Self {
            preloaded: Vec::new(),
            instances: HashMap::new(),
            free_ids: VecDeque::new(),
            next_id: 0,
            callback: None,
        }
    }
}

/// Internal cross-platform audio mixing engine (singleton).
///
/// The public API mirrors upstream v0.2.0: use the free functions in this
/// module instead of touching this type directly.
pub struct AudioPool {
    inner: Mutex<PoolInner>,
    /// Set true while the cpal output stream is alive.
    playing: AtomicBool,
    /// Guard ensuring the audio thread is started only once.
    stream_started: AtomicBool,
    /// The audio thread keeps the `Stream` alive; we just hold the join handle
    /// (which is `Send + Sync`). The thread itself owns the non-`Send` stream.
    _audio_thread: std::thread::JoinHandle<()>,
}

static INSTANCE: Lazy<AudioPool> = Lazy::new(AudioPool::new);

/// Load a WAV or MP3 file and resample it into the canonical format.
fn load_audio_file(path: &Path) -> Result<Arc<PreloadedAudio>> {
    let data = match path
        .extension()
        .and_then(|e| e.to_str())
        .map(|s| s.to_ascii_lowercase())
    {
        Some(ext) if matches!(ext.as_str(), "mp3" | "mp2" | "mp1") => {
            decode_mp3_to_standard(path)?
        }
        _ => {
            let wav = load_wav(path)?;
            convert_to_standard(&wav)?
        }
    };
    Ok(Arc::new(PreloadedAudio {
        data,
        marked_for_removal: AtomicBool::new(false),
    }))
}

impl AudioPool {
    /// Create a new pool (rarely needed directly; prefer [`AudioPool::instance`]).
    pub fn new() -> Self {
        Self {
            inner: Mutex::new(PoolInner::default()),
            playing: AtomicBool::new(false),
            stream_started: AtomicBool::new(false),
            _audio_thread: std::thread::Builder::new()
                .name("yumo-audio-sentinel".into())
                .spawn(|| {})
                .expect("failed to spawn sentinel"),
        }
    }

    /// Singleton accessor.
    pub fn instance() -> &'static Self {
        &INSTANCE
    }

    /// Preload (asynchronously) a WAV/MP3 file, returning its id.
    ///
    /// `ready` (if given) is set to `false` immediately and `true` once the
    /// load finishes — success or failure.
    pub(crate) fn preload_audio<P: AsRef<Path>>(
        &self,
        filename: P,
        ready: Option<&Arc<ReadySign>>,
    ) -> Result<usize> {
        if let Some(r) = ready {
            r.store(false, Ordering::SeqCst);
        }

        let id: usize;
        {
            let mut g = self.inner.lock();
            g.preloaded.push(None);
            id = g.preloaded.len() - 1;
        }
        let r = ready.map(|x| x.clone());
        let filename = filename.as_ref().to_path_buf();
        std::thread::Builder::new()
            .name(format!("yumo-preload-{id}"))
            .spawn(move || {
                let pool = AudioPool::instance();
                let outcome = load_audio_file(&filename);
                {
                    let mut g = pool.inner.lock();
                    if let Ok(p) = outcome {
                        g.preloaded[id] = Some(p);
                    }
                }
                if let Some(r) = r {
                    r.store(true, Ordering::SeqCst);
                }
            })
            .map_err(|e| {
                Exception::new(
                    ExceptionType::UnknownError,
                    format!("无法启动预加载线程: {e}"),
                )
            })?;
        Ok(id)
    }

    fn allocate_instance_id(inner: &mut PoolInner) -> usize {
        if let Some(id) = inner.free_ids.pop_front() {
            id
        } else {
            let id = inner.next_id;
            inner.next_id += 1;
            id
        }
    }

    /// Add a preloaded audio (by id) to the playback mix and start playing.
    /// Returns the new instance id.
    pub(crate) fn add_audio(&self, preloaded_id: usize, volume: f32) -> Result<usize> {
        let instance_id;
        let need_open;
        {
            let mut g = self.inner.lock();
            if preloaded_id >= g.preloaded.len() || g.preloaded[preloaded_id].is_none() {
                return Err(Exception::new(
                    ExceptionType::InvalidID,
                    "无效的预加载音频ID",
                ));
            }
            let src = g.preloaded[preloaded_id].as_ref().unwrap();
            if src.data.is_empty() {
                return Err(Exception::new(
                    ExceptionType::InvalidData,
                    "预加载音频数据为空",
                ));
            }
            let inst = PlayInstance {
                source: preloaded_id,
                position: 0,
                volume: volume.clamp(0.0, 1.0),
                active: true,
                stopped: false,
                muted: false,
            };
            instance_id = Self::allocate_instance_id(&mut g);
            g.instances.insert(instance_id, inst);
            need_open = !self.playing.load(Ordering::SeqCst);
        }
        if need_open {
            self.ensure_device_open();
        }
        Ok(instance_id)
    }

    /// Convenience wrapper: preload + add in one shot. `instance_out` and
    /// `ready_out` are updated asynchronously when the worker finishes.
    pub(crate) fn add_audio_file<P: AsRef<Path>>(
        &self,
        filename: P,
        volume: f32,
        instance_out: Option<&Arc<std::sync::Mutex<Option<AudioInstance>>>>,
        ready_out: Option<&Arc<ReadySign>>,
    ) {
        let ready = ready_out.cloned();
        let inst_slot = instance_out.cloned();

        if let Some(r) = &ready {
            r.store(false, Ordering::SeqCst);
        }

        let preloaded_id = match self.preload_audio(filename, ready.as_ref()) {
            Ok(id) => id,
            Err(_) => {
                if let Some(r) = &ready {
                    r.store(true, Ordering::SeqCst);
                }
                return;
            }
        };
        let vol = volume;
        std::thread::Builder::new()
            .name("yumo-addaudio".into())
            .spawn(move || {
                let pool = AudioPool::instance();
                let r = ready
                    .clone()
                    .unwrap_or_else(|| Arc::new(ReadySign::new(false)));
                while !r.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                r.store(false, Ordering::SeqCst);
                let ok = {
                    let g = pool.inner.lock();
                    preloaded_id < g.preloaded.len()
                        && g.preloaded[preloaded_id]
                            .as_ref()
                            .map(|p| !p.data.is_empty())
                            .unwrap_or(false)
                };
                if !ok {
                    r.store(true, Ordering::SeqCst);
                    return;
                }
                if let Ok(id) = pool.add_audio(preloaded_id, vol) {
                    if let Some(slot) = &inst_slot {
                        *slot.lock().unwrap() = Some(AudioInstance::new(id));
                    }
                    {
                        let g = pool.inner.lock();
                        if let Some(Some(p)) = g.preloaded.get(preloaded_id) {
                            p.marked_for_removal.store(true, Ordering::SeqCst);
                        }
                    }
                }
                r.store(true, Ordering::SeqCst);
            })
            .ok();
    }

    /// Open the cpal output stream on a dedicated audio thread that owns the
    /// non-`Send` `Stream` handle. The thread runs the cpal stream and exits
    /// when `playing` flips back to false (e.g. when the stream is dropped).
    ///
    /// The stream config adapts to what the device actually supports:
    /// sample format (f32/i16/u16/f64), channel count and — when 44.1kHz is
    /// unavailable — a light linear-interpolation resampler is used.
    fn ensure_device_open(&self) {
        if self.playing.load(Ordering::SeqCst) {
            return;
        }
        // Ensure we only attempt to start once.
        if self.stream_started.swap(true, Ordering::SeqCst) {
            // Another caller is starting; wait briefly for it.
            for _ in 0..200 {
                if self.playing.load(Ordering::SeqCst) {
                    return;
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            return;
        }

        // Reset positions of still-active instances.
        {
            let mut g = self.inner.lock();
            let needs_reset: Vec<usize> = g
                .instances
                .iter()
                .filter_map(|(id, inst)| {
                    if let Some(src) = g.preloaded.get(inst.source).and_then(|x| x.as_ref()) {
                        if inst.active && inst.position < src.data.len() {
                            return Some(*id);
                        }
                    }
                    None
                })
                .collect();
            for id in needs_reset {
                if let Some(inst) = g.instances.get_mut(&id) {
                    inst.position = 0;
                }
            }
        }

        // The audio thread captures a *trait-object* callback that reads from
        // the shared pool state via the static `INSTANCE`. The `Stream` itself
        // never crosses threads.
        std::thread::Builder::new()
            .name("yumo-cpal-stream".into())
            .spawn(move || {
                let pool = AudioPool::instance();
                let host = cpal::default_host();
                let device = match host.default_output_device() {
                    Some(d) => d,
                    None => {
                        eprintln!("[yumo] 无可用音频输出设备");
                        pool.stream_started.store(false, Ordering::SeqCst);
                        return;
                    }
                };
                let (config, sample_format) = pick_output_config(&device);
                let device_channels = config.channels as usize;
                let stream: Stream = match build_output_stream(&device, &config, sample_format, device_channels) {
                    Ok(s) => s,
                    Err(e) => {
                        eprintln!("[yumo] 无法打开音频输出流: {e}");
                        pool.stream_started.store(false, Ordering::SeqCst);
                        return;
                    }
                };
                if stream.play().is_err() {
                    eprintln!("[yumo] 启动音频流失败");
                    pool.stream_started.store(false, Ordering::SeqCst);
                    return;
                }
                pool.playing.store(true, Ordering::SeqCst);

                // Keep the stream alive until playback should stop. We poll
                // `playing` periodically; when no instances remain active the
                // mixer sets it false and we drop the stream + exit.
                while pool.playing.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(50));
                }
                drop(stream);
                pool.stream_started.store(false, Ordering::SeqCst);
            })
            .ok();
    }

    /// Pull one chunk of mixed audio, advancing instance positions.
    ///
    /// Global state comes from the [`global`] signal instance; finished
    /// instances are recycled and their ids reported to the playback-finished
    /// callback *outside* the pool lock (upstream deadlock fix `38d5433`).
    fn render_chunk(&self, out: &mut [i16]) {
        let chunk = out.len();
        for x in out.iter_mut() {
            *x = 0;
        }

        // --- Phase 1: snapshot under lock ---
        struct Job {
            id: usize,
            position: usize,
            volume: f32,
            muted_inst: bool,
            src: Arc<PreloadedAudio>,
        }
        let global_stopped = global.stopped();
        if global_stopped {
            return;
        }
        let global_muted = global.muted();
        let global_volume = global.volume();
        let jobs: Vec<Job>;
        {
            let g = self.inner.lock();
            jobs = g
                .instances
                .iter()
                .filter_map(|(id, inst)| {
                    if !inst.active || inst.stopped {
                        return None;
                    }
                    let src = g.preloaded.get(inst.source).and_then(|x| x.as_ref())?.clone();
                    if inst.position >= src.data.len() {
                        return None;
                    }
                    Some(Job {
                        id: *id,
                        position: inst.position,
                        volume: inst.volume,
                        muted_inst: inst.muted,
                        src,
                    })
                })
                .collect();
        }

        // --- Phase 2: mix without holding the lock ---
        let mut mixed: Vec<i32> = vec![0i32; chunk];
        let mut updates: Vec<(usize, usize, bool)> = Vec::with_capacity(jobs.len());
        let mut finished_ids: Vec<usize> = Vec::new();
        for job in &jobs {
            let data = &job.src.data;
            let remaining = data.len() - job.position;
            let take = chunk.min(remaining);
            let new_position = job.position + take;
            let finished = new_position >= data.len();
            updates.push((job.id, new_position, finished));
            if finished {
                finished_ids.push(job.id);
            }

            if job.muted_inst || global_muted {
                continue;
            }
            let vol = job.volume * global_volume;
            for i in 0..take {
                let s = data[job.position + i] as i32;
                let v = (s as f32 * vol).round() as i32;
                mixed[i] = mixed[i].saturating_add(v);
            }
        }
        for (i, m) in mixed.iter().enumerate() {
            out[i] = (*m).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }

        // --- Phase 3: commit position updates, recycle finished instances,
        // --- clean up marked preloads; snapshot the callback.
        let callback: Option<Arc<PlaybackFinishedCallback>>;
        let any_active;
        {
            let mut g = self.inner.lock();
            for (id, new_pos, finished) in &updates {
                if let Some(inst) = g.instances.get_mut(id) {
                    inst.position = *new_pos;
                    if *finished {
                        inst.active = false;
                    }
                }
            }
            // Recycle finished instances (id becomes reusable).
            for id in &finished_ids {
                g.instances.remove(id);
                g.free_ids.push_back(*id);
            }
            let mut referenced = std::collections::HashSet::new();
            for inst in g.instances.values() {
                if inst.active {
                    referenced.insert(inst.source);
                }
            }
            for (i, slot) in g.preloaded.iter_mut().enumerate() {
                if let Some(p) = slot {
                    if p.marked_for_removal.load(Ordering::SeqCst) && !referenced.contains(&i) {
                        *slot = None;
                    }
                }
            }
            any_active = g.instances.values().any(|inst| {
                let len = g
                    .preloaded
                    .get(inst.source)
                    .and_then(|x| x.as_ref())
                    .map(|p| p.data.len())
                    .unwrap_or(0);
                inst.active && inst.position < len
            });
            callback = g.callback.clone();
        }

        // --- Phase 4: notify playback-finished OUTSIDE the lock ---
        if let Some(cb) = callback {
            for id in finished_ids {
                cb(id);
            }
        }

        // If nothing is active anymore, signal the audio thread to stop.
        if !any_active {
            self.playing.store(false, Ordering::SeqCst);
        }
    }

    // ---- Engine query/control (used by free functions / AudioInstance) ----

    pub(crate) fn remove_preloaded_audio(&self, preloaded_id: usize) {
        let mut g = self.inner.lock();
        if preloaded_id >= g.preloaded.len() {
            return;
        }
        let referenced = g
            .instances
            .values()
            .any(|inst| inst.active && inst.source == preloaded_id);
        if !referenced {
            g.preloaded[preloaded_id] = None;
        } else if let Some(Some(p)) = g.preloaded.get(preloaded_id) {
            p.marked_for_removal.store(true, Ordering::SeqCst);
        }
    }

    pub(crate) fn get_preloaded_count(&self) -> usize {
        self.inner.lock().preloaded.len()
    }
    pub(crate) fn get_playing_count(&self) -> usize {
        self.inner.lock().instances.len()
    }

    pub(crate) fn reset_all(&self) {
        let mut g = self.inner.lock();
        for inst in g.instances.values_mut() {
            inst.position = 0;
        }
    }

    pub(crate) fn remove_instance(&self, instance_id: usize) -> bool {
        let mut g = self.inner.lock();
        if g.instances.remove(&instance_id).is_some() {
            g.free_ids.push_back(instance_id);
            true
        } else {
            false
        }
    }

    pub(crate) fn is_playing(&self, instance_id: usize) -> bool {
        let g = self.inner.lock();
        match g.instances.get(&instance_id) {
            Some(inst) => {
                let len = g
                    .preloaded
                    .get(inst.source)
                    .and_then(|x| x.as_ref())
                    .map(|p| p.data.len())
                    .unwrap_or(0);
                inst.active && inst.position < len
            }
            None => false,
        }
    }

    pub(crate) fn instance_exists(&self, instance_id: usize) -> bool {
        self.inner.lock().instances.contains_key(&instance_id)
    }

    pub(crate) fn instance_active(&self, instance_id: usize) -> bool {
        self.inner
            .lock()
            .instances
            .get(&instance_id)
            .map(|i| i.active)
            .unwrap_or(false)
    }

    pub(crate) fn instance_position(&self, instance_id: usize) -> Result<usize> {
        let g = self.inner.lock();
        g.instances
            .get(&instance_id)
            .map(|i| i.position)
            .ok_or_else(|| Exception::new(ExceptionType::InvalidID, "无效的播放实例ID"))
    }

    pub(crate) fn instance_set_position(&self, instance_id: usize, position: usize) -> Result<()> {
        let mut g = self.inner.lock();
        g.instances
            .get_mut(&instance_id)
            .map(|i| i.position = position)
            .ok_or_else(|| Exception::new(ExceptionType::InvalidID, "无效的播放实例ID"))
    }

    pub(crate) fn instance_volume(&self, instance_id: usize) -> Result<f32> {
        let g = self.inner.lock();
        g.instances
            .get(&instance_id)
            .map(|i| i.volume)
            .ok_or_else(|| Exception::new(ExceptionType::InvalidID, "无效的播放实例ID"))
    }

    pub(crate) fn instance_set_volume(&self, instance_id: usize, volume: f32) -> Result<()> {
        let mut g = self.inner.lock();
        g.instances
            .get_mut(&instance_id)
            .map(|i| i.volume = volume.clamp(0.0, 1.0))
            .ok_or_else(|| Exception::new(ExceptionType::InvalidID, "无效的播放实例ID"))
    }

    pub(crate) fn instance_stopped(&self, instance_id: usize) -> Result<bool> {
        let g = self.inner.lock();
        g.instances
            .get(&instance_id)
            .map(|i| i.stopped)
            .ok_or_else(|| Exception::new(ExceptionType::InvalidID, "无效的播放实例ID"))
    }

    pub(crate) fn instance_set_stopped(&self, instance_id: usize, stopped: bool) -> Result<()> {
        let mut g = self.inner.lock();
        g.instances
            .get_mut(&instance_id)
            .map(|i| i.stopped = stopped)
            .ok_or_else(|| Exception::new(ExceptionType::InvalidID, "无效的播放实例ID"))
    }

    pub(crate) fn instance_muted(&self, instance_id: usize) -> Result<bool> {
        let g = self.inner.lock();
        g.instances
            .get(&instance_id)
            .map(|i| i.muted)
            .ok_or_else(|| Exception::new(ExceptionType::InvalidID, "无效的播放实例ID"))
    }

    pub(crate) fn instance_set_muted(&self, instance_id: usize, muted: bool) -> Result<()> {
        let mut g = self.inner.lock();
        g.instances
            .get_mut(&instance_id)
            .map(|i| i.muted = muted)
            .ok_or_else(|| Exception::new(ExceptionType::InvalidID, "无效的播放实例ID"))
    }
}

impl Default for AudioPool {
    fn default() -> Self {
        Self::new()
    }
}

/// Pick the best output configuration for a device:
///   1. 44.1kHz stereo (any supported sample format),
///   2. 44.1kHz with whatever channel count is available,
///   3. the device's default configuration (may need resampling).
fn pick_output_config(device: &cpal::Device) -> (StreamConfig, SampleFormat) {
    let mut best_stereo: Option<(StreamConfig, SampleFormat)> = None;
    let mut best_any: Option<(StreamConfig, SampleFormat)> = None;
    if let Ok(ranges) = device.supported_output_configs() {
        for range in ranges {
            let fmt = range.sample_format();
            let channels = range.channels();
            let rate_ok = range.min_sample_rate().0 <= TARGET_SAMPLE_RATE
                && range.max_sample_rate().0 >= TARGET_SAMPLE_RATE;
            if !rate_ok {
                continue;
            }
            let cfg = range.with_sample_rate(SampleRate(TARGET_SAMPLE_RATE));
            if channels == TARGET_CHANNELS && best_stereo.is_none() {
                best_stereo = Some((cfg.config(), fmt));
            } else if best_any.is_none() {
                best_any = Some((cfg.config(), fmt));
            }
        }
    }
    if let Some(c) = best_stereo {
        return c;
    }
    if let Some(c) = best_any {
        return c;
    }
    if let Ok(c) = device.default_output_config() {
        return (c.config(), c.sample_format());
    }
    (
        StreamConfig {
            channels: TARGET_CHANNELS,
            sample_rate: SampleRate(TARGET_SAMPLE_RATE),
            buffer_size: cpal::BufferSize::Default,
        },
        SampleFormat::I16,
    )
}

/// Render one device buffer: pull a stereo 44.1kHz chunk from the pool, map
/// it to the device channel count and sample format (with a light linear
/// interpolation when the device rate differs from 44.1kHz).
fn render_to_device<T, F>(out: &mut [T], device_channels: usize, device_rate: u32, conv: F)
where
    F: Fn(f32) -> T + Copy,
{
    let frames = out.len() / device_channels.max(1);
    if frames == 0 {
        return;
    }
    let mut stereo = vec![0i16; frames * 2];
    AudioPool::instance().render_chunk(&mut stereo);
    let in_frames = stereo.len() / 2;

    if device_rate == TARGET_SAMPLE_RATE {
        for f in 0..frames {
            let l = stereo[f * 2] as f32 / 32768.0;
            let r = stereo[f * 2 + 1] as f32 / 32768.0;
            for c in 0..device_channels {
                let v = if device_channels == 1 {
                    (l + r) * 0.5
                } else if c == 0 {
                    l
                } else {
                    r
                };
                out[f * device_channels + c] = conv(v);
            }
        }
    } else {
        // 44.1kHz -> device_rate, linear interpolation over source frames.
        let step = TARGET_SAMPLE_RATE as f64 / device_rate as f64;
        let mut pos = 0.0f64;
        for f in 0..frames {
            let idx = pos.floor() as usize;
            let frac = pos - idx as f64;
            let i0 = idx.min(in_frames - 1);
            let i1 = (idx + 1).min(in_frames - 1);
            let l = lerp(stereo[i0 * 2], stereo[i1 * 2], frac) / 32768.0;
            let r = lerp(stereo[i0 * 2 + 1], stereo[i1 * 2 + 1], frac) / 32768.0;
            for c in 0..device_channels {
                let v = if device_channels == 1 {
                    (l + r) * 0.5
                } else if c == 0 {
                    l
                } else {
                    r
                };
                out[f * device_channels + c] = conv(v);
            }
            pos += step;
        }
    }
}

#[inline]
fn lerp(a: i16, b: i16, t: f64) -> f32 {
    (a as f64 + (b as f64 - a as f64) * t) as f32
}

/// Build a cpal output stream whose data callback renders through
/// [`render_to_device`] in the device's native sample format.
fn build_output_stream(
    device: &cpal::Device,
    config: &StreamConfig,
    sample_format: SampleFormat,
    device_channels: usize,
) -> std::result::Result<Stream, cpal::BuildStreamError> {
    let device_rate = config.sample_rate.0;
    let err_cb = |err| eprintln!("[yumo] 音频流错误: {err:?}");
    match sample_format {
        SampleFormat::F32 => device.build_output_stream(
            config,
            move |out: &mut [f32], _| {
                render_to_device(out, device_channels, device_rate, |v| v);
            },
            err_cb,
            None,
        ),
        SampleFormat::F64 => device.build_output_stream(
            config,
            move |out: &mut [f64], _| {
                render_to_device(out, device_channels, device_rate, |v| v as f64);
            },
            err_cb,
            None,
        ),
        SampleFormat::I16 => device.build_output_stream(
            config,
            move |out: &mut [i16], _| {
                render_to_device(out, device_channels, device_rate, |v| {
                    (v.clamp(-1.0, 1.0) * 32767.0) as i16
                });
            },
            err_cb,
            None,
        ),
        SampleFormat::U16 => device.build_output_stream(
            config,
            move |out: &mut [u16], _| {
                render_to_device(out, device_channels, device_rate, |v| {
                    ((v.clamp(-1.0, 1.0) * 32767.0) as i32 + 32768) as u16
                });
            },
            err_cb,
            None,
        ),
        // Unknown / new formats: fall back to i16.
        _ => device.build_output_stream(
            config,
            move |out: &mut [i16], _| {
                render_to_device(out, device_channels, device_rate, |v| {
                    (v.clamp(-1.0, 1.0) * 32767.0) as i16
                });
            },
            err_cb,
            None,
        ),
    }
}

/// Handle to one playback instance (mirrors upstream `audioInstance`).
///
/// The handle is a lightweight id wrapper: operations are forwarded to the
/// internal pool engine. An instance whose id was recycled by the mixer (or
/// removed by the user) reports errors with `InvalidID`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioInstance {
    instance_id: usize,
}

impl AudioInstance {
    pub(crate) fn new(instance_id: usize) -> Self {
        Self { instance_id }
    }

    /// The instance id (mirrors upstream `audioInstance::instanceId`).
    pub fn id(&self) -> usize {
        self.instance_id
    }

    /// Whether the instance is still playing (active and not past the end).
    pub fn is_playing(&self) -> bool {
        AudioPool::instance().is_playing(self.instance_id)
    }

    /// Whether the instance is active in the pool.
    pub fn active(&self) -> bool {
        AudioPool::instance().instance_active(self.instance_id)
    }

    /// Current playback position in sample frames (mirrors upstream
    /// `audioInstance::position`).
    pub fn position(&self) -> Result<usize> {
        AudioPool::instance().instance_position(self.instance_id)
    }
    pub fn set_position(&self, position: usize) -> Result<()> {
        AudioPool::instance().instance_set_position(self.instance_id, position)
    }

    /// Instance volume, clamped to [0.0, 1.0] on write.
    pub fn volume(&self) -> Result<f32> {
        AudioPool::instance().instance_volume(self.instance_id)
    }
    pub fn set_volume(&self, volume: f32) -> Result<()> {
        AudioPool::instance().instance_set_volume(self.instance_id, volume)
    }

    /// Paused state: when `true` the position does NOT advance.
    pub fn stopped(&self) -> Result<bool> {
        AudioPool::instance().instance_stopped(self.instance_id)
    }
    pub fn set_stopped(&self, stopped: bool) -> Result<()> {
        AudioPool::instance().instance_set_stopped(self.instance_id, stopped)
    }

    /// Muted state: skipped in mix but position advances.
    pub fn muted(&self) -> Result<bool> {
        AudioPool::instance().instance_muted(self.instance_id)
    }
    pub fn set_muted(&self, muted: bool) -> Result<()> {
        AudioPool::instance().instance_set_muted(self.instance_id, muted)
    }

    /// Remove this instance from the pool (its id becomes reusable).
    pub fn remove(&self) -> bool {
        AudioPool::instance().remove_instance(self.instance_id)
    }
}

// ===== Global free-function wrapper layer =====
// Mirrors the upstream C++ free functions (`preloadAudio`, `addAudio`, ...).

/// Preload a WAV/MP3 file asynchronously, returning its id.
pub fn preload_audio<P: AsRef<Path>>(
    filename: P,
    ready: Option<&Arc<ReadySign>>,
) -> Result<usize> {
    AudioPool::instance().preload_audio(filename, ready)
}

/// Add a preloaded audio (by id) to the playback mix and start playing.
pub fn add_audio(preloaded_id: usize, volume: f32) -> Result<AudioInstance> {
    let id = AudioPool::instance().add_audio(preloaded_id, volume)?;
    Ok(AudioInstance::new(id))
}

/// Convenience wrapper: preload + add in one shot (mirrors upstream
/// `addAudio(filename, ...)`). `instance_out` receives the play instance once
/// playback starts; `ready` flips to `true` when the load finishes.
pub fn add_audio_file<P: AsRef<Path>>(
    filename: P,
    volume: f32,
    instance_out: Option<&Arc<std::sync::Mutex<Option<AudioInstance>>>>,
    ready: Option<&Arc<ReadySign>>,
) {
    AudioPool::instance().add_audio_file(filename, volume, instance_out, ready)
}

/// Re-acquire a play instance by id (mirrors upstream `regain`).
///
/// Returns `None` if the instance id does not exist (recycled or removed).
pub fn regain(instance_id: usize) -> Option<AudioInstance> {
    if AudioPool::instance().instance_exists(instance_id) {
        Some(AudioInstance::new(instance_id))
    } else {
        None
    }
}

/// Register the playback-finished callback (mirrors upstream
/// `registerPlaybackFinishedCallback`). Called with the recycled instance id,
/// always outside the pool lock.
pub fn register_playback_finished_callback<F>(callback: F)
where
    F: Fn(usize) + Send + Sync + 'static,
{
    AudioPool::instance().inner.lock().callback = Some(Arc::new(callback));
}

/// Unregister the playback-finished callback (mirrors upstream
/// `unregisterPlaybackFinishedCallback`).
pub fn unregister_playback_finished_callback() {
    AudioPool::instance().inner.lock().callback = None;
}

/// Remove a preloaded audio object (mirrors upstream `removePreloadedAudio`).
/// If still referenced by playing instances, it is marked and cleaned later.
pub fn remove_preloaded_audio(preloaded_id: usize) {
    AudioPool::instance().remove_preloaded_audio(preloaded_id)
}

/// Number of preloaded audio objects (mirrors upstream `getPreloadedCount`).
pub fn get_preloaded_count() -> usize {
    AudioPool::instance().get_preloaded_count()
}

/// Number of currently playing instances (mirrors upstream `getPlayingCount`).
pub fn get_playing_count() -> usize {
    AudioPool::instance().get_playing_count()
}

/// Reset all playback instance positions to the start (mirrors upstream
/// `resetAll`).
pub fn reset_all() {
    AudioPool::instance().reset_all()
}

/// Remove a playback instance by id (mirrors upstream `remove`).
pub fn remove(instance_id: usize) -> bool {
    AudioPool::instance().remove_instance(instance_id)
}

// Keep `AUDIO_CHUNK_SIZE` referenced for parity with the C++ source.
const _: usize = AUDIO_CHUNK_SIZE;
