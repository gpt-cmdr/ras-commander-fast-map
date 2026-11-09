use ras_commander_fast_map::{
    MapError,
    boundary_surface::{
        self as surface, Composition, Coverage, SampleInputs, SignedField, SignedSample,
    },
};

fn pair(depth: f32, wse: f32) -> SignedSample {
    SignedSample { depth, wse }
}

fn sample() -> SampleInputs {
    SampleInputs {
        two_d_owner: 0,
        two_d: SignedSample::missing(),
        storage_owner: 0,
        storage_level: f32::NAN,
        xs_claim: false,
        xs: SignedSample::missing(),
        terrain: 1.,
    }
}

#[test]
fn coverage_precedes_missing_dry_and_clipped_visibility() {
    let mut input = sample();
    input.two_d_owner = 3;
    input.storage_owner = 1;
    input.storage_level = 30.;
    input.xs_claim = true;
    input.xs = pair(39., 40.);
    let chosen = surface::compose_sample(input).unwrap();
    assert_eq!(chosen.coverage, Coverage::TwoD(3));
    assert!(chosen.values.depth.is_nan());
    input.two_d = pair(-3., -2.);
    assert_eq!(surface::compose_sample(input).unwrap().values, input.two_d);
    input.two_d_owner = 0;
    input.two_d = SignedSample::missing();
    input.storage_level = f32::NAN;
    let chosen = surface::compose_sample(input).unwrap();
    assert_eq!(chosen.coverage, Coverage::Storage(1));
    assert!(chosen.values.depth.is_nan());
    input.storage_owner = 0;
    input.xs = SignedSample::missing();
    let chosen = surface::compose_sample(input).unwrap();
    assert_eq!(chosen.coverage, Coverage::Xs);
    assert!(chosen.values.depth.is_nan());
}

#[test]
fn original_signed_bits_include_negative_zero_and_numeric_minus9999() {
    let mut input = sample();
    input.two_d_owner = 1;
    input.two_d = pair(-0.0, 1.);
    assert_eq!(
        surface::compose_sample(input)
            .unwrap()
            .values
            .depth
            .to_bits(),
        (-0.0_f32).to_bits()
    );
    input.two_d = pair(-9999., -9998.);
    assert_eq!(surface::compose_sample(input).unwrap().values, input.two_d);
    input.two_d_owner = 0;
    input.two_d = SignedSample::missing();
    input.xs_claim = true;
    input.xs = pair(-0.0, 1.);
    assert_eq!(
        surface::compose_sample(input)
            .unwrap()
            .values
            .depth
            .to_bits(),
        (-0.0_f32).to_bits()
    );
    input.storage_owner = 1;
    input.storage_level = -9998.;
    assert_eq!(
        surface::compose_sample(input).unwrap().values,
        pair(-9999., -9998.)
    );
}

#[test]
fn selected_final_wse_sentinel_keeps_coverage_and_never_falls_back() {
    let mut input = sample();
    input.xs_claim = true;
    input.xs = pair(-10000., -9999.);
    for expected in [Coverage::Xs, Coverage::Storage(1), Coverage::TwoD(1)] {
        match expected {
            Coverage::Storage(_) => {
                input.storage_owner = 1;
                input.storage_level = -9999.;
            }
            Coverage::TwoD(_) => {
                input.two_d_owner = 1;
                input.two_d = pair(-10000., -9999.);
            }
            _ => {}
        }
        let output = surface::compose_sample(input).unwrap();
        assert_eq!(output.coverage, expected);
        assert!(output.values.depth.is_nan() && output.values.wse.is_nan());
    }
    input.two_d = pair(-9999., -9998.);
    assert_eq!(surface::compose_sample(input).unwrap().values, input.two_d);
}

#[test]
fn missing_terrain_keeps_physical_coverage_in_every_component() {
    let mut input = sample();
    input.terrain = f32::NAN;
    input.xs_claim = true;
    input.xs = pair(4., 5.);
    for expected in [Coverage::Xs, Coverage::Storage(1), Coverage::TwoD(1)] {
        match expected {
            Coverage::Storage(_) => {
                input.storage_owner = 1;
                input.storage_level = 6.;
            }
            Coverage::TwoD(_) => {
                input.two_d_owner = 1;
                input.two_d = pair(9., 10.);
            }
            _ => {}
        }
        let output = surface::compose_sample(input).unwrap();
        assert_eq!(output.coverage, expected);
        assert!(output.values.depth.is_nan() && output.values.wse.is_nan());
    }
}

#[test]
fn storage_uses_first_stored_geometry_claim_even_if_its_level_is_missing() {
    let mut owners = [0; 3];
    assert_eq!(
        surface::claim_first_storage(&mut owners, 1, 2, &[true, false, true]).unwrap(),
        2
    );
    assert_eq!(
        surface::claim_first_storage(&mut owners, 2, 2, &[true; 3]).unwrap(),
        1
    );
    assert_eq!(owners, [1, 2, 1]);
    let mut d = [8.; 3];
    let mut w = [9.; 3];
    let mut coverage = [Coverage::None; 3];
    let missing = [f32::NAN; 3];
    let counts = surface::compose_into(
        Composition {
            two_d_owners: &[0; 3],
            two_d_count: 0,
            two_d: SignedField {
                depth: &missing,
                wse: &missing,
            },
            storage_owners: &owners,
            storage_levels: &[f32::NAN, 0.],
            xs_claims: &[1; 3],
            xs: SignedField {
                depth: &[9.; 3],
                wse: &[10.; 3],
            },
            terrain: &[1.; 3],
            max_working_bytes: 1024,
        },
        &mut d,
        &mut w,
        &mut coverage,
    )
    .unwrap();
    assert!(d[0].is_nan() && d[2].is_nan());
    assert_eq!(d[1], -1.);
    assert_eq!(counts.storage_owned, 3);
    assert_eq!(counts.finite_samples, 1);
    assert_eq!(
        coverage,
        [
            Coverage::Storage(1),
            Coverage::Storage(2),
            Coverage::Storage(1)
        ]
    );
}

