//! Whole-simulation stored-map products: arrival time, duration and percent time
//! inundated (`native-whole-simulation-horizontal/1`).
//!
//! Every stored output time is rendered by the unchanged horizontal method
//! (cell-constant WSE, native float32 visibility) and accumulated per pixel in bounded memory: first wet time, last wet
//! time, wet count and the number of separate wet periods.
//!
//! A pixel is wet at an output time when its rendered depth is strictly greater
//! than `threshold` **and** its 2D cell passes RAS Mapper's thresholded cell gate
//! `wse > cell_minimum + threshold` (float32). Physical 2D cells, plus optional
//! storage-area level pools (`storage-area-level-pool/1`) when the horizontal
//! request supplies them: a pool pixel is wet when its rendered depth is
//! strictly greater than `threshold`; a level pool has no cell gate. With times `t` as float32 hours and `n` output times:
//!
//! - percent = float32(100 * wet_count / n);
//! - arrival = t[first wet];
//! - duration = t[end] - t[first wet] in float32, where `end` is the first dry
//!   output time after the last wet one, or the last wet one when that dry
//!   time would be the final output time or there is none;
//! - NoData (NaN) where the pixel is never wet.
//!
//! Qualified against native HEC-RAS 7.0.1 (threshold 0.5 ft) on Davis p02 and
//! BaldEagleCrkMulti2D bridges p05: every valid pixel, arrival and percent value
//! bit for bit, including initially wet, multi-period and threshold-equal
//! samples; RAS Mapper's horizontal and sloping products are identical. Duration
//! is unqualified (some native recession durations are one interval shorter)
//! and is delivered only with `allow_unqualified`, as are irregular intervals.
use crate::{MapError, horizontal, sink};
use rayon::prelude::*;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{fs, path::Path, time::Instant};

pub const SCHEMA: &str = "ras-commander-whole-simulation-request/1";
pub const METHOD_ID: &str = "native-whole-simulation-horizontal/1";
pub const PRODUCTS: [&str; 3] = ["arrival_time", "duration", "percent_inundated"];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    pub schema: String,
    /// Horizontal request whose profiles are the stored output times, in order.
    pub horizontal: horizontal::Request,
    /// Output time of each profile in hours, strictly increasing.
    pub times_hours: Vec<f64>,
    /// Wet when the rendered depth is strictly greater than this (model units).
    pub threshold: f32,
    /// Little-endian float32 cell minimum elevations, in the horizontal
    /// request's cell order (the native thresholded cell gate).
    pub cell_minimum_file: String,
    #[serde(default = "all_products")]
    pub products: Vec<String>,
    /// Deliver products even when unqualified cases are present (still reported).
    #[serde(default)]
    pub allow_unqualified: bool,
}

/// Default: the products qualified bit for bit. Duration is opt-in (unqualified).
fn all_products() -> Vec<String> {
    vec!["arrival_time".into(), "percent_inundated".into()]
}

pub fn method_record() -> Value {
    json!({
        "method_id": METHOD_ID,
        "render": "horizontal-cell-constant/2 per stored output time; physical 2D cells and optional storage-area level pools",
        "storage_area_surfaces": crate::storage_area::METHOD_ID,
        "storage_area_wet": "rendered pool depth > threshold (float32, strict); no cell gate",
        "wet": "rendered depth valid and depth > threshold (float32, strict)",
        "percent_inundated": "float32(100 * wet_count / n_times)",
        "arrival_time": "first wet stored output time (hours)",
        "duration": "duration_end - arrival (hours)",
        "nodata": "never wet, including unmapped pixels",
        "memory": "32 bytes per mapped pixel plus 12 bytes per window pixel for products; output times are evaluated sequentially, pixels in parallel",
        "cell_gate": "wse > cell_minimum + threshold (float32)",
        "duration_end": "first dry output time after the last wet one, unless that is the final output time or none (then the last wet time)",
        "time_arithmetic": "float32 hours",
        "qualified": "native HEC-RAS 7.0.1 horizontal and sloping stored maps, Davis p02 (217 times) and BaldEagleCrkMulti2D bridges p05 (433 times), threshold 0.5 ft; arrival_time and percent_inundated",
        "unqualified_refused": ["irregular_intervals", "duration"],
        "reported": ["initially_wet_pixels", "multiple_period_pixels", "threshold_equal_samples"],
        "known_differences": "duration: Davis 103/4,879 and bridges 11/959,043 pixels one output interval longer than native; arrival and percent identical",
        "not_implemented": "sloping rendering; recession; subinterval interpolation"
    })
}

