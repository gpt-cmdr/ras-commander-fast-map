//! Coverage-first composition of caller-prepared signed boundary samples.
//!
//! Geometry, profile alignment and native terrain sampling belong to callers.
//! These primitives do not qualify vector boundaries or change raster methods.
use crate::MapError;

/// A signed depth and WSE pair. Both NaNs mean missing, never a numeric sentinel.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SignedSample {
    pub depth: f32,
    pub wse: f32,
}

impl SignedSample {
    pub fn missing() -> Self {
        Self {
            depth: f32::NAN,
            wse: f32::NAN,
        }
    }

    fn validate(self) -> Result<(), MapError> {
        if (self.depth.is_nan() && self.wse.is_nan())
            || (self.depth.is_finite() && self.wse.is_finite())
        {
            Ok(())
        } else {
            Err(MapError::Invalid(
                "boundary signed pair must be finite or paired NaNs".into(),
            ))
        }
    }
}

/// The selected physical coverage, retained even for missing/clipped samples.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Coverage {
    None,
    TwoD(u32),
    Storage(u32),
    Xs,
}

#[derive(Clone, Copy, Debug)]
pub struct SampleInputs {
    /// Zero is unowned; positive labels refer to caller-preserved physical cells.
    pub two_d_owner: u32,
    pub two_d: SignedSample,
    /// The caller selects the first stored storage polygon at this location.
    pub storage_owner: u32,
    pub storage_level: f32,
    pub xs_claim: bool,
    pub xs: SignedSample,
    pub terrain: f32,
}

#[derive(Clone, Copy, Debug)]
pub struct ComposedSample {
    pub coverage: Coverage,
    pub values: SignedSample,
}

/// Compose a single location: physical 2D, then storage, then XS. Coverage is
/// chosen before missing terrain/profile, clipping or signed depth visibility.
/// Finite component values are copied bit for bit; storage subtracts in f32.
pub fn compose_sample(input: SampleInputs) -> Result<ComposedSample, MapError> {
    input.two_d.validate()?;
    input.xs.validate()?;
    if input.two_d_owner == u32::MAX
        || input.storage_owner == u32::MAX
        || input.storage_level.is_infinite()
        || input.terrain.is_infinite()
        || (input.two_d_owner == 0 && input.two_d.depth.is_finite())
        || (!input.xs_claim && input.xs.depth.is_finite())
    {
        return Err(MapError::Invalid(
            "inconsistent boundary sample ownership/values".into(),
        ));
    }
    let coverage = if input.two_d_owner != 0 {
        Coverage::TwoD(input.two_d_owner)
    } else if input.storage_owner != 0 {
        Coverage::Storage(input.storage_owner)
    } else if input.xs_claim {
        Coverage::Xs
    } else {
        Coverage::None
    };
    let values = if input.terrain.is_nan() {
        SignedSample::missing()
    } else {
        match coverage {
            Coverage::TwoD(_) => input.two_d,
            Coverage::Storage(_)
                if input.storage_level.is_finite() && input.storage_level != -9999.0_f32 =>
            {
                let depth = input.storage_level - input.terrain;
                if !depth.is_finite() {
                    return Err(MapError::Invalid(
                        "boundary storage signed depth overflow".into(),
                    ));
                }
                SignedSample {
                    depth,
                    wse: input.storage_level,
                }
            }
            Coverage::Storage(_) | Coverage::None => SignedSample::missing(),
            Coverage::Xs => input.xs,
        }
    };
    // Only the selected final WSE carries this native missing sentinel; valid
    // negative signed depths, including -9999, remain untouched.
    let values = if values.wse == -9999.0_f32 {
        SignedSample::missing()
    } else {
        values
    };
    Ok(ComposedSample { coverage, values })
}

#[derive(Clone, Copy)]
pub struct SignedField<'a> {
    pub depth: &'a [f32],
    pub wse: &'a [f32],
}

/// All inputs address the same original terrain sample locations and profile.
/// Storage labels use the first stored area, not maximum WSE or wet-only order.
pub struct Composition<'a> {
    pub two_d_owners: &'a [u32],
    pub two_d_count: usize,
    pub two_d: SignedField<'a>,
    pub storage_owners: &'a [u32],
    pub storage_levels: &'a [f32],
    pub xs_claims: &'a [u8],
    pub xs: SignedField<'a>,
    pub terrain: &'a [f32],
    pub max_working_bytes: usize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Counts {
    pub two_d_owned: usize,
    pub storage_owned: usize,
    pub xs_owned: usize,
    pub unowned: usize,
    pub finite_samples: usize,
}

