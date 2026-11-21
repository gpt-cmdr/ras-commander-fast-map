//! Fast-Map's opt-in bounded corner fits on the grouped path, as fim-commander's
//! `fim_commander.mapping._corner_bound` requests and checks them.
//!
//! A bounded caller puts `controls.corner_fit_bound = {"mode": "input_range",
//! "tolerance": t}` in the worker config (`map_depth_groups(corner_fit_bound=)`).
//! The Python worker (`_cohort`) copies it into the sloping model as
//! `options.corner_fit_bound` and checks the engine's report with
//! `check_corner_bound_report`; this module does the same, so the native worker
//! never maps native corner fits under a bounded request, nor accepts a bounded
//! report under a native one.

use crate::error::{Error, Result};
use serde_json::{Value, json};

pub const NATIVE_SLOPING_METHOD: &str = "rasmapper-plain-sloping/1";
pub const BOUNDED_CORNER_METHOD: &str = "rasmapper-plain-sloping-bounded-corners/1";

/// The largest finite binary32: Fast-Map reads the tolerance as a 32-bit float.
const F32_MAX: f64 = f32::MAX as f64;

/// `validated_bound`: `None` (native) or `{"mode": "input_range", "tolerance": t}`
/// with finite `0 <= t <= f32::MAX`; `-0` is `+0`, an integer tolerance becomes a
/// float, as Python's `float(tolerance) + 0.0` writes it.
///
/// One refusal of its own: a nonzero tolerance below 1e-4 or from 1e16 up.
/// Python writes such floats with an exponent (`1e-05`, `1e+16`) and this
/// crate's JSON writer writes them differently (`1e-5`, `1e16`), so the request
/// bytes would differ from the Python worker's. Every decimal-notation value is
/// written identically (shortest round-trip digits in both).
pub fn validated(value: Option<&Value>) -> Result<Option<Value>> {
    let value = match value {
        None | Some(Value::Null) => return Ok(None),
        Some(v) => v,
    };
    let object = value
        .as_object()
        .filter(|o| o.len() == 2 && o.contains_key("mode") && o.contains_key("tolerance"))
        .ok_or_else(|| Error::invalid("corner_fit_bound requires exactly mode and tolerance"))?;
    let tolerance = &object["tolerance"];
    if object["mode"] != "input_range" {
        return Err(Error::invalid(
            "corner_fit_bound mode must be 'input_range'",
        ));
    }
    let t = match tolerance.as_f64() {
        Some(t) if !tolerance.is_boolean() && t.is_finite() && (0.0..=F32_MAX).contains(&t) => {
            t + 0.0
        }
        _ => {
            return Err(Error::invalid(
                "corner_fit_bound tolerance must be finite and nonnegative (WSE units)",
            ));
        }
    };
    if t != 0.0 && !(1e-4..1e16).contains(&t) {
        return Err(Error::unsupported(
            "corner_fit_bound tolerance must be 0 or within [1e-4, 1e16) for the native worker",
        ));
    }
    Ok(Some(json!({"mode": "input_range", "tolerance": t})))
}

/// `require_bounded_engine`: the engine advertises the bounded method on every
/// path the grouped mapper uses.
pub fn require_bounded_engine(method: &Value) -> Result<()> {
    let listed = method["depth_groups"]["sloping_methods"]
        .as_array()
        .is_some_and(|m| m.iter().any(|v| v == BOUNDED_CORNER_METHOD));
    if method["sloping"]["bounded_corners"]["method_id"] != BOUNDED_CORNER_METHOD
        || method["sloping_bounded_corners"]["method_id"] != BOUNDED_CORNER_METHOD
        || !listed
    {
        return Err(Error::unsupported(format!(
            "Engine lacks the opt-in sloping method {BOUNDED_CORNER_METHOD}"
        )));
    }
    Ok(())
}

/// Python's `struct.pack("<f", float(a)) == struct.pack("<f", float(b))` for two
/// JSON numbers (booleans and non-numbers are unequal).
fn same_f32(a: &Value, b: &Value) -> bool {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) if !a.is_boolean() && !b.is_boolean() => {
            (x as f32).to_bits() == (y as f32).to_bits()
        }
        _ => false,
    }
}

/// `_count`: a JSON integer >= 0 (a float such as `0.0` is not a count).
fn count(value: &Value) -> bool {
    value.is_u64()
}

