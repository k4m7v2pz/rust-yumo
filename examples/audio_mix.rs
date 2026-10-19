//! Mirrors `test/4_audio_mix_test.cpp` — preload 3 files, add one every 5s.
use std::sync::atomic::Ordering;
use std::sync::Arc;

use yumo::{add_audio, preload_audio, ReadySign};

fn main() {
    println!("=== Yumo Audio 混合播放测试 (Rust) ===");
    let files = [
        "test/audio/test.wav",
        "test/audio/test2.wav",
        "test/audio/test3.wav",
    ];
    println!("\n异步预加载音频文件...");
    let mut ids = [0usize; 3];
    let flags: [Arc<ReadySign>; 3] = [
        Arc::new(ReadySign::new(false)),
        Arc::new(ReadySign::new(false)),
        Arc::new(ReadySign::new(false)),
    ];
    for (i, f) in files.iter().enumerate() {
        println!("  预加载: {} (后台加载)", f);
        ids[i] = preload_audio(f, Some(&flags[i])).expect("preload");
        println!("  -> 预加载ID: {}", ids[i]);
    }
    println!("\n等待所有预加载完成...");
    for (i, r) in flags.iter().enumerate() {
        while !r.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        println!("  音频 {} 加载完成", i);
    }
    println!("\n预加载完成，共 {} 个音频", yumo::get_preloaded_count());
    println!("每5秒添加一个音频进行播放...");
    for i in 0..3 {
        println!("\n[第 {} 次] 等待5秒...", i + 1);
        std::thread::sleep(std::time::Duration::from_secs(5));
        println!("[第 {} 次] 添加预加载音频 ID={} 到播放池", i + 1, ids[i]);
        match add_audio(ids[i], 1.0) {
            Ok(inst) => {
                println!("  -> 播放实例ID: {}", inst.id());
                println!("  当前播放实例数: {}", yumo::get_playing_count());
            }
            Err(e) => println!("  -> 添加失败: {e}"),
        }
    }
    println!("\n所有音频已开始播放！");
    println!("按 Enter 键停止所有播放...");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();
    yumo::global.set_stop(true);
    println!("已停止所有播放");
    println!("\n=== 测试完成 ===");
}
