//! Tests that need the fetched packs are `#[ignore]`d (network on a cold cache).
//! Run them with `cargo test --test rank -- --ignored`.

use projectmp4::presets::rank::{eligible, score_bytes, Tier};
use projectmp4::presets::{rank, score_milk, Manifest};
use projectmp4::Packs;
use std::collections::HashMap;
use std::fs;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Instant;

fn packs() -> Packs {
    static P: OnceLock<Result<Packs, String>> = OnceLock::new();
    // Same cache as tests/packs.rs, so a prior `--test packs --ignored` run is reused.
    P.get_or_init(|| Packs::ensure(&std::env::temp_dir().join("projectmp4-packs-test-cache")).map_err(|e| e.to_string()))
        .clone()
        .unwrap_or_else(|e| panic!("populating the pack cache failed: {e}"))
}

fn fixture() -> Vec<serde_json::Value> {
    let text = fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures/cotc_scores.json")).unwrap();
    serde_json::from_str(&text).unwrap()
}

#[test]
fn fixture_is_the_full_prototype_output() {
    let rows = fixture();
    assert_eq!(rows.len(), 9795);
    assert_eq!(rows.iter().filter(|r| r["sub"] == "").count(), 4, "the flat `! Transition` records");
}

#[test]
#[ignore = "needs the Cream of the Crop pack (network on a cold cache)"]
fn scoring_matches_the_python_prototype() {
    let packs = packs();
    let rows = fixture();
    let t = Instant::now();
    let mut compared = 0usize;
    let mut bad: Vec<String> = Vec::new();
    for r in &rows {
        // `! Transition` is flat (sub == ""), so skip empty components.
        let path: PathBuf = ["cat", "sub", "name"]
            .iter()
            .map(|k| r[k].as_str().unwrap())
            .filter(|s| !s.is_empty())
            .fold(packs.cotc.clone(), |p, c| p.join(c));
        let bytes = match fs::read(&path) {
            Ok(b) => b,
            Err(e) => {
                bad.push(format!("{}: unreadable: {e}", path.display()));
                continue;
            }
        };
        compared += 1;
        let got = score_bytes(&bytes);
        let want = (r["bass"].as_u64().unwrap() as u32, r["react"].as_u64().unwrap() as u32, r["shader"].as_bool().unwrap());
        if (got.bass, got.react, got.shader) != want {
            bad.push(format!("{}: got {:?}, want {want:?}", path.display(), (got.bass, got.react, got.shader)));
        }
    }
    eprintln!("scored {compared} presets in {:?}", t.elapsed());
    assert_eq!(compared, 9795, "not every fixture record was compared; first problems: {:?}", &bad[..bad.len().min(5)]);
    assert!(bad.is_empty(), "{} of {compared} presets differ from the prototype; first 10:\n{}", bad.len(), bad[..bad.len().min(10)].join("\n"));
}

#[test]
#[ignore = "needs the Cream of the Crop pack (network on a cold cache)"]
fn profiles_are_curated_ranked_and_sized() {
    let packs = packs();
    let pool = eligible(&packs, "deathstep").unwrap();
    assert!(pool.len() > 400, "pool suspiciously small: {}", pool.len());
    let p = rank(&packs, "deathstep", 40).unwrap();
    assert_eq!(p.presets.len(), 40);

    // The point of the ranking: what a human put in a best-of wins. The shipped
    // forty must come from the curated sets, not from the arithmetic.
    let by_path: HashMap<_, _> = pool.iter().map(|c| (c.path.clone(), c)).collect();
    let uncurated: Vec<_> =
        p.presets.iter().filter(|f| by_path[*f].tier == Tier::Uncurated).collect();
    assert!(uncurated.is_empty(), "uncurated presets took curated slots: {uncurated:?}");

    // Curation is the primary key, so the tiers must come out in blocks: once the
    // list drops to a lower tier it can never climb back.
    let tiers: Vec<Tier> = p.presets.iter().map(|f| by_path[f].tier).collect();
    assert!(tiers.windows(2).all(|w| w[0] >= w[1]), "tiers are not in blocks: {tiers:?}");

    // One slot per look: the packs ship `nz` / `roam` / `ret` remixes of the same
    // preset under different file names, and forty slots must be forty different
    // things to watch. The suffixes are stripped here rather than by calling the
    // library's own helper, so this is a check and not a restatement.
    let family = |f: &PathBuf| {
        let mut n = f.file_name().unwrap().to_str().unwrap().to_lowercase();
        n = n.trim_end_matches(".milk").to_string();
        loop {
            let t = n.trim_end().trim_end_matches(['+', ']', ')']).trim_end();
            match [" nz", " roam3", " roam", " ret", " flacc,roams", " - 1-ret"]
                .iter()
                .find_map(|s| t.strip_suffix(s))
            {
                Some(shorter) => n = shorter.to_string(),
                None => return t.to_string(),
            }
        }
    };
    let mut fams: Vec<String> = p.presets.iter().map(family).collect();
    fams.sort();
    fams.dedup();
    assert_eq!(fams.len(), 40, "the profile ships two variants of the same preset");

    // Ranked within the tier, not arbitrary: non-increasing by the ranking score.
    let scores: Vec<i64> = p.presets.iter().map(|f| by_path[f].rank_score).collect();
    assert!(scores.windows(2).all(|w| w[0] >= w[1]), "not in score order: {scores:?}");

    // Independent of the ranking formula: deathstep is about bass, so the selection
    // must be more bass-reactive than a typical preset of the tier it comes from.
    let mut bass: Vec<u32> =
        pool.iter().filter(|c| c.tier != Tier::Uncurated).map(|c| c.score.bass).collect();
    bass.sort();
    let med_bass = bass[bass.len() / 2];
    let min_sel = p.presets.iter().map(|f| by_path[f].score.bass).min().unwrap();
    assert!(min_sel >= med_bass, "weakest pick bass={min_sel} < curated median bass={med_bass}");
}