/// `check_corner_bound_report`: a bounded request's report says so on the
/// report and on every frame; a native request's report carries no bound.
pub fn check_report(report: &Value, bound: Option<&Value>) -> Result<()> {
    let Some(bound) = bound else {
        if report.get("corner_fit_bound").is_some() || report.get("sloping_method_id").is_some() {
            return Err(Error::engine(
                "Native grouped report carries a corner bound nobody requested",
            ));
        }
        return Ok(());
    };
    let summary = &report["corner_fit_bound"];
    if report["sloping_method_id"] != BOUNDED_CORNER_METHOD
        || !summary.is_object()
        || summary["method_id"] != BOUNDED_CORNER_METHOD
        || summary["mode"] != bound["mode"]
        || !same_f32(&summary["tolerance"], &bound["tolerance"])
        || !count(&summary["bounded_corner_groups"])
        || !count(&summary["out_of_range_pixels"])
    {
        return Err(Error::engine(format!(
            "Grouped report lacks the requested {BOUNDED_CORNER_METHOD} record"
        )));
    }
    let empty = Vec::new();
    for job in report["jobs"].as_array().unwrap_or(&empty) {
        for group in job["groups"].as_array().unwrap_or(&empty) {
            for frame in group["frames"].as_array().unwrap_or(&empty) {
                let record = &frame["corner_fit_bound"];
                let sweep = &record["same_profile_out_of_range"];
                if !record.is_object()
                    || record["method_id"] != BOUNDED_CORNER_METHOD
                    || !same_f32(&record["tolerance"], &bound["tolerance"])
                    || !count(&record["bounded_corner_groups"])
                    || !count(&record["corner_groups_evaluated"])
                    || !sweep.is_object()
                    || !count(&sweep["out_of_range_pixels"])
                    || !count(&sweep["wet_pixels"])
                {
                    let index = match &frame["source_index"] {
                        Value::Null => "None".to_string(),
                        v => v.to_string(),
                    };
                    return Err(Error::engine(format!(
                        "Grouped frame {index} lacks its {BOUNDED_CORNER_METHOD} record"
                    )));
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::Kind;

    #[test]
    fn validated_matches_python_normalisation_and_refusals() {
        assert_eq!(validated(None).unwrap(), None);
        assert_eq!(validated(Some(&Value::Null)).unwrap(), None);
        let zero = validated(Some(&json!({"mode": "input_range", "tolerance": 0}))).unwrap();
        assert_eq!(
            serde_json::to_string(&zero).unwrap(),
            r#"{"mode":"input_range","tolerance":0.0}"#
        );
        let negative_zero =
            validated(Some(&json!({"tolerance": -0.0, "mode": "input_range"}))).unwrap();
        assert_eq!(
            serde_json::to_string(&negative_zero).unwrap(),
            r#"{"mode":"input_range","tolerance":0.0}"#
        );
        let half = validated(Some(&json!({"mode": "input_range", "tolerance": 0.5}))).unwrap();
        assert_eq!(
            serde_json::to_string(&half).unwrap(),
            r#"{"mode":"input_range","tolerance":0.5}"#
        );
        for bad in [
            json!({"mode": "input_range"}),
            json!({"mode": "input_range", "tolerance": -1.0}),
            json!({"mode": "input_range", "tolerance": true}),
            json!({"mode": "input_range", "tolerance": "0"}),
            json!({"mode": "first_wet", "tolerance": 0.0}),
            json!({"mode": "input_range", "tolerance": 0.0, "extra": 1}),
            json!({"mode": "input_range", "tolerance": 1e39}),
            json!([0.0]),
        ] {
            assert_eq!(
                validated(Some(&bad)).unwrap_err().kind,
                Kind::Invalid,
                "{bad}"
            );
        }
        for exponent in [1e-5, 1e16, 3.0e38] {
            let bound = json!({"mode": "input_range", "tolerance": exponent});
            assert_eq!(validated(Some(&bound)).unwrap_err().kind, Kind::Unsupported);
        }
    }

    #[test]
    fn decimal_tolerances_are_written_as_python_writes_them() {
        // Python's json.dumps(float) for values in [1e-4, 1e16): repr().
        for (value, python) in [
            (0.0001, "0.0001"),
            (0.25, "0.25"),
            (0.1, "0.1"),
            (1.0, "1.0"),
            (2.5e15, "2500000000000000.0"),
            (9999999999999998.0, "9999999999999998.0"),
        ] {
            let bound = validated(Some(&json!({"mode": "input_range", "tolerance": value})))
                .unwrap()
                .unwrap();
            assert_eq!(serde_json::to_string(&bound["tolerance"]).unwrap(), python);
        }
    }

    fn engine(bounded: bool) -> Value {
        let mut method = json!({
            "sloping": {"method_id": NATIVE_SLOPING_METHOD},
            "depth_groups": {"sloping_methods": [NATIVE_SLOPING_METHOD]},
        });
        if bounded {
            method["sloping"]["bounded_corners"] = json!({"method_id": BOUNDED_CORNER_METHOD});
            method["sloping_bounded_corners"] = json!({"method_id": BOUNDED_CORNER_METHOD});
            method["depth_groups"]["sloping_methods"] =
                json!([NATIVE_SLOPING_METHOD, BOUNDED_CORNER_METHOD]);
        }
        method
    }

    #[test]
    fn engine_gate_needs_every_advertisement() {
        require_bounded_engine(&engine(true)).unwrap();
        assert_eq!(
            require_bounded_engine(&engine(false)).unwrap_err().kind,
            Kind::Unsupported
        );
        for key in ["sloping", "sloping_bounded_corners", "depth_groups"] {
            let mut method = engine(true);
            match key {
                "sloping" => method["sloping"]["bounded_corners"] = Value::Null,
                "sloping_bounded_corners" => method["sloping_bounded_corners"] = Value::Null,
                _ => method["depth_groups"]["sloping_methods"] = json!([NATIVE_SLOPING_METHOD]),
            }
            assert!(require_bounded_engine(&method).is_err(), "{key}");
        }
    }

    fn bounded_report() -> Value {
        let frame = json!({
            "source_index": 3,
            "corner_fit_bound": {
                "method_id": BOUNDED_CORNER_METHOD, "tolerance": 0.0,
                "bounded_corner_groups": 1, "corner_groups_evaluated": 10,
                "same_profile_out_of_range": {"out_of_range_pixels": 0, "wet_pixels": 7},
            },
        });
        json!({
            "sloping_method_id": BOUNDED_CORNER_METHOD,
            "corner_fit_bound": {
                "method_id": BOUNDED_CORNER_METHOD, "mode": "input_range", "tolerance": 0.0,
                "bounded_corner_groups": 1, "out_of_range_pixels": 0,
            },
            "jobs": [{"groups": [{"frames": [frame]}]}],
        })
    }

    #[test]
    fn report_check_mirrors_check_corner_bound_report() {
        let bound = json!({"mode": "input_range", "tolerance": 0.0});
        check_report(&bounded_report(), Some(&bound)).unwrap();
        check_report(&json!({"jobs": []}), None).unwrap();
        // A native report under a bounded request: the defect this module closes.
        let native = json!({"jobs": [{"groups": [{"frames": [{"source_index": 3}]}]}]});
        assert_eq!(
            check_report(&native, Some(&bound)).unwrap_err().kind,
            Kind::Engine
        );
        assert_eq!(
            check_report(&bounded_report(), None).unwrap_err().kind,
            Kind::Engine
        );
        // The tolerance compares as binary32: 0.1 and the f32 nearest 0.1 agree.
        let tenth = json!({"mode": "input_range", "tolerance": 0.1});
        let mut report = bounded_report();
        report["corner_fit_bound"]["tolerance"] = json!(f64::from(0.1f32));
        report["jobs"][0]["groups"][0]["frames"][0]["corner_fit_bound"]["tolerance"] = json!(0.1);
        check_report(&report, Some(&tenth)).unwrap();
        // Each per-frame field and the summary counts are required integers.
        for (path, value) in [
            ("/corner_fit_bound/out_of_range_pixels", json!(0.0)),
            ("/corner_fit_bound/tolerance", json!(0.5)),
            ("/corner_fit_bound/mode", json!("first_wet")),
            (
                "/jobs/0/groups/0/frames/0/corner_fit_bound/corner_groups_evaluated",
                json!(-1),
            ),
            (
                "/jobs/0/groups/0/frames/0/corner_fit_bound/method_id",
                json!(NATIVE_SLOPING_METHOD),
            ),
            (
                "/jobs/0/groups/0/frames/0/corner_fit_bound/same_profile_out_of_range",
                Value::Null,
            ),
        ] {
            let mut report = bounded_report();
            *report.pointer_mut(path).unwrap() = value;
            let err = check_report(&report, Some(&bound)).unwrap_err();
            assert_eq!(err.kind, Kind::Engine, "{path}");
        }
        let mut frameless = bounded_report();
        frameless["jobs"][0]["groups"][0]["frames"][0]["corner_fit_bound"] = Value::Null;
        let err = check_report(&frameless, Some(&bound)).unwrap_err();
        assert_eq!(
            err.message,
            format!("Grouped frame 3 lacks its {BOUNDED_CORNER_METHOD} record")
        );
    }
}
