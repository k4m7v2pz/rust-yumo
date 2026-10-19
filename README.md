# Yumo Audio (Rust)

本仓库是一个**用于体验 AI 编程**的跨语言重写项目：把上游 C++/Win32 版 [`keyBonk-org/audio-player`](https://github.com/keyBonk-org/audio-player)（WTFPL）重写为 Rust。

- 重写目的仅为体验 AI 生成/重构代码与提交过程，不是真实的库交付
- **未进行测试**：仅编写了少量用于验证编译与管线的基础单元测试，未经充分验证
- 作者私下也不使用它
- 上游仍在持续更新，本重写仅在特定时点对齐上游 v0.2.0 接口，未长期跟进

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
|MP3   |支持     |支持大多数 MP3 文件（经 minimp3 解码）|
|PCM   |尚未支持 |-|
|MP4 / M4A / OGG / FLAC / WMA |不支持|-|

### 3. 是否兼容 C 语言

不兼容。Rust 库对外暴露 idiomatic Rust API（`Result`、`Arc` 等）。

## 二. 技术栈

- Rust（edition 2021）
- [`cpal`](https://crates.io/crates/cpal) 0.15 — 跨平台音频输出
- [`rubato`](https://crates.io/crates/rubato) 0.15 — 高质量纯 Rust 重采样
- [`minimp3`](https://crates.io/crates/minimp3) 0.5 — 纯 Rust MP3 解码
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
cargo run --example mp3
```

> 示例中的音频文件路径默认为 `test/audio/*.wav|*.mp3`（相对当前工作目录）。

## 四. API 接口

公共 API 跟随上游 v0.2.0 重构：`AudioPool` 单例被隐藏，改为**自由函数层 + `AudioInstance` 句柄 + 全局信号 `global`**。所有接口线程安全。

### 全局加载 / 播放

| 函数 | 说明 |
|------|------|
|`preload_audio(filename, ready)`|异步预加载 WAV/MP3 文件，返回预加载 ID|
|`add_audio(preloaded_id, volume)`|添加预加载音频并立即播放，返回 `AudioInstance`|
|`add_audio_file(filename, volume, instance_out, ready_out)`|便利接口：异步加载 + 播放，播放完成后自动清理预加载对象|
|`regain(instance_id)`|按 ID 重新获取播放实例（不存在返回 `None`）|
|`remove_preloaded_audio(preloaded_id)`|移除预加载音频对象|
|`get_preloaded_count()` / `get_playing_count()`|预加载数量 / 播放实例数量|
|`reset_all()` / `remove(instance_id)`|重置全部位置 / 移除指定实例|

### `AudioInstance`（播放实例句柄）

对应上游 `audioInstance` 代理类，直接操作实例状态：

| 方法 | 说明 |
|------|------|
|`id()`|实例 ID|
|`is_playing()` / `active()`|是否正在播放 / 是否激活|
|`position()` / `set_position(n)`|播放位置（采样点）|
|`volume()` / `set_volume(v)`|音量，写时夹紧到 [0.0, 1.0]|
|`stopped()` / `set_stopped(b)`|暂停（位置不推进）|
|`muted()` / `set_muted(b)`|静音（跳过混音，位置继续推进）|
|`remove()`|从播放池移除实例|

### 全局信号 `yumo::global`（`AudioSign`）

对应上游 `yumo::global`，控制全局状态：

| 方法 | 说明 |
|------|------|
|`set_mute(b)` / `muted()`|全局静音|
|`set_stop(b)` / `stopped()`|全局停止（挂起，位置不推进）|
|`set_volume(v)` / `volume()`|全局音量，夹紧到 [0.0, 1.0]|

### 播放完成回调

| 函数 | 说明 |
|------|------|
|`register_playback_finished_callback(cb)`|注册回调，播放实例被回收时以实例 ID 调用（**锁外调用**，对齐上游死锁修复）|
|`unregister_playback_finished_callback()`|注销回调|

错误以 `yumo::Exception`（含 `ExceptionType` 类别和消息）形式经 `Result` 返回；无效 ID 返回 `ExceptionType::InvalidID`。

## 五. 数据结构

### `PreloadedAudio`

存储共享的、已重采样的音频数据，可被多个播放实例引用。

### `AudioInstance`

轻量实例句柄（只含 ID），通过自由函数或 `regain` 获得；实例被回收后对其操作返回 `InvalidID`。

### `AudioSign`

全局控制信号（`mute` / `stop` / `volume`），内部为无锁原子变量。

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

`preload_audio` 在后台线程加载 WAV/MP3（按扩展名分派：`.mp3/.mp2/.mp1` 走 minimp3 解码，其余按 WAV 解析）、用 `rubato` 重采样到标准格式，通过调用者传入的 `Arc<AtomicBool>`（`ReadySign`）标记就绪状态。

### 重采样

使用 `rubato` 的 `SincFixedIn`（Blackman-Harris 窗、线性插值）将任意 PCM（WAV 或 MP3 解码结果）转换到 44.1kHz 立体声 16-bit。位深度转换（8/16/24/32 → f64 → i16）与声道转换（mono↔stereo）手写实现。

### 线程安全

- `parking_lot::Mutex` 保护共享状态；
- 全局信号 `yumo::global` 使用无锁原子变量；
- 所有公开接口都线程安全；
- 混音回调分四阶段：加锁快照 → 无锁混音 → 加锁提交/回收 → **锁外通知播放完成回调**（对齐上游死锁修复 `38d5433`），避免用户回调内调用库函数时死锁。

## 七. 使用示例

### 简化用法（快速播放）

```rust
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use yumo::{add_audio_file, AudioInstance, ReadySign};

let ready = Arc::new(ReadySign::new(false));
let inst: Arc<Mutex<Option<AudioInstance>>> = Arc::new(Mutex::new(None));
add_audio_file("test.wav", 1.0, Some(&inst), Some(&ready));
while !ready.load(Ordering::SeqCst) {
    std::thread::sleep(std::time::Duration::from_millis(10));
}
// inst.lock() 拿到播放实例，可直接控制
```

### 标准用法（预加载 + 控制）

```rust
use std::sync::atomic::Ordering;
use std::sync::Arc;
use yumo::{add_audio, preload_audio, ReadySign};

let ready = Arc::new(ReadySign::new(false));
let id = preload_audio("test.mp3", Some(&ready)).unwrap();
while !ready.load(Ordering::SeqCst) {
    std::thread::sleep(std::time::Duration::from_millis(10));
}
let inst = add_audio(id, 0.5).unwrap();
inst.set_volume(0.8).unwrap();   // 实例音量
yumo::global.set_volume(0.6);    // 全局音量
yumo::global.set_stop(true);     // 全局暂停
yumo::global.set_stop(false);    // 恢复
```

### 播放完成回调（锁外调用）

```rust
use yumo::register_playback_finished_callback;

register_playback_finished_callback(|instance_id| {
    println!("实例 {instance_id} 播放完成并被回收");
});
```

## 八. 文件结构

```
rust-yumo/
├── Cargo.toml
├── src/
│   ├── lib.rs            # 库入口与常量
│   ├── error.rs          # Exception / ExceptionType
│   ├── wav.rs            # WAV 解析
│   ├── mp3.rs            # MP3 解码（minimp3）
│   ├── resample.rs       # 重采样与格式转换
│   └── pool.rs           # 音频池引擎 + 公共 API（AudioInstance / global / 回调）
├── examples/
│   ├── simple.rs         # 单音频播放（对应原 test/5）
│   ├── audio_pool.rs     # 便利接口测试（对应原 test/3）
│   ├── audio_mix.rs      # 混合播放测试（对应原 test/4）
│   ├── play_control.rs   # 播放控制测试（对应原 test/6）
│   └── mp3.rs            # MP3 播放测试（对应上游 test/8）
├── tests/
│   ├── wav_and_resample.rs  # WAV 解析 / 重采样单元测试
│   └── mp3_decode.rs        # MP3 解码单元测试
├── test/audio/           # 测试音频文件（wav + mp3）
├── README.md
└── AGENTS.md
```

## 九. 许可

WTFPL（与上游 [`keyBonk-org/audio-player`](https://github.com/keyBonk-org/audio-player) 一致，见 `LICENSE`）。
