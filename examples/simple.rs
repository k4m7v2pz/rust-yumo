//! Mirrors `test/5_simple_test.cpp` — single audio playback.
use std::sync::atomic::Ordering;
use std::sync::Arc;

use yumo::{add_audio, preload_audio, ReadySign};

fn main() {
    println!("=== Yumo Audio 简单测试 (Rust) ===");
    let cwd = std::env::current_dir().unwrap();
    println!("当前工作目录: {}", cwd.display());

    let filename = std::path::Path::new("test/audio/test.wav");
    println!("\n添加音频: {}", filename.display());

    let ready = Arc::new(ReadySign::new(false));
    match preload_audio(filename, Some(&ready)) {
        Ok(id) => {
            println!("预加载ID: {}", id);
            println!("等待加载...");
            while !ready.load(Ordering::SeqCst) {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            println!("加载完成！");
            println!("添加播放...");
            match add_audio(id, 1.0) {
                Ok(inst) => {
                    println!("播放实例ID: {}", inst.id());
                    println!("正在播放: {}", inst.is_playing());
                    println!("播放实例数: {}", yumo::get_playing_count());
                    println!("\n播放中...按 Enter 键停止");
                    let mut buf = String::new();
                    std::io::stdin().read_line(&mut buf).ok();
                    yumo::global.set_stop(true);
                    println!("已停止播放");
                }
                Err(e) => println!("添加播放失败: {e}"),
            }
        }
        Err(e) => println!("预加载失败: {e}"),
    }
    println!("\n=== 测试完成 ===");
}
