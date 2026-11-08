//! Shared vector ID: stored-tile-grid-rounded-extent/1.
//! The Commander companion test pins these exact binary64 constants.
use ras_commander_fast_map::stored_grid::{Contract, ID};

#[test]
fn shared_rounded_extent_window_and_centers() {
    let raw = [1_000_000., 1.1, 0., 10_000_000., 0., -1.1];
    let output_bits: [u64; 6] = [
        0x412e848000000000,
        0x3ff1999999999800,
        0,
        0x416312d000000000,
        0,
        0xbff1999999999800,
    ];
    let contract = Contract {
        id: ID.into(),
        source_width: 512,
        source_height: 256,
        tile_size: 256,
        source_geo_transform: raw,
        output_geo_transform: output_bits.map(f64::from_bits),
    };
    let window = [255, 255, 257, 1];
    let layout = contract.validate(window).unwrap();
    assert_eq!(layout.source.map(f64::to_bits), raw.map(f64::to_bits));
    assert_eq!(layout.output.map(f64::to_bits), output_bits);
    assert_eq!(
        layout.window_transform(window).map(f64::to_bits),
        [
            0x412e86b100000000,
            0x3ff1999999999800,
            0,
            0x416312acf0000000,
            0,
            0xbff1999999999800,
        ]
    );
    for (row, col, expected) in [
        (0, 0, [0x412e84811999999a, 0x416312cfee666666]),
        (0, 255, [0x412e86b219999999, 0x416312cfee666666]),
        (0, 256, [0x412e86b44ccccccd, 0x416312cfee666666]),
        (255, 511, [0x412e88e54ccccccc, 0x416312acde666666]),
    ] {
        assert_eq!(layout.point(row, col).map(f64::to_bits), expected);
    }
}
