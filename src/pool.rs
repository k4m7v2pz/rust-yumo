//! Cross-platform audio mixing pool.
//!
//! Mirrors the original C++ `AudioPool` singleton. Instead of Win32
//! `waveOut*` + ACM, this implementation uses:
//!   * `cpal` for cross-platform audio output (Win/Linux/macOS),
//!   * a dedicated mixer thread that owns the non-`Send` cpal `Stream` and
//!     pulls mixed chunks from the shared pool state,
//!   * `rubato` for resampling (see [`crate::resample`]).
//!
//! The cpal `Stream` type is **not** `Send`/`Sync` on all platforms (notably
//! macOS CoreAudio). To keep `AudioPool` itself `Send + Sync`, the stream is
//! owned by a dedicated audio thread; the pool communicates with it via a
//! `crossbeam`-style atomic flag (`playing`) plus the shared `PoolInner`.

use crate::error::{Exception, ExceptionType, Result};
use crate::resample::convert_to_standard;
use crate::wav::load_wav;
use crate::{AUDIO_CHUNK_SIZE, TARGET_CHANNELS, TARGET_SAMPLE_RATE};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleRate, Stream};
use once_cell::sync::Lazy;
use parking_lot::Mutex;
use std::collections::{HashMap, VecDeque};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

/// Canonical audio representation: interleaved i16 PCM, 44.1kHz, stereo.
pub type StandardWavInfo = Vec<i16>;

/// A preloaded (and resampled) audio asset, shareable between many
/// [`PlayInstance`]s.
#[derive(Debug, Default)]
pub struct PreloadedAudio {
    /// Resampled audio data (44.1kHz, stereo, 16-bit interleaved).
    pub data: StandardWavInfo,
    /// Marked for removal: cleaned up once no live instances reference it.
    pub marked_for_removal: AtomicBool,
}

/// One playing copy of a [`PreloadedAudio`]. Tracks an independent position.
#[derive(Debug)]
pub struct PlayInstance {
    /// Index into `AudioPool::preloaded_audios`.
    pub source: usize,
    pub position: usize,
    pub volume: f32,
    pub active: bool,
    /// Paused: position is NOT advanced.
    pub stopped: bool,
    /// Muted: skipped in mix but position IS advanced.
    pub muted: bool,
}

/// Shared, lock-protected pool state. Accessed by both client threads and the
/// audio mixer thread.
struct PoolInner {
    preloaded: Vec<Option<Arc<PreloadedAudio>>>,
    instances: HashMap<usize, PlayInstance>,
    free_ids: VecDeque<usize>,
    next_id: usize,
    /// Globally suspended (position not advanced).
    stopped: bool,
    /// Globally muted (position advanced, mix skipped).
    muted: bool,
}

impl Default for PoolInner {
    fn default() -> Self {
        Self {
            preloaded: Vec::new(),
            instances: HashMap::new(),
            free_ids: VecDeque::new(),
            next_id: 0,
            stopped: false,
            muted: false,
        }
    }
}

/// Cross-platform audio mixing pool, singleton.
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

