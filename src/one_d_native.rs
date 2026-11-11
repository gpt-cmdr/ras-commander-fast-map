//! Experimental native-layout XS scan conversion over caller-prepared global geometry.
//!
//! The caller supplies the native render ranks and original terrain tile layout.
//! This independent interoperability implementation follows the reviewed algorithm
//! description; it does not load HDF, GIS runtimes, or native vendor assemblies.
use crate::{MapError, one_d::Segment};
use rstar::{AABB, RTree, RTreeObject};

pub const METHOD_ID: &str = "one-d-xs-surface-diagnostic/4";
pub const CLIPPED_METHOD_ID: &str = "one-d-xs-surface-diagnostic/5";

pub fn clipped_method_record() -> serde_json::Value {
    let mut record = method_record();
    record["method_id"] = serde_json::json!(CLIPPED_METHOD_ID);
    record["scope"] = serde_json::json!(
        "prepared XS surfaces with explicit levee and XS-end-obstruction clipping; no blocked polygons or encroachments"
    );
    record["clipping"] = serde_json::json!(
        "caller-prepared normalized Float64 M per TIN point and eight Float32 barrier fields per XS; claim retained when clipped"
    );
    record
}

pub fn method_record() -> serde_json::Value {
    serde_json::json!({
        "method_id": METHOD_ID, "experimental": true, "native_parity_qualified": false,
        "topology": "caller-supplied stored XS TIN and native render-rank permutation",
        "arithmetic": "global-double plane scan conversion; per-native-terrain-tile row/column increments; unclamped Float32 weight; separate Float32 weighted sum, no fused multiply-add",
        "edge_rule": "inclusive scanline pixel-center spans; first render rank then first stored triangle claims before visibility",
        "terrain_layout": "original full square north-up raster and terrain tile size; AOI uses global tile history",
        "visibility": "finite WSE >= f32(terrain + 0.001); depth = f32(WSE - terrain)",
        "scope": "prepared XS surfaces only; storage composition and obstruction clipping are external",
        "profile_missing": "either missing endpoint retains ownership but yields nodata",
        "render_workers": {"control": "threads", "default": 1, "min": 1, "max": 64,
            "split": "whole source tile rows; per-tile claims, candidate order and scanline increments unchanged",
            "outputs": "bit-identical for every worker count", "report_timing": true},
        "native_geotiff_outputs": true,
        "native_final_format": crate::one_d_delivery::capability(),
        "inline_storage": crate::one_d_session::storage_capability(),
        "multi_profile": {"control": "levels_files", "min": 1, "max": crate::one_d_session::MAX_PROFILES,
            "layout": "profile-NNNN/{depth,wse}.bin and report.json per profile; top-level report lists profiles",
            "shared": "inputs, terrain and prepared surface are read and built once",
            "outputs": "bit-identical to one request per profile"}
    })
}

#[derive(Clone, Copy, Debug)]
pub struct Grid {
    pub width: usize,
    pub height: usize,
    pub geo_transform: [f64; 6],
    pub tile_size: usize,
}

#[derive(Clone, Copy, Debug)]
pub struct Window {
    pub col: usize,
    pub row: usize,
    pub width: usize,
    pub height: usize,
}

/// One complete native sample raster, independent of the ordinary tile lattice.
/// Boundary callers supply the extended 257-sample tile's actual bounds.
#[derive(Clone, Copy, Debug)]
pub struct SampleRaster {
    /// Projected [min_x, min_y, max_x, max_y], without coordinate conversion.
    pub bounds: [f64; 4],
    pub size: usize,
}

impl SampleRaster {
    fn tile(self) -> Result<Tile, MapError> {
        let [min_x, min_y, max_x, max_y] = self.bounds;
        if self.size == 0
            || self.size > i32::MAX as usize
            || self.bounds.iter().any(|v| !v.is_finite())
            || min_x >= max_x
            || min_y >= max_y
        {
            return Err(MapError::Invalid("invalid XS sample raster".into()));
        }
        let cell = ((max_x - min_x) / self.size as f64).max((max_y - min_y) / self.size as f64);
        if !cell.is_finite() || cell <= 0.0 {
            return Err(MapError::Invalid("XS sample raster cell overflow".into()));
        }
        Ok(Tile {
            min_x,
            min_y,
            max_x,
            max_y,
            cell,
            size: self.size,
        })
    }
}

#[derive(Clone)]
struct Triangle {
    bounds: AABB<[f64; 2]>,
    vertices: [[f64; 3]; 3],
    plane: [f64; 4],
    rank: u32,
    id: usize,
    upstream: usize,
    downstream: usize,
}

impl RTreeObject for Triangle {
    type Envelope = AABB<[f64; 2]>;
    fn envelope(&self) -> Self::Envelope {
        self.bounds
    }
}

pub struct PreparedSurface {
    tree: RTree<Triangle>,
    cross_sections: usize,
    geometry_bytes: usize,
    max_bytes: usize,
    clipping: Option<Clipping>,
}

struct Clipping {
    clips: Vec<crate::one_d_clip::Clip>,
    planes: Vec<[f64; 4]>,
    point_m: Vec<[f64; 3]>,
}

fn clipping_bytes(
    points: usize,
    triangles: usize,
    cross_sections: usize,
) -> Result<usize, MapError> {
    points
        .checked_mul(8)
        .and_then(|n| triangles.checked_mul(56).and_then(|m| n.checked_add(m)))
        .and_then(|n| {
            cross_sections
                .checked_mul(32 + 2 * std::mem::size_of::<crate::one_d_clip::Clip>())
                .and_then(|m| n.checked_add(m))
        })
        .ok_or_else(|| MapError::Budget("1D clipping allocation overflow".into()))
}

fn reserve<T>(v: &mut Vec<T>, count: usize) -> Result<(), MapError> {
    v.try_reserve_exact(count)
        .map_err(|e| MapError::Budget(e.to_string()))
}