/// Conservative admission for all caller-owned inputs and outputs. Composition
/// itself allocates no arrays; this is a planned byte limit, not process RSS.
pub fn planned_bytes(samples: usize, storage_count: usize) -> Result<usize, MapError> {
    let per_sample = 2 * std::mem::size_of::<u32>()
        + 7 * std::mem::size_of::<f32>()
        + std::mem::size_of::<u8>()
        + std::mem::size_of::<Coverage>();
    samples
        .checked_mul(per_sample)
        .and_then(|n| {
            storage_count
                .checked_mul(std::mem::size_of::<f32>())
                .and_then(|m| n.checked_add(m))
        })
        .ok_or_else(|| MapError::Budget("boundary composition allocation overflow".into()))
}

impl Composition<'_> {
    fn sample(&self, index: usize) -> SampleInputs {
        let owner = self.storage_owners[index];
        SampleInputs {
            two_d_owner: self.two_d_owners[index],
            two_d: SignedSample {
                depth: self.two_d.depth[index],
                wse: self.two_d.wse[index],
            },
            storage_owner: owner,
            storage_level: if owner == 0 {
                f32::NAN
            } else {
                self.storage_levels[owner as usize - 1]
            },
            xs_claim: self.xs_claims[index] != 0,
            xs: SignedSample {
                depth: self.xs.depth[index],
                wse: self.xs.wse[index],
            },
            terrain: self.terrain[index],
        }
    }
}

/// Compose aligned signed fields without allocation. Validate every input and
/// selected arithmetic result before changing any output, including on overflow.
pub fn compose_into(
    input: Composition<'_>,
    depth: &mut [f32],
    wse: &mut [f32],
    coverage: &mut [Coverage],
) -> Result<Counts, MapError> {
    let n = input.terrain.len();
    if input.two_d_count >= u32::MAX as usize
        || input.storage_levels.len() >= u32::MAX as usize
        || [
            input.two_d_owners.len(),
            input.two_d.depth.len(),
            input.two_d.wse.len(),
            input.storage_owners.len(),
            input.xs_claims.len(),
            input.xs.depth.len(),
            input.xs.wse.len(),
            depth.len(),
            wse.len(),
            coverage.len(),
        ]
        .iter()
        .any(|&len| len != n)
    {
        return Err(MapError::Invalid(
            "boundary composition counts/lengths differ".into(),
        ));
    }
    if planned_bytes(n, input.storage_levels.len())? > input.max_working_bytes {
        return Err(MapError::Budget(
            "boundary composition exceeds planned byte limit".into(),
        ));
    }
    if input.storage_levels.iter().any(|v| v.is_infinite())
        || input.xs_claims.iter().any(|&v| v > 1)
        || input
            .two_d_owners
            .iter()
            .any(|&v| v as usize > input.two_d_count)
        || input
            .storage_owners
            .iter()
            .any(|&v| v as usize > input.storage_levels.len())
    {
        return Err(MapError::Invalid(
            "boundary composition owner/value is out of range".into(),
        ));
    }
    let mut counts = Counts::default();
    for index in 0..n {
        let chosen = compose_sample(input.sample(index))?;
        match chosen.coverage {
            Coverage::TwoD(_) => counts.two_d_owned += 1,
            Coverage::Storage(_) => counts.storage_owned += 1,
            Coverage::Xs => counts.xs_owned += 1,
            Coverage::None => counts.unowned += 1,
        }
        counts.finite_samples += usize::from(chosen.values.depth.is_finite());
    }
    // All reads are immutable; this second pass cannot introduce a new failure.
    for index in 0..n {
        let chosen = compose_sample(input.sample(index))?;
        depth[index] = chosen.values.depth;
        wse[index] = chosen.values.wse;
        coverage[index] = chosen.coverage;
    }
    Ok(counts)
}

/// Add one polygon's coverage in stored order. Existing first claims survive;
/// level/terrain validity never participates in this geometry-only operation.
pub fn claim_first_storage(
    owners: &mut [u32],
    area: u32,
    area_count: usize,
    covered: &[bool],
) -> Result<usize, MapError> {
    if owners.len() != covered.len()
        || area == 0
        || area_count >= u32::MAX as usize
        || area as usize > area_count
        || owners.iter().any(|&v| v as usize > area_count)
    {
        return Err(MapError::Invalid("invalid boundary storage claims".into()));
    }
    let mut count = 0;
    for (owner, &inside) in owners.iter_mut().zip(covered) {
        if *owner == 0 && inside {
            *owner = area;
            count += 1;
        }
    }
    Ok(count)
}
