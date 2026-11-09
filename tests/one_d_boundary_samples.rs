use ras_commander_fast_map::{
    MapError,
    one_d::Segment,
    one_d_clip::{Barrier, Clip},
    one_d_native::{PreparedSurface, SampleRaster},
};

fn segment(start: usize, triangle: usize, upstream: usize) -> Segment {
    Segment {
        point_start: start,
        point_count: 4,
        triangle_start: triangle,
        triangle_count: 2,
        upstream,
        downstream: upstream + 1,
    }
}

fn square() -> [[f64; 4]; 4] {
    [
        [-1., -1., 0.5, -1.],
        [258., -1., 0.5, -1.],
        [258., 258., 0.5, -1.],
        [-1., 258., 0.5, -1.],
    ]
}

fn surface(max_bytes: usize) -> PreparedSurface {
    PreparedSurface::new(
        &square(),
        &[[0, 1, 2], [0, 2, 3]],
        &[segment(0, 0, 0)],
        2,
        &[0],
        max_bytes,
    )
    .unwrap()
}

fn raster(size: usize) -> SampleRaster {
    SampleRaster {
        bounds: [0., 0., size as f64, size as f64],
        size,
    }
}

#[test]
fn signed_samples_retain_dry_numeric_sentinel_and_nodata_claims() {
    let surface = surface(1 << 20);
    let mut d = [7.; 4];
    let mut w = [8.; 4];
    let mut claims = [0; 4];
    assert_eq!(
        surface
            .render_signed_raster(
                &[1., 1.],
                &[2., 10000., f32::NAN, 0.],
                raster(2),
                &mut d,
                &mut w,
                &mut claims,
            )
            .unwrap(),
        3
    );
    assert_eq!(d[0], -1.);
    assert_eq!(d[1], -9999.);
    assert!(d[2].is_nan() && w[2].is_nan());
    assert_eq!(d[3], 1.);
    assert_eq!(claims, [1; 4]);
}

#[test]
fn final_wse_sentinel_is_missing_without_masking_endpoint_or_depth_sentinels() {
    let s = surface(1 << 20);
    for (levels, missing) in [
        ([-9999., -9999.], true),
        ([-10000., -9998.], true),
        ([-9999., -9997.], false),
    ] {
        let mut d = [0.];
        let mut w = [0.];
        let mut claims = [0];
        let count = s
            .render_signed_raster(&levels, &[1.], raster(1), &mut d, &mut w, &mut claims)
            .unwrap();
        assert_eq!(count, usize::from(!missing));
        assert_eq!(claims, [1]);
        if missing {
            assert!(d[0].is_nan() && w[0].is_nan());
        } else {
            assert_eq!(d[0], -9999.);
            assert_eq!(w[0], -9998.);
        }
        claims.fill(0);
        let count = s
            .sample_signed_points(&levels, &[[0.5, 0.5]], &[1.], &mut d, &mut w, &mut claims)
            .unwrap();
        assert_eq!(count, usize::from(!missing));
        assert_eq!(claims, [1]);
        if missing {
            assert!(d[0].is_nan() && w[0].is_nan());
        } else {
            assert_eq!(d[0], -9999.);
            assert_eq!(w[0], -9998.);
        }
    }
}

#[test]
fn independent_extended_raster_keeps_all_257_samples_and_external_claims() {
    let surface = surface(2 << 20);
    let n = 257 * 257;
    let mut d = vec![0.; n];
    let mut w = vec![0.; n];
    let mut claims = vec![0; n];
    claims[0] = 1;
    let grid = SampleRaster {
        bounds: [0., -1., 257., 256.],
        size: 257,
    };
    assert_eq!(
        surface
            .render_signed_raster(&[4., 4.], &vec![5.; n], grid, &mut d, &mut w, &mut claims,)
            .unwrap(),
        n - 1
    );
    assert!(d[0].is_nan() && w[0].is_nan());
    assert_eq!(d[256], -1.);
    assert_eq!(d[n - 1], -1.);
    assert!(claims.iter().all(|&v| v == 1));
}