fn geometry_bytes(points: usize, triangles: usize, segments: usize) -> Result<usize, MapError> {
    // Includes staged input arrays, bulk R-tree construction, all tile-candidate
    // references, rank validation and stored range metadata. Not a hard RSS cap.
    points
        .checked_mul(32)
        .and_then(|n| triangles.checked_mul(1024).and_then(|m| n.checked_add(m)))
        .and_then(|n| segments.checked_mul(64).and_then(|m| n.checked_add(m)))
        .ok_or_else(|| MapError::Budget("native XS geometry allocation overflow".into()))
}

impl PreparedSurface {
    pub fn planned_bytes(
        points: usize,
        triangles: usize,
        segments: usize,
        tile_size: usize,
    ) -> Result<usize, MapError> {
        if tile_size == 0 || tile_size > i32::MAX as usize {
            return Err(MapError::Invalid(
                "native XS tile size must be positive and fit Int32".into(),
            ));
        }
        geometry_bytes(points, triangles, segments)?
            .checked_add(
                tile_size
                    .checked_mul(tile_size)
                    .ok_or_else(|| MapError::Budget("native XS tile allocation overflow".into()))?,
            )
            .ok_or_else(|| MapError::Budget("native XS allocation overflow".into()))
    }

    pub fn planned_clipped_bytes(
        points: usize,
        triangles: usize,
        segments: usize,
        cross_sections: usize,
        tile_size: usize,
    ) -> Result<usize, MapError> {
        Self::planned_bytes(points, triangles, segments, tile_size)?
            .checked_add(clipping_bytes(points, triangles, cross_sections)?)
            .ok_or_else(|| MapError::Budget("1D clipped surface allocation overflow".into()))
    }

    /// Explicit normalized-M and barrier extension; old constructors retain /4.
    #[allow(clippy::too_many_arguments)]
    pub fn new_with_clipping(
        points: &[[f64; 4]],
        triangles: &[[i32; 3]],
        segments: &[Segment],
        cross_sections: usize,
        render_ranks: &[u32],
        normalized_m: &[f64],
        clips: &[crate::one_d_clip::Clip],
        max_bytes: usize,
    ) -> Result<Self, MapError> {
        let admitted = geometry_bytes(points.len(), triangles.len(), segments.len())?
            .checked_add(clipping_bytes(
                points.len(),
                triangles.len(),
                cross_sections,
            )?)
            .ok_or_else(|| MapError::Budget("1D clipped surface allocation overflow".into()))?;
        if admitted > max_bytes {
            return Err(MapError::Budget(
                "1D clipped surface exceeds admitted bytes".into(),
            ));
        }
        if normalized_m.len() != points.len()
            || normalized_m.iter().any(|m| !m.is_finite())
            || clips.len() != cross_sections
        {
            return Err(MapError::Invalid("1D clipping requires one finite normalized M per point and one barrier record per XS".into()));
        }
        for clip in clips {
            crate::one_d_clip::validate(*clip)?;
        }
        let mut surface = Self::new(
            points,
            triangles,
            segments,
            cross_sections,
            render_ranks,
            max_bytes,
        )?;
        let mut planes = Vec::new();
        reserve(&mut planes, triangles.len())?;
        let mut point_m = Vec::new();
        reserve(&mut point_m, triangles.len())?;
        for segment in segments {
            for indices in
                &triangles[segment.triangle_start..segment.triangle_start + segment.triangle_count]
            {
                let p = indices.map(|i| {
                    let index = segment.point_start + i as usize;
                    [points[index][0], points[index][1], normalized_m[index]]
                });
                point_m.push(p.map(|v| v[2]));
                let u: [f64; 3] = std::array::from_fn(|i| p[1][i] - p[0][i]);
                let v: [f64; 3] = std::array::from_fn(|i| p[2][i] - p[0][i]);
                let a = u[1] * v[2] - u[2] * v[1];
                let b = u[2] * v[0] - u[0] * v[2];
                let c = u[0] * v[1] - u[1] * v[0];
                let d = ((0.0 - a) * p[0][0] - b * p[0][1]) - c * p[0][2];
                if ![a, b, c, d].iter().all(|v| v.is_finite()) || c == 0.0 {
                    return Err(MapError::Invalid("1D normalized M plane overflow".into()));
                }
                planes.push([a, b, c, d]);
            }
        }
        let mut copied_clips = Vec::new();
        reserve(&mut copied_clips, clips.len())?;
        copied_clips.extend_from_slice(clips);
        surface.clipping = Some(Clipping {
            clips: copied_clips,
            planes,
            point_m,
        });
        surface.geometry_bytes = admitted;
        Ok(surface)
    }

