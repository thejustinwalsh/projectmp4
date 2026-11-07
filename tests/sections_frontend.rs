#[path = "common/agentdeck.rs"]
mod agentdeck;
#[path = "common/cached_stems.rs"]
mod cached_stems;
use projectmp4::audio::sections::frontend::*;

const SR: u32 = 48_000;

fn sine(freq: f64, n: usize) -> Vec<f32> {
    (0..n).map(|i| (2.0 * std::f64::consts::PI * freq * i as f64 / SR as f64).sin() as f32).collect()
}

fn energy(x: &[f32]) -> f64 {
    x.iter().map(|v| (*v as f64).powi(2)).sum()
}

#[test]
fn every_frontend_produces_four_equal_length_channels() {
    let fronts: [(&str, Box<dyn FourChannel>); 2] = [("mix", Box::new(MixFourWays)), ("hpss", Box::new(HpssBands))];
    for (name, f) in &fronts {
        for n in [0usize, 1, 100, 2047, 2048, 5000, 48_000] {
            let x = sine(440.0, n);
            let c = f.channels(&x, SR).unwrap_or_else(|e| panic!("{name} n={n}: {e}"));
            for (i, ch) in c.iter().enumerate() {
                assert_eq!(ch.len(), n, "{name} n={n} slot {i}");
                assert!(ch.iter().all(|v| v.is_finite()), "{name} n={n} slot {i}");
            }
        }
        assert!(f.channels(&[0.0, f32::NAN], SR).is_err(), "{name} must reject NaN");
    }
    let x = sine(440.0, 1000);
    let m = MixFourWays.channels(&x, SR).unwrap();
    assert!(m.iter().all(|c| *c == x));
}

#[test]
fn hpss_sends_transients_to_the_drum_slot() {
    // A click every 0.5 s over a sustained 1 kHz sine. For each slot, the share of its energy
    // lying within 25 ms of a click. A sine contributes evenly in time (share ~ the 10% duty
    // of the windows); a click contributes only inside them.
    let n = SR as usize * 6;
    let period = SR as usize / 2;
    let mut x = sine(1000.0, n).iter().map(|v| v * 0.3).collect::<Vec<_>>();
    let clicks: Vec<usize> = (1..11).map(|i| i * period).collect();
    for &c in &clicks {
        x[c] += 1.0;
    }
    let ch = HpssBands.channels(&x, SR).unwrap();
    let near = |i: usize| clicks.iter().any(|&c| i.abs_diff(c) <= 1200);
    let share: Vec<f64> = ch
        .iter()
        .map(|c| {
            let total = energy(c);
            let inside: f64 = c.iter().enumerate().filter(|(i, _)| near(*i)).map(|(_, v)| (*v as f64).powi(2)).sum();
            inside / total.max(1e-30)
        })
        .collect();
    eprintln!("click-window energy share per slot [bass, drums, other, vocals]: {share:?}");
    // 1 = drums. Stated factor: at least 3x every other slot, and above one half outright.
    for s in [0, 2, 3] {
        assert!(share[1] > 3.0 * share[s], "drums {} vs slot {s} {}", share[1], share[s]);
    }
    assert!(share[1] > 0.5, "drums share {}", share[1]);
    // And it is not the mix four times under another name.
    assert!(ch.iter().all(|c| c.iter().zip(&x).any(|(a, b)| (a - b).abs() > 0.05)));
    // The sustained tone lands in the harmonic mid-band slots, not drums.
    assert!(energy(&ch[2]) > energy(&ch[1]) && energy(&ch[3]) > energy(&ch[1]));
    assert!(energy(&ch[0]) < 0.01 * energy(&ch[2]), "1 kHz tone leaked into bass");
}

#[test]
fn hpss_reconstructs_the_mix_when_its_slots_are_recombined() {
    // Bass + other are disjoint harmonic bands, so bass+drums+other should approximate the
    // mix away from the edges (vocals overlaps other and is excluded).
    let n = SR as usize * 2;
    let mut x = sine(100.0, n);
    for (i, v) in sine(1500.0, n).iter().enumerate() {
        x[i] = x[i] * 0.5 + v * 0.3;
    }
    x[SR as usize] += 0.8;
    let c = HpssBands.channels(&x, SR).unwrap();
    let err: f64 = (4096..n - 4096).map(|i| ((c[0][i] + c[1][i] + c[2][i]) - x[i]) as f64).map(|d| d * d).sum();
    let sig: f64 = x[4096..n - 4096].iter().map(|v| (*v as f64).powi(2)).sum();
    assert!(err < 0.01 * sig, "residual {} of {}", err, sig);
}

#[test]
fn cached_stems_returns_the_bundles_own_stems_at_the_requested_length() {
    let root = agentdeck::root();
    if !root.exists() {
        if std::env::var_os("PROJECTMP4_NO_PARITY_ORACLE").is_some() {
            eprintln!("SKIPPED by PROJECTMP4_NO_PARITY_ORACLE: {} absent", root.display());
            return;
        }
        panic!("parity oracle missing: {} does not exist; set PROJECTMP4_NO_PARITY_ORACLE=1 to opt out", root.display());
    }
    let bundle = root.join("stems/bundles/9fe3f71b0a5b39d816094e3dabe93f8c94ad3aa3567f2df3825236d756202b20");
    let drums = agentdeck::read_wav48_mono(&bundle.join("drums.wav"));
    let want = drums.len();
    let c = cached_stems::CachedStems::new(&bundle).channels(&vec![0.0; want], SR).unwrap();
    assert!(c.iter().all(|s| s.len() == want));
    assert_eq!(c[1], drums);
    assert!(c[0] != c[1] && c[2] != c[3]);
    // Shorter request truncates, longer pads with silence.
    let s = cached_stems::CachedStems::new(&bundle).channels(&vec![0.0; 100], SR).unwrap();
    assert!(s.iter().all(|x| x.len() == 100));
    let l = cached_stems::CachedStems::new(&bundle).channels(&vec![0.0; want + 7], SR).unwrap();
    assert_eq!(l[1][want..], [0.0; 7]);
    assert!(cached_stems::CachedStems::new(&bundle).channels(&[0.0; 4], 44_100).is_err());
}
