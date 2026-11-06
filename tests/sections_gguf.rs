use projectmp4::audio::sections::{Weights, SCHEMA};
use std::path::Path;

const MODEL: &str = "assets/models/harmonix-fold0-f32.gguf";

#[test]
fn the_vendored_model_matches_the_schema_exactly() {
    let w = Weights::load(Path::new(MODEL)).unwrap();
    assert_eq!(SCHEMA.len(), 491, "schema transcription lost entries");
    for (name, dims) in SCHEMA {
        let (data, shape) = w.tensor(name).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!(shape, *dims, "{name} shape");
        assert_eq!(data.len(), dims.iter().product::<usize>(), "{name} length");
        assert!(data.iter().all(|v| v.is_finite()), "{name} has non-finite weights");
    }
    assert_eq!(w.len(), 491, "model carries tensors the schema does not name");
}

#[test]
fn a_truncated_or_lying_gguf_is_an_error_not_a_panic() {
    let real = std::fs::read(MODEL).unwrap();

    // (a) the first 64 bytes only
    assert!(Weights::parse(&real[..64]).is_err());

    // (b) full header, body truncated to half
    assert!(Weights::parse(&real[..real.len() / 2]).is_err());

    // (c) every tensor-dimension field overwritten with u64::MAX, one at a time.
    // Locate the first tensor's name in the directory; its dims follow name, n_dims.
    let name = b"beat_classifier.classifier.bias";
    let at = real.windows(name.len()).position(|w| w == name).unwrap();
    let dims_at = at + name.len() + 4;
    let mut lying = real.clone();
    lying[dims_at..dims_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(Weights::parse(&lying).is_err());

    // (d) a count that would overflow when multiplied
    let mut huge = real.clone();
    huge[8..16].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(Weights::parse(&huge).is_err());

    // (e) two dims whose product overflows u64
    let mut overflow = real.clone();
    let ndims_at = at + name.len();
    overflow[ndims_at..ndims_at + 4].copy_from_slice(&2u32.to_le_bytes());
    // dims now read as [u64::MAX, <next 8 bytes>]; whatever follows, parse must not panic.
    overflow[dims_at..dims_at + 8].copy_from_slice(&u64::MAX.to_le_bytes());
    assert!(Weights::parse(&overflow).is_err());

    // (f) wrong magic and wrong version
    let mut bad = real.clone();
    bad[0] = b'X';
    assert!(Weights::parse(&bad).is_err());
    let mut v2 = real.clone();
    v2[4..8].copy_from_slice(&2u32.to_le_bytes());
    assert!(Weights::parse(&v2).is_err());
}

#[test]
fn an_absent_tensor_names_itself_in_the_error() {
    let w = Weights::load(Path::new(MODEL)).unwrap();
    let err = w.tensor("does.not.exist").unwrap_err();
    assert!(err.to_string().contains("does.not.exist"), "{err}");
}
