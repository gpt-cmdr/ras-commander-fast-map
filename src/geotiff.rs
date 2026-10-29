//! Lossless native product delivery. Pixel arithmetic is completed before encoding.
use crate::MapError;
use std::path::Path;
use wbraster::{DataType, Raster, crs_info::CrsInfo, raster::RasterData};

pub(crate) fn write(
    path: &Path,
    values: Vec<f32>,
    width: usize,
    height: usize,
    transform: [f64; 6],
    epsg: u32,
) -> Result<(), MapError> {
    let raster = Raster {
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
        data: RasterData::F32(values),
    };
    let dataset =
        terrio::RasterDataset::with_driver(raster, terrio::formats::RasterDriver::GTiff, true);
    let options = terrio::RasterWriteOptions {
        compression: Some("LZW".into()),
        geo_transform: Some(transform),
        // Match GDAL's default strip target to keep the comparison about the writer.
        creation_options: vec![("BLOCKYSIZE".into(), (8192 / (width * 4)).max(1).to_string())],
        ..Default::default()
    };
    terrio::write_raster_dataset(
        &dataset,
        path.to_str()
            .ok_or_else(|| MapError::Invalid("product path is not UTF8".into()))?,
        &options,
    )
    .map_err(|e| MapError::Invalid(format!("native GeoTIFF write: {e}")))
}
