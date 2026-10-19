//! MP3 decoding tests (no audio device required).
//!
//! The MP3 fixtures are the same three files used by the upstream test suite
//! (`test/8_mp3_test.cpp`), embedded via `include_bytes!` so the tests run
//! anywhere.

/// test1.mp3 (14.7 KB, shortest)
const TEST1_MP3: &[u8] = include_bytes!("../test/audio/test1.mp3");
/// test2.mp3 (21.1 KB)
const TEST2_MP3: &[u8] = include_bytes!("../test/audio/test2.mp3");
/// test3.mp3 (5.6 KB)
const TEST3_MP3: &[u8] = include_bytes!("../test/audio/test3.mp3");

#[test]
fn decodes_mp3_to_pcm() {
    for (name, bytes) in [
        ("test1.mp3", TEST1_MP3),
        ("test2.mp3", TEST2_MP3),
        ("test3.mp3", TEST3_MP3),
    ] {
        let pcm = yumo::mp3::decode_mp3_reader(std::io::Cursor::new(bytes))
            .unwrap_or_else(|e| panic!("{name} 解码失败: {e}"));
        assert!(pcm.sample_rate > 0, "{name} 采样率无效");
        assert!(pcm.channels == 1 || pcm.channels == 2, "{name} 声道数异常");
        assert!(!pcm.data.is_empty(), "{name} 解码结果为空");
    }
}

#[test]
fn mp3_converts_to_standard_format() {
    let pcm = yumo::mp3::decode_mp3_reader(std::io::Cursor::new(TEST2_MP3)).unwrap();
    let standard =
        yumo::resample::convert_pcm_to_standard(pcm.sample_rate, pcm.channels, &pcm.data).unwrap();
    // Canonical format: 44.1kHz stereo interleaved i16.
    assert_eq!(standard.len() % 2, 0);
    assert!(!standard.is_empty());
}
