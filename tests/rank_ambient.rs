//! The ambient profile against fabricated packs, so it runs without the network.
//! The real-pack ranking tests are in `tests/rank.rs` (`--ignored`).

#[path = "common/tmp.rs"]
mod tmp;
use projectmp4::presets::rank::eligible;
use projectmp4::Packs;
use std::path::Path;
use tmp::Tmp;

const SHADER: &str = "warp_1=`shader_type(ps_2_0)\n";

fn milk(dir: &Path, name: &str, equations: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(name), format!("{SHADER}{equations}")).unwrap();
}

/// Hypnotic category holding one preset per behaviour, under a fabricated pack root.
fn packs(root: &Tmp) -> Packs {
    let hypnotic = root.join("cotc/Hypnotic");
    // No audio variable at all: a screensaver.
    milk(&hypnotic, "inert.milk", "per_frame_1=zoom=1.01;\n");
    // Mid/treble/volume drift only.
    milk(&hypnotic, "drift.milk", "per_frame_1=zoom=1+treb*0.1+mid*0.1+vol*0.1;\n");
    // A little bass beside the drift: gentle enough.
    milk(&hypnotic, "gentle.milk", "per_frame_1=zoom=1+bass*0.1+treb*0.1;\n");
    // Follows the kick hard.
    milk(&hypnotic, "kick.milk", "per_frame_1=zoom=1+bass*0.1+bass*0.2+bass*0.3+bass_att*0.4+bass_att*0.5+treb*0.1;\n");
    Packs { cotc: root.join("cotc"), classic: root.join("classic"), textures: root.join("textures") }
}

fn names(root: &Tmp, profile: &str) -> Vec<String> {
    eligible(&packs(root), profile)
        .unwrap()
        .into_iter()
        .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
        .collect()
}

#[test]
fn ambient_never_selects_a_preset_that_ignores_the_audio() {
    let root = Tmp::new("rank-ambient");
    let got = names(&root, "ambient");
    assert!(!got.contains(&"inert.milk".to_string()), "a screensaver was selected: {got:?}");
    // The reactive ones are what is left, drift first (more non-bass reactivity, no bass).
    assert_eq!(got, ["drift.milk", "gentle.milk"], "kick-followers are capped out, the rest ranked");
}

#[test]
fn ambient_caps_kick_following_presets_instead_of_inverting_the_ranking() {
    let root = Tmp::new("rank-ambient-cap");
    let got = names(&root, "ambient");
    assert!(!got.contains(&"kick.milk".to_string()), "a kick-follower passed the ambient cap: {got:?}");
    assert!(got.contains(&"gentle.milk".to_string()), "a gentle bass reference must still qualify: {got:?}");
}

#[test]
fn every_profile_requires_some_audio_reference() {
    let root = Tmp::new("rank-inert-all");
    // `Hypnotic` is not in the deathstep categories; give it its own.
    milk(&root.join("cotc/Reaction"), "inert.milk", "per_frame_1=zoom=1.01;\n");
    milk(&root.join("cotc/Reaction"), "reactive.milk", "per_frame_1=zoom=1+bass*0.1;\n");
    let packs = Packs { cotc: root.join("cotc"), classic: root.join("classic"), textures: root.join("textures") };
    for profile in ["deathstep", "dubstep", "dancehall"] {
        let got: Vec<_> = eligible(&packs, profile)
            .unwrap()
            .into_iter()
            .map(|c| c.path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(got, ["reactive.milk"], "{profile}");
    }
}
