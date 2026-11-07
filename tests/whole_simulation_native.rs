//! Opt-in complete-raster comparison with independently exported native maps.
//! Set FAST_MAP_WHOLE_SIMULATION_FIXTURE to a fixture.json, then run this
//! ignored test explicitly. Model data and native reference buffers stay external.
use ras_commander_fast_map::whole_simulation::{self, Request};
use serde::Deserialize;
use std::{collections::BTreeMap, fs, path::PathBuf};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Fixture {
    schema: String,
    request: PathBuf,
    expected: BTreeMap<String, PathBuf>,
    pixels: usize,
}

#[test]
#[ignore = "requires independent native whole-simulation reference fixture"]
fn complete_native_temporal_masks_and_float32_bits() {
    let manifest = PathBuf::from(
        std::env::var_os("FAST_MAP_WHOLE_SIMULATION_FIXTURE")
            .expect("set FAST_MAP_WHOLE_SIMULATION_FIXTURE to fixture.json"),
    );
    let base = manifest.parent().unwrap();
    let fixture: Fixture = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert_eq!(fixture.schema, "fast-map-native-temporal-fixture/1");
    assert!(fixture.pixels > 0);
    let request_path = base.join(fixture.request);
    let request: Request = serde_json::from_slice(&fs::read(&request_path).unwrap()).unwrap();
    assert!(!request.allow_unqualified);
    assert_eq!(request.products.len(), fixture.expected.len());
    let out = std::env::temp_dir().join(format!("fast-map-native-temporal-{}", std::process::id()));
    assert!(
        !out.exists(),
        "remove previous test output: {}",
        out.display()
    );
    whole_simulation::run(&request, request_path.parent().unwrap(), &out).unwrap();
    for product in &request.products {
        let expected = fs::read(base.join(&fixture.expected[product])).unwrap();
        let actual = fs::read(out.join("whole_simulation").join(format!("{product}.bin"))).unwrap();
        assert_eq!(expected.len(), fixture.pixels * 4, "{product} reference");
        assert_eq!(actual.len(), expected.len(), "{product} shape");
        let mut valid = 0;
        for (i, (a, b)) in actual
            .chunks_exact(4)
            .zip(expected.chunks_exact(4))
            .enumerate()
        {
            let actual = f32::from_le_bytes(a.try_into().unwrap());
            let expected = f32::from_le_bytes(b.try_into().unwrap());
            assert_eq!(actual.is_nan(), expected.is_nan(), "{product} mask at {i}");
            if !expected.is_nan() {
                assert!(expected.is_finite(), "{product} invalid reference at {i}");
                assert_eq!(
                    actual.to_bits(),
                    expected.to_bits(),
                    "{product} value at {i}"
                );
                valid += 1;
            }
        }
        assert!(valid > 0, "{product} reference has no valid pixels");
    }
    fs::remove_dir_all(out).unwrap();
}
