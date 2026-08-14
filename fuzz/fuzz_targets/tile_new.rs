#![no_main]

use libfuzzer_sys::fuzz_target;
use opdf_core::Tile;

#[derive(Debug, arbitrary::Arbitrary)]
struct TileInput {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

fuzz_target!(|input: TileInput| {
    // Cap pixel-buffer generation so libFuzzer spends its time on
    // interesting width/height combinations rather than allocating
    // gigabytes of zeroed input on every iteration.
    if input.pixels.len() > 1 << 20 {
        return;
    }

    let result = Tile::new(input.width, input.height, input.pixels.clone());

    if let Ok(tile) = result {
        // If construction succeeded, the invariant Tile::new exists to
        // guarantee must hold: the pixel buffer is exactly width * height * 4
        // bytes, computed without overflow (checked as u64 here, on the
        // host, deliberately wider than the u32 inputs to avoid the fuzz
        // harness itself overflowing).
        let expected = u64::from(input.width) * u64::from(input.height) * 4;
        assert_eq!(tile.pixels().len() as u64, expected, "Tile::new accepted a buffer of the wrong length");
    }
});
