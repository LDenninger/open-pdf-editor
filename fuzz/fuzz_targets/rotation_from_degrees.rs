#![no_main]

use libfuzzer_sys::fuzz_target;
use opdf_core::Rotation;

fuzz_target!(|degrees: i32| {
    let result = Rotation::from_degrees(degrees);

    match result {
        Ok(rotation) => {
            assert!(
                rotation.degrees() % 90 == 0,
                "a successfully constructed Rotation must be a multiple of 90 degrees"
            );
            // rotated_by must never panic, for any composition -- exercise it here too.
            let _ = rotation.rotated_by(Rotation::Quarter);
            let _ = rotation.rotated_by(rotation);
        }
        Err(_) => {
            assert!(degrees % 90 != 0, "from_degrees rejected a multiple of 90: {degrees}");
        }
    }
});