#[test]
#[ignore = "needs the Cream of the Crop pack (network on a cold cache)"]
fn dubstep_aliases_deathstep() {
    let packs = packs();
    let d = rank(&packs, "deathstep", 40).unwrap().presets;
    assert!(!d.is_empty());
    assert_eq!(rank(&packs, "dubstep", 40).unwrap().presets, d);
}

#[test]
#[ignore = "needs the Cream of the Crop pack (network on a cold cache)"]
fn genres_differ_and_fill_only_from_their_categories() {
    let packs = packs();
    // A profile is curated presets first; Cream of the Crop only fills what the
    // curations cannot, and that fill is still confined to the genre's categories.
    let cats = |name: &str, allowed: &[&str]| {
        let p = rank(&packs, name, 40).unwrap();
        assert_eq!(p.presets.len(), 40, "{name}");
        for f in &p.presets {
            let Ok(rel) = f.strip_prefix(&packs.cotc) else { continue };
            let cat = rel.components().next().unwrap().as_os_str().to_str().unwrap().to_string();
            assert!(allowed.contains(&cat.as_str()), "{name}: {} is in {cat}", f.display());
        }
        p.presets
    };
    let death = cats("deathstep", &["Reaction", "Geometric", "Supernova"]);
    let dance = cats("dancehall", &["Reaction", "Dancer"]);
    let amb = cats("ambient", &["Hypnotic", "Fractal"]);
    assert_ne!(death, dance);
    assert_ne!(death, amb);
}

#[test]
#[ignore = "needs the Cream of the Crop pack (network on a cold cache)"]
fn blacklisted_presets_never_reach_a_pool() {
    let packs = packs();
    let pool: Vec<PathBuf> = eligible(&packs, "deathstep").unwrap().into_iter().map(|c| c.path).collect();
    // Control: these exist on disk, are shaders in a deathstep category, so only the blacklist excludes them.
    let known = [
        "Supernova/Radiate/Stahlregen & EoS + Phat + Pithlit + Unchained - Raw Mash Up.milk",
        "Reaction/Feedback/$$$ Royal - Mashup (452).milk",
    ];
    let mut checked = 0;
    for k in known {
        let p = packs.cotc.join(k);
        if p.exists() && score_bytes(&fs::read(&p).unwrap()).shader {
            checked += 1;
            assert!(!pool.contains(&p), "{k} is blacklisted but was eligible");
        }
    }
    assert!(checked > 0, "no control file was a shader in the pack; the check proved nothing");
}

#[test]
#[ignore = "needs the Cream of the Crop pack (network on a cold cache)"]
fn committed_profiles_match_a_fresh_ranking() {
    let fresh = Manifest::generate(&packs(), 40).unwrap();
    let committed = Manifest::load(&PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("presets/profiles.toml")).unwrap();
    assert_eq!(fresh.profiles.len(), 4);
    assert!(fresh.profiles.iter().all(|p| p.presets.len() == 40));
    assert_eq!(committed, fresh, "presets/profiles.toml is stale: run `cargo run -- rank`");
}

#[test]
fn score_milk_is_exposed_for_str_input() {
    assert_eq!(score_milk("per_frame_1=a=bass_att;").bass, 3);
}
