//! Lossless native product delivery. Pixel arithmetic is completed before encoding.
use crate::MapError;
use crate::crs::{GeoKeys, OutputCrs};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use wbraster::{DataType, Raster, crs_info::CrsInfo, raster::RasterData};

/// Bounded row writer (no pixel allocation in the header), any CRS. With
/// GeoKeys the caller must call [`finish_crs`] after terrio closes the file;
/// [`RowWriter`] does.
pub(crate) fn create_then_finish_crs(
    path: &Path,
    width: usize,
    height: usize,
    transform: [f64; 6],
    crs: &OutputCrs,
    compression: crate::native_output::Compression,
) -> Result<terrio::GeoTiffWriter, MapError> {
    create_with_strip_rows(
        path,
        width,
        height,
        transform,
        crs,
        compression,
        strip_rows(width),
    )
}

fn create_with_strip_rows(
    path: &Path,
    width: usize,
    height: usize,
    transform: [f64; 6],
    crs: &OutputCrs,
    compression: crate::native_output::Compression,
    rows: usize,
) -> Result<terrio::GeoTiffWriter, MapError> {
    validate_shape(width, height)?;
    let header = Raster {
        cols: width,
        rows: height,
        bands: 1,
        x_min: transform[0],
        y_min: transform[3] + transform[5] * height as f64,
        cell_size_x: transform[1],
        cell_size_y: -transform[5],
        nodata: f64::NAN,
        data_type: DataType::F32,
        crs: crs_info(crs),
        metadata: Vec::new(),
        data: RasterData::F32(Vec::new()),
    };
    let dataset =
        terrio::RasterDataset::with_driver(header, terrio::formats::RasterDriver::GTiff, true);
    terrio::GeoTiffWriter::create(
        path.to_str()
            .ok_or_else(|| MapError::Invalid("product path is not UTF8".into()))?,
        &dataset,
        &terrio::RasterWriteOptions {
            compression: Some(compression.name().into()),
            geo_transform: Some(transform),
            creation_options: vec![("BLOCKYSIZE".into(), rows.to_string())],
            ..Default::default()
        },
    )
    .map_err(|e| MapError::Io(format!("native GeoTIFF create: {e}")))
}

/// terrio's CRS for `crs`: the EPSG code, or none for GeoKeys, which
/// [`finish_crs`] adds verbatim once terrio has closed the file.
pub(crate) fn crs_info(crs: &OutputCrs) -> CrsInfo {
    match crs {
        OutputCrs::Epsg(code) => CrsInfo::from_epsg(*code),
        OutputCrs::GeoKeys(_) => CrsInfo::default(),
    }
}

/// Complete a file written through terrio with `crs`: nothing for an EPSG
/// code; the verbatim GeoKey tags otherwise.
pub(crate) fn finish_crs(path: &Path, crs: &OutputCrs) -> Result<(), MapError> {
    match crs {
        OutputCrs::Epsg(_) => Ok(()),
        OutputCrs::GeoKeys(keys) => crate::crs::insert_geokeys(path, keys),
    }
}

pub(crate) fn strip_rows(width: usize) -> usize {
    (STRIP_TARGET_BYTES / width.saturating_mul(4).max(1)).max(1)
}

/// A wide raster still needs at least one row. Otherwise each uncompressed
/// strip is at most 256 KiB; encoding never needs a full product copy.
const STRIP_TARGET_BYTES: usize = 256 * 1024;

fn validate_shape(width: usize, height: usize) -> Result<usize, MapError> {
    if width == 0 || height == 0 || width > u32::MAX as usize || height > u32::MAX as usize {
        return Err(MapError::Invalid(
            "native GeoTIFF dimensions invalid".into(),
        ));
    }
    width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(4))
        .ok_or_else(|| MapError::Budget("native GeoTIFF dimensions overflow".into()))
}

/// Incremental single-band writer. Calls must contain consecutive complete rows.
/// Only a bounded strip copy is passed to terrio, even for a larger input chunk.
pub(crate) struct RowWriter {
    writer: terrio::GeoTiffWriter,
    width: usize,
    height: usize,
    written: usize,
    /// Path and GeoKeys to add after terrio closes the file.
    geokeys: Option<(PathBuf, Arc<GeoKeys>)>,
}

