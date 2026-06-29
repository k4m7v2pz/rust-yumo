//! Mirrors `test/3_audio_pool_test.cpp` — add_audio_file convenience API.
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use yumo::AudioPool;

fn main() {
    println!("=== Yumo Audio addAudio 字符串版本测试 (Rust) ===");
    let pool = AudioPool::instance();
    let files = [
        "test/audio/test.wav",
        "test/audio/test2.wav",
        "test/audio/test3.wav",
    ];
    println!("\n使用 add_audio_file 便利接口（异步加载播放）...");
    let mut ready_flags: Vec<Arc<AtomicBool>> = Vec::new();
    let mut inst_slots: Vec<Arc<AtomicUsize>> = Vec::new();
    for (i, f) in files.iter().enumerate() {
        println!("\n添加音频: {}", f);
        let r = Arc::new(AtomicBool::new(false));
        let s = Arc::new(AtomicUsize::new(usize::MAX));
        pool.add_audio_file(f, 1.0, Some(&s), Some(&r));
        println!("  -> 后台加载播放，instanceId 将在播放开始后写入");
        ready_flags.push(r);
        inst_slots.push(s);
        let _ = i;
    }
    println!("\n等待所有音频加载完成...");
    for (i, r) in ready_flags.iter().enumerate() {
        while !r.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        println!("  音频 {} 加载完成，instanceId={}", i, inst_slots[i].load(Ordering::SeqCst));
    }
    println!("\n所有音频已提交加载！");
    println!("预加载音频数: {}", pool.get_preloaded_count());
    println!("\n按 Enter 键停止");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();
    pool.stop_all();
    println!("已停止所有播放");
    println!("\n=== 测试完成 ===");
}
