//! Opt-in complete-raster native gates. Inputs and native payloads stay outside
//! the repository. Set FAST_MAP_VELOCITY_NATIVE to a qualification manifest.
use ras_commander_fast_map::sloping_session;
use serde::Deserialize;
use std::{fs, path::PathBuf};

#[derive(Deserialize)]
struct Manifest {
    cases: Vec<Case>,
}
#[derive(Deserialize)]
struct Case {
    request: PathBuf,
    products: Vec<Product>,
}
#[derive(Deserialize)]
struct Product {
    output: PathBuf,
    reference: PathBuf,
}

#[test]
#[ignore = "requires independently exported native fixtures"]
fn native_velocity_and_derived_complete_masks_and_bits() {
    let manifest = PathBuf::from(
        std::env::var_os("FAST_MAP_VELOCITY_NATIVE")
            .expect("set FAST_MAP_VELOCITY_NATIVE to the external qualification manifest"),
    );
    let root = manifest.parent().unwrap();
    let manifest: Manifest = serde_json::from_slice(&fs::read(&manifest).unwrap()).unwrap();
    assert!(!manifest.cases.is_empty());
    for (index, case) in manifest.cases.iter().enumerate() {
        let path = root.join(&case.request);
        let mut request: sloping_session::Request =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        request.stream_outputs = false;
        request.geotiff_outputs = false;
        let output = std::env::temp_dir().join(format!(
            "fast-map-velocity-native-{}-{index}",
            std::process::id()
        ));
        assert!(
            !output.exists(),
            "output must be fresh: {}",
            output.display()
        );
        let report = sloping_session::run(&request, path.parent().unwrap(), &output).unwrap();
        assert_eq!(report["status"], "complete");
        assert!(!case.products.is_empty());
        for product in &case.products {
            let actual = fs::read(output.join(&product.output)).unwrap();
            let expected = fs::read(root.join(&product.reference)).unwrap();
            assert_eq!(actual.len(), expected.len());
            assert_eq!(actual.len() % 4, 0);
            for (pixel, (a, b)) in actual
                .as_chunks::<4>()
                .0
                .iter()
                .zip(expected.as_chunks::<4>().0.iter())
                .enumerate()
            {
                let a = f32::from_le_bytes(*a);
                let b = f32::from_le_bytes(*b);
                assert_eq!(
                    a.is_nan(),
                    b.is_nan(),
                    "{} pixel {pixel}: mask",
                    product.output.display()
                );
                if !a.is_nan() {
                    assert_eq!(
                        a.to_bits(),
                        b.to_bits(),
                        "{} pixel {pixel}: bits",
                        product.output.display()
                    );
                }
            }
        }
        fs::remove_dir_all(output).unwrap();
    }
}
