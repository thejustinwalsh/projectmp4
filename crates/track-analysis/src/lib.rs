mod features;
pub mod scheduling;
use beat_this::{calculate_bpm, BeatThis, RtenRuntime};
use serde_json::{json, Value};
use std::{
    ffi::{c_char, CStr},
    path::Path,
    time::Instant,
};

#[repr(C)]
struct Options {
    abi: u32,
    frames: u32,
    beats: u32,
    min_loop: u32,
    max_loop: u32,
    test: u32,
    disable_pruning: u32,
    candidates: u32,
    checks: u64,
    score_frames: u64,
}
#[repr(C)]
#[derive(Clone, Copy, Default)]
struct Pair {
    start: u32,
    end: u32,
    note: f64,
    loudness: f64,
    score: f64,
}
extern "C" {
    fn deck_loops_rank_dj(
        chroma: *const f64,
        power: *const f64,
        beats: *const u32,
        options: *const Options,
        output: *mut Pair,
        capacity: u32,
        count: *mut u32,
        error: *mut c_char,
        error_capacity: u32,
    ) -> i32;
}

pub fn analyze(models: &Path, input: &Path) -> Result<Value, Box<dyn std::error::Error>> {
    let started = Instant::now();
    let pcm = deck_media_codec::decode_mono(input)?;
    let duration = pcm.len() as f64 / 48000.;
    let peak = pcm.iter().fold(0_f32, |max, sample| max.max(sample.abs()));
    if peak < 1e-6 {
        return Err("Silent audio has no usable beat/loop suggestions".into());
    }
    // Drop the RTen model/arenas before feature extraction. Only one model job
    // runs in the app; the process is cancellable independently of playback.
    let result = {
        let mut tracker = BeatThis::new(
            &RtenRuntime,
            &models.join("mel_spectrogram.onnx"),
            &models.join("beat_this_small.onnx"),
        )?;
        tracker.analyze_audio(&pcm, 48000)?
    };
    let bpm = calculate_bpm(&result).unwrap_or(0.);
    let features = features::extract(&pcm);
    assemble(
        duration,
        bpm,
        &result.beats,
        &result.downbeats,
        features,
        started,
    )
}

fn assemble(
    duration: f64,
    bpm: f32,
    beats: &[f32],
    downbeat_values: &[f32],
    features: features::Features,
    started: Instant,
) -> Result<Value, Box<dyn std::error::Error>> {
    let mut frames = Vec::<u32>::new();
    let mut times = Vec::<f64>::new();
    for beat in beats {
        let time = f64::from(*beat);
        if !time.is_finite() || time < 0. || time >= duration {
            continue;
        }
        let frame = (time * 48000. / features::HOP as f64).round() as u32;
        if frame as usize >= features.power.len()
            || frames.last().is_some_and(|previous| frame <= *previous)
        {
            continue;
        }
        frames.push(frame);
        times.push(time);
    }
    let mut loops = Vec::new();
    let mut candidates_scored = 0;
    let mut best_loop_score: f64 = 0.;
    if frames.len() >= 5 {
        let test = if bpm > 0. {
            ((12. * 60. / f64::from(bpm)) * 48000. / features::HOP as f64).floor() as u32
        } else {
            562
        };
        let options = Options {
            abi: 1,
            frames: features.power.len() as u32,
            beats: frames.len() as u32,
            min_loop: 1,
            max_loop: features.power.len() as u32,
            test: test.clamp(1, 4096),
            disable_pruning: 0,
            candidates: 100000,
            checks: 10000000,
            score_frames: 100000000,
        };
        let mut pairs = vec![Pair::default(); 100000];
        let mut count = 0;
        let mut error = [0_i8; 256];
        let status = unsafe {
            deck_loops_rank_dj(
                features.chroma.as_ptr(),
                features.power.as_ptr(),
                frames.as_ptr(),
                &options,
                pairs.as_mut_ptr(),
                pairs.len() as u32,
                &mut count,
                error.as_mut_ptr(),
                error.len() as u32,
            )
        };
        if status != 0 {
            return Err(unsafe { CStr::from_ptr(error.as_ptr()) }
                .to_string_lossy()
                .into_owned()
                .into());
        }
        candidates_scored = count;
        if count > 0 {
            best_loop_score = pairs[0].score;
        }
        for pair in pairs
            .into_iter()
            .take(count as usize)
            .filter(|pair| pair.score >= 0.8)
            .take(24)
        {
            let start_index = frames.binary_search(&pair.start).unwrap();
            let end_index = frames.binary_search(&pair.end).unwrap();
            // Musical boundaries remain on the neural grid. The mixer applies
            // a short join crossfade; no GPL-derived zero-crossing code is used.
            loops.push(json!({"start":times[start_index],"end":times[end_index],
                "startSample":(times[start_index]*48000.).round() as u64,
                "endSample":(times[end_index]*48000.).round() as u64,
                "beats":end_index-start_index,"score":pair.score,"noteDistance":pair.note,
                "loudnessDifference":pair.loudness,"seamCrossfadeFrames":144}));
        }
    }
    let local_bpm: Vec<_> = times
        .windows(2)
        .filter_map(|window| {
            let value = 60. / (window[1] - window[0]);
            if (40. ..=300.).contains(&value) {
                Some(json!({"time":window[0],"bpm":value}))
            } else {
                None
            }
        })
        .collect();
    let downbeats: Vec<_> = downbeat_values
        .iter()
        .copied()
        .filter(|time| time.is_finite() && *time >= 0. && f64::from(*time) < duration)
        .collect();
    let cues: Vec<_> = downbeats.iter().take(4).copied().collect();
    Ok(
        json!({"version":2,"engine":"beat-this-rs/1.1.0-small1-rten","features":"dj-chroma-a440-48k-v1",
        "loopEngine":"pymusiclooper-78313f8-dj-v1","sampleRate":48000,"duration":duration,
        "bpm":bpm,"beats":times,"downbeats":downbeats,"localBpm":local_bpm,"cues":cues,
        "loops":loops,"loopCandidatesScored":candidates_scored,"bestLoopScore":best_loop_score,
        "confidence":null,"sections":null,"elapsedMs":started.elapsed().as_secs_f64()*1000.}),
    )
}

