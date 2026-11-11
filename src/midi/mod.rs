//! MIDI input from a class-compliant controller, with a bounded queue the
//! render loop drains once per frame without blocking.
//!
//! CoreMIDI delivers on its own thread. That thread pushes into a fixed-size
//! ring; the frame loop pops from it. Neither side allocates after `open`.

pub mod learn;
pub mod profile;

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use midir::os::unix::VirtualInput;
use midir::{MidiInput, MidiInputConnection, MidiOutput, MidiOutputConnection};

use crate::ffi::{Error, Result};

const CLIENT: &str = "projectmp4";

/// Queue capacity. When full the OLDEST message is dropped: a stale jog tick
/// is worthless, but a button press that never arrives is worse than a late
/// one, so the newest always survives.
const CAP: usize = 512;

struct Shared {
    queue: Mutex<VecDeque<[u8; 3]>>,
    dropped: AtomicU64,
}

impl Shared {
    fn new() -> Shared {
        Shared {
            queue: Mutex::new(VecDeque::with_capacity(CAP)),
            dropped: AtomicU64::new(0),
        }
    }

    /// Runs on the CoreMIDI thread. Never panics: no indexing, no unwrap, and
    /// a poisoned lock is recovered rather than propagated.
    fn push(&self, bytes: &[u8]) {
        let msg = match *bytes {
            [s, a, b] if (0x80..=0xEF).contains(&s) => [s, a, b],
            [s, a] if (0xC0..=0xDF).contains(&s) => [s, a, 0],
            _ => return, // SysEx, real-time, running-status fragments
        };
        let mut q = self.queue.lock().unwrap_or_else(|p| p.into_inner());
        if q.len() >= CAP {
            q.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        q.push_back(msg);
    }
}

enum Leds {
    Untried,
    Ready(MidiOutputConnection),
    Absent,
}

pub struct MidiIn {
    shared: Arc<Shared>,
    /// Held to keep the port open; dropping it closes the connection.
    _conn: MidiInputConnection<()>,
    /// Name of the chosen input, reused to find its output port.
    match_name: Option<String>,
    leds: Leds,
}

fn names(input: &MidiInput) -> Vec<String> {
    input
        .ports()
        .iter()
        .map(|p| input.port_name(p).unwrap_or_else(|_| "<unnamed>".into()))
        .collect()
}

fn client() -> Result<MidiInput> {
    MidiInput::new(CLIENT).map_err(|e| Error::msg(format!("cannot open the MIDI system: {e}")))
}

impl MidiIn {
    /// Every visible MIDI source, for `midi-learn --list` and for error messages.
    pub fn ports() -> Result<Vec<String>> {
        Ok(names(&client()?))
    }

    /// Open the first source whose name contains `name_contains`, or the first
    /// source at all when it is `None`.
    pub fn open(name_contains: Option<&str>) -> Result<MidiIn> {
        let input = client()?;
        let available = names(&input);
        let listing = || {
            if available.is_empty() {
                "none".to_string()
            } else {
                available.join(", ")
            }
        };
        let ports = input.ports();
        let found = ports.iter().zip(&available).find(|(_, n)| match name_contains {
            Some(want) => n.contains(want),
            None => true,
        });
        let Some((port, chosen)) = found else {
            return Err(Error::msg(match name_contains {
                Some(want) => format!(
                    "no MIDI source whose name contains {want:?}; available sources: {}",
                    listing()
                ),
                None => format!("no MIDI source is connected; available sources: {}", listing()),
            }));
        };
        let shared = Arc::new(Shared::new());
        let cb = Arc::clone(&shared);
        let conn = input
            .connect(port, "projectmp4-in", move |_stamp, bytes, _| cb.push(bytes), ())
            .map_err(|e| Error::msg(format!("cannot open MIDI source: {e}")))?;
        Ok(MidiIn {
            shared,
            _conn: conn,
            match_name: Some(chosen.clone()),
            leds: Leds::Untried,
        })
    }

    /// Open a virtual source under this name. Tests use it; it is also how a
    /// user can drive the bindings from another app.
    pub fn open_virtual(name: &str) -> Result<MidiIn> {
        let input = client()?;
        let shared = Arc::new(Shared::new());
        let cb = Arc::clone(&shared);
        let conn = input
            .create_virtual(name, move |_stamp, bytes, _| cb.push(bytes), ())
            .map_err(|e| Error::msg(format!("cannot create virtual MIDI source {name:?}: {e}")))?;
        Ok(MidiIn {
            shared,
            _conn: conn,
            match_name: None,
            // Our own virtual port is also a destination; never light it.
            leds: Leds::Absent,
        })
    }

    /// Hand every queued message to `f`, oldest first, and empty the queue.
    /// Allocation-free: the ring is preallocated and each pop holds the lock
    /// only for one element, so `f` never runs under it.
    pub fn drain(&mut self, mut f: impl FnMut([u8; 3])) {
        loop {
            let next = {
                let mut q = self.shared.queue.lock().unwrap_or_else(|p| p.into_inner());
                q.pop_front()
            };
            match next {
                Some(m) => f(m),
                None => break,
            }
        }
    }

    /// Messages dropped because the queue was full, since the last call.
    pub fn overflowed(&mut self) -> u64 {
        self.shared.dropped.swap(0, Ordering::Relaxed)
    }

    /// Light an LED: velocity 0x00 off, 0x01 dim, 0x7F bright on this device
    /// family. No matching output port is not an error: returns false.
    pub fn led(&mut self, status: u8, number: u8, velocity: u8) -> bool {
        if let Leds::Untried = self.leds {
            self.leds = self.connect_output();
        }
        match &mut self.leds {
            Leds::Ready(out) => out.send(&[status, number & 0x7F, velocity & 0x7F]).is_ok(),
            _ => false,
        }
    }

    fn connect_output(&self) -> Leds {
        let Ok(out) = MidiOutput::new(CLIENT) else {
            return Leds::Absent;
        };
        let port = out.ports().into_iter().find(|p| {
            let n = out.port_name(p).unwrap_or_default();
            match &self.match_name {
                Some(want) => n.contains(want.as_str()),
                None => false,
            }
        });
        match port.and_then(|p| out.connect(&p, "projectmp4-led").ok()) {
            Some(c) => Leds::Ready(c),
            None => Leds::Absent,
        }
    }
}