#[test]
fn raster_promoted_order_and_stitch_stored_order_are_distinct() {
    let mut points = square().to_vec();
    points.extend(square());
    let s = PreparedSurface::new(
        &points,
        &[[0, 1, 2], [0, 2, 3], [0, 1, 2], [0, 2, 3]],
        &[segment(0, 0, 0), segment(4, 2, 2)],
        4,
        &[1, 0],
        1 << 20,
    )
    .unwrap();
    let mut d = [0.];
    let mut w = [0.];
    let mut claims = [0];
    s.render_signed_raster(
        &[10., 10., 20., 20.],
        &[0.],
        raster(1),
        &mut d,
        &mut w,
        &mut claims,
    )
    .unwrap();
    assert_eq!(w, [20.]);
    claims.fill(0);
    s.sample_signed_points(
        &[10., 10., 20., 20.],
        &[[0.5, 0.5]],
        &[0.],
        &mut d,
        &mut w,
        &mut claims,
    )
    .unwrap();
    assert_eq!(w, [10.]);
    claims.fill(0);
    s.sample_signed_points(
        &[f32::NAN, 10., 20., 20.],
        &[[0.5, 0.5]],
        &[0.],
        &mut d,
        &mut w,
        &mut claims,
    )
    .unwrap();
    assert!(d[0].is_nan() && w[0].is_nan());
    assert_eq!(claims, [1]);
    claims.fill(0);
    s.render_signed_raster(
        &[10., 10., f32::NAN, 20.],
        &[0.],
        raster(1),
        &mut d,
        &mut w,
        &mut claims,
    )
    .unwrap();
    assert!(d[0].is_nan() && w[0].is_nan());
    assert_eq!(claims, [1]);
}

#[test]
fn clipping_retains_claims_in_both_sample_paths() {
    let clip = Clip {
        left_levee: Some(Barrier {
            m: 0.,
            elevation: 100.,
        }),
        ..Clip::default()
    };
    let mut points = square();
    for point in &mut points {
        point[3] = 1.;
    }
    let s = PreparedSurface::new_with_clipping(
        &points,
        &[[0, 1, 2], [0, 2, 3]],
        &[segment(0, 0, 0)],
        2,
        &[0],
        &[-1.; 4],
        &[clip; 2],
        1 << 20,
    )
    .unwrap();
    let mut d = [0.];
    let mut w = [0.];
    let mut claims = [0];
    assert_eq!(
        s.render_signed_raster(&[10.; 2], &[0.], raster(1), &mut d, &mut w, &mut claims)
            .unwrap(),
        0
    );
    assert_eq!(claims, [1]);
    assert!(d[0].is_nan() && w[0].is_nan());
    claims.fill(0);
    assert_eq!(
        s.sample_signed_points(&[10.; 2], &[[0.5, 0.5]], &[0.], &mut d, &mut w, &mut claims)
            .unwrap(),
        0
    );
    assert_eq!(claims, [1]);
    assert!(d[0].is_nan() && w[0].is_nan());
}

#[test]
fn sample_cell_is_recomputed_from_supplied_bounds() {
    let s = PreparedSurface::new(
        &[[0., 0., 0., 0.], [2., 0., 1., 0.], [0., 2., 0., 0.]],
        &[[0, 1, 2]],
        &[Segment {
            point_count: 3,
            triangle_count: 1,
            ..segment(0, 0, 0)
        }],
        2,
        &[0],
        1 << 20,
    )
    .unwrap();
    let mut d = [0.; 4];
    let mut w = [0.; 4];
    let mut claims = [0; 4];
    s.render_signed_raster(
        &[1., 0.],
        &[0.; 4],
        SampleRaster {
            bounds: [0., 0., 2., 1.],
            size: 2,
        },
        &mut d,
        &mut w,
        &mut claims,
    )
    .unwrap();
    assert_eq!(claims, [1, 1, 0, 0]);
    assert_eq!(&w[..2], &[0.25, 0.75]);
}

