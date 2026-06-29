//! Mirrors `test/6_play_control_test.cpp` — stop / resume / mute / volume.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use yumo::AudioPool;

fn main() {
    println!("=== Yumo Audio 播放控制测试 (Rust) ===");
    let pool = AudioPool::instance();
    let files = [
        "test/audio/test.wav",
        "test/audio/test2.wav",
        "test/audio/test3.wav",
    ];
    let mut ids = [0usize; 3];
    let flags: [Arc<AtomicBool>; 3] = [Arc::new(AtomicBool::new(false)),
                                       Arc::new(AtomicBool::new(false)),
                                       Arc::new(AtomicBool::new(false))];
    for (i, f) in files.iter().enumerate() {
        ids[i] = pool.preload_audio(f, Some(&flags[i])).expect("preload");
    }
    for r in flags.iter() {
        while !r.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
    }
    let mut instances = [0usize; 3];
    for (i, id) in ids.iter().enumerate() {
        instances[i] = pool.add_audio(*id, 1.0).expect("add");
    }
    println!("播放实例: {:?}", instances);
    println!("3 秒后停止实例 0...");
    std::thread::sleep(std::time::Duration::from_secs(3));
    pool.stop(instances[0]);
    println!("已停止实例 {}", instances[0]);
    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("恢复实例 0");
    pool.resume_instance(instances[0]);
    println!("2 秒后静音实例 1...");
    std::thread::sleep(std::time::Duration::from_secs(2));
    pool.set_muted(instances[1], true);
    println!("已静音实例 {}", instances[1]);
    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("设置实例 2 音量到 0.3");
    let _ = pool.set_volume(instances[2], 0.3);
    std::thread::sleep(std::time::Duration::from_secs(2));
    println!("全局停止");
    pool.stop_all();
    println!("按 Enter 键退出");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();
    pool.resume();
    println!("\n=== 测试完成 ===");
}
