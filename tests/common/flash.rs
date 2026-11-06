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

/// Mid-grey everywhere except grid cell `(row, col)` of the 8x8 grid, which is `v`.
pub fn cell_frame(row: usize, col: usize, v: u8) -> Vec<u8> {
    let mut f = solid(128);
    for y in row * H / 8..(row + 1) * H / 8 {
        for x in col * W / 8..(col + 1) * W / 8 {
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
    let grey: Vec<[u8; 3]> = (0..64).map(|_| [next() as u8, next() as u8, next() as u8]).collect();
    let mut f = solid(0);
    for y in 0..H {
        for x in 0..W {
            let c = (y * 8 / H) * 8 + x * 8 / W;
            let i = (y * W + x) * 4;
            f[i..i + 3].copy_from_slice(&grey[c]);
        }
    }
    f
}

/// A `size`x`size` square at (`x0`, `y0`) of value `v` on a mid-grey `w`x`h` field.
pub fn square_frame(w: usize, h: usize, x0: usize, y0: usize, size: usize, v: u8) -> Vec<u8> {
    let mut f = vec![128u8; w * h * 4];
    for p in f.chunks_exact_mut(4) {
        p[3] = 255;
    }
    for y in y0..y0 + size {
        for x in x0..x0 + size {
            let i = (y * w + x) * 4;
            f[i..i + 3].fill(v);
        }
    }
    f
}

/// The mean relative luminance of the `size`x`size` square at (`x0`, `y0`).
pub fn square_luma(f: &[u8], w: usize, x0: usize, y0: usize, size: usize) -> f32 {
    let mut crop = Vec::with_capacity(size * size * 4);
    for y in y0..y0 + size {
        crop.extend_from_slice(&f[(y * w + x0) * 4..(y * w + x0 + size) * 4]);
    }
    projectmp4::render::flash::region_luma(&crop, size, size, 1, 1)[0]
}
