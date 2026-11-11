//! The binding profile: raw MIDI bytes in, a verb out.
//!
//! These tests are the grammar's specification. The committed profile for the
//! Numark MixTrack Go is checked line by line against the parser, so a profile
//! that parses but binds nothing cannot pass.

use projectmp4::control::{Action, StemSource};
use projectmp4::midi::profile::Profile;
use std::path::PathBuf;

fn committed() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("profiles/midi/numark-mixtrack-go.toml")
}

#[test]
fn the_committed_mixtrack_go_profile_parses() {
    let p = Profile::load(&committed()).expect("the shipped profile must parse");
    assert_eq!(p.name(), "Numark MixTrack Go");
    // Enough bindings that a parser silently dropping lines is caught.
    assert!(p.binding_count() > 50, "only {} bindings parsed", p.binding_count());
}

/// Every address the shipped profile names must actually produce a verb.
///
/// Without this a profile is decoration: it parses, and nothing is bound. The
/// addresses are re-read from the file here rather than listed, so adding a
/// line to the profile extends this test automatically.
#[test]
fn every_binding_in_the_committed_profile_resolves() {
    let text = std::fs::read_to_string(committed()).unwrap();
    let mut p = Profile::load(&committed()).unwrap();
    let mut checked = 0;
    for line in text.lines() {
        let line = line.trim();
        let Some((addr, rest)) = line.split_once('=') else { continue };
        let (addr, verb) = (addr.trim(), rest.trim());
        let Some((status, number)) = addr.split_once(':') else { continue };
        let Ok(status) = u8::from_str_radix(status.trim().trim_start_matches("0x"), 16) else { continue };
        let Ok(number) = u8::from_str_radix(number.trim().trim_start_matches("0x"), 16) else { continue };
        checked += 1;

        // Each kind of binding has its own idea of a value that means something,
        // so one probe byte cannot serve them all.
        if verb.contains("msb") {
            // Half of a 14-bit number. Firing here would send the parameter to
            // whatever the stale low seven bits happened to be.
            assert!(p.resolve([status, number, 0x40]).is_none(), "{addr}: an MSB alone must not resolve ({verb})");
            continue;
        }
        if verb.contains("lsb") {
            // Prime the pair, then the LSB completes it.
            let msb = number.wrapping_sub(0x20);
            assert!(p.resolve([status, msb, 0x40]).is_none(), "{addr}: priming MSB resolved early");
            assert!(p.resolve([status, number, 0x40]).is_some(), "{addr}: a primed LSB resolved to nothing ({verb})");
            continue;
        }
        // 0x40 is the one value a relative encoder treats as no movement, because
        // this device family disagrees about its sign. Turn it a click instead.
        let value = if verb.contains("relative") { 0x01 } else { 0x40 };
        assert!(
            p.resolve([status, number, value]).is_some(),
            "{addr} is in the profile but resolves to nothing ({verb})"
        );
    }
    assert!(checked > 50, "only {checked} address lines were found; the parser or this test is wrong");
}

#[test]
fn absolute_controls_span_their_whole_range() {
    // `drive 0.0 2.0` on the deck 1 filter knob.
    let mut p = Profile::parse("name = \"t\"\n0xB0:0x1A = drive 0.0 2.0\n").unwrap();
    assert_eq!(p.resolve([0xB0, 0x1A, 0]), Some(Action::Drive(0.0)));
    assert_eq!(p.resolve([0xB0, 0x1A, 127]), Some(Action::Drive(2.0)));
    match p.resolve([0xB0, 0x1A, 64]) {
        Some(Action::Drive(v)) => assert!((v - 1.0).abs() < 0.02, "midpoint was {v}"),
        other => panic!("{other:?}"),
    }
}

#[test]
fn stem_blend_maps_the_fader_onto_zero_to_one() {
    let mut p = Profile::parse("name = \"t\"\n0xBF:0x08 = stem-blend\n").unwrap();
    assert_eq!(p.resolve([0xBF, 0x08, 0]), Some(Action::StemBlend(0.0)));
    assert_eq!(p.resolve([0xBF, 0x08, 127]), Some(Action::StemBlend(1.0)));
}

#[test]
fn a_fourteen_bit_pair_resolves_once_with_the_joined_value() {
    let mut p = Profile::parse(
        "name = \"t\"\n0xB0:0x09 = drive-14 msb 0.0 2.0\n0xB0:0x29 = drive-14 lsb\n",
    )
    .unwrap();
    // The MSB alone is half a number: it must not fire.
    assert_eq!(p.resolve([0xB0, 0x09, 0x7F]), None, "an MSB alone fired");
    // 0x7F << 7 | 0x7F == 16383, the top of the 14-bit range.
    match p.resolve([0xB0, 0x29, 0x7F]) {
        Some(Action::Drive(v)) => assert!((v - 2.0).abs() < 1e-6, "top of range gave {v}"),
        other => panic!("{other:?}"),
    }
    // And the bottom.
    assert_eq!(p.resolve([0xB0, 0x09, 0x00]), None);
    assert_eq!(p.resolve([0xB0, 0x29, 0x00]), Some(Action::Drive(0.0)));
}

