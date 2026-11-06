//! Measures a rendered mp4 against the WCAG 2.3.1 flash threshold:
//!   cargo run --release --example flash_report -- target/guarded.mp4
//! Decodes with `ffmpeg -f rawvideo`, then runs the in-crate measurement over
//! the frame-mean luminance and over square windows of the WCAG area at positions
//! the guard does not itself enforce. Skips cleanly
//! (exit 0) when ffmpeg or ffprobe is not installed.
use projectmp4::render::flash::{count_flashes, windows_over_limit, worst_second, FLASH_LIMIT_PER_SECOND};
use std::io::Read;
use std::process::{Command, Stdio};

fn main() {
    let Some(path) = std::env::args().nth(1) else {
        eprintln!("usage: flash_report <video.mp4>");
        std::process::exit(2);
    };
    let probe = Command::new("ffprobe")
        .args(["-v", "error", "-select_streams", "v:0", "-show_entries", "stream=width,height,r_frame_rate", "-of", "csv=p=0", &path])
        .output();
    let probe = match probe {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        Ok(o) => {
            eprintln!("ffprobe failed: {}", String::from_utf8_lossy(&o.stderr));
            std::process::exit(1);
        }
        Err(e) => {
            println!("skipped: ffprobe not available ({e})");
            return;
        }
    };
    let f: Vec<&str> = probe.split(',').collect();
    let (w, h): (usize, usize) = (f[0].parse().expect("width"), f[1].parse().expect("height"));
    let (n, d) = f[2].split_once('/').map_or((f[2], "1"), |x| x);
    let fps = (n.parse::<f64>().expect("fps") / d.parse::<f64>().expect("fps")).round() as u32;

    let mut child = match Command::new("ffmpeg")
        .args(["-v", "error", "-nostdin", "-i", &path, "-f", "rawvideo", "-pix_fmt", "rgba", "-"])
        .stdout(Stdio::piped())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            println!("skipped: ffmpeg not available ({e})");
            return;
        }
    };
    let mut out = child.stdout.take().unwrap();
    let mut buf = vec![0u8; w * h * 4];
    // Regions the guard does NOT enforce as such, so the result is not the guard
    // marking its own homework: the frame mean, then square windows of the WCAG
    // area (266 px at 1080p) on a stride that divides the frame evenly in neither
    // axis, then 16 randomly placed ones from a fixed seed.
    let s = ((266.0 * h as f64 / 1080.0).round() as usize).clamp(1, w.min(h));
    let mut wins: Vec<(usize, usize)> = Vec::new();
    for y in (0..=h - s).step_by(89) {
        for x in (0..=w - s).step_by(97) {
            wins.push((x, y));
        }
    }
    let mut rng = 0x2545_F491_4F6C_DD1Du64;
    let mut next = |m: usize| {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng % m as u64) as usize
    };
    for _ in 0..16 {
        wins.push((next(w - s + 1), next(h - s + 1)));
    }
    let lut = projectmp4::render::flash::srgb_to_linear_lut();
    let stride = w + 1;
    let mut sat = vec![0f64; stride * (h + 1)];
    let mut mean: Vec<f32> = Vec::new();
    let mut series: Vec<Vec<f32>> = vec![Vec::new(); wins.len()];
    while out.read_exact(&mut buf).is_ok() {
        for y in 0..h {
            let mut run = 0f64;
            for x in 0..w {
                let i = (y * w + x) * 4;
                run += (0.2126 * lut[buf[i] as usize] + 0.7152 * lut[buf[i + 1] as usize] + 0.0722 * lut[buf[i + 2] as usize]) as f64;
                sat[(y + 1) * stride + x + 1] = sat[y * stride + x + 1] + run;
            }
        }
        mean.push((sat[h * stride + w] / (w * h) as f64) as f32);
        for (k, &(x, y)) in wins.iter().enumerate() {
            let v = sat[(y + s) * stride + x + s] - sat[y * stride + x + s] - sat[(y + s) * stride + x] + sat[y * stride + x];
            series[k].push((v / (s * s) as f64) as f32);
        }
    }
    assert!(child.wait().unwrap().success(), "ffmpeg failed decoding {path}");
    let len = mean.len();
    assert!(len > 0, "no frames decoded from {path}");

    let jump = |v: &[f32]| v.windows(2).map(|p| (p[1] - p[0]).abs()).fold(0f32, f32::max);
    let fl = count_flashes(&mean);
    let (worst_mean, at) = worst_second(len, &fl, fps);
    let (over, total) = windows_over_limit(len, &fl, fps);
    let secs = len as f64 / fps as f64;
    println!("{path}: {len} frames, {w}x{h} @ {fps} fps, {secs:.2} s");
    println!("mean luminance        {:.4}", mean.iter().map(|&x| x as f64).sum::<f64>() / len as f64);
    println!("largest 1-frame jump  {:.3} (frame mean), {:.3} (any {s}x{s} window)", jump(&mean), series.iter().map(|v| jump(v)).fold(0f32, f32::max));
    println!("WCAG flashes found    {} ({:.2} per second), frame mean", fl.len(), fl.len() as f64 / secs);
    println!("worst 1s window       {worst_mean} flashes at t={:.2}s, frame mean (limit is {FLASH_LIMIT_PER_SECOND})", at as f64 / fps as f64);
    println!("1s windows over limit {over} of {total}, frame mean");
    let mut worst = worst_mean;
    let (mut worst_over, mut worst_flashes) = (over, fl.len());
    let mut at_win = None;
    for (k, v) in series.iter().enumerate() {
        let f = count_flashes(v);
        let (c, _) = worst_second(len, &f, fps);
        worst_over = worst_over.max(windows_over_limit(len, &f, fps).0);
        worst_flashes = worst_flashes.max(f.len());
        if c > worst {
            worst = c;
            at_win = Some(wins[k]);
        }
    }
    println!(
        "{} independent {s}x{s} windows (stride 97x89 plus 16 seeded random): worst 1s window {worst}{}, most 1s windows over limit in any one {worst_over}, most flashes in any one {worst_flashes}",
        wins.len(),
        at_win.map_or(String::new(), |(x, y)| format!(" at ({x},{y})")),
    );
    if worst > FLASH_LIMIT_PER_SECOND {
        println!("FAIL: exceeds the general flash threshold");
        std::process::exit(1);
    }
}
