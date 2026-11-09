//! Flash scoring: the synthetic drive, the content-hash cache, the ceiling's
//! shortfall behaviour, and (ignored, need the packs and a GL context) real
//! presets scored at low and full resolution.
//! Run the ignored ones with `cargo test --release --test flashscore -- --ignored`.
//! Everything here is headless: pixels come from an offscreen framebuffer.

#[path = "common/tmp.rs"]
mod tmp;

use projectmp4::gl_context::GlContext;
use projectmp4::presets::flashscore::*;
use projectmp4::presets::packs::default_cache_dir;
use projectmp4::Packs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::Instant;
use tmp::Tmp;

const STROBE: &str = "suksma - water cooled red uranium vs dotes - 3d overlap.milk";
const CALM: &str = "to know the reason i am trapped is my own weakness and fear roam3 nz+.milk";

/// GL contexts are not nested or shared across threads here: one scorer at a time.
static GL: Mutex<()> = Mutex::new(());

fn packs() -> Packs {
    Packs::ensure(&default_cache_dir().expect("cache dir")).unwrap_or_else(|e| panic!("populating the pack cache failed: {e}"))
}

fn find(dir: &Path, name: &str) -> Option<PathBuf> {
    for e in std::fs::read_dir(dir).ok()?.flatten() {
        let p = e.path();
        if e.file_type().ok()?.is_dir() {
            if let Some(f) = find(&p, name) {
                return Some(f);
            }
        } else if p.file_name().is_some_and(|n| n == name) {
            return Some(p);
        }
    }
    None
}

fn preset(packs: &Packs, name: &str) -> PathBuf {
    find(&packs.cotc, name).unwrap_or_else(|| panic!("{name} is not in the pack"))
}

/// Scores each preset on its own context at `w`x`h`.
fn score_at(packs: &Packs, w: u32, h: u32, names: &[&str]) -> Vec<FlashScore> {
    let _g = GL.lock().unwrap_or_else(|e| e.into_inner());
    let gl = GlContext::new(w, h).unwrap();
    let pm = gl.create_instance().unwrap();
    pm.set_texture_search_paths(&[packs.textures.as_path()]).unwrap();
    let drive = synthetic_drive(SCORE_SECS);
    names
        .iter()
        .map(|n| score_preset(&pm, &gl, &preset(packs, n), &drive, SCORE_FPS, SCORE_SECS).unwrap_or_else(|e| panic!("{n}: {e}")))
        .collect()
}

#[test]
#[ignore = "needs the preset packs and a GL context"]
fn a_known_strobe_preset_scores_over_the_ceiling() {
    let s = score_at(&packs(), SCORE_WIDTH, SCORE_HEIGHT, &[STROBE])[0];
    eprintln!("{STROBE}: {s:?}");
    assert!(s.worst_second > FLASH_CEILING, "{s:?}");
}

#[test]
#[ignore = "needs the preset packs and a GL context"]
fn a_known_calm_preset_scores_under_the_ceiling() {
    let s = score_at(&packs(), SCORE_WIDTH, SCORE_HEIGHT, &[CALM])[0];
    eprintln!("{CALM}: {s:?}");
    assert!(s.worst_second <= FLASH_CEILING, "{s:?}");
}

#[test]
#[ignore = "needs the preset packs and a GL context"]
fn the_low_resolution_score_tracks_the_full_resolution_one() {
    // Downsampling averages out local strobes, so a low-res score could understate a
    // 1080p flash rate and wave a strobe through. It must not be materially more permissive.
    let packs = packs();
    let low = score_at(&packs, SCORE_WIDTH, SCORE_HEIGHT, &[STROBE, CALM]);
    let high = score_at(&packs, 1920, 1080, &[STROBE, CALM]);
    for (i, n) in [STROBE, CALM].iter().enumerate() {
        eprintln!("{n}: low {:?} high {:?}", low[i], high[i]);
        assert!(
            low[i].worst_second + 1 >= high[i].worst_second,
            "{n}: low-res worst_second {} is more than 1 below full-res {}; the scoring resolution is too low",
            low[i].worst_second,
            high[i].worst_second
        );
    }
}

