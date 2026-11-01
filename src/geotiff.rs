//! Lossless native product delivery. Pixel arithmetic is completed before encoding.
use crate::MapError;
use std::path::Path;
use wbraster::{DataType, Raster, crs_info::CrsInfo, raster::RasterData};

/// Bounded row writer. The header contains no pixel allocation.
pub(crate) fn create(
    path: &Path,
    width: usize,
    height: usize,
    transform: [f64; 6],
    epsg: u32,
    compression: crate::native_output::Compression,
) -> Result<terrio::GeoTiffWriter, MapError> {
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
        crs: CrsInfo::from_epsg(epsg),
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
            creation_options: vec![("BLOCKYSIZE".into(), strip_rows(width).to_string())],
            ..Default::default()
        },
    )
    .map_err(|e| MapError::Io(format!("native GeoTIFF create: {e}")))
}

pub(crate) fn strip_rows(width: usize) -> usize {
    (8192 / (width * 4)).max(1)
}

pub(crate) fn write(
    path: &Path,
    values: Vec<f32>,
    width: usize,
    height: usize,
    transform: [f64; 6],
    epsg: u32,
    compression: crate::native_output::Compression,
) -> Result<(), MapError> {
    let mut writer = create(path, width, height, transform, epsg, compression)?;
    for data in values.chunks(width * strip_rows(width).min(height)) {
        writer
            .write_rows(&RasterData::F32(data.to_vec()), data.len() / width, None)
            .map_err(|e| MapError::Io(format!("native GeoTIFF rows: {e}")))?;
    }
    writer
        .finish()
        .map_err(|e| MapError::Io(format!("native GeoTIFF finish: {e}")))
}
