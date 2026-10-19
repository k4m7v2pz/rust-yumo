//! Mirrors upstream `test/8_mp3_test.cpp` — MP3 format support.
//!
//! MP3 decoding is done with the pure-Rust `minimp3` crate (upstream uses the
//! Windows ACM decoder); the decoded PCM goes through the same resample/mix
//! pipeline as WAV files.
use std::sync::atomic::Ordering;
use std::sync::Arc;

use yumo::{add_audio, preload_audio, ReadySign};

fn main() {
    println!("=== Yumo Audio MP3 测试 (Rust) ===");
    let files = [
        "test/audio/test1.mp3",
        "test/audio/test2.mp3",
        "test/audio/test3.mp3",
    ];
    let mut ids = [0usize; 3];
    let flags: [Arc<ReadySign>; 3] = [
        Arc::new(ReadySign::new(false)),
        Arc::new(ReadySign::new(false)),
        Arc::new(ReadySign::new(false)),
    ];
    for (i, f) in files.iter().enumerate() {
        println!("预加载 MP3: {}", f);
        ids[i] = preload_audio(f, Some(&flags[i])).expect("preload");
        println!("  -> 预加载ID: {}", ids[i]);
    }
    for (i, r) in flags.iter().enumerate() {
        while !r.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        println!("  MP3 {} 加载完成 (id={})", i, ids[i]);
    }
    println!("\n开始播放 3 个 MP3 文件（混合）...");
    for id in ids.iter() {
        match add_audio(*id, 1.0) {
            Ok(inst) => println!("  播放实例: {}", inst.id()),
            Err(e) => println!("  添加失败: {e}"),
        }
    }
    println!("\n按 Enter 键停止");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();
    yumo::global.set_stop(true);
    println!("已停止所有播放");
    println!("\n=== 测试完成 ===");
}
