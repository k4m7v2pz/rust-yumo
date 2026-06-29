# Yumo Audio (Rust)

Yumo Audio 是一个音频混合播放库，本仓库为其 Rust 跨平台重写版本。

原始 C++/Win32 版本仅支持 Windows（依赖 `waveOut*` 与 Windows ACM）。本 Rust 版本通过 [`cpal`](https://crates.io/crates/cpal)（音频输出）和 [`rubato`](https://crates.io/crates/rubato)（重采样）实现，**支持 Windows / Linux / macOS**。

> 90% 由 vibe coding 生成，请谨慎使用。

## 一. 兼容性

### 1. 平台

| 平台 | 是否支持 | 音频后端 |
|:----:|:--------:|:--------:|
|Windows|支持|WASAPI (via cpal)|
|Linux  |支持|ALSA / PulseAudio / JACK (via cpal)|
|macOS  |支持|CoreAudio (via cpal)|

### 2. 文件格式

（格式按扩展名记）

| 格式 | 是否支持 | 支持程度 |
|:------:|:---------:|:----------:|
|WAV   |支持     |仅 PCM；8/16/24/32-bit，单声道/立体声；携带扩展信息的 fmt 块会被跳过扩展部分|
|PCM   |尚未支持 |-|
|MP3 / MP4 / M4A / OGG / FLAC / WMA |不支持|-|

### 3. 是否兼容 C 语言

不兼容。Rust 库对外暴露 idiomatic Rust API（`Result`、`Arc` 等）。

## 二. 技术栈

- Rust（edition 2021）
- [`cpal`](https://crates.io/crates/cpal) 0.15 — 跨平台音频输出
- [`rubato`](https://crates.io/crates/rubato) 0.15 — 高质量纯 Rust 重采样
- [`parking_lot`](https://crates.io/crates/parking_lot) — 互斥锁
- [`once_cell`](https://crates.io/crates/once_cell) — 单例

## 三. 构建

```sh
cargo build --release
```

运行测试：

```sh
cargo test
```

运行示例（见 `examples/`）：

```sh
cargo run --example simple
cargo run --example audio_pool
cargo run --example audio_mix
cargo run --example play_control
```

> 示例中的音频文件路径默认为 `test/audio/*.wav`（相对当前工作目录）。

## 四. API 接口

所有接口都通过单例 `AudioPool::instance()` 访问，且线程安全。

### 核心接口

| 接口 | 说明 |
|------|------|
|`AudioPool::instance()`|获取单例实例|
|`preload_audio(filename, ready)`|异步预加载 WAV 文件，返回预加载 ID|
|`add_audio(preloaded_id, volume)`|添加预加载音频并立即播放，返回播放实例 ID|
|`add_audio_file(filename, volume, instance_id_out, ready_out)`|便利接口：异步加载 + 播放，播放完成后自动清理预加载对象|
|`stop_all()`|停止所有播放（挂起，位置不推进）|
|`resume()`|恢复播放|

### 查询接口

| 接口 | 说明 |
|------|------|
|`get_preloaded_count()`|预加载音频数量|
|`get_playing_count()`|当前播放实例数量|
|`is_playing(instance_id)`|检查播放实例是否正在播放|

### 控制接口

| 接口 | 说明 |
|------|------|
|`set_volume(instance_id, volume)`|设置播放实例音量（0.0–1.0）|
|`get_volume(instance_id)`|获取播放实例音量|
|`set_global_mute(muted)`|全局静音（位置继续推进，跳过混音）|
|`reset_all()`|重置所有播放实例位置到开头|
|`stop(instance_id)`|停止指定实例（位置不推进）|
|`resume_instance(instance_id)`|恢复指定实例|
|`set_muted(instance_id, muted)`|设置指定实例静音|
|`remove(instance_id)`|移除播放实例|
|`remove_preloaded_audio(preloaded_id)`|移除预加载音频对象|

错误以 `yumo::Exception`（含 `ExceptionType` 类别和消息）形式经 `Result` 返回。

## 五. 数据结构

### `PreloadedAudio`

存储共享的、已重采样的音频数据，可被多个播放实例引用。

### `PlayInstance`

每次 `add_audio` 创建一个，独立追踪播放位置、音量、停止/静音状态。

### `StandardWavInfo`

标准音频格式：`Vec<i16>` — 44.1kHz、双声道、16-bit 交错 PCM。

## 六. 核心实现

### 音频格式

| 参数 | 值 |
|------|-----|
|采样率|44.1kHz|
|声道数|双声道|
|位深度|16-bit|
|回调块|由 cpal 设备决定（约几十毫秒）|

### 跨平台音频线程

由于 cpal 的 `Stream` 类型在部分平台（如 macOS CoreAudio）上不是 `Send`/`Sync`，本库把 `Stream` 交给一个专用的音频线程持有；池主状态（`PoolInner`）用 `parking_lot::Mutex` 保护，由客户端线程与音频回调线程共享。这使 `AudioPool` 本身保持 `Send + Sync`。

### 异步加载

`preload_audio` 在后台线程加载 WAV、用 `rubato` 重采样到标准格式，通过调用者传入的 `Arc<AtomicBool>` 标记就绪状态。

### 重采样

使用 `rubato` 的 `SincFixedIn`（Blackman-Harris 窗、线性插值）将任意 PCM WAV 转换到 44.1kHz 立体声 16-bit。位深度转换（8/16/24/32 → f64 → i16）与声道转换（mono↔stereo）手写实现。

### 线程安全

- `parking_lot::Mutex` 保护共享状态；
- `Arc<AtomicBool>` / `Arc<AtomicUsize>` 用于跨线程状态/ID 同步；
- 所有公开接口都线程安全；
- 混音回调分三阶段：加锁快照 → 无锁混音 → 加锁提交，避免长时间持锁。

## 七. 使用示例

### 简化用法（快速播放）

```rust
use yumo::AudioPool;

let pool = AudioPool::instance();
pool.add_audio_file("test.wav", 1.0, None, None);
pool.add_audio_file("test2.wav", 1.0, None, None);
// ... 等待播放 ...
```

### 标准用法（预加载 + 控制）

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use yumo::AudioPool;

let pool = AudioPool::instance();
let ready = Arc::new(AtomicBool::new(false));
let id = pool.preload_audio("test.wav", Some(&ready)).unwrap();
while !ready.load(Ordering::SeqCst) {
    std::thread::sleep(std::time::Duration::from_millis(10));
}
let inst = pool.add_audio(id, 0.5).unwrap();
pool.stop_all();
pool.resume();
```

## 八. 文件结构

```
rust-yumo/
├── Cargo.toml
├── src/
│   ├── lib.rs            # 库入口与常量
│   ├── error.rs          # Exception / ExceptionType
│   ├── wav.rs            # WAV 解析
│   ├── resample.rs       # 重采样与格式转换
│   └── pool.rs           # AudioPool 核心
├── examples/
│   ├── simple.rs         # 单音频播放（对应原 test/5）
│   ├── audio_pool.rs     # 便利接口测试（对应原 test/3）
│   ├── audio_mix.rs      # 混合播放测试（对应原 test/4）
│   └── play_control.rs   # 播放控制测试（对应原 test/6）
├── tests/
│   └── wav_and_resample.rs  # WAV 解析 / 重采样单元测试
├── test/audio/           # 测试音频文件
├── README.md
└── agent.md
```

## 九. 许可

MIT（沿用原仓库）。
