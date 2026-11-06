//! Times `FlashGuard::limit` on 1920x1080 frames, headless (no window, no GL):
//!   cargo run --release --example guard_timing
//! Two inputs: alternating dim and bright pseudorandom noise (the guard engages and
//! blends every frame, the costliest path) and a calm frame (it only measures and copies).
use projectmp4::render::flash::FlashGuard;
use std::time::Instant;

fn noise(seed: u64) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    (0..1920 * 1080 * 4)
        .map(|_| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            s as u8
        })
        .collect()
}

fn time(label: &str, frames: &[Vec<u8>], n: usize) {
    let mut g = FlashGuard::new(1920, 1080, 60, 0.48);
    let mut us: Vec<f64> = Vec::with_capacity(n);
    let mut buf = frames[0].clone();
    for i in 0..n {
        buf.copy_from_slice(&frames[i % frames.len()]);
        let t = Instant::now();
        g.limit(&mut buf);
        us.push(t.elapsed().as_secs_f64() * 1e6);
    }
    us.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let mean = us.iter().sum::<f64>() / n as f64;
    let p99 = us[(n as f64 * 0.99) as usize - 1];
    let budget = 1e6 / 60.0;
    println!(
        "{label}: mean {mean:.0} us, p99 {p99:.0} us per call; {:.1}% / {:.1}% of the {budget:.0} us budget at 60 fps; engaged {}/{n}",
        100.0 * mean / budget,
        100.0 * p99 / budget,
        g.engaged_frames()
    );
}

fn main() {
    // Alternate dim and bright noise so every region swings and the guard blends each frame.
    let frames: Vec<Vec<u8>> = (0..4)
        .map(|i| noise(i).into_iter().map(|b| if i % 2 == 0 { b >> 2 } else { b | 0xC0 }).collect())
        .collect();
    time("noise (engaged)", &frames, 300);
    time("calm (passes through)", &[vec![120u8; 1920 * 1080 * 4]], 300);
}
