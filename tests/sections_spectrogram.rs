#[path = "common/agentdeck.rs"]
mod agentdeck;
use projectmp4::audio::sections::spectrogram::*;

fn sine(freq: f64, sr: f64, n: usize) -> Vec<f32> {
    (0..n).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / sr).sin() as f32).collect()
}

fn window_at(pcm: &[f32], t: usize) -> [f32; 2048] {
    let mut w = [0f32; 2048];
    let start = (t * HOP) as i64 - 1024;
    for (j, v) in w.iter_mut().enumerate() {
        let i = start + j as i64;
        if i >= 0 && (i as usize) < pcm.len() {
            *v = pcm[i as usize];
        }
    }
    w
}

fn bands(pcm: &[f32], t: usize) -> [f32; 81] {
    let mut o = [0f32; 81];
    frame_bands(&window_at(pcm, t), &mut o).unwrap();
    o
}

// ---- resampler, on its own ----

#[test]
fn resampler_length_follows_the_reference_rule() {
    for n in [0usize, 1, 159, 160, 161, 48000, 576000, 576001] {
        let out = resample_48k_to_44k1(&vec![0.25; n]).unwrap();
        assert_eq!(out.len(), (n * 147 + 159) / 160, "n={n}");
    }
}

#[test]
fn resampler_keeps_a_1khz_sine_at_1khz() {
    let out = resample_48k_to_44k1(&sine(1000.0, 48000.0, 96000)).unwrap();
    // Upward zero crossings with linear interpolation, away from the clamped edges.
    let mut xs = Vec::new();
    for i in 2000..out.len() - 2000 {
        if out[i] <= 0.0 && out[i + 1] > 0.0 {
            xs.push(i as f64 + (-out[i] / (out[i + 1] - out[i])) as f64);
        }
    }
    let periods = (xs.len() - 1) as f64;
    let freq = periods * SR as f64 / (xs[xs.len() - 1] - xs[0]);
    let cents = 1200.0 * (freq / 1000.0).log2();
    assert!(cents.abs() < 0.5, "{freq} Hz, {cents} cents off");
}

#[test]
fn resampler_has_zero_group_delay_and_unit_dc_gain() {
    // Source 160 is output 147 exactly (phase 0): the impulse peaks there.
    let mut x = vec![0f32; 1600];
    x[160] = 1.0;
    let y = resample_48k_to_44k1(&x).unwrap();
    let peak = y.iter().enumerate().max_by(|a, b| a.1.partial_cmp(b.1).unwrap()).unwrap().0;
    assert_eq!(peak, 147);
    assert!(y[147] > 0.9, "peak {}", y[147]);
    // Constant input stays constant, including the clamped edges.
    let dc = resample_48k_to_44k1(&vec![0.5; 1000]).unwrap();
    assert!(dc.iter().all(|v| (v - 0.5).abs() < 1e-6));
    // Clamped edges: a constant 3-sample input yields exactly that constant at output 0.
    assert!((resample_48k_to_44k1(&[1.0; 3]).unwrap()[0] - 1.0).abs() < 1e-6);
}

#[test]
fn resampler_rejects_non_finite_input() {
    assert!(resample_48k_to_44k1(&[0.0, f32::NAN]).is_err());
}

// ---- framing and bank ----

#[test]
fn framing_is_centred_and_hop_441_exactly() {
    assert_eq!(frames_for(0), 0);
    assert_eq!(frames_for(1), 1);
    assert_eq!(frames_for(441), 1);
    assert_eq!(frames_for(442), 2);
    assert_eq!(frames_for(44100), 100, "441 hop must give exactly 100 frames per second");
    let mut pcm = vec![0f32; 441 * 8];
    pcm[0] = 1.0;
    let e = |t: usize| bands(&pcm, t).iter().sum::<f32>();
    assert!(e(0) > e(3), "{} vs {}", e(0), e(3));
    assert!(e(3) == 0.0, "window 3 starts at 299, past the impulse's reach");
    let (t, frames) = stack(&[pcm.clone(), pcm.clone(), pcm.clone(), pcm]).unwrap();
    assert_eq!((t.len(), frames), (4 * 8 * 81, 8));
}

