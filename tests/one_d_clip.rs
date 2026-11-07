use ras_commander_fast_map::{
    MapError,
    one_d_clip::{Barrier, Clip, decode, validate, visible},
};

fn barrier(m: f32, elevation: f32) -> Option<Barrier> {
    Some(Barrier { m, elevation })
}

#[test]
fn decode_accepts_complete_pairs_and_refuses_partial_or_nonfinite_records() {
    let nan = f32::NAN;
    assert_eq!(decode([nan; 8]).unwrap(), Clip::default());
    let value = decode([-2., -10., 2., 30., -1.5, 12., 1.5, 13.]).unwrap();
    assert_eq!(value.left_levee, barrier(-2., -10.));
    assert_eq!(value.right_levee, barrier(2., 30.));
    assert_eq!(value.left_end, barrier(-1.5, 12.));
    assert_eq!(value.right_end, barrier(1.5, 13.));
    for pair in [
        [nan, 1.],
        [1., nan],
        [f32::INFINITY, 1.],
        [1., f32::NEG_INFINITY],
        [-2.01, 1.],
        [2.01, 1.],
    ] {
        for offset in [0, 2, 4, 6] {
            let mut record = [nan; 8];
            record[offset..offset + 2].copy_from_slice(&pair);
            assert!(matches!(decode(record), Err(MapError::Invalid(_))));
        }
    }
}

#[test]
fn levees_require_both_endpoint_records_and_strictly_higher_elevations() {
    let u = Clip {
        left_levee: barrier(-1., 20.),
        ..Clip::default()
    };
    let d = Clip {
        left_levee: barrier(-1., 30.),
        ..Clip::default()
    };
    assert!(!visible(u, d, 10., 15., 0.5, -1.1, 12.5));
    assert!(visible(u, d, 20., 15., 0.5, -1.1, 17.5));
    assert!(visible(u, d, 10., 30., 0.5, -1.1, 20.));
    assert!(visible(u, Clip::default(), 10., 15., 0.5, -1.1, 12.5));
    assert!(visible(Clip::default(), d, 10., 15., 0.5, -1.1, 12.5));
}

#[test]
fn levees_keep_exact_boundary_ties_and_use_the_correct_side() {
    let clip = Clip {
        left_levee: barrier(-1., 20.),
        right_levee: barrier(1., 20.),
        ..Clip::default()
    };
    for m in [-1., 0., 1.] {
        assert!(visible(clip, clip, 10., 10., 0.5, m, 10.));
    }
    assert!(!visible(
        clip,
        clip,
        10.,
        10.,
        0.5,
        f32::from_bits((-1_f32).to_bits() + 1),
        10.
    ));
    assert!(!visible(
        clip,
        clip,
        10.,
        10.,
        0.5,
        f32::from_bits(1_f32.to_bits() + 1),
        10.
    ));
}

#[test]
fn levee_m_interpolation_preserves_weighted_sum_rounding() {
    let u = Clip {
        left_levee: barrier(-1.9, 20.),
        ..Clip::default()
    };
    let d = Clip {
        left_levee: barrier(-1.8, 20.),
        ..Clip::default()
    };
    let native_boundary = f32::from_bits(0xbfe6c8b5);
    let difference_form = -1.8_f32 + 0.03_f32 * (-1.9_f32 - -1.8_f32);
    assert_eq!(difference_form.to_bits(), 0xbfe6c8b4);
    // At the native boundary this pixel remains visible. Reassociation would
    // incorrectly move the left boundary inward by one Float32 step.
    assert!(visible(u, d, 10., 10., 0.03, native_boundary, 10.));
    assert!(!visible(
        u,
        d,
        10.,
        10.,
        0.03,
        f32::from_bits(native_boundary.to_bits() + 1),
        10.
    ));
}

