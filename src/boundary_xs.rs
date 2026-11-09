//! Receipt-adapted stored XS payload loading for signed boundary sampling.
//! HDF preparation and source identity binding remain caller responsibilities.
use crate::{MapError, model, one_d::Segment, one_d_clip, one_d_native, one_d_session};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct Profile {
    pub id: String,
    pub levels_file: PathBuf,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct XsInput {
    pub point_count: usize,
    pub triangle_count: usize,
    pub segment_count: usize,
    pub cross_section_count: usize,
    pub points_file: PathBuf,
    pub triangles_file: PathBuf,
    pub tin_info_file: PathBuf,
    pub xs_ids_file: PathBuf,
    pub render_order_file: PathBuf,
    pub normalized_m_file: PathBuf,
    pub xs_clip_file: PathBuf,
    pub clipping_scope: String,
    pub profiles: Vec<Profile>,
}

fn add(total: usize, count: usize, bytes: usize) -> Result<usize, MapError> {
    count
        .checked_mul(bytes)
        .and_then(|n| total.checked_add(n))
        .ok_or_else(|| MapError::Budget("boundary XS payload allocation overflow".into()))
}

/// Conservative preparation and retained-profile admission before payload reads.
pub fn planned_bytes(input: &XsInput) -> Result<usize, MapError> {
    let mut total = one_d_native::PreparedSurface::planned_clipped_bytes(
        input.point_count,
        input.triangle_count,
        input.segment_count,
        input.cross_section_count,
        257,
    )?;
    for (count, bytes) in [
        (input.point_count, 40),
        (input.triangle_count, 12),
        (input.segment_count, 76),
        (input.cross_section_count, 64),
    ] {
        total = add(total, count, bytes)?;
    }
    total = add(
        total,
        input.cross_section_count,
        input
            .profiles
            .len()
            .checked_mul(4)
            .ok_or_else(|| MapError::Budget("boundary XS profiles overflow".into()))?,
    )?;
    total = add(total, input.profiles.len(), 128)?;
    for profile in &input.profiles {
        total = add(total, profile.id.len(), 2)?;
    }
    add(total, 1, 8192)
}

fn file_bytes(path: &Path, count: usize, stride: usize) -> Result<(), MapError> {
    let expected = count
        .checked_mul(stride)
        .ok_or_else(|| MapError::Budget("boundary XS file size overflow".into()))?;
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() != expected as u64
    {
        return Err(MapError::Invalid(format!(
            "invalid boundary XS payload length/type: {}",
            path.display()
        )));
    }
    Ok(())
}

pub struct PreparedXs {
    surface: one_d_native::PreparedSurface,
    profiles: Vec<(String, Vec<f32>)>,
}

impl PreparedXs {
    /// Check every descriptor and profile file length before reading numbers.
    /// This loads no terrain and makes no complete boundary/parity claim.
    pub fn load(input: &XsInput, base: &Path, max_bytes: usize) -> Result<Self, MapError> {
        if input.clipping_scope != one_d_session::CLIPPING_SCOPE
            || input.point_count < 3
            || input.triangle_count == 0
            || input.segment_count == 0
            || input.cross_section_count < 2
            || input.profiles.is_empty()
        {
            return Err(MapError::Invalid(
                "invalid boundary XS counts/clipping scope/profiles".into(),
            ));
        }
        let planned = planned_bytes(input)?;
        if planned > max_bytes {
            return Err(MapError::Budget(format!(
                "boundary XS planned {planned} bytes exceeds {max_bytes}"
            )));
        }
        let mut names = HashSet::new();
        names
            .try_reserve(input.profiles.len())
            .map_err(|e| MapError::Budget(e.to_string()))?;
        for profile in &input.profiles {
            model::validate_id(&profile.id)?;
            if !names.insert(model::id_key(&profile.id)) {
                return Err(MapError::Invalid(
                    "duplicate boundary XS profile identity".into(),
                ));
            }
        }
        for (path, count, stride) in [
            (&input.points_file, input.point_count, 32),
            (&input.triangles_file, input.triangle_count, 12),
            (&input.tin_info_file, input.segment_count, 16),
            (&input.xs_ids_file, input.segment_count, 8),
            (&input.render_order_file, input.segment_count, 4),
            (&input.normalized_m_file, input.point_count, 8),
            (&input.xs_clip_file, input.cross_section_count, 32),
        ] {
            file_bytes(&base.join(path), count, stride)?;
        }
        for profile in &input.profiles {
            file_bytes(
                &base.join(&profile.levels_file),
                input.cross_section_count,
                4,
            )?;
        }
        let points = one_d_session::read_records(
            &base.join(&input.points_file),
            input.point_count,
            |b: [u8; 32]| -> [f64; 4] {
                std::array::from_fn(|i| f64::from_le_bytes(b.as_chunks::<8>().0[i]))
            },
        )?;
        let triangles = one_d_session::read_records(
            &base.join(&input.triangles_file),
            input.triangle_count,
            |b: [u8; 12]| -> [i32; 3] {
                std::array::from_fn(|i| i32::from_le_bytes(b.as_chunks::<4>().0[i]))
            },
        )?;
        let info = one_d_session::read_records(
            &base.join(&input.tin_info_file),
            input.segment_count,
            |b: [u8; 16]| -> [i32; 4] {
                std::array::from_fn(|i| i32::from_le_bytes(b.as_chunks::<4>().0[i]))
            },
        )?;
        let ids = one_d_session::read_records(
            &base.join(&input.xs_ids_file),
            input.segment_count,
            |b: [u8; 8]| -> [i32; 2] {
                std::array::from_fn(|i| i32::from_le_bytes(b.as_chunks::<4>().0[i]))
            },
        )?;
        let nonnegative = |value: i32| {
            usize::try_from(value)
                .map_err(|_| MapError::Invalid("negative boundary XS range/ID".into()))
        };
        let mut segments = Vec::new();
        segments
            .try_reserve_exact(input.segment_count)
            .map_err(|e| MapError::Budget(e.to_string()))?;
        for (row, xs) in info.iter().zip(&ids) {
            segments.push(Segment {
                point_start: nonnegative(row[0])?,
                point_count: nonnegative(row[1])?,
                triangle_start: nonnegative(row[2])?,
                triangle_count: nonnegative(row[3])?,
                upstream: nonnegative(xs[0])?,
                downstream: nonnegative(xs[1])?,
            });
        }
        let ranks = one_d_session::read_records(
            &base.join(&input.render_order_file),
            input.segment_count,
            u32::from_le_bytes,
        )?;
        let normalized = one_d_session::read_records(
            &base.join(&input.normalized_m_file),
            input.point_count,
            f64::from_le_bytes,
        )?;
        let rows = one_d_session::read_records(
            &base.join(&input.xs_clip_file),
            input.cross_section_count,
            |b: [u8; 32]| -> [f32; 8] {
                std::array::from_fn(|i| f32::from_le_bytes(b.as_chunks::<4>().0[i]))
            },
        )?;
        let mut clips = Vec::new();
        clips
            .try_reserve_exact(input.cross_section_count)
            .map_err(|e| MapError::Budget(e.to_string()))?;
        for row in rows {
            clips.push(one_d_clip::decode(row)?);
        }
        let retained_levels = input
            .cross_section_count
            .checked_mul(4)
            .and_then(|n| n.checked_mul(input.profiles.len()))
            .ok_or_else(|| MapError::Budget("boundary XS retained profiles overflow".into()))?;
        let retained_profiles = input
            .profiles
            .iter()
            .try_fold(retained_levels, |n, p| add(n, 1, p.id.len() + 128))?;
        let surface_limit = max_bytes.checked_sub(retained_profiles).ok_or_else(|| {
            MapError::Budget("boundary XS retained profiles exceed budget".into())
        })?;
        let surface = one_d_native::PreparedSurface::new_with_clipping(
            &points,
            &triangles,
            &segments,
            input.cross_section_count,
            &ranks,
            &normalized,
            &clips,
            surface_limit,
        )?;
        let mut profiles = Vec::new();
        profiles
            .try_reserve_exact(input.profiles.len())
            .map_err(|e| MapError::Budget(e.to_string()))?;
        for profile in &input.profiles {
            let levels = one_d_session::read_records(
                &base.join(&profile.levels_file),
                input.cross_section_count,
                f32::from_le_bytes,
            )?;
            if levels.iter().any(|v| !v.is_finite()) {
                return Err(MapError::Invalid(
                    "boundary XS profile WSE must be finite".into(),
                ));
            }
            profiles.push((profile.id.clone(), levels));
        }
        Ok(Self { surface, profiles })
    }

    pub fn surface(&self) -> &one_d_native::PreparedSurface {
        &self.surface
    }

    pub fn profile_ids(&self) -> impl Iterator<Item = &str> {
        self.profiles.iter().map(|(id, _)| id.as_str())
    }

    /// Borrow the already admitted/validated levels for the exact profile ID.
    pub fn levels(&self, profile_id: &str) -> Result<&[f32], MapError> {
        self.profiles
            .iter()
            .find(|(id, _)| id == profile_id)
            .map(|(_, levels)| levels.as_slice())
            .ok_or_else(|| MapError::Invalid("unknown boundary XS profile ID".into()))
    }
}
