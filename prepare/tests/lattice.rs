//! Window arithmetic and admission sizes against values Python computed
//! (`_aoi_lattice.index_range`, `_aoi.center_index_range`, rasterio window
//! transforms and bounds, CPython 3.12 `sys.getsizeof`).

use fast_map_prepare::{aoi, cohort, grid::Affine};
use serde_json::Value;

fn expected() -> Value {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/expected-lattice.json");
    serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap()
}

fn f(v: &Value) -> f64 {
    v.as_f64().unwrap()
}

#[test]
fn index_ranges_match_python() {
    let e = expected();
    for (name, func) in [
        (
            "index_range",
            aoi::index_range as fn(f64, f64, f64, f64, i64) -> (i64, i64),
        ),
        ("center_index_range", aoi::center_index_range),
    ] {
        for row in e[name].as_array().unwrap() {
            let r = row.as_array().unwrap();
            let got = func(
                f(&r[0]),
                f(&r[1]),
                f(&r[2]),
                f(&r[3]),
                r[4].as_i64().unwrap(),
            );
            let want = (r[5][0].as_i64().unwrap(), r[5][1].as_i64().unwrap());
            assert_eq!(got, want, "{name} {row}");
        }
    }
}

#[test]
fn window_transforms_match_rasterio_bit_for_bit() {
    let t = Affine::from_gdal([
        1738955.4338581087,
        6.561666666666606,
        0.0,
        14017250.5176182,
        0.0,
        -6.561666666666606,
    ]);
    for row in expected()["window_transform"].as_array().unwrap() {
        let (col, r) = (row[0].as_i64().unwrap(), row[1].as_i64().unwrap());
        let gt = t.window_transform(col, r).to_gdal();
        let want: Vec<f64> = row[2].as_array().unwrap().iter().map(f).collect();
        assert_eq!(
            gt.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
            want.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
        let (left, bottom) = t.apply(col as f64, (r + 10) as f64);
        let (right, top) = t.apply((col + 10) as f64, r as f64);
        let bounds: Vec<f64> = row[3].as_array().unwrap().iter().map(f).collect();
        assert_eq!(
            [left, bottom, right, top].map(f64::to_bits).to_vec(),
            bounds.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
        );
    }
}

#[test]
fn admission_sizes_match_cpython_312() {
    let e = expected();
    assert!(e["python"].as_str().unwrap().starts_with("3.12"));
    for (s, size) in e["getsizeof"].as_object().unwrap() {
        assert_eq!(cohort::str_sizeof(s), size.as_u64().unwrap(), "{s:?}");
    }
    for (v, size) in e["getsizeof_int"].as_object().unwrap() {
        assert_eq!(
            cohort::int_sizeof(v.parse().unwrap()),
            size.as_u64().unwrap(),
            "{v}"
        );
    }
}
