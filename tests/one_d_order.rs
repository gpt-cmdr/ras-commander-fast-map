use ras_commander_fast_map::{MapError, one_d_order::render_ranks};

fn names(values: &[&str]) -> Vec<String> {
    values.iter().map(|value| (*value).to_owned()).collect()
}

#[test]
fn ordinary_surfaces_keep_stored_order() {
    assert_eq!(
        render_ranks(&[[0, 1], [1, 2]], &names(&["River", "River", "River"])).unwrap(),
        [0, 1]
    );
    assert!(render_ranks(&[], &[]).unwrap().is_empty());
}

#[test]
fn confluence_promotes_main_stem_even_when_stored_last() {
    // Ordinary surface 0 precedes tributary 1 and main-stem continuation 2.
    let ids = [[3, 4], [0, 2], [1, 2]];
    let rivers = names(&["Tributary", "Main", "Main", "Other", "Other"]);
    assert_eq!(render_ranks(&ids, &rivers).unwrap(), [1, 2, 0]);
}

#[test]
fn split_promotes_first_same_river_downstream_surface() {
    let ids = [[2, 3], [0, 1], [0, 2]];
    let rivers = names(&["Main", "Side", "Main", "Main"]);
    assert_eq!(render_ranks(&ids, &rivers).unwrap(), [1, 2, 0]);
}

#[test]
fn junction_with_both_confluence_and_split_promotes_nothing() {
    let ids = [[0, 2], [1, 2], [2, 3], [2, 4]];
    let rivers = names(&["Tributary", "Main", "Main", "Side", "Main"]);
    assert_eq!(render_ranks(&ids, &rivers).unwrap(), [0, 1, 2, 3]);
}

#[test]
fn renamed_river_does_not_promote_and_comparison_is_exact() {
    let ids = [[0, 2], [1, 2]];
    for rivers in [
        names(&["Tributary", "Main", "Renamed"]),
        names(&["Tributary", "main", "Main"]),
        names(&["Tributary", "Main ", "Main"]),
    ] {
        assert_eq!(render_ranks(&ids, &rivers).unwrap(), [0, 1]);
    }
}

#[test]
fn promotion_uses_ascending_feature_id_not_surface_order() {
    // Confluence at XS 5 promotes surface 1; lower XS 2 promotes surface 3.
    let ids = [[0, 5], [4, 5], [1, 2], [3, 2]];
    let rivers = names(&["T0", "T1", "MainB", "MainB", "MainA", "MainA"]);
    assert_eq!(render_ranks(&ids, &rivers).unwrap(), [2, 1, 3, 0]);
}

#[test]
fn first_matching_surface_wins_and_duplicate_promotions_are_removed() {
    // Surface 1 is selected at split 0 and confluence 3, but ranks only once.
    let ids = [[1, 3], [0, 3], [0, 2], [4, 3]];
    let rivers = names(&["Main", "Side", "Branch", "Main", "Main"]);
    assert_eq!(render_ranks(&ids, &rivers).unwrap(), [1, 0, 2, 3]);
    let mut permutation = render_ranks(&ids, &rivers).unwrap();
    permutation.sort_unstable();
    assert_eq!(permutation, [0, 1, 2, 3]);
}

#[test]
fn first_matching_name_in_stored_order_is_promoted() {
    let ids = [[3, 4], [0, 2], [1, 2]];
    let rivers = names(&["Main", "Main", "Main", "Other", "Other"]);
    assert_eq!(render_ranks(&ids, &rivers).unwrap(), [1, 0, 2]);
}

#[test]
fn invalid_endpoint_is_rejected() {
    assert!(matches!(
        render_ranks(&[[0, 2]], &names(&["A", "A"])),
        Err(MapError::Invalid(_))
    ));
    assert!(matches!(
        render_ranks(&[[usize::MAX, 0]], &names(&["A"])),
        Err(MapError::Invalid(_))
    ));
}