#[test]
fn equal_height_end_obstructions_clip_only_below_their_height() {
    for left in [true, false] {
        let clip = if left {
            Clip {
                left_end: barrier(-1., 5.),
                ..Clip::default()
            }
        } else {
            Clip {
                right_end: barrier(1., 5.),
                ..Clip::default()
            }
        };
        let outside = if left { -1.5 } else { 1.5 };
        // Equal obstruction heights activate clipping even though the endpoint
        // water levels are not both below the obstruction elevation.
        assert!(!visible(clip, clip, 10., 0., 0.25, outside, 2.5));
        assert!(visible(clip, clip, 10., 0., 0.5, outside, 5.));
        assert!(visible(clip, clip, 10., 0., 0.75, outside, 7.5));
        assert!(visible(
            clip,
            clip,
            10.,
            0.,
            0.75,
            if left { -1. } else { 1. },
            4.
        ));
    }
}

#[test]
fn unequal_height_end_obstructions_need_both_endpoint_levels_below() {
    for left in [true, false] {
        let u = if left {
            Clip {
                left_end: barrier(-1., 20.),
                ..Clip::default()
            }
        } else {
            Clip {
                right_end: barrier(1., 20.),
                ..Clip::default()
            }
        };
        let d = if left {
            Clip {
                left_end: barrier(-1., 30.),
                ..Clip::default()
            }
        } else {
            Clip {
                right_end: barrier(1., 30.),
                ..Clip::default()
            }
        };
        let outside = if left { -1.5 } else { 1.5 };
        assert!(!visible(u, d, 10., 15., 0.5, outside, 12.5));
        assert!(visible(u, d, 20., 15., 0.5, outside, 17.5));
        assert!(visible(u, Clip::default(), 10., 15., 0.5, outside, 12.5));
    }
}

#[test]
fn interpolation_does_not_clamp_the_native_weight() {
    let u = Clip {
        right_levee: barrier(0., 20.),
        ..Clip::default()
    };
    let d = Clip {
        right_levee: barrier(1., 20.),
        ..Clip::default()
    };
    // Extrapolating a native edge weight moves the bound to -0.25. Clamping
    // would incorrectly keep this m=-0.125 pixel inside the right boundary.
    assert!(!visible(u, d, 10., 10., 1.25, -0.125, 10.));
}

#[test]
fn caller_constructed_clips_must_validate_before_use() {
    assert!(validate(Clip::default()).is_ok());
    assert!(
        validate(Clip {
            left_end: barrier(-2., -30.),
            ..Clip::default()
        })
        .is_ok()
    );
    for invalid in [
        Barrier {
            m: f32::NAN,
            elevation: 1.,
        },
        Barrier {
            m: 0.,
            elevation: f32::INFINITY,
        },
        Barrier {
            m: 2.01,
            elevation: 1.,
        },
    ] {
        for clip in [
            Clip {
                left_levee: Some(invalid),
                ..Clip::default()
            },
            Clip {
                right_levee: Some(invalid),
                ..Clip::default()
            },
            Clip {
                left_end: Some(invalid),
                ..Clip::default()
            },
            Clip {
                right_end: Some(invalid),
                ..Clip::default()
            },
        ] {
            assert!(matches!(validate(clip), Err(MapError::Invalid(_))));
        }
    }
}

#[test]
fn default_strip_bounds_apply_without_explicit_barriers_and_keep_ties() {
    let clip = Clip::default();
    for m in [-2., -1., 0., 1., 2.] {
        assert!(visible(clip, clip, 10., 10., 0.5, m, 10.));
    }
    assert!(!visible(
        clip,
        clip,
        10.,
        10.,
        0.5,
        f32::from_bits((-2_f32).to_bits() + 1),
        10.
    ));
    assert!(!visible(
        clip,
        clip,
        10.,
        10.,
        0.5,
        f32::from_bits(2_f32.to_bits() + 1),
        10.
    ));
}

#[test]
fn default_strip_bounds_preserve_float32_interpolation_of_endpoint_constants() {
    let clip = Clip::default();
    // Here Float32(1-weight) loses the unit, so each weighted limit is zero.
    // Replacing the native weighted sums with fixed +/-2 would retain this pixel.
    let weight = 33_554_432_f32;
    assert_eq!(1_f32 - weight, -weight);
    assert!(!visible(clip, clip, 0., 0., weight, 0.25, 0.));
    assert!(!visible(clip, clip, 0., 0., weight, -0.25, 0.));
    assert!(visible(clip, clip, 0., 0., weight, 0., 0.));
}
