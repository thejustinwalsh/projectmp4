use std::{ffi::{c_char, c_void, CStr}, fs::File, panic::{catch_unwind, AssertUnwindSafe}, path::Path, ptr};
use symphonia::core::{
    codecs::{audio::AudioDecoderOptions, registry::CodecRegistry},
    formats::{probe::Hint, FormatOptions, TrackType}, io::MediaSourceStream, meta::MetadataOptions,
};
use symphonia_adapter_libopus::OpusDecoder;
pub mod loudness;

#[repr(C)]
pub struct DecodedAudio { pub samples: *const f32, pub frames: u64, pub owner: *mut c_void }

// This decoder handles the actual 48 kHz Opus-in-MP4 Suno player representation.
// Other formats/rates remain with the existing decoder until the canonical
// cross-platform resampler is connected. Never silently change the time base.
fn visit_channels(path: &Path, mut visit: impl FnMut(&[f32], usize) -> Result<(), Box<dyn std::error::Error>>) -> Result<u64, Box<dyn std::error::Error>> {
    let source = MediaSourceStream::new(Box::new(File::open(path)?), Default::default());
    let mut hint = Hint::new();
    if let Some(extension) = path.extension().and_then(|value| value.to_str()) { hint.with_extension(extension); }
    let mut format = symphonia::default::get_probe().probe(&hint, source, FormatOptions::default(), MetadataOptions::default())?;
    let track = format.first_track_known_codec(TrackType::Audio).ok_or("No audio track")?;
    let id = track.id;
    let params = track.codec_params.as_ref().and_then(|value| value.audio()).ok_or("No audio parameters")?;
    let mut codecs = CodecRegistry::new();
    symphonia::default::register_enabled_codecs(&mut codecs);
    codecs.register_audio_decoder::<OpusDecoder>();
    let mut decoder = codecs.make_audio_decoder(params, &AudioDecoderOptions::default())?;
    let mut frames = 0_u64;
    let mut packet_samples: Vec<f32> = Vec::new();
    while let Some(packet) = format.next_packet()? {
        if packet.track_id != id { continue; }
        let decoded = decoder.decode(&packet)?;
        let channels = decoded.spec().channels().count();
        if decoded.spec().rate() != 48000 || !(1..=2).contains(&channels) {
            return Err("Opus deck decoding requires 48 kHz mono or stereo".into());
        }
        decoded.copy_to_vec_interleaved(&mut packet_samples);
        let incoming = packet_samples.len() / channels;
        if frames + incoming as u64 > 48000 * 60 * 20 { return Err("Track exceeds 20 minutes".into()); }
        visit(&packet_samples, channels)?;
        frames += incoming as u64;
    }
    if frames < 2 { return Err("No decoded audio samples".into()); }
    Ok(frames)
}

fn decode_channels(path: &Path, mono: bool) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    let mut samples = Vec::new();
    visit_channels(path, |packet_samples, channels| {
        for frame in packet_samples.chunks_exact(channels) {
            if mono {
                let value = frame.iter().sum::<f32>() / channels as f32;
                samples.push(if value.is_finite() { value } else { 0.0 });
                continue;
            }
            for channel in 0..2 {
                let sample = frame[channel.min(channels - 1)];
                samples.push(if sample.is_finite() { sample } else { 0.0 });
            }
        }
        Ok(())
    })?;
    if samples.len() < 4 { return Err("No decoded audio samples".into()); }
    Ok(samples)
}

/// Streaming stereo packets share playback's exact decoder/time origin. The
/// borrowed buffer is reused after the callback; no whole-track PCM is retained.
pub fn visit_stereo(path: &Path, mut visit: impl FnMut(&[f32]) -> Result<(), Box<dyn std::error::Error>>) -> Result<u64, Box<dyn std::error::Error>> {
    let mut stereo = Vec::new();
    visit_channels(path, |packet, channels| {
        stereo.clear();
        for frame in packet.chunks_exact(channels) {
            for channel in 0..2 {
                let value = frame[channel.min(channels - 1)];
                stereo.push(if value.is_finite() { value } else { 0. });
            }
        }
        visit(&stereo)
    })
}

/// Same 48 kHz packet trimming and time origin as native deck playback.
pub fn decode_mono(path: &Path) -> Result<Vec<f32>, Box<dyn std::error::Error>> {
    decode_channels(path, true)
}

/// Caller supplies a null-terminated UTF-8 path and writable output/error buffers.
/// Owns returned PCM until deck_decode_free(owner). Run on a decode worker only.
#[no_mangle]
pub unsafe extern "C" fn deck_decode_file(path: *const c_char, output: *mut DecodedAudio, error: *mut u8, capacity: usize) -> i32 {
    if path.is_null() || output.is_null() { return -1; }
    ptr::write(output, DecodedAudio { samples: ptr::null(), frames: 0, owner: ptr::null_mut() });
    let result = catch_unwind(AssertUnwindSafe(|| {
        let path = CStr::from_ptr(path).to_str().map_err(|error| error.to_string())?;
        decode_channels(Path::new(path), false).map_err(|error| error.to_string())
    }));
    match result {
        Ok(Ok(samples)) => {
            let samples = Box::new(samples);
            ptr::write(output, DecodedAudio { samples: samples.as_ptr(), frames: (samples.len() / 2) as u64, owner: Box::into_raw(samples).cast() });
            0
        }
        failure => {
            let message = match failure { Ok(Err(message)) => message, _ => "Native decoder panic".to_owned() };
            if !error.is_null() && capacity > 0 {
                let size = message.len().min(capacity - 1);
                ptr::copy_nonoverlapping(message.as_ptr(), error, size); *error.add(size) = 0;
            }
            -1
        }
    }
}

/// Release PCM only after its audio callback references have been retired.
#[no_mangle]
pub unsafe extern "C" fn deck_decode_free(owner: *mut c_void) {
    if !owner.is_null() { drop(Box::from_raw(owner.cast::<Vec<f32>>())); }
}