#[test]
fn aligned_batch_reports_chosen_coverage_without_wet_only_fallback() {
    let nan = f32::NAN;
    let mut d = [8.; 4];
    let mut w = [9.; 4];
    let mut coverage = [Coverage::Xs; 4];
    let counts = surface::compose_into(
        Composition {
            two_d_owners: &[1, 0, 0, 0],
            two_d_count: 1,
            two_d: SignedField {
                depth: &[nan; 4],
                wse: &[nan; 4],
            },
            storage_owners: &[1, 1, 0, 0],
            storage_levels: &[0.],
            xs_claims: &[1, 1, 1, 0],
            xs: SignedField {
                depth: &[5., 5., -0.0, nan],
                wse: &[6., 6., 1., nan],
            },
            terrain: &[1.; 4],
            max_working_bytes: 1024,
        },
        &mut d,
        &mut w,
        &mut coverage,
    )
    .unwrap();
    assert_eq!(
        (
            counts.two_d_owned,
            counts.storage_owned,
            counts.xs_owned,
            counts.unowned
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(counts.finite_samples, 2);
    assert!(d[0].is_nan() && d[3].is_nan());
    assert_eq!(d[1], -1.);
    assert_eq!(d[2].to_bits(), (-0.0_f32).to_bits());
    assert_eq!(
        coverage,
        [
            Coverage::TwoD(1),
            Coverage::Storage(1),
            Coverage::Xs,
            Coverage::None
        ]
    );
}

#[test]
fn invalid_sample_values_and_ownership_are_refused_even_when_unselected() {
    for input in [
        SampleInputs {
            xs: pair(1., 2.),
            ..sample()
        },
        SampleInputs {
            two_d: pair(1., 2.),
            ..sample()
        },
        SampleInputs {
            xs_claim: true,
            xs: pair(1., f32::NAN),
            ..sample()
        },
        SampleInputs {
            two_d_owner: 1,
            two_d: pair(1., f32::INFINITY),
            ..sample()
        },
        SampleInputs {
            terrain: f32::INFINITY,
            ..sample()
        },
        SampleInputs {
            storage_level: f32::NEG_INFINITY,
            ..sample()
        },
        SampleInputs {
            storage_owner: u32::MAX,
            ..sample()
        },
    ] {
        assert!(matches!(
            surface::compose_sample(input),
            Err(MapError::Invalid(_))
        ));
    }
}

#[test]
fn batch_overflow_bad_labels_and_budgets_refuse_before_any_output_write() {
    for case in 0..4 {
        let mut d = [7.; 2];
        let mut w = [8.; 2];
        let mut coverage = [Coverage::Xs; 2];
        let missing = [f32::NAN; 2];
        let owners = if case == 1 { [1, 2] } else { [1, 1] };
        let input = Composition {
            two_d_owners: &[0; 2],
            two_d_count: 0,
            two_d: SignedField {
                depth: &missing,
                wse: &missing,
            },
            storage_owners: &owners,
            storage_levels: &[f32::MAX],
            xs_claims: if case == 3 { &[0, 2] } else { &[0; 2] },
            xs: SignedField {
                depth: &missing,
                wse: &missing,
            },
            terrain: &[0., -f32::MAX],
            max_working_bytes: if case == 2 { 0 } else { 1024 },
        };
        assert!(surface::compose_into(input, &mut d, &mut w, &mut coverage).is_err());
        assert_eq!(d, [7.; 2]);
        assert_eq!(w, [8.; 2]);
        assert_eq!(coverage, [Coverage::Xs; 2]);
    }
    assert!(matches!(
        surface::planned_bytes(usize::MAX, 1),
        Err(MapError::Budget(_))
    ));
    let mut owners = [1, 0];
    assert!(surface::claim_first_storage(&mut owners, 3, 2, &[true; 2]).is_err());
    assert_eq!(owners, [1, 0]);
}

#[test]
fn empty_and_mismatched_fields_have_explicit_contracts() {
    let empty = || Composition {
        two_d_owners: &[],
        two_d_count: 0,
        two_d: SignedField {
            depth: &[],
            wse: &[],
        },
        storage_owners: &[],
        storage_levels: &[],
        xs_claims: &[],
        xs: SignedField {
            depth: &[],
            wse: &[],
        },
        terrain: &[],
        max_working_bytes: 0,
    };
    assert_eq!(
        surface::compose_into(empty(), &mut [], &mut [], &mut []).unwrap(),
        Default::default()
    );
    let mut d = [7.];
    assert!(surface::compose_into(empty(), &mut d, &mut [], &mut []).is_err());
    assert_eq!(d, [7.]);
}