#[derive(Clone, Copy)]
struct Acc {
    first: u32,
    last: u32,
    count: u32,
    wet: bool,
    periods: u8,
}

const NEVER: u32 = u32::MAX;

/// Products and unqualified-case statistics from the accumulated pixels.
struct Accumulated {
    arrival: Vec<f32>,
    duration: Vec<f32>,
    percent: Vec<f32>,
    wet_pixels: u64,
    initially_wet: u64,
    multiple_periods: u64,
}

/// Scatters compact accumulators (window pixel `index[k]`) to window products.
fn finish(acc: &[Acc], index: &[usize], window: usize, times: &[f64]) -> Accumulated {
    let n = times.len() as f64;
    let mut out = Accumulated {
        arrival: vec![f32::NAN; window],
        duration: vec![f32::NAN; window],
        percent: vec![f32::NAN; window],
        wet_pixels: 0,
        initially_wet: 0,
        multiple_periods: 0,
    };
    for (a, &i) in acc.iter().zip(index) {
        if a.count == 0 {
            continue;
        }
        out.wet_pixels += 1;
        out.initially_wet += u64::from(a.first == 0);
        out.multiple_periods += u64::from(a.periods > 1);
        let first = times[a.first as usize] as f32;
        let last = a.last as usize;
        let end = if last + 2 < times.len() {
            last + 1
        } else {
            last
        };
        out.arrival[i] = first;
        out.duration[i] = times[end] as f32 - first;
        out.percent[i] = (100.0 * f64::from(a.count) / n) as f32;
    }
    out
}

#[inline]
fn step(a: &mut Acc, position: u32, depth: Option<f32>, threshold: f32) -> u64 {
    let wet = depth.is_some_and(|d| d > threshold);
    if wet {
        if a.first == NEVER {
            a.first = position;
        }
        if !a.wet {
            a.periods = a.periods.saturating_add(1);
        }
        a.last = position;
        a.count += 1;
    }
    a.wet = wet;
    u64::from(depth == Some(threshold))
}

/// Mapped pixels only (2D-owned or storage-area pixels with terrain); every
/// other window pixel is never wet and stays NoData.
#[derive(Default)]
struct Accumulator {
    threshold: f32,
    /// Per 2D cell: minimum + threshold (float32), the native cell gate.
    gate: Vec<f32>,
    index: Vec<usize>,
    /// 2D cell position, or storage-area position for entries at or after `storage_start`.
    source: Vec<u32>,
    ground: Vec<f32>,
    acc: Vec<Acc>,
    /// First accumulator of a storage-area (level-pool) pixel.
    storage_start: usize,
    equal: u64,
}

impl horizontal::Consumer for Accumulator {
    fn prepare(
        &mut self,
        owner: &[u32],
        ground: &[f32],
        storage: Option<&crate::storage_area::StorageAreaPixels>,
    ) -> Result<(), MapError> {
        for (i, (&label, &g)) in owner.iter().zip(ground).enumerate() {
            if label != 0 && g.is_finite() {
                self.index.push(i);
                self.source.push(label - 1);
                self.ground.push(g);
            }
        }
        self.storage_start = self.index.len();
        if let Some(storage) = storage {
            for ((&p, &label), &g) in storage
                .pixels
                .iter()
                .zip(&storage.labels)
                .zip(&storage.ground)
            {
                if g.is_finite() {
                    self.index.push(p);
                    self.source.push(label);
                    self.ground.push(g);
                }
            }
        }
        self.acc = vec![
            Acc {
                first: NEVER,
                last: NEVER,
                count: 0,
                wet: false,
                periods: 0,
            };
            self.index.len()
        ];
        Ok(())
    }

