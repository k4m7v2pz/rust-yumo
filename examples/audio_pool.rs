//! Mirrors `test/3_audio_pool_test.cpp` — `add_audio_file` convenience API,
//! rewritten against the upstream v0.2.0 free-function layer.
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use yumo::{add_audio_file, register_playback_finished_callback, AudioInstance, ReadySign};

fn main() {
    println!("=== Yumo Audio addAudio 字符串版本测试 (Rust) ===");

    // 注册播放完成回调（上游 v0.2.0 新接口）
    register_playback_finished_callback(|instance_id| {
        println!("[回调] 播放实例 {} 已结束并被回收", instance_id);
    });

    let files = [
        "test/audio/test.wav",
        "test/audio/test2.wav",
        "test/audio/test3.wav",
    ];
    println!("\n使用 add_audio_file 便利接口（异步加载播放）...");
    let mut ready_flags: Vec<Arc<ReadySign>> = Vec::new();
    let mut inst_slots: Vec<Arc<Mutex<Option<AudioInstance>>>> = Vec::new();
    for f in files.iter() {
        println!("\n添加音频: {}", f);
        let r = Arc::new(ReadySign::new(false));
        let s = Arc::new(Mutex::new(None));
        add_audio_file(f, 1.0, Some(&s), Some(&r));
        println!("  -> 后台加载播放，instance 将在播放开始后写入");
        ready_flags.push(r);
        inst_slots.push(s);
    }
    println!("\n等待所有音频加载完成...");
    for (i, r) in ready_flags.iter().enumerate() {
        while !r.load(Ordering::SeqCst) {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let inst = inst_slots[i].lock().unwrap().clone();
        println!(
            "  音频 {} 加载完成，instanceId={:?}",
            i,
            inst.map(|x| x.id())
        );
    }
    println!("\n所有音频已提交加载！");
    println!("预加载音频数: {}", yumo::get_preloaded_count());
    println!("\n按 Enter 键停止");
    let mut buf = String::new();
    std::io::stdin().read_line(&mut buf).ok();
    yumo::global.set_stop(true);
    println!("已停止所有播放");
    println!("\n=== 测试完成 ===");
}