#[test]
fn point_tolerance_is_applied_within_exact_triangle_bounds_without_clamping() {
    let s = PreparedSurface::new(
        &[[0., 0., 0., 0.], [1., 0., 1., 0.], [0., 1., 1., 0.]],
        &[[0, 1, 2]],
        &[Segment {
            point_count: 3,
            triangle_count: 1,
            ..segment(0, 0, 0)
        }],
        2,
        &[0],
        1 << 20,
    )
    .unwrap();
    let points = [[0.5, 0.50005], [0.5, 0.5002], [1.00001, 0.]];
    let mut d = [0.; 3];
    let mut w = [0.; 3];
    let mut claims = [0; 3];
    s.sample_signed_points(&[1., 0.], &points, &[0.; 3], &mut d, &mut w, &mut claims)
        .unwrap();
    assert_eq!(claims, [1, 0, 0]);
    assert_eq!(w[0], 1.00005_f32);
    assert!(w[1].is_nan() && w[2].is_nan());
}

#[test]
fn incremental_raster_weight_and_point_weight_have_independent_rounding() {
    let intercept = 0.35500002980232237;
    let s = PreparedSurface::new(
        &[
            [0., 0., intercept, 0.],
            [0., 2., intercept, 0.],
            [64., 0., intercept + 0.64, 0.],
        ],
        &[[0, 1, 2]],
        &[Segment {
            point_count: 3,
            triangle_count: 1,
            ..segment(0, 0, 0)
        }],
        2,
        &[0],
        1 << 20,
    )
    .unwrap();
    let mut d = vec![0.; 4096];
    let mut w = vec![0.; 4096];
    let mut claims = vec![0; 4096];
    s.render_signed_raster(
        &[1., 0.],
        &vec![0.; 4096],
        SampleRaster {
            bounds: [0., -62., 64., 2.],
            size: 64,
        },
        &mut d,
        &mut w,
        &mut claims,
    )
    .unwrap();
    assert_eq!(w[14].to_bits(), 0x3f00_0001);
    let mut pd = [0.];
    let mut pw = [0.];
    let mut pc = [0];
    s.sample_signed_points(&[1., 0.], &[[14.5, 1.5]], &[0.], &mut pd, &mut pw, &mut pc)
        .unwrap();
    assert_eq!(pw[0].to_bits(), 0x3f00_0000);
}

#[test]
fn invalid_buffers_raster_points_and_admission_refuse_before_mutation() {
    let s = surface(1 << 20);
    let mut d = [7.];
    let mut w = [8.];
    let mut claims = [0];
    let bad = SampleRaster {
        bounds: [0., 0., f64::INFINITY, 1.],
        size: 1,
    };
    assert!(
        s.render_signed_raster(&[1.; 2], &[0.], bad, &mut d, &mut w, &mut claims)
            .is_err()
    );
    assert!(
        s.sample_signed_points(
            &[1.; 2],
            &[[f64::NAN, 0.]],
            &[0.],
            &mut d,
            &mut w,
            &mut claims
        )
        .is_err()
    );
    assert!(
        s.render_signed_raster(
            &[1.; 2],
            &[f32::INFINITY],
            raster(1),
            &mut d,
            &mut w,
            &mut claims
        )
        .is_err()
    );
    assert_eq!(d, [7.]);
    assert_eq!(w, [8.]);
    assert_eq!(claims, [0]);
    let small = surface(2500);
    let mut d = [7.; 100];
    let mut w = [8.; 100];
    let mut claims = [0; 100];
    assert!(matches!(
        small.render_signed_raster(
            &[1.; 2],
            &[0.; 100],
            raster(10),
            &mut d,
            &mut w,
            &mut claims
        ),
        Err(MapError::Budget(_))
    ));
    assert_eq!(d, [7.; 100]);
    assert_eq!(w, [8.; 100]);
    assert_eq!(claims, [0; 100]);
}
