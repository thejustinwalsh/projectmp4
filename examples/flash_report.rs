//! Measures a rendered mp4 against the WCAG 2.3.1 flash threshold:
//!   cargo run --release --example flash_report -- target/guarded.mp4
//! Decodes with `ffmpeg -f rawvideo`, then runs the in-crate measurement over
//! the frame-mean luminance and over each cell of a 8x8 grid. Skips cleanly
//! (exit 0) when ffmpeg or ffprobe is not installed.
use projectmp4::render::flash::{count_flashes, region_luma, windows_over_limit, worst_second, FLASH_LIMIT_PER_SECOND};
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
    let mut mean: Vec<f32> = Vec::new();
    let mut cells: Vec<Vec<f32>> = vec![Vec::new(); 64];
    while out.read_exact(&mut buf).is_ok() {
        mean.push(region_luma(&buf, w, h, 1, 1)[0]);
        for (i, l) in region_luma(&buf, w, h, 8, 8).into_iter().enumerate() {
            cells[i].push(l);
        }
    }
    assert!(child.wait().unwrap().success(), "ffmpeg failed decoding {path}");
    let len = mean.len();
    assert!(len > 0, "no frames decoded from {path}");

    let jump = mean.windows(2).map(|p| (p[1] - p[0]).abs()).fold(0f32, f32::max);
    let fl = count_flashes(&mean);
    let (worst, at) = worst_second(len, &fl, fps);
    let (over, total) = windows_over_limit(len, &fl, fps);
    let secs = len as f64 / fps as f64;
    println!("{path}: {len} frames, {w}x{h} @ {fps} fps, {secs:.2} s");
    println!("mean luminance        {:.4}", mean.iter().map(|&x| x as f64).sum::<f64>() / len as f64);
    println!("largest 1-frame jump  {jump:.3}");
    println!("WCAG flashes found    {} ({:.2} per second)", fl.len(), fl.len() as f64 / secs);
    println!("worst 1s window       {worst} flashes at t={:.2}s (limit is {FLASH_LIMIT_PER_SECOND})", at as f64 / fps as f64);
    println!("1s windows over limit {over} of {total}");
    let cell_worst = cells.iter().map(|c| worst_second(len, &count_flashes(c), fps).0).max().unwrap();
    println!("worst 1s window in any 8x8 cell (each ~32,400 px^2 at 1080p, under the WCAG area threshold): {cell_worst}");
    if worst > FLASH_LIMIT_PER_SECOND {
        println!("FAIL: exceeds the general flash threshold");
        std::process::exit(1);
    }
}
