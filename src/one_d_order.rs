//! Caller-side preparation of native cross-section surface render ranks.
//!
//! The rasterizer consumes only numeric ranks. This optional pure staging helper
//! accepts cross-section river names already decoded by the model preparation
//! layer; it does not read model files or infer hydraulic connectivity.
use crate::MapError;

#[derive(Clone, Default)]
struct Junction {
    incoming: usize,
    outgoing: usize,
    first_incoming_same_river: Option<usize>,
    first_outgoing_same_river: Option<usize>,
}

/// Return one zero-based render rank for each surface in stored order.
///
/// Surface endpoint IDs index `river_names`. Native raster ownership is first
/// claim in render order: visit cross-section IDs in ascending order, promote
/// the first stored same-river continuation at each confluence or flow split,
/// then append unpromoted surfaces in stored order. A cross section that is
/// both a confluence and a split promotes neither side. Duplicate promotions
/// keep their first occurrence.
///
/// River names are compared exactly and case-sensitively. Decode and remove
/// storage padding in the preparation layer; this function does not trim or
/// normalize names. Reach names play no part in the native promotion rule.
pub fn render_ranks(xs_ids: &[[usize; 2]], river_names: &[String]) -> Result<Vec<u32>, MapError> {
    let count = u32::try_from(xs_ids.len())
        .map_err(|_| MapError::Budget("1D surface count exceeds render-rank capacity".into()))?;
    // Validate IDs before allocating staging scratch.
    if xs_ids.iter().flatten().any(|&id| id >= river_names.len()) {
        return Err(MapError::Invalid(
            "1D surface endpoint outside cross-section river names".into(),
        ));
    }
    let mut junctions = Vec::new();
    junctions
        .try_reserve_exact(river_names.len())
        .map_err(|e| MapError::Budget(format!("1D render-order junction allocation: {e}")))?;
    junctions.resize(river_names.len(), Junction::default());
    for (surface, &[upstream, downstream]) in xs_ids.iter().enumerate() {
        junctions[upstream].outgoing += 1;
        junctions[downstream].incoming += 1;
        if river_names[upstream] == river_names[downstream] {
            junctions[upstream]
                .first_outgoing_same_river
                .get_or_insert(surface);
            junctions[downstream]
                .first_incoming_same_river
                .get_or_insert(surface);
        }
    }
    let mut ranks = Vec::new();
    ranks
        .try_reserve_exact(xs_ids.len())
        .map_err(|e| MapError::Budget(format!("1D render-rank allocation: {e}")))?;
    ranks.resize(xs_ids.len(), u32::MAX);
    let mut next = 0_u32;
    for junction in junctions {
        let promoted = if junction.incoming > 1 && junction.outgoing <= 1 {
            junction.first_incoming_same_river
        } else if junction.incoming <= 1 && junction.outgoing > 1 {
            junction.first_outgoing_same_river
        } else {
            None
        };
        if let Some(surface) = promoted
            && ranks[surface] == u32::MAX
        {
            ranks[surface] = next;
            next += 1;
        }
    }
    for rank in &mut ranks {
        if *rank == u32::MAX {
            *rank = next;
            next += 1;
        }
    }
    debug_assert_eq!(next, count);
    Ok(ranks)
}