#[test]
fn a_relative_encoder_signs_both_directions() {
    let mut p = Profile::parse("name = \"t\"\n0xB0:0x06 = step-preset relative\n").unwrap();
    assert_eq!(p.resolve([0xB0, 0x06, 0x01]), Some(Action::StepPreset { forward: true }));
    assert_eq!(p.resolve([0xB0, 0x06, 0x7F]), Some(Action::StepPreset { forward: false }));
}

#[test]
fn a_button_fires_on_note_on_only() {
    let mut p = Profile::parse("name = \"t\"\n0x90:0x00 = transport-toggle\n").unwrap();
    assert_eq!(p.resolve([0x90, 0x00, 0x7F]), Some(Action::TransportToggle));
    // A note-on with velocity 0 is a note-off by convention, and a note-off
    // status is one outright. Neither may re-fire a momentary button.
    assert_eq!(p.resolve([0x90, 0x00, 0x00]), None, "velocity 0 fired the button");
    assert_eq!(p.resolve([0x80, 0x00, 0x00]), None, "note-off fired the button");
}

#[test]
fn stem_snaps_name_their_source() {
    let mut p = Profile::parse(
        "name = \"t\"\n0x90:0x46 = stem-snap vocals\n0x90:0x48 = stem-snap instrumental\n0x90:0x47 = stem-snap all\n",
    )
    .unwrap();
    assert_eq!(p.resolve([0x90, 0x46, 0x7F]), Some(Action::StemSnap(StemSource::Vocals)));
    assert_eq!(p.resolve([0x90, 0x48, 0x7F]), Some(Action::StemSnap(StemSource::Instrumental)));
    assert_eq!(p.resolve([0x90, 0x47, 0x7F]), Some(Action::StemSnap(StemSource::All)));
}

#[test]
fn jump_preset_carries_its_slot() {
    let mut p = Profile::parse("name = \"t\"\n0x94:0x17 = jump-preset 3\n").unwrap();
    assert_eq!(p.resolve([0x94, 0x17, 0x7F]), Some(Action::JumpPreset { slot: 3 }));
}

#[test]
fn seek_by_carries_its_sign() {
    let mut p =
        Profile::parse("name = \"t\"\n0x90:0x01 = seek-by -10.0\n0x90:0x05 = seek-by 10.0\n").unwrap();
    assert_eq!(p.resolve([0x90, 0x01, 0x7F]), Some(Action::SeekBy { secs: -10.0 }));
    assert_eq!(p.resolve([0x90, 0x05, 0x7F]), Some(Action::SeekBy { secs: 10.0 }));
}

#[test]
fn an_unbound_address_is_ignored() {
    let mut p = Profile::parse("name = \"t\"\n0x90:0x00 = transport-toggle\n").unwrap();
    assert_eq!(p.resolve([0x90, 0x7E, 0x7F]), None);
    assert_eq!(p.resolve([0x91, 0x00, 0x7F]), None, "a different channel is a different address");
}

// --- what must be refused, and with a message that says why -----------------

fn refused(text: &str, wanted: &str) {
    let Err(e) = Profile::parse(text) else { panic!("accepted {text:?}") };
    assert!(
        e.0.to_lowercase().contains(wanted),
        "message {:?} does not mention {wanted:?}",
        e.0
    );
}

#[test]
fn a_duplicate_address_is_refused() {
    // Two verbs on one address is unresolvable, and silently keeping the last
    // would make a typo behave as a working binding.
    refused("name = \"t\"\n0x90:0x00 = transport-toggle\n0x90:0x00 = resync\n", "0x90:0x00");
}

#[test]
fn an_lsb_without_its_msb_is_refused() {
    refused("name = \"t\"\n0xB0:0x29 = drive-14 lsb\n", "msb");
}

#[test]
fn an_msb_without_its_lsb_is_refused() {
    refused("name = \"t\"\n0xB0:0x09 = drive-14 msb 0.0 2.0\n", "lsb");
}

#[test]
fn an_inverted_or_empty_range_is_refused() {
    refused("name = \"t\"\n0xB0:0x1A = drive 2.0 0.0\n", "range");
    refused("name = \"t\"\n0xB0:0x1A = drive 1.0 1.0\n", "range");
}

#[test]
fn an_unknown_verb_is_refused_naming_it() {
    refused("name = \"t\"\n0x90:0x00 = teleport\n", "teleport");
}

#[test]
fn a_malformed_address_is_refused() {
    refused("name = \"t\"\nnot-an-address = resync\n", "not-an-address");
    refused("name = \"t\"\n0xZZ:0x00 = resync\n", "0xzz");
}

#[test]
fn a_profile_without_a_name_is_refused() {
    refused("0x90:0x00 = resync\n", "name");
}

#[test]
fn a_status_byte_that_is_not_channel_voice_is_refused() {
    // 0xF0 is system-exclusive: it carries no channel and no controller sends
    // a binding on it, so a profile naming it is a mistake worth catching.
    refused("name = \"t\"\n0xF0:0x00 = resync\n", "0xf0");
}