    pub fn new(
        points: &[[f64; 4]],
        triangles: &[[i32; 3]],
        segments: &[Segment],
        cross_sections: usize,
        render_ranks: &[u32],
        max_bytes: usize,
    ) -> Result<Self, MapError> {
        let admitted_geometry = geometry_bytes(points.len(), triangles.len(), segments.len())?;
        if admitted_geometry > max_bytes {
            return Err(MapError::Budget(
                "native XS geometry exceeds admitted bytes".into(),
            ));
        }
        if cross_sections < 2 || segments.is_empty() || render_ranks.len() != segments.len() {
            return Err(MapError::Invalid(
                "native XS requires cross sections, segments and one render rank per segment"
                    .into(),
            ));
        }
        let mut seen = Vec::new();
        reserve(&mut seen, segments.len())?;
        seen.resize(segments.len(), false);
        for rank in render_ranks {
            let i = *rank as usize;
            if i >= seen.len() || seen[i] {
                return Err(MapError::Invalid(
                    "native XS ranks must be a permutation".into(),
                ));
            }
            seen[i] = true;
        }
        if points
            .iter()
            .any(|p| !p.iter().all(|v| v.is_finite()) || !(0.0..=1.0).contains(&p[2]))
        {
            return Err(MapError::Invalid(
                "native XS points must be finite, with stored weights in [0,1]".into(),
            ));
        }
        let mut prepared = Vec::new();
        reserve(&mut prepared, triangles.len())?;
        let (mut point_end, mut triangle_end) = (0usize, 0usize);
        for (segment_index, segment) in segments.iter().enumerate() {
            if segment.point_start != point_end
                || segment.triangle_start != triangle_end
                || segment.point_count < 3
                || segment.triangle_count == 0
                || segment.upstream >= cross_sections
                || segment.downstream >= cross_sections
                || segment.upstream == segment.downstream
            {
                return Err(MapError::Invalid(
                    "native XS segment range or endpoint invalid".into(),
                ));
            }
            point_end = segment
                .point_start
                .checked_add(segment.point_count)
                .filter(|n| *n <= points.len())
                .ok_or_else(|| MapError::Invalid("native XS point range out of bounds".into()))?;
            triangle_end = segment
                .triangle_start
                .checked_add(segment.triangle_count)
                .filter(|n| *n <= triangles.len())
                .ok_or_else(|| {
                    MapError::Invalid("native XS triangle range out of bounds".into())
                })?;
            let local_points = &points[segment.point_start..point_end];
            for (offset, indices) in triangles[segment.triangle_start..triangle_end]
                .iter()
                .enumerate()
            {
                if indices
                    .iter()
                    .any(|i| *i < 0 || *i as usize >= local_points.len())
                {
                    return Err(MapError::Invalid(
                        "native XS triangle index outside segment".into(),
                    ));
                }
                let p = indices.map(|i| {
                    let q = local_points[i as usize];
                    [q[0], q[1], q[2]]
                });
                let u: [f64; 3] = std::array::from_fn(|i| p[1][i] - p[0][i]);
                let v: [f64; 3] = std::array::from_fn(|i| p[2][i] - p[0][i]);
                let a = u[1] * v[2] - u[2] * v[1];
                let b = u[2] * v[0] - u[0] * v[2];
                let c = u[0] * v[1] - u[1] * v[0];
                let d = ((0.0 - a) * p[0][0] - b * p[0][1]) - c * p[0][2];
                if ![a, b, c, d].iter().all(|n| n.is_finite()) || c == 0.0 {
                    return Err(MapError::Invalid(
                        "native XS degenerate or overflowing plane".into(),
                    ));
                }
                prepared.push(Triangle {
                    bounds: AABB::from_corners(
                        [
                            p[0][0].min(p[1][0]).min(p[2][0]),
                            p[0][1].min(p[1][1]).min(p[2][1]),
                        ],
                        [
                            p[0][0].max(p[1][0]).max(p[2][0]),
                            p[0][1].max(p[1][1]).max(p[2][1]),
                        ],
                    ),
                    vertices: p,
                    plane: [a, b, c, d],
                    rank: render_ranks[segment_index],
                    id: segment.triangle_start + offset,
                    upstream: segment.upstream,
                    downstream: segment.downstream,
                });
            }
        }
        if point_end != points.len() || triangle_end != triangles.len() {
            return Err(MapError::Invalid(
                "native XS segments must cover all input records".into(),
            ));
        }
        Ok(Self {
            tree: RTree::bulk_load(prepared),
            cross_sections,
            geometry_bytes: admitted_geometry,
            max_bytes,
            clipping: None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    fn validate_signed(
        &self,
        levels: &[f32],
        terrain: &[f32],
        depth: &[f32],
        wse: &[f32],
        claims: &[u8],
        samples: usize,
        bytes_per_sample: usize,
    ) -> Result<(), MapError> {
        if levels.len() != self.cross_sections
            || levels.iter().any(|v| v.is_infinite())
            || terrain.len() != samples
            || depth.len() != samples
            || wse.len() != samples
            || claims.len() != samples
            || claims.iter().any(|&v| v > 1)
            || terrain.iter().any(|v| v.is_infinite())
        {
            return Err(MapError::Invalid("invalid XS signed sample buffers".into()));
        }
        // All working buffers are caller-owned; reserve room for their complete
        // lengths as well as candidate references before any output changes.
        let bytes = samples
            .checked_mul(bytes_per_sample)
            .and_then(|n| {
                self.tree
                    .size()
                    .checked_mul(std::mem::size_of::<&Triangle>())
                    .and_then(|m| n.checked_add(m))
            })
            .and_then(|n| n.checked_add(self.geometry_bytes))
            .ok_or_else(|| MapError::Budget("XS signed sample allocation overflow".into()))?;
        if bytes > self.max_bytes {
            return Err(MapError::Budget(
                "XS signed samples exceed admitted bytes".into(),
            ));
        }
        Ok(())
    }

    fn signed_value(
        &self,
        triangle: &Triangle,
        levels: &[f32],
        ground: f32,
        weight: f32,
        m: f32,
    ) -> Result<Option<(f32, f32)>, MapError> {
        let us = levels[triangle.upstream];
        let ds = levels[triangle.downstream];
        if !ground.is_finite() || !us.is_finite() || !ds.is_finite() {
            return Ok(None);
        }
        let water = weight * us + (1.0_f32 - weight) * ds;
        if !weight.is_finite() || !m.is_finite() || !water.is_finite() {
            return Err(MapError::Invalid("XS signed surface overflow".into()));
        }
        // Native signed depth tests the final WSE sentinel, not its endpoints.
        // A computed depth of -9999 remains an ordinary numeric sample.
        if water == -9999.0_f32 {
            return Ok(None);
        }
        if let Some(data) = &self.clipping
            && !crate::one_d_clip::visible(
                data.clips[triangle.upstream],
                data.clips[triangle.downstream],
                us,
                ds,
                weight,
                m,
                water,
            )
        {
            return Ok(None);
        }
        let delta = water - ground;
        if !delta.is_finite() {
            return Err(MapError::Invalid("XS signed depth overflow".into()));
        }
        Ok(Some((delta, water)))
    }

    /// Conservative admission using the same triangle query as signed rendering.
    pub(crate) fn intersects_signed_raster(&self, raster: SampleRaster) -> Result<bool, MapError> {
        let tile = raster.tile()?;
        Ok(self
            .tree
            .locate_in_envelope_intersecting(tile.bounds())
            .next()
            .is_some())
    }

    /// Sample a complete supplied raster using native rank/scanline arithmetic.
    /// Initial claims (0 or 1) exclude higher-priority physical coverage. Claims
    /// are retained even for missing or clipped values; outputs are reset to NaN.
    /// Signed depths bypass the raster plot threshold. This is a sampling
    /// primitive, not a complete boundary mapping or native vector parity claim.
    #[allow(clippy::too_many_arguments)]
    pub fn render_signed_raster(
        &self,
        levels: &[f32],
        terrain: &[f32],
        raster: SampleRaster,
        depth: &mut [f32],
        wse: &mut [f32],
        claims: &mut [u8],
    ) -> Result<usize, MapError> {
        let tile = raster.tile()?;
        let samples = raster
            .size
            .checked_mul(raster.size)
            .ok_or_else(|| MapError::Budget("XS sample raster overflow".into()))?;
        self.validate_signed(levels, terrain, depth, wse, claims, samples, 13)?;
        let mut candidates = Vec::new();
        reserve(&mut candidates, self.tree.size())?;
        candidates.extend(self.tree.locate_in_envelope_intersecting(tile.bounds()));
        candidates.sort_unstable_by_key(|t| (t.rank, t.id));
        depth.fill(f32::NAN);
        wse.fill(f32::NAN);
        let mut valid = 0;
        let mut failure = None;
        for t in candidates {
            let plane = self.clipping.as_ref().map(|data| data.planes[t.id]);
            t.scan(&tile, plane, |row, col, weight, m| {
                let index = row * raster.size + col;
                if claims[index] != 0 || failure.is_some() {
                    return;
                }
                claims[index] = 1;
                match self.signed_value(t, levels, terrain[index], weight as f32, m as f32) {
                    Ok(Some((d, w))) => {
                        depth[index] = d;
                        wse[index] = w;
                        valid += 1;
                    }
                    Ok(None) => {}
                    Err(error) => failure = Some(error),
                }
            });
        }
        if let Some(error) = failure {
            return Err(error);
        }
        Ok(valid)
    }

    /// Sample original terrain stitch points using stored triangle order and
    /// double barycentrics, then Float32 Z/M. This deliberately differs from
    /// promoted-rank incremental raster sampling. Claims have the same contract
    /// as `render_signed_raster`; the caller supplies original point terrain.
    #[allow(clippy::too_many_arguments)]
    pub fn sample_signed_points(
        &self,
        levels: &[f32],
        points: &[[f64; 2]],
        terrain: &[f32],
        depth: &mut [f32],
        wse: &mut [f32],
        claims: &mut [u8],
    ) -> Result<usize, MapError> {
        self.validate_signed(levels, terrain, depth, wse, claims, points.len(), 29)?;
        if points.iter().flatten().any(|v| !v.is_finite()) {
            return Err(MapError::Invalid("nonfinite XS sample point".into()));
        }
        depth.fill(f32::NAN);
        wse.fill(f32::NAN);
        let mut valid = 0;
        for (index, &point) in points.iter().enumerate() {
            if claims[index] != 0 {
                continue;
            }
            let found = self
                .tree
                .locate_in_envelope_intersecting(AABB::from_point(point))
                .filter_map(|t| t.point_weights(point).map(|weights| (t, weights)))
                .min_by_key(|(t, _)| t.id);
            if let Some((t, weights)) = found {
                claims[index] = 1;
                let weight = ((t.vertices[0][2] * weights[0] + t.vertices[1][2] * weights[1])
                    + t.vertices[2][2] * weights[2]) as f32;
                let m = self
                    .clipping
                    .as_ref()
                    .map(|data| {
                        let values = data.point_m[t.id];
                        ((values[0] * weights[0] + values[1] * weights[1]) + values[2] * weights[2])
                            as f32
                    })
                    .unwrap_or(0.0);
                if let Some((d, w)) = self.signed_value(t, levels, terrain[index], weight, m)? {
                    depth[index] = d;
                    wse[index] = w;
                    valid += 1;
                }
            }
        }
        Ok(valid)
    }

    /// Render a source-grid window without resetting the original tile lattice.
    /// Geometry claims are independent of terrain validity and profile visibility.
    pub fn render_into(
        &self,
        levels: &[f32],
        terrain: &[f32],
        grid: Grid,
        window: Window,
        depth: &mut [f32],
        wse: &mut [f32],
    ) -> Result<usize, MapError> {
        self.render_into_with(levels, terrain, grid, window, depth, wse, None)
    }

    /// Bytes of per-worker render scratch: one claim byte per tile pixel plus
    /// candidate references for every stored triangle.
    pub fn worker_scratch_bytes(&self, tile_size: usize) -> Option<usize> {
        tile_size.checked_mul(tile_size).and_then(|n| {
            self.tree
                .size()
                .checked_mul(std::mem::size_of::<&Triangle>())
                .and_then(|m| n.checked_add(m))
        })
    }

    /// Renders whole source-tile rows independently. Every tile keeps its own
    /// claim scratch, candidate order and per-scanline incremental arithmetic, so
    /// the result is identical for any worker count; only tile rows are split.
    #[allow(clippy::too_many_arguments)]
    pub fn render_into_with(
        &self,
        levels: &[f32],
        terrain: &[f32],
        grid: Grid,
        window: Window,
        depth: &mut [f32],
        wse: &mut [f32],
        pool: Option<&rayon::ThreadPool>,
    ) -> Result<usize, MapError> {
        let layout = Layout::new(grid, window)?;
        let pixels = window
            .width
            .checked_mul(window.height)
            .ok_or_else(|| MapError::Budget("native XS window overflow".into()))?;
        if levels.len() != self.cross_sections
            || levels.iter().any(|n| n.is_infinite())
            || terrain.len() != pixels
            || depth.len() != pixels
            || wse.len() != pixels
            || terrain.iter().any(|n| n.is_infinite())
        {
            return Err(MapError::Invalid(
                "native XS profile, terrain or output length invalid".into(),
            ));
        }
        let tile_pixels = grid
            .tile_size
            .checked_mul(grid.tile_size)
            .ok_or_else(|| MapError::Budget("native XS tile overflow".into()))?;
        // Worker 0 is covered by the admitted geometry; each extra worker adds
        // its own claim and candidate scratch.
        let workers = pool.map_or(1, |p| p.current_num_threads().max(1));
        let extra = self
            .worker_scratch_bytes(grid.tile_size)
            .and_then(|n| n.checked_mul(workers - 1))
            .ok_or_else(|| MapError::Budget("native XS worker scratch overflow".into()))?;
        if self
            .geometry_bytes
            .checked_add(tile_pixels)
            .and_then(|n| n.checked_add(extra))
            .is_none_or(|n| n > self.max_bytes)
        {
            return Err(MapError::Budget(
                "native XS tile exceeds admitted geometry and scratch bytes".into(),
            ));
        }
        let start_row = window.row / grid.tile_size;
        let end_row = (window.row + window.height - 1) / grid.tile_size;
        let start_col = window.col / grid.tile_size;
        let end_col = (window.col + window.width - 1) / grid.tile_size;
        // Validate every used tile before touching output buffers.
        for tr in start_row..=end_row {
            for tc in start_col..=end_col {
                layout.tile(tr, tc)?;
            }
        }
        depth.fill(f32::NAN);
        wse.fill(f32::NAN);
        // One output band per source tile row, clipped to the window.
        let mut bands = Vec::new();
        reserve(&mut bands, end_row - start_row + 1)?;
        let (mut depth_rest, mut wse_rest) = (depth, wse);
        for tr in start_row..=end_row {
            let top = (tr * grid.tile_size).max(window.row);
            let bottom = ((tr + 1) * grid.tile_size).min(window.row + window.height);
            let len = (bottom - top) * window.width;
            let (d, dr) = std::mem::take(&mut depth_rest).split_at_mut(len);
            let (w, wr) = std::mem::take(&mut wse_rest).split_at_mut(len);
            depth_rest = dr;
            wse_rest = wr;
            bands.push((tr, top, d, w));
        }
        let band = |(tr, top, d, w): (usize, usize, &mut [f32], &mut [f32])| {
            let mut claimed = Vec::<u8>::new();
            reserve(&mut claimed, tile_pixels)?;
            claimed.resize(tile_pixels, 0);
            let mut candidates = Vec::<&Triangle>::new();
            reserve(&mut candidates, self.tree.size())?;
            let mut valid = 0;
            for tc in start_col..=end_col {
                valid += self.render_tile(
                    &layout,
                    levels,
                    &terrain[(top - window.row) * window.width..][..d.len()],
                    grid,
                    window,
                    (tr, tc, top),
                    &mut claimed,
                    &mut candidates,
                    d,
                    w,
                )?;
            }
            Ok::<usize, MapError>(valid)
        };
        match pool {
            Some(pool) if workers > 1 => {
                use rayon::prelude::*;
                pool.install(|| {
                    bands
                        .into_par_iter()
                        .map(band)
                        .sum::<Result<usize, MapError>>()
                })
            }
            _ => bands.into_iter().map(band).sum(),
        }
    }

    /// One source tile; `band` outputs start at global row `top`.
    #[allow(clippy::too_many_arguments)]
    fn render_tile<'a>(
        &'a self,
        layout: &Layout,
        levels: &[f32],
        terrain: &[f32],
        grid: Grid,
        window: Window,
        (tr, tc, top): (usize, usize, usize),
        claimed: &mut [u8],
        candidates: &mut Vec<&'a Triangle>,
        depth: &mut [f32],
        wse: &mut [f32],
    ) -> Result<usize, MapError> {
        let tile = layout.tile(tr, tc)?;
        claimed.fill(0);
        candidates.clear();
        candidates.extend(self.tree.locate_in_envelope_intersecting(tile.bounds()));
        candidates.sort_unstable_by_key(|t| (t.rank, t.id));
        let mut valid = 0;
        for t in candidates.iter() {
            let m_plane = self.clipping.as_ref().map(|data| data.planes[t.id]);
            t.scan(&tile, m_plane, |r, c, z, m| {
                let local = r * grid.tile_size + c;
                if claimed[local] != 0 {
                    return;
                }
                claimed[local] = 1;
                let row = tr * grid.tile_size + r;
                let col = tc * grid.tile_size + c;
                if row < window.row
                    || row >= window.row + window.height
                    || col < window.col
                    || col >= window.col + window.width
                {
                    return;
                }
                let out = (row - top) * window.width + col - window.col;
                let ground = terrain[out];
                let us = levels[t.upstream];
                let ds = levels[t.downstream];
                if !ground.is_finite() || !us.is_finite() || !ds.is_finite() {
                    return;
                }
                let weight = z as f32;
                let water = weight * us + (1.0_f32 - weight) * ds;
                if let Some(data) = &self.clipping
                    && !crate::one_d_clip::visible(
                        data.clips[t.upstream],
                        data.clips[t.downstream],
                        us,
                        ds,
                        weight,
                        m as f32,
                        water,
                    )
                {
                    return;
                }
                let d = water - ground;
                if water.is_finite() && d.is_finite() && water >= ground + 0.001_f32 {
                    depth[out] = d;
                    wse[out] = water;
                    valid += 1;
                }
            });
        }
        Ok(valid)
    }
}

struct Layout {
    grid: Grid,
    pitch: f64,
}
impl Layout {
    fn new(grid: Grid, window: Window) -> Result<Self, MapError> {
        let g = grid.geo_transform;
        if grid.width == 0
            || grid.height == 0
            || grid.tile_size == 0
            || grid.tile_size > i32::MAX as usize
            || window.width == 0
            || window.height == 0
            || !g.iter().all(|v| v.is_finite())
            || g[1] <= 0.0
            || g[5] >= 0.0
            || g[2] != 0.0
            || g[4] != 0.0
            || window
                .col
                .checked_add(window.width)
                .is_none_or(|n| n > grid.width)
            || window
                .row
                .checked_add(window.height)
                .is_none_or(|n| n > grid.height)
        {
            return Err(MapError::Invalid(
                "native XS invalid global grid or source window".into(),
            ));
        }
        if g[1] != -g[5] {
            return Err(MapError::Unsupported(
                "native XS scan conversion requires square pixels".into(),
            ));
        }
        let max_x = g[0] + grid.width as f64 * g[1];
        let min_y = g[3] + grid.height as f64 * g[5];
        let cs = ((max_x - g[0]) / grid.width as f64).max((g[3] - min_y) / grid.height as f64);
        let pitch0 = grid.tile_size as f64 * cs;
        let columns = grid.width.div_ceil(grid.tile_size);
        let rows = grid.height.div_ceil(grid.tile_size);
        if columns.checked_mul(grid.tile_size).is_none()
            || rows.checked_mul(grid.tile_size).is_none()
        {
            return Err(MapError::Budget(
                "native XS padded source grid overflow".into(),
            ));
        }
        let right = g[0] + columns as f64 * pitch0;
        let bottom = g[3] - rows as f64 * pitch0;
        let pitch = ((right - g[0]) / columns as f64).max((g[3] - bottom) / rows as f64);
        if ![max_x, min_y, right, bottom, pitch]
            .iter()
            .all(|v| v.is_finite())
            || pitch <= 0.0
        {
            return Err(MapError::Invalid(
                "native XS global tile geometry overflow".into(),
            ));
        }
        Ok(Self { grid, pitch })
    }
    fn tile(&self, row: usize, col: usize) -> Result<Tile, MapError> {
        let g = self.grid.geo_transform;
        let min_x = g[0] + self.pitch * col as f64;
        let max_x = g[0] + self.pitch * (col + 1) as f64;
        let max_y = g[3] - self.pitch * row as f64;
        let min_y = g[3] - self.pitch * (row + 1) as f64;
        let cell = ((max_x - min_x) / self.grid.tile_size as f64)
            .max((max_y - min_y) / self.grid.tile_size as f64);
        if ![min_x, max_x, min_y, max_y, cell]
            .iter()
            .all(|v| v.is_finite())
            || cell <= 0.0
        {
            return Err(MapError::Invalid(
                "native XS tile coordinates overflow".into(),
            ));
        }
        Ok(Tile {
            min_x,
            max_x,
            min_y,
            max_y,
            cell,
            size: self.grid.tile_size,
        })
    }
}

struct Tile {
    min_x: f64,
    max_x: f64,
    min_y: f64,
    max_y: f64,
    cell: f64,
    size: usize,
}
impl Tile {
    fn bounds(&self) -> AABB<[f64; 2]> {
        AABB::from_corners([self.min_x, self.min_y], [self.max_x, self.max_y])
    }
    fn x(&self, col: i64) -> f64 {
        self.min_x + self.cell * (col as f64 + 0.5)
    }
    fn y(&self, row: i64) -> f64 {
        self.max_y - self.cell * (row as f64 + 0.5)
    }
    fn col(&self, x: f64) -> i64 {
        ((x - self.min_x) / self.cell).floor() as i64
    }
    fn row(&self, y: f64) -> i64 {
        ((self.max_y - y) / self.cell).floor() as i64
    }
}
impl Triangle {
    fn point_weights(&self, [x, y]: [f64; 2]) -> Option<[f64; 3]> {
        let [p, q, r] = self.vertices;
        let a = q[1] - r[1];
        let b = p[0] - r[0];
        let c = r[0] - q[0];
        let d = p[1] - r[1];
        let determinant = a * b + c * d;
        let dx = x - r[0];
        let dy = y - r[1];
        let first = (a * dx + c * dy) / determinant;
        let second = (b * dy - d * dx) / determinant;
        let third = (1.0 - first) - second;
        let weights = [first, second, third];
        weights
            .iter()
            .all(|w| (-0.0001..=1.0001).contains(w))
            .then_some(weights)
    }

    fn scan(
        &self,
        tile: &Tile,
        m_plane: Option<[f64; 4]>,
        mut emit: impl FnMut(usize, usize, f64, f64),
    ) {
        let [a, b, c, d] = self.plane;
        let step_z = ((0.0 - a) / c) * tile.cell;
        let step_m = m_plane
            .map(|[a, _, c, _]| ((0.0 - a) / c) * tile.cell)
            .unwrap_or(0.0);
        let mut p = self.vertices;
        // Strict swaps leave Y ties in stored vertex order.
        if p[0][1] > p[1][1] {
            p.swap(0, 1);
        }
        if p[1][1] > p[2][1] {
            p.swap(1, 2);
        }
        if p[0][1] > p[1][1] {
            p.swap(0, 1);
        }
        let [lo, mid, hi] = p;
        if lo[1] == hi[1] {
            return;
        }
        let mut top = if hi[1] > tile.max_y {
            0
        } else {
            tile.row(hi[1])
        };
        if hi[1] <= tile.max_y && hi[1] < tile.y(top) {
            top += 1;
        }
        let mut bottom = if lo[1] < tile.min_y {
            tile.size as i64 - 1
        } else {
            tile.row(lo[1])
        };
        if lo[1] >= tile.min_y && lo[1] > tile.y(bottom) {
            bottom -= 1;
        }
        top = top.max(0);
        bottom = bottom.min(tile.size as i64 - 1);
        if top > bottom {
            return;
        }
        let long = (hi[0] - lo[0]) / (hi[1] - lo[1]);
        let up = (hi[0] - mid[0]) / (hi[1] - mid[1]);
        let low = (mid[0] - lo[0]) / (mid[1] - lo[1]);
        let delta_long = long * tile.cell;
        let delta_up = up * tile.cell;
        let delta_low = low * tile.cell;
        let mut y = tile.y(top);
        let mut x_long = hi[0] + long * (y - hi[1]);
        let mut x_up = hi[0] + up * (y - hi[1]);
        let mut x_low = mid[0] + low * (y - mid[1]);
        for row in top..=bottom {
            let mut left = x_long;
            let mut right = if y <= mid[1] && !x_low.is_nan() {
                x_low
            } else {
                x_up
            };
            if left > right {
                std::mem::swap(&mut left, &mut right);
            }
            if left.is_finite() && right.is_finite() {
                let mut first = tile.col(left);
                if tile.x(first) < left {
                    first = first.saturating_add(1);
                }
                let mut last = tile.col(right);
                if tile.x(last) > right {
                    last = last.saturating_sub(1);
                }
                if last >= 0 && first < tile.size as i64 {
                    first = first.max(0);
                    last = last.min(tile.size as i64 - 1);
                    if first <= last {
                        let mut z = ((0.0 - d) - a * tile.x(first) - b * y) / c;
                        let mut m = m_plane
                            .map(|[a, b, c, d]| ((0.0 - d) - a * tile.x(first) - b * y) / c)
                            .unwrap_or(0.0);
                        for col in first..=last {
                            emit(row as usize, col as usize, z, m);
                            z += step_z;
                            m += step_m;
                        }
                    }
                }
            }
            y -= tile.cell;
            x_long -= delta_long;
            x_up -= delta_up;
            x_low -= delta_low;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn square() -> ([[f64; 4]; 4], [[i32; 3]; 2], Segment) {
        (
            [
                [0., 0., 1., 0.],
                [0., 4., 1., 0.],
                [4., 0., 0., 0.],
                [4., 4., 0., 0.],
            ],
            [[0, 2, 1], [1, 2, 3]],
            Segment {
                point_start: 0,
                point_count: 4,
                triangle_start: 0,
                triangle_count: 2,
                upstream: 0,
                downstream: 1,
            },
        )
    }
    fn grid() -> Grid {
        Grid {
            width: 4,
            height: 4,
            geo_transform: [0., 1., 0., 4., 0., -1.],
            tile_size: 2,
        }
    }
    fn window() -> Window {
        Window {
            col: 0,
            row: 0,
            width: 4,
            height: 4,
        }
    }
    #[test]
    fn inclusive_scanlines_and_global_crop_are_identical() {
        let (p, t, s) = square();
        let surface = PreparedSurface::new(&p, &t, &[s], 2, &[0], 65536).unwrap();
        let mut depth = vec![0.; 16];
        let mut wse = vec![0.; 16];
        assert_eq!(
            surface
                .render_into(
                    &[12., 8.],
                    &[0.; 16],
                    grid(),
                    window(),
                    &mut depth,
                    &mut wse
                )
                .unwrap(),
            16
        );
        for row in 0..4 {
            for col in 0..4 {
                assert_eq!(wse[row * 4 + col], 11.5 - col as f32);
            }
        }
        let crop = Window {
            col: 1,
            row: 1,
            width: 2,
            height: 2,
        };
        let mut cd = [0.; 4];
        let mut cw = [0.; 4];
        surface
            .render_into(&[12., 8.], &[0.; 4], grid(), crop, &mut cd, &mut cw)
            .unwrap();
        assert_eq!(cw, [wse[5], wse[6], wse[9], wse[10]]);
    }
    #[test]
    fn tile_row_workers_match_serial_bits_for_every_window() {
        // Overlapping random segments, NaN levels/terrain, odd grid and crops.
        let mut seed = 0x9e37_79b9_7f4a_7c15_u64;
        let mut next = move || {
            seed = seed
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            (seed >> 11) as f64 / (1u64 << 53) as f64
        };
        let g = Grid {
            width: 37,
            height: 29,
            geo_transform: [100., 0.5, 0., 50., 0., -0.5],
            tile_size: 4,
        };
        let (mut points, mut triangles, mut segments) = (vec![], vec![], vec![]);
        for i in 0..14 {
            let start = points.len();
            for _ in 0..6 {
                points.push([97. + next() * 24., 33. + next() * 20., next(), 0.]);
            }
            let first = triangles.len();
            triangles.extend([[0, 1, 2], [1, 2, 3], [2, 3, 4], [3, 4, 5]]);
            segments.push(Segment {
                point_start: start,
                point_count: 6,
                triangle_start: first,
                triangle_count: 4,
                upstream: i,
                downstream: i + 1,
            });
        }
        let ranks: Vec<u32> = (0..14).rev().collect();
        let surface =
            PreparedSurface::new(&points, &triangles, &segments, 15, &ranks, 1 << 30).unwrap();
        let mut levels: Vec<f32> = (0..15).map(|i| 3. + i as f32 * 0.37).collect();
        levels[6] = f32::NAN;
        let windows = [
            window_of(0, 0, 37, 29),
            window_of(3, 5, 30, 21),
            window_of(9, 1, 1, 27),
            window_of(0, 27, 37, 2),
        ];
        for w in windows {
            let terrain: Vec<f32> = (0..w.width * w.height)
                .map(|i| {
                    if i % 17 == 3 {
                        f32::NAN
                    } else {
                        next() as f32 * 6.
                    }
                })
                .collect();
            let n = w.width * w.height;
            let (mut d0, mut w0) = (vec![0.; n], vec![0.; n]);
            let serial = surface
                .render_into(&levels, &terrain, g, w, &mut d0, &mut w0)
                .unwrap();
            assert!(serial > 0);
            for threads in 1..=5 {
                let pool = rayon::ThreadPoolBuilder::new()
                    .num_threads(threads)
                    .build()
                    .unwrap();
                let (mut d1, mut w1) = (vec![1.; n], vec![1.; n]);
                let valid = surface
                    .render_into_with(&levels, &terrain, g, w, &mut d1, &mut w1, Some(&pool))
                    .unwrap();
                assert_eq!(valid, serial);
                assert!(d0.iter().zip(&d1).all(|(a, b)| a.to_bits() == b.to_bits()));
                assert!(w0.iter().zip(&w1).all(|(a, b)| a.to_bits() == b.to_bits()));
            }
        }
    }
    fn window_of(col: usize, row: usize, width: usize, height: usize) -> Window {
        Window {
            col,
            row,
            width,
            height,
        }
    }
    #[test]
    fn worker_scratch_is_admitted_before_output_mutation() {
        let (p, t, s) = square();
        let surface =
            PreparedSurface::new(&p, &t, &[s], 2, &[0], 4 * 32 + 2 * 1024 + 64 + 4).unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(3)
            .build()
            .unwrap();
        let (mut d, mut w) = (vec![7.; 16], vec![7.; 16]);
        assert!(matches!(
            surface.render_into_with(
                &[12., 8.],
                &[0.; 16],
                grid(),
                window(),
                &mut d,
                &mut w,
                Some(&pool)
            ),
            Err(MapError::Budget(_))
        ));
        assert!(d.iter().chain(&w).all(|v| *v == 7.));
        surface
            .render_into(&[12., 8.], &[0.; 16], grid(), window(), &mut d, &mut w)
            .unwrap();
    }
    #[test]
    fn rank_claims_before_dry_and_missing_and_validates_permutation() {
        let (p, t, s) = square();
        let mut pp = p.to_vec();
        pp.extend(p);
        let mut tt = t.to_vec();
        tt.extend(t);
        let mut second = s;
        second.point_start = 4;
        second.triangle_start = 2;
        second.upstream = 2;
        second.downstream = 3;
        assert!(PreparedSurface::new(&pp, &tt, &[s, second], 4, &[0, 0], 65536).is_err());
        for bad in [0., f32::NAN] {
            let surf = PreparedSurface::new(&pp, &tt, &[s, second], 4, &[1, 0], 65536).unwrap();
            let mut d = [1.; 16];
            let mut w = [1.; 16];
            assert_eq!(
                surf.render_into(
                    &[12., 8., bad, bad],
                    &[1.; 16],
                    grid(),
                    window(),
                    &mut d,
                    &mut w
                )
                .unwrap(),
                0
            );
            assert!(d.iter().all(|n| n.is_nan()));
            assert!(w.iter().all(|n| n.is_nan()));
        }
    }
    #[test]
    fn padded_source_overflow_is_rejected_before_output_mutation() {
        let (p, t, s) = square();
        let surf = PreparedSurface::new(&p, &t, &[s], 2, &[0], 65536).unwrap();
        let g = Grid {
            width: usize::MAX,
            height: 1,
            geo_transform: [0., 1., 0., 1., 0., -1.],
            tile_size: 4095,
        };
        let window = Window {
            col: usize::MAX - 1,
            row: 0,
            width: 1,
            height: 1,
        };
        let mut d = [7.];
        let mut w = [8.];
        let error = surf
            .render_into(&[12., 8.], &[0.], g, window, &mut d, &mut w)
            .unwrap_err();
        assert!(error.to_string().contains("padded source grid overflow"));
        assert_eq!(d, [7.]);
        assert_eq!(w, [8.]);
    }
    #[test]
    fn invalid_inputs_do_not_mutate_outputs() {
        let (p, t, s) = square();
        let surf = PreparedSurface::new(&p, &t, &[s], 2, &[0], 65536).unwrap();
        let mut d = [7.; 16];
        let mut w = [8.; 16];
        let mut g = grid();
        g.geo_transform[5] = -2.;
        assert!(
            surf.render_into(&[12., 8.], &[0.; 16], g, window(), &mut d, &mut w)
                .is_err()
        );
        assert_eq!(d, [7.; 16]);
        assert_eq!(w, [8.; 16]);
        assert!(PreparedSurface::planned_bytes(0, 0, 0, usize::MAX).is_err());
    }
    #[test]
    fn incremental_plane_rounding_is_not_replaced_by_closed_form() {
        // Synthetic midpoint case: fourteen double additions land just above
        // the Float32 rounding midpoint; direct evaluation lands on the tie.
        let intercept = 0.35500002980232237;
        let t = Triangle {
            bounds: AABB::from_corners([0., 0.], [64., 2.]),
            vertices: [
                [0., 0., intercept],
                [0., 2., intercept],
                [64., 0., intercept + 0.64],
            ],
            plane: [-0.01, 0., 1., -intercept],
            rank: 0,
            id: 0,
            upstream: 0,
            downstream: 1,
        };
        let tile = Tile {
            min_x: 0.,
            max_x: 64.,
            min_y: -62.,
            max_y: 2.,
            cell: 1.,
            size: 64,
        };
        let mut found = None;
        t.scan(&tile, None, |row, col, weight, _| {
            if row == 0 && col == 14 {
                found = Some((weight as f32).to_bits());
            }
        });
        assert_eq!(found, Some(0x3f00_0001));
        let direct = (intercept + 0.01 * 14.5) as f32;
        assert_eq!(direct.to_bits(), 0x3f00_0000);
        let surface = PreparedSurface {
            tree: RTree::bulk_load(vec![t]),
            cross_sections: 2,
            geometry_bytes: 0,
            max_bytes: 1 << 20,
            clipping: None,
        };
        let grid = Grid {
            width: 64,
            height: 64,
            geo_transform: [0., 1., 0., 2., 0., -1.],
            tile_size: 64,
        };
        let mut depth = vec![0.; 4096];
        let mut water = vec![0.; 4096];
        surface
            .render_into(
                &[1., 0.],
                &vec![0.; 4096],
                grid,
                Window {
                    col: 0,
                    row: 0,
                    width: 64,
                    height: 64,
                },
                &mut depth,
                &mut water,
            )
            .unwrap();
        let mut crop_depth = [0.];
        let mut crop_water = [0.];
        surface
            .render_into(
                &[1., 0.],
                &[0.],
                grid,
                Window {
                    col: 14,
                    row: 0,
                    width: 1,
                    height: 1,
                },
                &mut crop_depth,
                &mut crop_water,
            )
            .unwrap();
        assert_eq!(water[14].to_bits(), 0x3f00_0001);
        assert_eq!(crop_water[0].to_bits(), water[14].to_bits());
    }

    #[test]
    fn constructor_admission_binds_later_tile_scratch() {
        let (p, t, s) = square();
        let cap = geometry_bytes(p.len(), t.len(), 1).unwrap();
        let surface = PreparedSurface::new(&p, &t, &[s], 2, &[0], cap).unwrap();
        let mut depth = [7.; 16];
        let mut wse = [8.; 16];
        assert!(matches!(
            surface.render_into(
                &[12., 8.],
                &[0.; 16],
                grid(),
                window(),
                &mut depth,
                &mut wse
            ),
            Err(MapError::Budget(_))
        ));
        assert_eq!(depth, [7.; 16]);
        assert_eq!(wse, [8.; 16]);
    }
}
