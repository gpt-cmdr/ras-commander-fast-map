use ras_commander_fast_map::{
    MapError,
    boundary_contour::{self as contour, Grid, Limits, MultiPolygon, Unit},
};
use rayon::prelude::*;
fn limits() -> Limits {
    Limits {
        max_working_bytes: 32 << 20,
    }
}
fn grid(n: usize) -> Grid {
    Grid {
        rows: n,
        cols: n,
        transform: [0., 1., 0., n as f64, 0., -1.],
    }
}
fn output(units: &[Unit]) -> contour::Output {
    contour::finish(units, None, limits()).unwrap()
}
fn area(p: &MultiPolygon) -> f64 {
    p.iter()
        .map(|p| {
            p.iter()
                .map(|r| {
                    r.windows(2)
                        .map(|w| w[0][0] * w[1][1] - w[1][0] * w[0][1])
                        .sum::<f64>()
                        * 0.5
                })
                .sum::<f64>()
        })
        .sum()
}
#[test]
fn holes_are_preserved_and_closed() {
    let mut field = vec![Some(1.); 25];
    field[12] = Some(-1.);
    let unit = contour::grid(0, grid(5), &field, limits()).unwrap();
    let out = output(&[unit]);
    assert_eq!(out.coordinates.len(), 1);
    assert_eq!(out.coordinates[0].len(), 2);
    for ring in &out.coordinates[0] {
        assert_eq!(ring.first(), ring.last())
    }
    assert!(area(&out.coordinates) > 0. && area(&out.coordinates) < 16.);
}
#[test]
fn nodata_three_corner_fan_does_not_fill_missing_wedge() {
    let unit = contour::grid(0, grid(2), &[None, Some(1.), Some(1.), Some(1.)], limits()).unwrap();
    let out = output(&[unit]);
    assert_eq!(out.coordinates.len(), 1);
    assert_eq!(area(&out.coordinates), 0.5);
    let missing = contour::grid(0, grid(2), &[None, None, Some(1.), Some(1.)], limits()).unwrap();
    assert!(output(&[missing]).coordinates.is_empty());
}
#[test]
fn valid_minus_9999_depth_is_not_missing() {
    let xy = [[0., 0.], [10000., 0.], [0., 10000.]];
    let a = contour::tin(
        0,
        &xy,
        &[Some(-9999.), Some(1.), Some(1.)],
        &[[0, 1, 2]],
        limits(),
    )
    .unwrap();
    let b = contour::tin(0, &xy, &[None, Some(1.), Some(1.)], &[[0, 1, 2]], limits()).unwrap();
    assert!(!output(&[a]).coordinates.is_empty());
    assert!(output(&[b]).coordinates.is_empty());
}
#[test]
fn zero_is_included_without_extra_cutoff() {
    let zero = contour::grid(0, grid(2), &[Some(0.); 4], limits()).unwrap();
    assert_eq!(area(&output(&[zero]).coordinates), 1.);
    let half = contour::grid(
        0,
        grid(2),
        &[Some(-1.), Some(1.), Some(-1.), Some(1.)],
        limits(),
    )
    .unwrap();
    assert_eq!(area(&output(&[half]).coordinates), 0.5);
}
#[test]
fn tin_winding_is_not_a_missing_wet_region() {
    let xy = [[0., 0.], [4., 0.], [0., 4.]];
    let d = [Some(1.); 3];
    let a = contour::tin(0, &xy, &d, &[[0, 1, 2]], limits()).unwrap();
    let b = contour::tin(0, &xy, &d, &[[2, 1, 0]], limits()).unwrap();
    assert_eq!(output(&[a]), output(&[b]));
}
#[test]
fn aoi_holes_and_nested_islands_remain_topological() {
    let unit = contour::grid(0, grid(6), &[Some(1.); 36], limits()).unwrap();
    let outer = vec![[0., 0.], [6., 0.], [6., 6.], [0., 6.], [0., 0.]];
    let hole = vec![[1., 1.], [4., 1.], [4., 4.], [1., 4.], [1., 1.]];
    let island = vec![[2., 2.], [3., 2.], [3., 3.], [2., 3.], [2., 2.]];
    let aoi = vec![vec![outer, hole], vec![island]];
    let out = contour::finish(std::slice::from_ref(&unit), Some(&aoi), limits()).unwrap();
    assert_eq!(out.coordinates.len(), 2);
    assert!(out.coordinates.iter().any(|p| p.len() == 2));
    assert_eq!(area(&out.coordinates), 17.);
    let reverse: MultiPolygon = aoi
        .into_iter()
        .map(|p| {
            p.into_iter()
                .map(|mut r| {
                    r.reverse();
                    r
                })
                .collect()
        })
        .collect();
    assert_eq!(
        out,
        contour::finish(&[unit], Some(&reverse), limits()).unwrap()
    );
}
#[test]
fn collection_order_and_thread_count_do_not_change_output() {
    let run = |threads| {
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(threads)
            .build()
            .unwrap();
        let mut units: Vec<_> = pool.install(|| {
            (0..8u64)
                .into_par_iter()
                .map(|i| {
                    let mut g = grid(3);
                    g.transform[0] = i as f64;
                    let mut v = [Some(1.); 9];
                    v[4] = Some(-1.);
                    contour::grid(i, g, &v, limits()).unwrap()
                })
                .collect()
        });
        let forward = output(&units);
        units.reverse();
        assert_eq!(forward, output(&units));
        forward
    };
    assert_eq!(run(1), run(4));
}
#[test]
fn fragment_units_cannot_reuse_sequence() {
    let a = contour::grid(7, grid(2), &[Some(1.); 4], limits()).unwrap();
    assert!(matches!(
        contour::finish(&[a.clone(), a], None, limits()),
        Err(MapError::Invalid(_))
    ));
}
#[test]
fn invalid_samples_coordinates_and_budget_fail_explicitly() {
    assert!(matches!(
        contour::grid(0, grid(2), &[Some(f32::NAN); 4], limits()),
        Err(MapError::Invalid(_))
    ));
    let mut g = grid(2);
    g.transform[0] = ((1u64 << 61) as f64) / 1000.;
    assert!(matches!(
        contour::grid(0, g, &[Some(1.); 4], limits()),
        Err(MapError::Invalid(_))
    ));
    assert!(matches!(
        contour::grid(
            0,
            grid(2),
            &[Some(1.); 4],
            Limits {
                max_working_bytes: 0
            }
        ),
        Err(MapError::Budget(_))
    ));
    assert!(matches!(
        contour::tin(0, &[[0., 0.]], &[Some(1.)], &[[0, 1, 2]], limits()),
        Err(MapError::Invalid(_))
    ));
}
#[test]
fn haloed_window_and_final_clip_match_full_contour() {
    let full: Vec<_> = (0..25).map(|i| Some((i % 5) as f32 - 1.5)).collect();
    let all = contour::grid(0, grid(5), &full, limits()).unwrap();
    let aoi = vec![vec![vec![
        [1.8, 1.2],
        [2.2, 1.2],
        [2.2, 2.8],
        [1.8, 2.8],
        [1.8, 1.2],
    ]]];
    let mut cropped = vec![];
    for r in 1..5 {
        for c in 1..4 {
            cropped.push(full[r * 5 + c]);
        }
    }
    let g = Grid {
        rows: 4,
        cols: 3,
        transform: [1., 1., 0., 4., 0., -1.],
    };
    let window = contour::grid(0, g, &cropped, limits()).unwrap();
    assert_eq!(
        contour::finish(&[all], Some(&aoi), limits()).unwrap(),
        contour::finish(&[window], Some(&aoi), limits()).unwrap()
    );
}