#[test]
fn a_dc_free_sine_puts_its_energy_in_one_band_region() {
    let pcm = sine(1000.0, 44100.0, 44100);
    let b = bands(&pcm, 50);
    let peak = b.iter().enumerate().max_by(|a, c| a.1.partial_cmp(c.1).unwrap()).unwrap().0;
    assert!(peak > 0 && peak < 80, "peak band {peak}");
    // Bank centres rise with index: a 4 kHz tone must peak in a strictly higher band.
    let hi = bands(&sine(4000.0, 44100.0, 44100), 50);
    let hi_peak = hi.iter().enumerate().max_by(|a, c| a.1.partial_cmp(c.1).unwrap()).unwrap().0;
    assert!(hi_peak > peak, "bank orientation: {hi_peak} <= {peak}");
    let silent: f32 = bands(&vec![0.0; 44100], 50).iter().sum();
    assert_eq!(silent, 0.0);
    assert!(b.iter().sum::<f32>() > silent + 1.0);
}

#[test]
fn non_finite_pcm_is_an_error_naming_the_frame() {
    let mut pcm = vec![0.1f32; 44100];
    pcm[5000] = f32::NAN;
    let e = stack(&[pcm.clone(), pcm.clone(), pcm.clone(), pcm]).unwrap_err();
    // Sample 5000 first falls in frame t when 441*t-1024+2048 > 5000, i.e. t = 10.
    assert!(e.0.contains("frame 10"), "{}", e.0);
}

#[test]
fn stems_of_different_lengths_are_rejected() {
    let a = vec![0.0f32; 1000];
    assert!(stack(&[a.clone(), a.clone(), a.clone(), vec![0.0; 999]]).is_err());
}

// ---- parity against agent-deck's cached reference ----

#[test]
fn bands_match_the_cached_reference_spectrogram_on_real_stems() {
    let root = agentdeck::root();
    if !root.exists() {
        eprintln!("SKIPPED: {} absent; parity oracle unavailable", root.display());
        return;
    }
    let art = root.join("sections/jobs/artifacts");
    // result 51d3... (1200 frames) -> tensor 9b0e... -> bundle 3db3... (bundleHash of 9fe3...)
    let result_id = "51d39155ea01168339fc4b4da801750363a6cf79588ffab3a534e50862da3b48";
    let tensor_id = "9b0e74cd48c15c61af660f591701928ddbebaf81cb32d5c81dee9e193d5db0b1";
    let stage_id = "3087ce4c701969e81c74b6413c7249b1894d3c5e3d0f1723b1ab8d4dfe53164c";
    let bundle_dir = root.join("stems/bundles/9fe3f71b0a5b39d816094e3dabe93f8c94ad3aa3567f2df3825236d756202b20");

    let json = |p: std::path::PathBuf| -> serde_json::Value {
        serde_json::from_slice(&std::fs::read(&p).unwrap_or_else(|e| panic!("{p:?}: {e}"))).unwrap()
    };
    // Confirm the pairing by recorded identity, not by size alone.
    let meta = json(art.join(format!("{result_id}.json")));
    assert_eq!(meta["inputs"][0]["identity"], tensor_id);
    let stage = json(art.join(format!("{stage_id}.stage.json")));
    assert_eq!(stage["units"][0]["identity"], tensor_id);
    let result = json(art.join(result_id));
    let manifest = json(bundle_dir.join("manifest.json"));
    assert_eq!(result["stemBundleHash"], manifest["bundleHash"]);
    let cached_frames = result["result"]["frames"].as_u64().unwrap() as usize;
    let tensor_path = art.join(tensor_id);
    assert_eq!(std::fs::metadata(&tensor_path).unwrap().len() as usize, cached_frames * 4 * 81 * 4);
    let tmeta = json(art.join(format!("{tensor_id}.json")));
    assert_eq!(tmeta["bytes"].as_u64().unwrap() as usize, cached_frames * 4 * 81 * 4);

    let stem = |name: &str| {
        let mono = agentdeck::read_wav48_mono(&bundle_dir.join(format!("{name}.wav")));
        resample_48k_to_44k1(&mono).unwrap()
    };
    let stems = [stem("bass"), stem("drums"), stem("other"), stem("vocals")];
    let (mine, my_frames) = stack(&stems).unwrap();
    assert_eq!(my_frames, cached_frames, "frame count");
    let cached = agentdeck::read_f32(&tensor_path);
    assert_eq!(mine.len(), cached.len());
    let mean = cached.iter().sum::<f32>() / cached.len() as f32;
    assert!(mean > 0.1 && cached.iter().any(|v| *v > 1.0), "oracle looks degenerate: mean {mean}");
    let max_abs_diff = mine.iter().zip(&cached).map(|(a, b)| (a - b).abs()).fold(0f32, f32::max);
    eprintln!("max_abs_diff = {max_abs_diff:e}");
    assert!(max_abs_diff < 1e-4, "spectrogram diverges from the reference: {max_abs_diff}");
}