#[test]
#[ignore = "needs the preset packs and a GL context; prints a measurement"]
fn scoring_throughput_is_reported() {
    let packs = packs();
    let _g = GL.lock().unwrap_or_else(|e| e.into_inner());
    let gl = GlContext::new(SCORE_WIDTH, SCORE_HEIGHT).unwrap();
    let pm = gl.create_instance().unwrap();
    pm.set_texture_search_paths(&[packs.textures.as_path()]).unwrap();
    let drive = synthetic_drive(SCORE_SECS);
    let p = preset(&packs, CALM);
    let _ = score_preset(&pm, &gl, &p, &drive, SCORE_FPS, SCORE_SECS).unwrap(); // warm shaders/driver
    let t = Instant::now();
    let reps = 3;
    for _ in 0..reps {
        score_preset(&pm, &gl, &p, &drive, SCORE_FPS, SCORE_SECS).unwrap();
    }
    let per = t.elapsed().as_secs_f64() / reps as f64;
    let frames = SCORE_SECS * SCORE_FPS as f64;
    eprintln!("THROUGHPUT {SCORE_WIDTH}x{SCORE_HEIGHT}: {:.1} fps, {per:.2} s per preset; 100={:.0}s 500={:.0}s 2000={:.0}s", frames / per, per * 100.0, per * 500.0, per * 2000.0);
}

fn score(w: u32, f: f32) -> FlashScore {
    FlashScore { worst_second: w, flashes_per_sec: f }
}

#[test]
fn the_cache_is_keyed_by_content_not_by_path() {
    let t = Tmp::new("flashscore-cache");
    let (a, b) = (t.join("a.milk"), t.join("sub-b.milk"));
    std::fs::write(&a, b"per_frame_1=zoom=1+bass;").unwrap();
    std::fs::write(&b, b"per_frame_1=zoom=1+bass;").unwrap();
    let (ha, hb) = (content_hash(&std::fs::read(&a).unwrap()), content_hash(&std::fs::read(&b).unwrap()));
    assert_eq!(ha, hb, "same bytes at two paths must share a key");

    let mut cache = FlashCache::load(&t.join("cache.toml"));
    assert!(cache.get(&ha).is_none());
    cache.insert(&ha, score(7, 2.5));
    assert_eq!(cache.get(&hb), Some(score(7, 2.5)));
    assert_eq!(cache.len(), 1, "one entry for two paths");

    std::fs::write(&b, b"per_frame_1=zoom=1+bass; ").unwrap();
    let hc = content_hash(&std::fs::read(&b).unwrap());
    assert_ne!(ha, hc);
    assert!(cache.get(&hc).is_none(), "a changed byte must miss");

    // Round trip through disk.
    cache.save().unwrap();
    let back = FlashCache::load(&t.join("cache.toml"));
    assert_eq!(back.get(&ha), Some(score(7, 2.5)));
    assert_eq!(back.len(), 1);
}

#[test]
fn a_missing_truncated_or_garbage_cache_file_is_an_empty_cache_not_an_error() {
    let t = Tmp::new("flashscore-corrupt");
    let p = t.join("cache.toml");
    assert!(FlashCache::load(&p).is_empty(), "missing file");
    let mut c = FlashCache::load(&p);
    let h = content_hash(b"x");
    c.insert(&h, score(2, 0.5));
    c.save().unwrap();
    let good = std::fs::read_to_string(&p).unwrap();
    for (what, bad) in [("garbage", "\u{0}\u{1}garbage".to_string()), ("empty", String::new()), ("bad number", good.replace("worst_second = 2", "worst_second = two"))] {
        std::fs::write(&p, &bad).unwrap();
        assert!(FlashCache::load(&p).is_empty(), "{what}");
    }
    std::fs::write(&p, &good[..good.len() / 2]).unwrap();
    assert!(FlashCache::load(&p).is_empty(), "truncated file");
    std::fs::write(&p, [0xffu8, 0xfe, 0x00]).unwrap();
    assert!(FlashCache::load(&p).is_empty(), "non-UTF-8 file");
}

#[test]
fn the_synthetic_drive_is_deterministic() {
    let a = synthetic_drive(SCORE_SECS);
    let b = synthetic_drive(SCORE_SECS);
    assert_eq!(a.len(), (SCORE_SECS * DRIVE_RATE as f64) as usize * 2);
    assert!(a.iter().zip(&b).all(|(x, y)| x.to_bits() == y.to_bits()), "two calls differ");
    // Gated at 2 Hz: loud in the first quarter second, quiet bed in the second.
    let rms = |s: &[f32]| (s.iter().map(|x| x * x).sum::<f32>() / s.len() as f32).sqrt();
    let q = DRIVE_RATE as usize / 4 * 2; // a quarter second of interleaved stereo
    assert!(rms(&a[..q]) > 0.5, "gate open");
    assert!(rms(&a[q..2 * q]) < 0.1 && rms(&a[q..2 * q]) > 0.0, "gate closed over a bed");
    assert!(a.iter().all(|x| x.abs() <= 1.0));
}