    fn profile(
        &mut self,
        position: usize,
        values: &[f32],
        storage: Option<&[f32]>,
    ) -> Result<(), MapError> {
        let (threshold, gate, start) = (self.threshold, &self.gate, self.storage_start);
        if storage.is_none() && self.index.len() != start {
            return Err(MapError::Internal(
                "storage-area profile values missing".into(),
            ));
        }
        let position = position as u32;
        self.equal += self
            .acc
            .par_iter_mut()
            .enumerate()
            .zip(self.source.par_iter())
            .zip(self.ground.par_iter())
            .map(|(((k, a), &source), &g)| -> Result<u64, MapError> {
                if k >= start {
                    // Level pool: the storage area's WSE, with no cell gate.
                    let value = storage.map_or(f32::NAN, |s| s[source as usize]);
                    return Ok(step(a, position, horizontal::plot(value, g)?, threshold));
                }
                let value = values[source as usize];
                // Fails the thresholded cell gate: never wet at this time.
                let passes = value > gate[source as usize];
                if !passes {
                    a.wet = false;
                    return Ok(0);
                }
                Ok(step(a, position, horizontal::plot(value, g)?, threshold))
            })
            .try_reduce(|| 0, |x, y| Ok(x + y))?;
        Ok(())
    }
}

fn regular(times: &[f64]) -> bool {
    times.len() < 3 || {
        let step = times[1] - times[0];
        times
            .windows(2)
            .all(|w| ((w[1] - w[0]) - step).abs() <= 1e-6)
    }
}