#[no_mangle]
pub extern "C" fn deck_analysis_version() -> u32 {
    1
}

/// Blocking worker-only C ABI. Paths are NUL-terminated UTF-8; all output storage
/// belongs to caller. No retained pointers or exceptions across the boundary.
#[no_mangle]
pub unsafe extern "C" fn deck_analyze_file(
    models: *const c_char,
    input: *const c_char,
    output: *mut u8,
    capacity: usize,
    written: *mut usize,
    error: *mut u8,
    error_capacity: usize,
) -> i32 {
    if !written.is_null() {
        *written = 0;
    }
    if !error.is_null() && error_capacity > 0 {
        *error = 0;
    }
    let result = std::panic::catch_unwind(|| -> Result<Vec<u8>, String> {
        if models.is_null() || input.is_null() || output.is_null() || written.is_null() {
            return Err("Missing analysis buffer".into());
        }
        let models = CStr::from_ptr(models)
            .to_str()
            .map_err(|error| error.to_string())?;
        let input = CStr::from_ptr(input)
            .to_str()
            .map_err(|error| error.to_string())?;
        let value =
            analyze(Path::new(models), Path::new(input)).map_err(|error| error.to_string())?;
        serde_json::to_vec(&value).map_err(|error| error.to_string())
    });
    match result {
        Ok(Ok(bytes)) if bytes.len() <= capacity => {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len());
            *written = bytes.len();
            0
        }
        other => {
            let message = match other {
                Ok(Ok(_)) => "Analysis output capacity exceeded".into(),
                Ok(Err(message)) => message,
                Err(_) => "Native analysis panic".into(),
            };
            if !error.is_null() && error_capacity > 0 {
                let size = message.len().min(error_capacity - 1);
                std::ptr::copy_nonoverlapping(message.as_ptr(), error, size);
                *error.add(size) = 0;
            }
            -1
        }
    }
}

/// One durable worker unit, preserving the original ABI. Return 1 means the
/// yielded JSON was written successfully; invoke again to continue this job.
#[no_mangle]
pub unsafe extern "C" fn deck_analyze_slice(
    models: *const c_char,
    input: *const c_char,
    checkpoint_dir: *const c_char,
    slice_ms: u64,
    output: *mut u8,
    capacity: usize,
    written: *mut usize,
    error: *mut u8,
    error_capacity: usize,
) -> i32 {
    if !written.is_null() {
        *written = 0;
    }
    if !error.is_null() && error_capacity > 0 {
        *error = 0;
    }
    let result = std::panic::catch_unwind(|| -> Result<(i32, Vec<u8>), String> {
        if models.is_null()
            || input.is_null()
            || checkpoint_dir.is_null()
            || output.is_null()
            || written.is_null()
        {
            return Err("Missing analysis buffer".into());
        }
        let models = CStr::from_ptr(models)
            .to_str()
            .map_err(|error| error.to_string())?;
        let input = CStr::from_ptr(input)
            .to_str()
            .map_err(|error| error.to_string())?;
        let checkpoint = CStr::from_ptr(checkpoint_dir)
            .to_str()
            .map_err(|error| error.to_string())?;
        let schedule = scheduling::Schedule {
            slice_ms,
            max_units: Some(1),
            cancel_file: None,
        };
        let (status, value) = match scheduling::analyze_resumable(
            Path::new(models),
            Path::new(input),
            Path::new(checkpoint),
            &schedule,
        ) {
            Ok(value) => (0, value),
            Err(error) => match error.downcast_ref::<scheduling::Yielded>() {
                Some(yielded) => (1, json!({"status":"yielded","reason":yielded.0})),
                None => return Err(error.to_string()),
            },
        };
        Ok((
            status,
            serde_json::to_vec(&value).map_err(|error| error.to_string())?,
        ))
    });
    match result {
        Ok(Ok((status, bytes))) if bytes.len() <= capacity => {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), output, bytes.len());
            *written = bytes.len();
            status
        }
        other => {
            let message = match other {
                Ok(Ok(_)) => "Analysis output capacity exceeded".into(),
                Ok(Err(message)) => message,
                Err(_) => "Native analysis panic".into(),
            };
            if !error.is_null() && error_capacity > 0 {
                let size = message.len().min(error_capacity - 1);
                std::ptr::copy_nonoverlapping(message.as_ptr(), error, size);
                *error.add(size) = 0;
            }
            -1
        }
    }
}

#[cfg(test)]
mod abi_tests {
    #[test]
    fn slice_rejects_null_paths_with_bounded_error_and_no_output() {
        let mut output = [99_u8; 8];
        let mut written = 123;
        let mut error = [99_u8; 4];
        let status = unsafe {
            super::deck_analyze_slice(
                std::ptr::null(),
                std::ptr::null(),
                std::ptr::null(),
                10,
                output.as_mut_ptr(),
                output.len(),
                &mut written,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        assert_eq!(status, -1);
        assert_eq!(written, 0);
        assert_eq!(output, [99; 8]);
        assert_eq!(error[3], 0);
    }
}
