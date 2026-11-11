use midir::{MidiOutput, MidiOutputConnection};
use projectmp4::midi::MidiIn;
use std::time::{Duration, Instant};

fn unique(test: &str) -> String {
    format!("projectmp4-{}-{test}", std::process::id())
}

fn sender(name: &str) -> MidiOutputConnection {
    let out = MidiOutput::new("test-out").unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        if let Some(p) = out
            .ports()
            .into_iter()
            .find(|p| out.port_name(p).unwrap_or_default().contains(name))
        {
            return out.connect(&p, "test").unwrap();
        }
        assert!(Instant::now() < deadline, "virtual port {name} never appeared");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Poll until `want` messages arrived (or the deadline passes), collecting them.
fn collect(m: &mut MidiIn, want: usize) -> Vec<[u8; 3]> {
    let mut got = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(3);
    while got.len() < want && Instant::now() < deadline {
        m.drain(|x| got.push(x));
        std::thread::sleep(Duration::from_millis(5));
    }
    got
}

#[test]
fn a_virtual_port_round_trips_three_byte_messages() {
    let n = unique("round_trip");
    let mut m = MidiIn::open_virtual(&n).unwrap();
    let mut s = sender(&n);
    s.send(&[0x90, 0x00, 0x7F]).unwrap();
    s.send(&[0xB0, 0x09, 0x40]).unwrap();
    assert_eq!(collect(&mut m, 2), vec![[0x90, 0, 0x7F], [0xB0, 9, 0x40]]);
}

#[test]
fn a_full_queue_drops_the_oldest_and_keeps_the_newest() {
    let n = unique("overflow");
    let mut m = MidiIn::open_virtual(&n).unwrap();
    let mut s = sender(&n);
    // Distinguishable: data1 = i % 128, data2 = i / 128. Do not drain until all arrive.
    for i in 0..600usize {
        s.send(&[0xB0, (i % 128) as u8, (i / 128) as u8]).unwrap();
    }
    // Wait for the callback to have seen all 600: dropped + queued == 600.
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut dropped = 0;
    let mut got = Vec::new();
    while Instant::now() < deadline {
        dropped += m.overflowed();
        if dropped >= 88 {
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    m.drain(|x| got.push(x));
    dropped += m.overflowed();
    assert_eq!(got.len(), 512);
    assert_eq!(dropped, 88);
    assert!(got.contains(&[0xB0, (599 % 128) as u8, (599 / 128) as u8]));
    assert!(!got.contains(&[0xB0, 0, 0]));
    assert_eq!(got[0], [0xB0, 88, 0]);
}

#[test]
fn sysex_and_realtime_are_never_queued() {
    let n = unique("sysex");
    let mut m = MidiIn::open_virtual(&n).unwrap();
    let mut s = sender(&n);
    s.send(&[0xF0, 0x7E, 0xF7]).unwrap();
    s.send(&[0xF8]).unwrap();
    s.send(&[0x90, 0x01, 0x02]).unwrap();
    assert_eq!(collect(&mut m, 2), vec![[0x90, 1, 2]]);
}

#[test]
fn a_two_byte_message_is_padded() {
    let n = unique("two_byte");
    let mut m = MidiIn::open_virtual(&n).unwrap();
    let mut s = sender(&n);
    s.send(&[0xC0, 0x05]).unwrap();
    assert_eq!(collect(&mut m, 1), vec![[0xC0, 5, 0]]);
}

#[test]
fn an_unmatched_port_name_is_an_error_that_lists_what_was_there() {
    let n = unique("listing");
    // A virtual *input* is a destination, so it is not a source. Publish a real
    // source (a virtual output) for the error to list.
    use midir::os::unix::VirtualOutput;
    let _keep = MidiOutput::new("test-src").unwrap().create_virtual(&n).unwrap();
    let e = MidiIn::open(Some("no-such-device-xyz")).err().expect("must fail");
    let msg = e.to_string();
    assert!(msg.contains("no-such-device-xyz"), "{msg}");
    assert!(msg.contains(&n), "{msg}");
}

#[test]
fn drain_after_the_connection_drops_is_empty_and_does_not_panic() {
    let n = unique("drop");
    let mut m = MidiIn::open_virtual(&n).unwrap();
    let s = sender(&n);
    drop(s);
    let mut count = 0;
    m.drain(|_| count += 1);
    assert_eq!(count, 0);
    assert_eq!(m.overflowed(), 0);
    assert!(!m.led(0x90, 0, 0x7F));
}
