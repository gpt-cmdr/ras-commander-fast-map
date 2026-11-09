use ras_commander_fast_map::{
    MapError,
    boundary_contour::{Grid, Limits},
    boundary_native_contour as native,
};
fn limits() -> Limits {
    Limits {
        max_working_bytes: 16 * 1024 * 1024,
    }
}
fn grid() -> Grid {
    Grid {
        cols: 2,
        rows: 2,
        transform: [0., 1., 0., 2., 0., -1.],
    }
}
#[test]
fn all_wet_uses_native_method_and_exact_sample_rectangle() {
    let unit = native::grid(0, grid(), &[Some(1.); 4], limits()).unwrap();
    let output = native::finish(&[unit], None, limits()).unwrap();
    assert_eq!(output.method_id, native::METHOD_ID);
    assert_eq!(
        output.coordinates,
        vec![vec![vec![
            [0.5, 0.5],
            [1.5, 0.5],
            [1.5, 1.5],
            [0.5, 1.5],
            [0.5, 0.5]
        ]]]
    );
}

#[test]
fn dry_stitch_addresses_are_budgeted_as_addresses_before_allocation() {
    let unit = native::tin(
        17,
        &[[0., 0.], [1., 0.], [0., 1.]],
        &[None; 3],
        &vec![[0, 1, 2]; 4096],
        Limits {
            max_working_bytes: 256 * 1024,
        },
    )
    .unwrap();
    assert_eq!(unit.sequence, 17);
    assert_eq!(unit.ring_count(), 0);
    assert!(matches!(
        native::tin(
            17,
            &[[0., 0.], [1., 0.], [0., 1.]],
            &[None; 3],
            &vec![[0, 1, 2]; 4096],
            Limits {
                max_working_bytes: 70 * 1024
            }
        ),
        Err(MapError::Budget(_))
    ));
}
#[test]
fn native_minus_9999_is_missing_and_keeps_three_sample_triangle() {
    let unit = native::grid(
        0,
        grid(),
        &[Some(-9999.), Some(1.), Some(1.), Some(1.)],
        limits(),
    )
    .unwrap();
    let output = native::finish(&[unit], None, limits()).unwrap();
    assert_eq!(
        output.coordinates,
        vec![vec![vec![[0.5, 0.5], [1.5, 0.5], [1.5, 1.5], [0.5, 0.5]]]]
    );
}
#[test]
fn float_max_upper_bound_and_uniform_negative_are_empty_without_overflow() {
    for value in [f32::MAX, f32::MIN, -1.] {
        let unit = native::grid(0, grid(), &[Some(value); 4], limits()).unwrap();
        assert!(
            native::finish(&[unit], None, limits())
                .unwrap()
                .coordinates
                .is_empty()
        );
    }
}
#[test]
fn empty_and_duplicate_units_are_explicit() {
    assert!(
        native::finish(&[], None, limits())
            .unwrap()
            .coordinates
            .is_empty()
    );
    let a = native::grid(1, grid(), &[Some(1.); 4], limits()).unwrap();
    let b = native::grid(1, grid(), &[Some(1.); 4], limits()).unwrap();
    assert!(native::finish(&[a, b], None, limits()).is_err());
}
#[test]
fn planned_budget_refuses_before_native_clip_allocations() {
    assert!(matches!(
        native::grid(
            0,
            grid(),
            &[Some(1.); 4],
            Limits {
                max_working_bytes: 1
            }
        ),
        Err(MapError::Budget(_))
    ));
}
#[test]
fn final_aoi_intersection_keeps_its_hole() {
    let unit = native::grid(0, grid(), &[Some(1.); 4], limits()).unwrap();
    let aoi = vec![vec![
        vec![[0.5, 0.5], [1.5, 0.5], [1.5, 1.5], [0.5, 1.5], [0.5, 0.5]],
        vec![
            [0.75, 0.75],
            [0.75, 1.25],
            [1.25, 1.25],
            [1.25, 0.75],
            [0.75, 0.75],
        ],
    ]];
    let result = native::finish(&[unit], Some(&aoi), limits()).unwrap();
    assert_eq!(result.coordinates.len(), 1);
    assert_eq!(result.coordinates[0].len(), 2);
}
#[test]
fn final_aoi_accepts_empty_subject_and_disjoint_intersection() {
    let aoi = vec![vec![vec![
        [10., 10.],
        [11., 10.],
        [11., 11.],
        [10., 11.],
        [10., 10.],
    ]]];
    assert!(
        native::finish(&[], Some(&aoi), limits())
            .unwrap()
            .coordinates
            .is_empty()
    );
    let unit = native::grid(0, grid(), &[Some(1.); 4], limits()).unwrap();
    assert!(
        native::finish(&[unit], Some(&aoi), limits())
            .unwrap()
            .coordinates
            .is_empty()
    );
}
#[test]
fn native_tin_winding_is_retained_and_indices_checked() {
    let xy = [[0., 0.], [1., 0.], [0., 1.]];
    let values = [Some(1.); 3];
    let unit = native::tin(0, &xy, &values, &[[0, 1, 2]], limits()).unwrap();
    assert_eq!(
        native::finish(&[unit], None, limits()).unwrap().coordinates[0][0],
        vec![[0., 0.], [1., 0.], [0., 1.], [0., 0.]]
    );
    assert!(native::tin(0, &xy, &values, &[[0, 1, 3]], limits()).is_err());
}

#[test]
fn native_large_tin_uses_all_overlapping_bins_without_an_artificial_size_cap() {
    let xy = [[0., 0.], [10., 0.], [0., 1.], [0., 0.], [10., 0.], [0., 1.]];
    let mut triangles = vec![[3, 4, 5]; 6144];
    triangles[0] = [0, 1, 2];
    let unit = native::tin(
        0,
        &xy,
        &[Some(1.), Some(1.), Some(1.), None, None, None],
        &triangles,
        limits(),
    )
    .unwrap();
    // The native spatial partition replicates this full-extent triangle into
    // both bins. Its two ordered per-bin rings are retained until final merge.
    assert_eq!(unit.ring_count(), 2);
    let output = native::finish(&[unit], None, limits()).unwrap();
    assert_eq!(
        output.coordinates,
        vec![vec![vec![[0., 0.], [10., 0.], [0., 1.], [0., 0.]]]]
    );
}
