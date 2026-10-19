//! Mirrors `test/6_play_control_test.cpp` — stop / resume / mute / volume,
//! rewritten against the upstream v0.2.0 `AudioInstance` + `global` API.
use std::sync::atomic::Ordering;
use std::sync::Arc;

use yumo::{add_audio, preload_audio, ReadySign};

fn main() {
    println!("=== Yumo Audio 播放控制测试 (Rust) ===");
    let files = [
        "test/audio/test.wav",
        "test/audio/test2.wav",
        "test/audio/test3.wav",
    ];
    let mut ids = [0usize; 3];
    let flags: [Arc<ReadySign>; 3] = [
        Arc::new(ReadySign::new(false)),
        Arc::new(ReadySign::new(false)),
        Arc::new(ReadySign::new(false)),
    ];
    for (i, f) in files.iter().enumerate() {
        ids[i] = preload_audio(f, Some(&flags[i])).expect("preload");
    }
    for r in flags.iter() {
        while !r.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let mut instances = Vec::new();
    for id in ids.iter() {
        instances.push(add_audio(*id, 1.0).expect("add"));
    }
    let ids: Vec<usize> = instances.iter().map(|i| i.id()).collect();
    println!("播放实例: {:?}", ids);

    println!("3 秒后停止实例 0...");
    std::thread::sleep(std::time::Duration::from_secs(3));
    instances[0].set_stopped(true).unwrap();
    println!("已停止实例 {}", instances[0].id());

    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("恢复实例 0");
    instances[0].set_stopped(false).unwrap();

    println!("2 秒后静音实例 1...");
    std::thread::sleep(std::time::Duration::from_secs(2));
    instances[1].set_muted(true).unwrap();
    println!("已静音实例 {}", instances[1].id());

    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("设置实例 2 音量到 0.3");
    instances[2].set_volume(0.3).unwrap();

    println!("2 秒后设置全局音量为 0.5");
    std::thread::sleep(std::time::Duration::from_secs(2));
    yumo::global.set_volume(0.5);

    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("全局停止（挂起）");
    yumo::global.set_stop(true);

    println!("按 Enter 键退出");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();
    yumo::global.set_stop(false);
    println!("\n=== 测试完成 ===");
}