impl AudioPool {
    /// Create a new pool (rarely needed directly; prefer [`AudioPool::instance`]).
    pub fn new() -> Self {
        // The audio thread blocks on a park loop; it only opens a cpal stream
        // once `playing` is requested via `ensure_device_open`.
        let handle = std::thread::Builder::new()
            .name("yumo-audio-thread".into())
            .spawn(|| {})
            .expect("failed to spawn yumo audio thread placeholder");
        // The above is a no-op placeholder; the real stream lives in a thread
        // spawned lazily inside `ensure_device_open`. We drop the placeholder
        // and store a sleeping sentinel instead.
        let _ = handle;
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

    /// Singleton accessor (mirrors `AudioPool::getInstance()`).
    pub fn instance() -> &'static Self { &INSTANCE }

    /// Preload (asynchronously) a WAV file, returning its id.
    ///
    /// `ready` (if given) is set to `false` immediately and `true` once the
    /// load finishes — success or failure.
    pub fn preload_audio<P: AsRef<Path>>(
        &self,
        filename: P,
        ready: Option<&Arc<AtomicBool>>,
    ) -> Result<usize> {
        if let Some(r) = ready { r.store(false, Ordering::SeqCst); }

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
                let outcome: Result<Arc<PreloadedAudio>> = (|| {
                    let wav = load_wav(&filename)?;
                    let data = convert_to_standard(&wav)?;
                    Ok(Arc::new(PreloadedAudio {
                        data,
                        marked_for_removal: AtomicBool::new(false),
                    }))
                })();
                {
                    let mut g = pool.inner.lock();
                    if let Ok(p) = outcome {
                        g.preloaded[id] = Some(p);
                    }
                }
                if let Some(r) = r { r.store(true, Ordering::SeqCst); }
            })
            .map_err(|e| Exception::new(ExceptionType::UnknownError, format!("无法启动预加载线程: {e}")))?;
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
    pub fn add_audio(&self, preloaded_id: usize, volume: f32) -> Result<usize> {
        let instance_id;
        let need_open;
        {
            let mut g = self.inner.lock();
            if preloaded_id >= g.preloaded.len() || g.preloaded[preloaded_id].is_none() {
                return Err(Exception::new(ExceptionType::InvalidInput, "无效的预加载音频ID"));
            }
            let src = g.preloaded[preloaded_id].as_ref().unwrap();
            if src.data.is_empty() {
                return Err(Exception::new(ExceptionType::InvalidInput, "预加载音频数据为空"));
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

    /// Convenience wrapper: preload + add in one shot. `instance_id_out` and
    /// `ready_out` are updated asynchronously when the worker finishes.
    pub fn add_audio_file<P: AsRef<Path>>(
        &self,
        filename: P,
        volume: f32,
        instance_id_out: Option<&Arc<AtomicUsize>>,
        ready_out: Option<&Arc<AtomicBool>>,
    ) {
        let ready = ready_out.cloned();
        let inst_slot = instance_id_out.cloned();

        if let Some(r) = &ready { r.store(false, Ordering::SeqCst); }

        let preloaded_id = match self.preload_audio(filename, ready.as_ref()) {
            Ok(id) => id,
            Err(_) => {
                if let Some(r) = &ready { r.store(true, Ordering::SeqCst); }
                return;
            }
        };
        let vol = volume;
        std::thread::Builder::new()
            .name("yumo-addaudio".into())
            .spawn(move || {
                let pool = AudioPool::instance();
                let r = ready.clone().unwrap_or_else(|| Arc::new(AtomicBool::new(false)));
                while !r.load(Ordering::SeqCst) {
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                r.store(false, Ordering::SeqCst);
                let ok = {
                    let g = pool.inner.lock();
                    preloaded_id < g.preloaded.len()
                        && g.preloaded[preloaded_id].as_ref().map(|p| !p.data.is_empty()).unwrap_or(false)
                };
                if !ok {
                    r.store(true, Ordering::SeqCst);
                    return;
                }
                if let Ok(id) = pool.add_audio(preloaded_id, vol) {
                    if let Some(slot) = &inst_slot { slot.store(id, Ordering::SeqCst); }
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
    fn ensure_device_open(&self) {
        if self.playing.load(Ordering::SeqCst) { return; }
        // Ensure we only attempt to start once.
        if self.stream_started.swap(true, Ordering::SeqCst) {
            // Another caller is starting; wait briefly for it.
            for _ in 0..200 {
                if self.playing.load(Ordering::SeqCst) { return; }
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
                        if inst.active && inst.position < src.data.len() { return Some(*id); }
                    }
                    None
                })
                .collect();
            for id in needs_reset {
                if let Some(inst) = g.instances.get_mut(&id) { inst.position = 0; }
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
                let config = cpal::StreamConfig {
                    channels: TARGET_CHANNELS,
                    sample_rate: SampleRate(TARGET_SAMPLE_RATE),
                    buffer_size: cpal::BufferSize::Default,
                };
                let stream: Stream = match device.build_output_stream(
                    &config,
                    |out: &mut [i16], _info: &cpal::OutputCallbackInfo| {
                        AudioPool::instance().render_chunk(out);
                    },
                    |err| eprintln!("[yumo] 音频流错误: {err:?}"),
                    None,
                ) {
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
    fn render_chunk(&self, out: &mut [i16]) {
        let chunk = out.len();
        for x in out.iter_mut() { *x = 0; }

        // --- Phase 1: snapshot under lock ---
        struct Job {
            id: usize,
            position: usize,
            volume: f32,
            muted_inst: bool,
            src: Arc<PreloadedAudio>,
        }
        let global_muted;
        let global_stopped;
        let jobs: Vec<Job>;
        {
            let g = self.inner.lock();
            global_stopped = g.stopped;
            global_muted = g.muted;
            if global_stopped {
                return;
            }
            jobs = g
                .instances
                .iter()
                .filter_map(|(id, inst)| {
                    if !inst.active || inst.stopped { return None; }
                    let src = g.preloaded.get(inst.source).and_then(|x| x.as_ref())?.clone();
                    if inst.position >= src.data.len() { return None; }
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
        for job in &jobs {
            let data = &job.src.data;
            let remaining = data.len() - job.position;
            let take = chunk.min(remaining);
            let new_position = job.position + take;
            let finished = new_position >= data.len();
            updates.push((job.id, new_position, finished));

            if job.muted_inst || global_muted {
                continue;
            }
            for i in 0..take {
                let s = data[job.position + i] as i32;
                let v = (s as f32 * job.volume).round() as i32;
                mixed[i] = mixed[i].saturating_add(v);
            }
        }
        for (i, m) in mixed.iter().enumerate() {
            out[i] = (*m).clamp(i16::MIN as i32, i16::MAX as i32) as i16;
        }

        // --- Phase 3: commit position updates and clean up ---
        let any_active;
        {
            let mut g = self.inner.lock();
            for (id, new_pos, finished) in &updates {
                if let Some(inst) = g.instances.get_mut(id) {
                    inst.position = *new_pos;
                    if *finished { inst.active = false; }
                }
            }
            let mut referenced = std::collections::HashSet::new();
            for inst in g.instances.values() {
                if inst.active { referenced.insert(inst.source); }
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
        }
        // If nothing is active anymore, signal the audio thread to stop.
        if !any_active {
            self.playing.store(false, Ordering::SeqCst);
        }
    }

    // ---- Public query/control API ----

    pub fn remove_preloaded_audio(&self, preloaded_id: usize) {
        let mut g = self.inner.lock();
        if preloaded_id >= g.preloaded.len() { return; }
        let referenced = g.instances.values().any(|inst| inst.active && inst.source == preloaded_id);
        if !referenced {
            g.preloaded[preloaded_id] = None;
        } else if let Some(Some(p)) = g.preloaded.get(preloaded_id) {
            p.marked_for_removal.store(true, Ordering::SeqCst);
        }
    }

    pub fn get_preloaded_count(&self) -> usize { self.inner.lock().preloaded.len() }
    pub fn get_playing_count(&self) -> usize { self.inner.lock().instances.len() }

    pub fn is_playing(&self, instance_id: usize) -> bool {
        let g = self.inner.lock();
        match g.instances.get(&instance_id) {
            Some(inst) => {
                let len = g.preloaded.get(inst.source).and_then(|x| x.as_ref()).map(|p| p.data.len()).unwrap_or(0);
                inst.active && inst.position < len
            }
            None => false,
        }
    }

    pub fn stop_all(&self) { self.inner.lock().stopped = true; }
    pub fn resume(&self) { self.inner.lock().stopped = false; }
    pub fn set_global_mute(&self, muted: bool) { self.inner.lock().muted = muted; }

    pub fn reset_all(&self) {
        let mut g = self.inner.lock();
        for inst in g.instances.values_mut() { inst.position = 0; }
    }

    pub fn set_volume(&self, instance_id: usize, volume: f32) -> Result<()> {
        let mut g = self.inner.lock();
        let inst = g.instances.get_mut(&instance_id)
            .ok_or_else(|| Exception::new(ExceptionType::InvalidInput, "无效的播放实例ID"))?;
        inst.volume = volume.clamp(0.0, 1.0);
        Ok(())
    }

    pub fn get_volume(&self, instance_id: usize) -> Result<f32> {
        let g = self.inner.lock();
        let inst = g.instances.get(&instance_id)
            .ok_or_else(|| Exception::new(ExceptionType::InvalidInput, "无效的播放实例ID"))?;
        Ok(inst.volume)
    }

    pub fn stop(&self, instance_id: usize) -> bool {
        let mut g = self.inner.lock();
        match g.instances.get_mut(&instance_id) {
            Some(inst) => { inst.stopped = true; true }
            None => false,
        }
    }

    pub fn resume_instance(&self, instance_id: usize) -> bool {
        let mut g = self.inner.lock();
        match g.instances.get_mut(&instance_id) {
            Some(inst) => { inst.stopped = false; true }
            None => false,
        }
    }

    pub fn set_muted(&self, instance_id: usize, muted: bool) -> bool {
        let mut g = self.inner.lock();
        match g.instances.get_mut(&instance_id) {
            Some(inst) => { inst.muted = muted; true }
            None => false,
        }
    }

    pub fn remove(&self, instance_id: usize) -> bool {
        let mut g = self.inner.lock();
        if g.instances.remove(&instance_id).is_some() {
            g.free_ids.push_back(instance_id);
            true
        } else {
            false
        }
    }
}

impl Default for AudioPool {
    fn default() -> Self { Self::new() }
}

// Keep `AUDIO_CHUNK_SIZE` referenced for parity with the C++ source.
const _: usize = AUDIO_CHUNK_SIZE;
