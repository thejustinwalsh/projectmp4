//! Frame fixtures for the flash-guard suite. Frames are small (the guard's
//! behaviour is per-region, not per-pixel) so 200-frame runs stay quick unoptimised.
#![allow(dead_code)]

pub const W: usize = 160;
pub const H: usize = 96;

/// A whole frame of one grey level, opaque.
pub fn solid(v: u8) -> Vec<u8> {
    let mut f = vec![v; W * H * 4];
    for p in f.chunks_exact_mut(4) {
        p[3] = 255;
    }
    f
}

/// Mid-grey everywhere except grid cell `(row, col)` of the 4x4 grid, which is `v`.
pub fn cell_frame(row: usize, col: usize, v: u8) -> Vec<u8> {
    let mut f = solid(128);
    for y in row * H / 4..(row + 1) * H / 4 {
        for x in col * W / 4..(col + 1) * W / 4 {
            let i = (y * W + x) * 4;
            f[i..i + 3].fill(v);
        }
    }
    f
}

/// Each grid cell a different pseudorandom grey, deterministic in `seed`.
pub fn random_cells(seed: u64) -> Vec<u8> {
    let mut s = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15) | 1;
    let mut next = || {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        s
    };
    let grey: Vec<[u8; 3]> = (0..16).map(|_| [next() as u8, next() as u8, next() as u8]).collect();
    let mut f = solid(0);
    for y in 0..H {
        for x in 0..W {
            let c = (y * 4 / H) * 4 + x * 4 / W;
            let i = (y * W + x) * 4;
            f[i..i + 3].copy_from_slice(&grey[c]);
        }
    }
    f
}