fn cand(name: &str) -> projectmp4::presets::rank::Candidate {
    projectmp4::presets::rank::Candidate {
        path: PathBuf::from(name),
        score: projectmp4::presets::rank::Score { bass: 0, react: 1, shader: true },
        tier: projectmp4::presets::rank::Tier::Uncurated,
        rank_score: 0,
    }
}

#[test]
fn a_profile_short_of_presets_reports_the_shortfall_rather_than_filling_with_strobes() {
    use projectmp4::presets::rank::select;
    // Best-first pool of six: only "calm1" and "calm2" clear the ceiling. Asking for four
    // must return those two and surface the gap, never top up from the strobes.
    let pool: Vec<_> = ["strobe1", "calm1", "strobe2", "strobe3", "calm2", "strobe4"].iter().map(|n| cand(n)).collect();
    let (picked, stats) = select(&pool, 4, FLASH_CEILING, |c| {
        let name = c.path.to_str().unwrap();
        Ok(Some(score(if name.starts_with("calm") { 1 } else { 9 }, 0.0)))
    })
    .unwrap();
    let names: Vec<_> = picked.iter().map(|(p, _)| p.to_str().unwrap()).collect();
    assert_eq!(names, ["calm1", "calm2"]);
    assert!(picked.iter().all(|(_, w)| *w <= FLASH_CEILING));
    assert_eq!(stats.passed, 2);
    assert_eq!(4 - picked.len(), 2, "the shortfall");
    assert_eq!(stats.rejected.len(), 4, "every strobe is reported, none is used");
    assert_eq!(stats.examined, 6, "a short pool is walked to the end");
}

#[test]
fn scoring_stops_as_soon_as_enough_presets_have_passed() {
    use projectmp4::presets::rank::select;
    let pool: Vec<_> = (0..100).map(|i| cand(&format!("p{i}"))).collect();
    let mut calls = 0;
    // Every third preset strobes.
    let (picked, stats) = select(&pool, 10, FLASH_CEILING, |c| {
        calls += 1;
        let i: usize = c.path.to_str().unwrap()[1..].parse().unwrap();
        Ok(Some(score(if i % 3 == 0 { 8 } else { 0 }, 0.0)))
    })
    .unwrap();
    assert_eq!(picked.len(), 10);
    assert_eq!(calls, 15, "10 passes and the 5 strobes interleaved among them, no more");
    assert_eq!(stats.examined, calls);
    // Rank order is kept.
    assert_eq!(picked[0].0, PathBuf::from("p1"));
}

#[test]
fn an_unloadable_preset_is_set_aside_and_counted_apart_from_strobes() {
    use projectmp4::presets::rank::select;
    let pool: Vec<_> = ["broken", "strobe", "calm"].iter().map(|n| cand(n)).collect();
    let (picked, stats) = select(&pool, 5, FLASH_CEILING, |c| {
        Ok(match c.path.to_str().unwrap() {
            "broken" => None,
            "strobe" => Some(score(9, 0.0)),
            _ => Some(score(0, 0.0)),
        })
    })
    .unwrap();
    assert_eq!(picked.len(), 1);
    assert_eq!(stats.unloadable, [PathBuf::from("broken")]);
    assert_eq!(stats.rejected, [(PathBuf::from("strobe"), 9)]);
}

#[test]
#[ignore = "needs the preset packs and a GL context"]
fn a_preset_that_will_not_load_is_unscoreable_not_a_strobe() {
    let bad = PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/unparsable.milk"));
    let packs = packs();
    let _g = GL.lock().unwrap_or_else(|e| e.into_inner());
    let r = projectmp4::presets::flashscore::with_scorer(&packs, |s| s.score(&bad)).unwrap();
    assert!(r.is_none(), "{r:?}");
}

#[test]
fn the_cache_file_name_carries_the_scorer_version() {
    assert!(FlashCache::file_name().contains(&format!("-s{SCORER_VERSION}-")), "{}", FlashCache::file_name());
}

#[test]
fn the_flash_ceiling_is_dormant_until_switched_on() {
    // Committed state: ranking applies no ceiling, so presets/profiles.toml stays valid.
    // The next session flips this const (and regenerates the manifest).
    assert!(!projectmp4::presets::rank::FLASH_CEILING_ENABLED);
}