impl RowWriter {
    pub(crate) fn create(
        path: &Path,
        width: usize,
        height: usize,
        transform: [f64; 6],
        crs: &OutputCrs,
        compression: crate::native_output::Compression,
    ) -> Result<Self, MapError> {
        Ok(Self {
            writer: create_with_strip_rows(
                path,
                width,
                height,
                transform,
                crs,
                compression,
                strip_rows(width),
            )?,
            width,
            height,
            written: 0,
            geokeys: match crs {
                OutputCrs::Epsg(_) => None,
                OutputCrs::GeoKeys(keys) => Some((path.to_path_buf(), keys.clone())),
            },
        })
    }

    pub(crate) fn write_rows(&mut self, values: &[f32]) -> Result<(), MapError> {
        if !values.len().is_multiple_of(self.width) {
            return Err(MapError::Invalid(
                "native GeoTIFF requires complete rows".into(),
            ));
        }
        let rows = values.len() / self.width;
        if rows > self.height - self.written {
            return Err(MapError::Invalid(
                "native GeoTIFF row count exceeds height".into(),
            ));
        }
        // At most one strip (one TIFF block row) per terrio call, with the
        // strips left over from an earlier call: terrio encodes a single chunk
        // on the calling thread and never reaches its pool, so no helper thread
        // is needed here (`terrio_pool`; the thread-budget tests sample it).
        for chunk in values.chunks(self.width * strip_rows(self.width).min(self.height)) {
            self.writer
                .write_rows(
                    &RasterData::F32(chunk.to_vec()),
                    chunk.len() / self.width,
                    None,
                )
                .map_err(|e| MapError::Io(format!("native GeoTIFF rows: {e}")))?;
        }
        self.written += rows;
        Ok(())
    }

    pub(crate) fn finish(self) -> Result<(), MapError> {
        if self.written != self.height {
            return Err(MapError::Invalid("native GeoTIFF incomplete rows".into()));
        }
        let writer = self.writer;
        crate::terrio_pool::call(|| writer.finish())?
            .map_err(|e| MapError::Io(format!("native GeoTIFF finish: {e}")))?;
        match self.geokeys {
            Some((path, keys)) => crate::crs::insert_geokeys(&path, &keys),
            None => Ok(()),
        }
    }
}

/// Row-streamed product file: legacy strips, or the opt-in final delivery format.
pub(crate) enum ProductWriter {
    Strips(Box<RowWriter>),
    Final(Box<crate::final_tiff::Writer>),
}

impl ProductWriter {
    /// `transform` is the product window's; `layout` carries the final placement.
    pub(crate) fn create(
        path: &Path,
        width: usize,
        height: usize,
        transform: [f64; 6],
        crs: &OutputCrs,
        compression: crate::native_output::Compression,
        layout: Option<&crate::final_tiff::Layout>,
    ) -> Result<Self, MapError> {
        Ok(match layout {
            Some(layout) => Self::Final(Box::new(crate::final_tiff::Writer::create_crs(
                path, layout, crs,
            )?)),
            None => Self::Strips(Box::new(RowWriter::create(
                path,
                width,
                height,
                transform,
                crs,
                compression,
            )?)),
        })
    }

    pub(crate) fn write_rows(
        &mut self,
        values: &[f32],
        pool: &rayon::ThreadPool,
    ) -> Result<(), MapError> {
        match self {
            Self::Strips(writer) => writer.write_rows(values),
            Self::Final(writer) => writer.write_rows(values, pool),
        }
    }

    pub(crate) fn finish(self) -> Result<(), MapError> {
        match self {
            Self::Strips(writer) => writer.finish(),
            Self::Final(writer) => writer.finish().map(|_| ()),
        }
    }
}

