//! Read-only access to agent-deck's cache, used as numerical ground truth.
#![allow(dead_code)]

use std::path::PathBuf;

pub fn root() -> PathBuf {
    PathBuf::from(std::env::var("HOME").unwrap()).join("Library/Application Support/agent-deck")
}

/// The stem producer's exact 44-byte, 48 kHz stereo F32 WAV -> mono, each sample
/// `(L+R)*0.5f` in f32, as the reference downmixes per tap.
pub fn read_wav48_mono(path: &std::path::Path) -> Vec<f32> {
    let b = std::fs::read(path).unwrap();
    assert!(b.len() >= 44 && &b[..4] == b"RIFF" && &b[36..40] == b"data", "{path:?} header");
    let u16le = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
    let u32le = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
    assert_eq!((u16le(20), u16le(22), u32le(24), u16le(34)), (3, 2, 48000, 32), "{path:?} format");
    let n = u32le(40) as usize;
    assert_eq!(n + 44, b.len());
    b[44..]
        .chunks_exact(8)
        .map(|c| {
            let l = f32::from_le_bytes([c[0], c[1], c[2], c[3]]);
            let r = f32::from_le_bytes([c[4], c[5], c[6], c[7]]);
            (l + r) * 0.5f32
        })
        .collect()
}

pub fn read_f32(path: &std::path::Path) -> Vec<f32> {
    std::fs::read(path)
        .unwrap()
        .chunks_exact(4)
        .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
        .collect()
}
