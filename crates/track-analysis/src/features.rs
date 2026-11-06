use rustfft::{num_complex::Complex, FftPlanner};
pub const HOP: usize = 512;
const FFT: usize = 2048;
pub struct Features {
    pub chroma: Vec<f64>,
    pub power: Vec<f64>,
}

/// DJ front-end v1: centered periodic Hann, actual 48kHz pitch classes,
/// A-weighted peak power dB, per-frame infinity-normalized chroma. No trim:
/// feature, beat and sample origins agree. This differs from librosa's automatic
/// tuning/default-sr front-end; the PyMusicLooper ranking stage stays unchanged.
pub fn extract(pcm: &[f32]) -> Features {
    extract_range(pcm, 0, pcm.len() / HOP + 1)
}

/// Frame ranges retain the full PCM origin and centered halo, so splitting at
/// any frame produces exactly the same values as the one-shot front end.
pub fn extract_range(pcm: &[f32], start: usize, end: usize) -> Features {
    assert!(start <= end && end <= pcm.len() / HOP + 1);
    let frames = end - start;
    let mut chroma = vec![0.; frames * 12];
    let mut power = vec![-100.; frames];
    let fft = FftPlanner::<f64>::new().plan_fft_forward(FFT);
    let mut buffer = vec![Complex::new(0., 0.); FFT];
    let mut scratch = vec![Complex::new(0., 0.); fft.get_inplace_scratch_len()];
    let window: Vec<_> = (0..FFT)
        .map(|i| 0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / FFT as f64).cos())
        .collect();
    let mut filters = vec![[0.; 12]; FFT / 2 + 1];
    let mut a_weight = vec![0.; FFT / 2 + 1];
    for bin in 1..=FFT / 2 {
        let frequency = bin as f64 * 48000. / FFT as f64;
        let f2 = frequency * frequency;
        let ra = 12194.0_f64.powi(2) * f2 * f2
            / ((f2 + 20.6_f64.powi(2))
                * ((f2 + 107.7_f64.powi(2)) * (f2 + 737.9_f64.powi(2))).sqrt()
                * (f2 + 12194.0_f64.powi(2)));
        a_weight[bin] = ra * ra * 10.0_f64.powf(0.2);
        if !(40. ..=8000.).contains(&frequency) {
            continue;
        }
        let midi = 69. + 12. * (frequency / 440.).log2();
        for pitch in 0..12 {
            let distance = (midi - pitch as f64 + 6.).rem_euclid(12.) - 6.;
            filters[bin][pitch] = (-0.5 * (distance / 0.6).powi(2)).exp();
        }
    }
    for frame in 0..frames {
        for i in 0..FFT {
            let source = ((frame + start) * HOP + i) as isize - (FFT / 2) as isize;
            buffer[i] = Complex::new(
                if source >= 0 {
                    pcm.get(source as usize).copied().unwrap_or(0.) as f64 * window[i]
                } else {
                    0.
                },
                0.,
            );
        }
        fft.process_with_scratch(&mut buffer, &mut scratch);
        let mut peak: f64 = 1e-10;
        for bin in 1..=FFT / 2 {
            let magnitude = buffer[bin].norm_sqr();
            peak = peak.max(magnitude * a_weight[bin]);
            for pitch in 0..12 {
                chroma[frame * 12 + pitch] += magnitude * filters[bin][pitch];
            }
        }
        power[frame] = 10. * peak.log10();
        let max = chroma[frame * 12..frame * 12 + 12]
            .iter()
            .copied()
            .fold(0., f64::max);
        if max > 1e-10 {
            for pitch in 0..12 {
                chroma[frame * 12 + pitch] /= max;
            }
        }
    }
    Features { chroma, power }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn split_ranges_preserve_halo_and_origin() {
        let pcm: Vec<_> = (0..48000)
            .map(|i| ((i * 73 % 997) as f32 - 500.) / 500.)
            .collect();
        let whole = extract(&pcm);
        let mut split = Features {
            chroma: vec![],
            power: vec![],
        };
        for start in (0..whole.power.len()).step_by(7) {
            let chunk = extract_range(&pcm, start, (start + 7).min(whole.power.len()));
            split.chroma.extend(chunk.chroma);
            split.power.extend(chunk.power);
        }
        assert_eq!(whole.chroma, split.chroma);
        assert_eq!(whole.power, split.power);
    }
    #[test]
    fn pitch_timebase_and_silence() {
        let pcm: Vec<_> = (0..48000)
            .map(|i| (std::f64::consts::TAU * 440. * i as f64 / 48000.).sin() as f32)
            .collect();
        let result = extract(&pcm);
        assert_eq!(result.power.len(), 94);
        let pitches = &result.chroma[40 * 12..41 * 12];
        assert_eq!(
            pitches
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .unwrap()
                .0,
            9
        );
        assert_eq!(pitches[9], 1.);
        let silent = extract(&vec![0.; 48000]);
        assert!(silent.chroma.iter().all(|value| *value == 0.));
        assert!(silent.power.iter().all(|value| *value == -100.));
    }
}