pub(crate) fn write(
    path: &Path,
    values: Vec<f32>,
    width: usize,
    height: usize,
    transform: [f64; 6],
    crs: &OutputCrs,
    compression: crate::native_output::Compression,
) -> Result<(), MapError> {
    let bytes = validate_shape(width, height)?;
    if values.len() != bytes / 4 {
        return Err(MapError::Invalid(
            "native GeoTIFF buffer dimensions mismatch".into(),
        ));
    }
    let mut writer = RowWriter::create(path, width, height, transform, crs, compression)?;
    writer.write_rows(&values)?;
    writer.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        time::{Instant, SystemTime, UNIX_EPOCH},
    };

    fn directory() -> PathBuf {
        let path = std::env::temp_dir().join(format!(
            "fast-map-row-writer-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&path).unwrap();
        path
    }
    fn read(path: &Path) -> Vec<f32> {
        let mut raster = terrio::LazyRaster::open_tiff(path.to_str().unwrap())
            .unwrap()
            .unwrap();
        let window = terrio::Window {
            x: 0,
            y: 0,
            width: raster.width(),
            height: raster.height(),
        };
        let ds = raster
            .read_window(
                window,
                &terrio::WindowOptions {
                    bands: vec![1],
                    overview: None,
                },
            )
            .unwrap();
        let RasterData::F32(values) = ds.raster.data else {
            panic!("not Float32")
        };
        values
    }
    #[test]
    fn strip_bound_and_overflow() {
        assert_eq!(strip_rows(28916), 2);
        for width in [1, 4, 512, 2048, 28916, 100000] {
            assert!(strip_rows(width) * width * 4 <= STRIP_TARGET_BYTES.max(width * 4));
        }
        assert!(validate_shape(0, 1).is_err());
        assert!(validate_shape(1, 0).is_err());
        assert!(validate_shape(usize::MAX, 2).is_err());
    }
    #[test]
    fn incremental_rows_preserve_all_bits_and_reject_bad_shapes() {
        let dir = directory();
        let path = dir.join("rows.tif");
        let (width, height) = (2049, 37);
        let values: Vec<f32> = (0..width * height)
            .map(|i| match i % 7 {
                0 => f32::NAN,
                1 => -0.0,
                2 => f32::from_bits(1),
                _ => i as f32 * 0.125,
            })
            .collect();
        for compression in [
            crate::native_output::Compression::None,
            crate::native_output::Compression::Lzw,
        ] {
            let mut w = RowWriter::create(
                &path,
                width,
                height,
                [3., 10., 0., 1000., 0., -10.],
                &OutputCrs::Epsg(3433),
                compression,
            )
            .unwrap();
            assert!(w.write_rows(&values[..width - 1]).is_err());
            w.write_rows(&values[..width * 3]).unwrap();
            w.write_rows(&values[width * 3..width * 30]).unwrap();
            w.write_rows(&values[width * 30..]).unwrap();
            assert!(w.write_rows(&values[..width]).is_err());
            w.finish().unwrap();
            let actual = read(&path);
            assert_eq!(
                values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                actual.iter().map(|v| v.to_bits()).collect::<Vec<_>>()
            );
        }
        let incomplete = RowWriter::create(
            &dir.join("short.tif"),
            2,
            2,
            [0., 1., 0., 2., 0., -1.],
            &OutputCrs::Epsg(3433),
            crate::native_output::Compression::None,
        )
        .unwrap();
        assert!(incomplete.finish().is_err());
        fs::remove_dir_all(dir).unwrap();
    }
    /// Opt-in isolated writer timing; old/new have identical data/compression/metadata.
    #[test]
    #[ignore]
    fn benchmark_wide_strips() {
        let dir = directory();
        let (width, height) = (28916, 256);
        let values: Vec<f32> = (0..width * height)
            .map(|i| {
                if i % 17 == 0 {
                    f32::NAN
                } else {
                    ((i / width + i % 301) as f32) * 0.125
                }
            })
            .collect();
        for round in 0..3 {
            for legacy in if round % 2 == 0 {
                [true, false]
            } else {
                [false, true]
            } {
                let rows = if legacy {
                    (8192 / (width * 4)).max(1)
                } else {
                    strip_rows(width)
                };
                let path = dir.join(format!("{round}-{legacy}.tif"));
                let tick = Instant::now();
                let mut w = create_with_strip_rows(
                    &path,
                    width,
                    height,
                    [0., 10., 0., 2560., 0., -10.],
                    &OutputCrs::Epsg(3433),
                    crate::native_output::Compression::Lzw,
                    rows,
                )
                .unwrap();
                for chunk in values.chunks(width * rows) {
                    w.write_rows(&RasterData::F32(chunk.to_vec()), chunk.len() / width, None)
                        .unwrap();
                }
                w.finish().unwrap();
                let seconds = tick.elapsed().as_secs_f64();
                assert_eq!(
                    values.iter().map(|v| v.to_bits()).collect::<Vec<_>>(),
                    read(&path).iter().map(|v| v.to_bits()).collect::<Vec<_>>()
                );
                println!(
                    "wide_strip_benchmark round={round} legacy={legacy} rows={rows} seconds={seconds:.6} bytes={}",
                    fs::metadata(&path).unwrap().len()
                );
            }
        }
        fs::remove_dir_all(dir).unwrap();
    }
}