pub fn run(req: &Request, base: &Path, out: &Path) -> Result<Value, MapError> {
    let started = Instant::now();
    let h = &req.horizontal;
    let n = req.times_hours.len();
    if req.schema != SCHEMA
        || n == 0
        || n != h.profiles.len()
        || n >= u32::MAX as usize
        || req.times_hours.iter().any(|t| !t.is_finite())
        || req.times_hours.windows(2).any(|w| w[1] <= w[0])
        || !req.threshold.is_finite()
        || req.threshold < 0.0
    {
        return Err(MapError::Invalid(
            "whole-simulation schema, times (finite, strictly increasing, one per profile) or threshold".into(),
        ));
    }
    if h.stream_outputs || h.min_depth != 0.0 {
        return Err(MapError::Invalid(
            "whole-simulation horizontal requests must not stream and must not add a depth cutoff"
                .into(),
        ));
    }
    let mut products = req.products.clone();
    products.sort();
    products.dedup();
    if products.is_empty()
        || products.len() != req.products.len()
        || products.iter().any(|p| !PRODUCTS.contains(&p.as_str()))
    {
        return Err(MapError::Invalid(
            "products must be unique arrival_time, duration or percent_inundated".into(),
        ));
    }
    if products.iter().any(|p| p == "duration") && !req.allow_unqualified {
        return Err(MapError::Unsupported(
            "duration is not qualified (native durations are one output interval shorter on some recession pixels); set allow_unqualified to deliver it".into(),
        ));
    }
    let [_, _, width, height] = h.window;
    let count = width
        .checked_mul(height)
        .filter(|&c| c > 0 && c <= crate::model::MAX_PROFILE_PIXELS as usize)
        .ok_or_else(|| MapError::Budget("whole-simulation window empty or too large".into()))?;
    // Compact accumulators (at most 32 B per window pixel) and the three
    // float32 products (12 B) are admitted out of the caller's budget before
    // the horizontal plan is checked.
    let own = (count as u64)
        .checked_mul(44)
        .ok_or_else(|| MapError::Budget("whole-simulation plan overflow".into()))?;
    let budget = h
        .max_working_bytes
        .checked_sub(own)
        .ok_or_else(|| MapError::Budget("whole-simulation accumulators exceed budget".into()))?;
    let horizontal_req = horizontal::Request {
        max_working_bytes: budget,
        ..h.clone()
    };
    let minimum = horizontal::read_cell_values(
        &crate::model::resolve(base, &req.cell_minimum_file),
        h.cell_count,
    )?;
    if minimum.iter().any(|m| !m.is_finite()) {
        return Err(MapError::Invalid(
            "cell minimum elevations must be finite".into(),
        ));
    }
    let mut accumulator = Accumulator {
        threshold: req.threshold,
        gate: minimum.iter().map(|m| m + req.threshold).collect(),
        ..Default::default()
    };
    let mut report = horizontal::run_with(&horizontal_req, base, out, Some(&mut accumulator))?;
    report["schema"] = "ras-commander-whole-simulation-report/1".into();
    report["whole_simulation"] = json!({"method": method_record(), "times": n,
        "threshold": req.threshold, "products": products, "status": "not_run"});
    if report["status"] != "complete" {
        report["whole_simulation"]["status"] = "aborted".into();
        sink::write_json(&out.join("report.json"), &report)?;
        return Ok(report);
    }
    let tick = Instant::now();
    let result = finish(
        &accumulator.acc,
        &accumulator.index,
        count,
        &req.times_hours,
    );
    let equal_samples = accumulator.equal;
    drop(accumulator);
    let irregular = !regular(&req.times_hours);
    let unqualified = json!({
        "irregular_intervals": irregular,
        "initially_wet_pixels": result.initially_wet,
        "multiple_period_pixels": result.multiple_periods,
        "threshold_equal_samples": equal_samples,
    });
    let any = irregular;
    report["whole_simulation"]["wet_pixels"] = result.wet_pixels.into();
    report["whole_simulation"]["unqualified"] = unqualified;
    if any && !req.allow_unqualified {
        report["status"] = "aborted".into();
        report["error"] =
            "unsupported: unqualified whole-simulation cases present (see whole_simulation.unqualified); set allow_unqualified to deliver them".into();
        report["whole_simulation"]["status"] = "refused".into();
        sink::write_json(&out.join("report.json"), &report)?;
        return Err(MapError::Unsupported(
            "unqualified whole-simulation cases present".into(),
        ));
    }
    let dir = out.join("whole_simulation");
    fs::create_dir(&dir)?;
    for product in &products {
        let values = match product.as_str() {
            "arrival_time" => &result.arrival,
            "duration" => &result.duration,
            _ => &result.percent,
        };
        horizontal::write_values(&dir.join(format!("{product}.bin")), values)?;
    }
    sink::write_json(
        &dir.join("unit.json"),
        &json!({"id":"whole_simulation","status":"complete","products":products}),
    )?;
    report["whole_simulation"]["status"] = "complete".into();
    report["whole_simulation"]["qualified"] = (!any).into();
    report["whole_simulation"]["product_seconds"] = tick.elapsed().as_secs_f64().into();
    report["total_seconds"] = started.elapsed().as_secs_f64().into();
    sink::write_json(&out.join("report.json"), &report)?;
    Ok(report)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::horizontal::Consumer;

    fn run_series(series: &[Vec<f32>], threshold: f32, times: &[f64]) -> (Accumulated, u64) {
        // One cell per pixel on ground 0 (exact float32 depths); NaN = dry cell.
        let n = series[0].len();
        let owner: Vec<u32> = (1..=n as u32).collect();
        let mut a = Accumulator {
            threshold,
            gate: vec![f32::NEG_INFINITY; n],
            ..Default::default()
        };
        a.prepare(&owner, &vec![0.0; n], None).unwrap();
        for (k, values) in series.iter().enumerate() {
            a.profile(k, values, None).unwrap();
        }
        (finish(&a.acc, &a.index, n, times), a.equal)
    }

    #[test]
    fn strict_threshold_first_last_count_and_periods() {
        let times = [0.0, 0.25, 0.5, 0.75, 1.0];
        let series = vec![
            vec![0.1, 0.6, 0.6, 0.6, 0.4, 0.6, 0.5, f32::NAN],
            vec![0.2, 0.7, 0.4, 0.1, 0.4, 0.6, 0.5, f32::NAN],
            vec![0.3, 0.8, 0.4, 0.7, 0.6, 0.4, 0.5, f32::NAN],
            vec![0.4, 0.4, 0.9, 0.1, 0.7, 0.4, 0.5, f32::NAN],
            vec![0.1, 0.1, 0.1, 0.1, 0.4, 0.4, 0.5, f32::NAN],
        ];
        let (r, equal) = run_series(&series, 0.5, &times);
        assert_eq!(equal, 5); // pixel 6 equals the threshold: reported, dry
        assert!(r.arrival[0].is_nan() && r.percent[0].is_nan() && r.duration[0].is_nan());
        // Pixel 1 wet 0..2, dry at 3 (not the final time): ends at t[3].
        assert_eq!(
            (r.arrival[1], r.duration[1], r.percent[1]),
            (0.0, 0.75, 60.0)
        );
        // Pixel 5 wet 0..1, dry at 2: ends at t[2].
        assert_eq!((r.arrival[5], r.duration[5]), (0.0, 0.5));
        // Pixels 2 and 4: last wet at 3, dry only at the final time: end at t[3].
        assert_eq!(
            (r.arrival[2], r.duration[2], r.percent[2]),
            (0.0, 0.75, 40.0)
        );
        assert_eq!(
            (r.arrival[4], r.duration[4], r.percent[4]),
            (0.5, 0.25, 40.0)
        );
        assert!(r.arrival[6].is_nan() && r.arrival[7].is_nan());
        assert_eq!(r.wet_pixels, 5);
        assert_eq!(r.initially_wet, 4); // pixels 1, 2, 3 and 5
        assert_eq!(r.multiple_periods, 2); // pixels 2 and 3
    }

    #[test]
    fn single_wet_sample_and_plot_tolerance() {
        let series: Vec<Vec<f32>> = [0.0_f32, 0.0, 0.0, 1.0, 0.0]
            .iter()
            .map(|d| vec![*d])
            .collect();
        let (r, _) = run_series(&series, 0.5, &[0.0, 5.0, 10.0, 15.0, 20.0]);
        assert_eq!(
            (r.arrival[0], r.duration[0], r.percent[0]),
            (15.0, 0.0, 20.0)
        );
        assert_eq!((r.initially_wet, r.multiple_periods), (0, 0));
        // Threshold 0: a depth under the native plot tolerance is not visible, so not wet.
        let (r, _) = run_series(&[vec![0.0005], vec![0.002]], 0.0, &[0.0, 1.0]);
        assert_eq!((r.arrival[0], r.percent[0]), (1.0, 50.0));
    }

    #[test]
    fn plot_matches_the_horizontal_kernel() {
        let ground = [1000.0_f32, 1000.0, 100000.0, 10.0, f32::NAN, 5.0];
        let values = [
            f32::from_bits(0x447a000f),
            f32::from_bits(0x447a0010),
            100000.0,
            10.0005,
            12.0,
            f32::NAN,
        ];
        for (&v, &g) in values.iter().zip(&ground) {
            let mut depth = [f32::NAN];
            let mut wse = [f32::NAN];
            horizontal::evaluate_for_test(&[1], &[g], &[v], &mut depth, &mut wse);
            let plotted = horizontal::plot(v, g).unwrap();
            assert_eq!(
                plotted.map(f32::to_bits),
                (!depth[0].is_nan()).then(|| depth[0].to_bits())
            );
        }
    }

    #[test]
    fn thresholded_cell_gate_and_wet_to_the_end() {
        // Two cells, pixel depth always 0.6 above ground 0; cell 0 minimum 0.2
        // (gate 0.7 fails), cell 1 minimum -1 (gate -0.5 passes).
        let mut a = Accumulator {
            threshold: 0.5,
            gate: vec![0.2 + 0.5, -1.0 + 0.5],
            ..Default::default()
        };
        a.prepare(&[1, 2], &[0.0, 0.0], None).unwrap();
        for k in 0..3 {
            a.profile(k, &[0.6, 0.6], None).unwrap();
        }
        let r = finish(&a.acc, &a.index, 2, &[0.0, 1.0, 2.0]);
        assert!(r.arrival[0].is_nan());
        // Wet at the final time: duration ends at the last wet time.
        assert_eq!(
            (r.arrival[1], r.duration[1], r.percent[1]),
            (0.0, 2.0, 100.0)
        );
    }

    #[test]
    fn storage_area_pools_have_no_cell_gate() {
        // Window of 4: pixel 0 is a 2D cell, pixels 2 and 3 are one storage
        // area on grounds 1.0 and 0.0, pixel 1 is unmapped.
        let storage = crate::storage_area::StorageAreaPixels {
            pixels: vec![2, 3],
            labels: vec![0, 0],
            ground: vec![1.0, 0.0],
            count: 1,
        };
        let mut a = Accumulator {
            threshold: 0.5,
            gate: vec![f32::INFINITY], // the 2D cell always fails its gate
            ..Default::default()
        };
        a.prepare(&[1, 0, 0, 0], &[0.0; 4], Some(&storage)).unwrap();
        // Pool WSE 0.6, 1.6, 1.4 (strictly > threshold above ground only where marked).
        for (k, pool) in [0.6_f32, 1.6, 1.4].into_iter().enumerate() {
            a.profile(k, &[9.0], Some(&[pool])).unwrap();
        }
        // Missing pool values with pool pixels present is an internal error.
        assert!(a.profile(3, &[9.0], None).is_err());
        let r = finish(&a.acc, &a.index, 4, &[0.0, 1.0, 2.0]);
        assert!(r.arrival[0].is_nan() && r.arrival[1].is_nan());
        // Ground 1.0: depth 0.6 only at t=1.
        assert_eq!(
            (r.arrival[2], r.percent[2]),
            (1.0, (100.0_f64 / 3.0) as f32)
        );
        // Ground 0.0: depth 0.6, 1.6, 1.4: wet throughout.
        assert_eq!((r.arrival[3], r.percent[3]), (0.0, 100.0));
    }

    #[test]
    fn times_are_float32_hours() {
        let times: Vec<f64> = (0..865).map(|k| k as f64 / 12.0).collect();
        let mut acc = vec![Acc {
            first: 244,
            last: 488,
            count: 245,
            wet: false,
            periods: 1,
        }];
        let r = finish(&acc, &[0], 1, &times);
        assert_eq!(
            r.duration[0].to_bits(),
            ((489.0_f64 / 12.0) as f32 - (244.0_f64 / 12.0) as f32).to_bits()
        );
        acc[0].last = 864;
        let r = finish(&acc, &[0], 1, &times);
        assert_eq!(r.duration[0], (72.0_f32) - (244.0_f64 / 12.0) as f32);
    }

    #[test]
    fn interval_regularity() {
        assert!(regular(&[0.0, 1.0 / 12.0, 2.0 / 12.0, 3.0 / 12.0]));
        assert!(regular(&[0.0, 7.0]));
        assert!(!regular(&[0.0, 1.0, 2.5]));
    }
}
