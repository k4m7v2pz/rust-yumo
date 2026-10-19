# Yumo Audio 音频混合播放库（Rust）

## 项目概述

这是一个跨平台（Windows / Linux / macOS）的音频混合播放库，本仓库为原始 C++/Win32 版本的 Rust 重写，API 跟随上游 `keyBonk-org/audio-player` 演进至 v0.2.0。

### 核心定位

- **同时播放**：多个音频可以同时播放并混合
- **灵活控制**：支持预加载、动态添加播放、实例级/全局级音量、静音、暂停
- **多格式**：WAV + MP3

### 设计目标

- `add_audio` 添加音频后立即播放
- 提供 `preload_audio` 接口允许预先加载标准音频对象
- 全局状态通过 `yumo::global`（`AudioSign`）控制（mute / stop / volume）
- 允许同一音频的重复播放（通过播放实例追踪独立位置）
- 播放实例通过 `AudioInstance` 句柄直接操作，播放完成经回调通知（锁外调用）

## 依赖库

| 库 | 说明 |
|----|------|
| cpal 0.15 | 跨平台音频输出 |
| rubato 0.15 | 纯 Rust 重采样 |
| minimp3 0.5 | 纯 Rust MP3 解码 |
| parking_lot 0.12 | 互斥锁 |
| once_cell 1.19 | 单例 |

### 编译命令

```sh
cargo build          # 构建库
cargo build --release
cargo test           # 运行测试
cargo run --example simple   # 运行示例
```

## API 接口

公共 API 为**自由函数层 + `AudioInstance` 句柄 + 全局信号**（`AudioPool` 单例已隐藏为内部引擎）。

### 全局加载 / 播放

| 函数 | 说明 |
|------|------|
| `preload_audio(filename, ready)` | 异步预加载 WAV/MP3，返回预加载 ID |
| `add_audio(preloaded_id, volume)` | 添加预加载音频并立即播放，返回 `AudioInstance` |
| `add_audio_file(filename, volume, instance_out, ready_out)` | 便利接口：异步加载 + 播放 |
| `regain(instance_id)` | 按 ID 重新获取播放实例（不存在返回 `None`） |
| `remove_preloaded_audio(preloaded_id)` | 移除预加载音频对象 |
| `get_preloaded_count()` / `get_playing_count()` | 预加载数量 / 播放实例数量 |
| `reset_all()` / `remove(instance_id)` | 重置全部位置 / 移除指定实例 |
| `register_playback_finished_callback(cb)` / `unregister_playback_finished_callback()` | 播放完成回调（锁外调用） |

### `AudioInstance` 方法

`id()` / `is_playing()` / `active()` / `position()` / `set_position(n)` / `volume()` / `set_volume(v)` / `stopped()` / `set_stopped(b)` / `muted()` / `set_muted(b)` / `remove()`

### 全局信号 `yumo::global`（`AudioSign`）

`set_mute(b)` / `muted()` / `set_stop(b)` / `stopped()` / `set_volume(v)` / `volume()`

## 数据结构

### PreloadedAudio

存储共享的、已重采样的音频数据，同一个预加载音频可创建多个播放实例。

```rust
pub struct PreloadedAudio {
    pub data: StandardWavInfo,           // 44.1kHz 双声道 16bit 交错 PCM
    pub marked_for_removal: AtomicBool,
}
```

### AudioInstance

轻量实例句柄（只含 ID），通过 `add_audio` / `regain` 获得；实例被回收后对其操作返回 `InvalidID`。

### StandardWavInfo

标准音频格式：`Vec<i16>` — 44.1kHz, 双声道, 16-bit 交错 PCM。

## 核心实现

### 音频格式

| 参数 | 值 |
|------|-----|
| 采样率 | 44.1kHz |
| 声道数 | 双声道 |
| 位深度 | 16位 |

### 音频输出线程（cpal）

由于 cpal 的 `Stream` 在部分平台（如 macOS CoreAudio）上不是 `Send`/`Sync`，把 `Stream` 交给专用音频线程持有；池主状态（`PoolInner`）用 `parking_lot::Mutex` 保护。输出流按设备支持自适应采样格式（f32/i16/u16/f64）与声道数；44.1kHz 不可用时做线性插值重采样。

### 混音回调（四阶段）

1. 加锁快照（全局状态 + 实例数据）
2. 无锁混音（含全局音量 × 实例音量，i16 饱和相加）
3. 加锁提交（推进位置、回收完成实例、清理待删除预加载对象）
4. **锁外通知**播放完成回调（对齐上游死锁修复 `38d5433`）

### 异步加载

`preload_audio` 后台线程按扩展名分派（`.mp3/.mp2/.mp1` → minimp3 解码，其余 → WAV 解析），经 `rubato` 重采样到标准格式，通过 `Arc<ReadySign>` 标记就绪。

## 使用示例

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
inst.set_volume(0.8).unwrap();
yumo::global.set_volume(0.6);
yumo::global.set_stop(true);   // 暂停
yumo::global.set_stop(false);  // 恢复
```

## 注意事项

1. **路径问题**：示例使用相对路径（`test/audio/*.wav|*.mp3`），需从 crate 根目录运行 `cargo run --example xxx`
2. **音频播放完毕**：实例播完后自动回收（ID 可复用），通过播放完成回调获知
3. **设备状态**：实例全部播完后音频线程自动停止并释放流，再次 `add_audio` 会重新打开
4. **内存管理**：音频数据由库管理，调用者无需释放
5. **无效 ID**：实例被回收后继续操作返回 `ExceptionType::InvalidID`

## 测试

```sh
cargo test
```

| 测试文件 | 说明 |
|------|------|
| `tests/wav_and_resample.rs` | WAV 解析 / 重采样 / 格式转换（9 项） |
| `tests/mp3_decode.rs` | MP3 解码与标准格式转换（2 项，嵌入测试素材） |

## 文件结构

```
rust-yumo/
├── Cargo.toml
├── src/
│   ├── lib.rs            # 库入口与常量
│   ├── error.rs          # Exception / ExceptionType
│   ├── wav.rs            # WAV 解析
│   ├── mp3.rs            # MP3 解码（minimp3）
│   ├── resample.rs       # 重采样与格式转换
│   └── pool.rs           # 音频池引擎 + 公共 API
├── examples/
│   ├── simple.rs         # 单音频播放（对应上游 test/5）
│   ├── audio_pool.rs     # 便利接口测试（对应上游 test/3）
│   ├── audio_mix.rs      # 混合播放测试（对应上游 test/4）
│   ├── play_control.rs   # 播放控制测试（对应上游 test/6）
│   └── mp3.rs            # MP3 播放测试（对应上游 test/8）
├── tests/
├── test/audio/           # 测试音频文件（wav + mp3）
├── README.md
└── agent.md
```

## AI开发须知

每次修改完库源码，运行 `cargo build --examples` 与 `cargo test` 确认全部通过后再提交；示例需要真实音频设备，无设备环境下只验证库与单元测试。
