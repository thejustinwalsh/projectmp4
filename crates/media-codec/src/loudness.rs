//! Stereo mastered-audio measurements. No model or complete-track PCM buffer.
use ebur128::{EbuR128, Mode};
use serde_json::{json, Value};
use std::path::Path;
type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;
const BIN_FRAMES: usize = 4800;

pub struct LoudnessScan {
    meter: EbuR128,
    bin_frames: usize,
    frames: u64,
    powers: Vec<f64>,
}
impl LoudnessScan {
    pub fn new() -> Result<Self> {
        Ok(Self { meter: EbuR128::new(2, 48000, Mode::I | Mode::TRUE_PEAK)?,
            bin_frames: 0, frames: 0, powers: Vec::new() })
    }
    fn bin_power(&self) -> Result<f64> {
        let lufs = self.meter.loudness_window(100)?;
        Ok(if lufs.is_finite() { 10_f64.powf((lufs + 0.691) / 10.) } else { 0. })
    }
    pub fn add(&mut self, mut stereo: &[f32]) -> Result<()> {
        if stereo.len() % 2 != 0 || stereo.iter().any(|x| !x.is_finite()) {
            return Err("Loudness needs finite interleaved stereo".into());
        }
        while !stereo.is_empty() {
            let n = (BIN_FRAMES - self.bin_frames).min(stereo.len() / 2);
            self.meter.add_frames_f32(&stereo[..n * 2])?;
            self.bin_frames += n; self.frames += n as u64;
            if self.frames > 48000 * 1200 { return Err("Loudness source exceeds 20 minutes".into()); }
            stereo = &stereo[n * 2..];
            if self.bin_frames == BIN_FRAMES {
                self.powers.push(self.bin_power()?); self.bin_frames = 0;
            }
        }
        Ok(())
    }
    pub fn finish(mut self) -> Result<Value> {
        if self.frames < 2 { return Err("Empty loudness source".into()); }
        let loudness = self.meter.loudness_global()?;
        let integrated = if loudness.is_finite() { Some(loudness) } else { None };
        // Capture gated programme loudness before padding. A final 100 ms energy
        // bin is zero-padded; flush FIR tails only for true-peak scanning.
        if self.bin_frames > 0 {
            self.meter.add_frames_f32(&vec![0.; (BIN_FRAMES - self.bin_frames) * 2])?;
            self.powers.push(self.bin_power()?);
        }
        self.meter.add_frames_f32(&[0.; 256])?;
        let true_peak = self.meter.true_peak(0)?.max(self.meter.true_peak(1)?);
        let sample_peak = self.meter.sample_peak(0)?.max(self.meter.sample_peak(1)?);
        Ok(json!({"version":1,"algorithm":"ebur128-0.1.10-stereo-48k-v1","frames":self.frames,
            "sampleRate":48000,"stepSeconds":0.1,"weightedPower":self.powers,
            "integratedLufs":integrated,"truePeak":true_peak,"samplePeak":sample_peak}))
    }
}
pub fn scan(path: &Path) -> Result<Value> {
    let mut meter = LoudnessScan::new()?;
    let frames = crate::visit_stereo(path, |pcm| meter.add(pcm))?;
    if frames != meter.frames { return Err("Loudness decoder frame mismatch".into()); }
    meter.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn signal(seconds: usize, amplitude: f32, anti_phase: bool) -> Vec<f32> {
        (0..seconds * 48000).flat_map(|i| {
            let x = (std::f64::consts::TAU * 1000. * i as f64 / 48000.).sin() as f32 * amplitude;
            [x, if anti_phase { -x } else { x }]
        }).collect()
    }
    fn measure(pcm: &[f32], chunk: usize) -> Value {
        let mut scan = LoudnessScan::new().unwrap();
        for part in pcm.chunks(chunk * 2) { scan.add(part).unwrap(); }
        scan.finish().unwrap()
    }
    #[test]
    fn stereo_reference_gain_and_packet_partition() {
        // Tech 3341 stereo 1 kHz sine, -23 dBFS peak -> approximately -23 LUFS.
        let pcm = signal(8, 10_f32.powf(-23. / 20.), false);
        let a = measure(&pcm, 511);
        assert!((a["integratedLufs"].as_f64().unwrap() + 23.).abs() < 0.1);
        assert_eq!(a, measure(&pcm, 123));
        let anti = measure(&signal(8, 10_f32.powf(-23. / 20.), true), 127);
        assert_eq!(a, anti);
        let quiet = measure(&signal(8, 10_f32.powf(-29. / 20.), false), 127);
        assert!((quiet["integratedLufs"].as_f64().unwrap() - a["integratedLufs"].as_f64().unwrap() + 6.).abs() < 0.001);
    }
    #[test]
    fn inter_sample_peaks_and_gating() {
        // fs/4 sine at pi/4: sampled peaks are 3.01 dB below its reconstructed peak.
        let pcm: Vec<f32> = (0..4 * 48000).flat_map(|i| {
            let x = (std::f64::consts::TAU * 12000. * i as f64 / 48000. + std::f64::consts::FRAC_PI_4).sin() as f32 * 0.8;
            [x, x]
        }).collect();
        let peak = measure(&pcm, 127);
        assert!((peak["samplePeak"].as_f64().unwrap() - 0.8 / 2_f64.sqrt()).abs() < 1e-5);
        assert!(peak["truePeak"].as_f64().unwrap() > 0.77);
        assert!(peak["truePeak"].as_f64().unwrap() < 0.86);
        // Relative gating excludes the long quiet tail rather than averaging its dB.
        let mut gated = signal(8, 10_f32.powf(-23. / 20.), false);
        gated.extend(signal(8, 10_f32.powf(-63. / 20.), false));
        let result = measure(&gated, 511);
        assert!((result["integratedLufs"].as_f64().unwrap() + 23.).abs() < 0.2);
    }
    #[test]
    fn silence_and_partial_tail_are_finite_and_do_not_invent_loudness() {
        let a = measure(&vec![0.; 96014], 123);
        assert!(a["integratedLufs"].is_null());
        assert_eq!(a["truePeak"], 0.);
        assert_eq!(a["frames"], 48007);
        assert_eq!(a["weightedPower"].as_array().unwrap().len(), 11);
        assert!(a["weightedPower"].as_array().unwrap().iter().all(|p| p.as_f64() == Some(0.)));
    }
}
