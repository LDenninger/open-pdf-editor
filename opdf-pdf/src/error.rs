//! Translation of `lopdf` errors into the workspace error type.

use opdf_core::Error;

/// Convert a `lopdf` error into the workspace error type.
///
/// `lopdf` is an implementation detail of this crate, so no `lopdf::Error` may
/// escape it. I/O failures keep their `std::io::Error` so a caller can still
/// match `Error::Io`. A file this crate could handle if it were intact becomes
/// `Error::Malformed`; an intact file using a feature this crate does not
/// implement becomes `Error::Unsupported`.
pub(crate) fn convert_lopdf_error(error: lopdf::Error) -> Error {
    match error {
        lopdf::Error::IO(io_error) => Error::Io(io_error),
        lopdf::Error::Unimplemented(feature) => Error::Unsupported(feature.to_string()),
        lopdf::Error::InvalidPassword | lopdf::Error::NotEncrypted | lopdf::Error::AlreadyEncrypted => Error::Unsupported(format!("encrypted pdf: {error}")),
        lopdf::Error::UnsupportedSecurityHandler(_) | lopdf::Error::Decryption(_) => Error::Unsupported(format!("encrypted pdf: {error}")),
        other => Error::Malformed(other.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use opdf_core::Error;

    #[test]
    fn maps_io_failures_to_the_io_variant() {
        let io_error = std::io::Error::new(std::io::ErrorKind::NotFound, "missing");
        assert!(matches!(convert_lopdf_error(lopdf::Error::IO(io_error)), Error::Io(_)));
    }

    #[test]
    fn maps_a_damaged_file_to_the_malformed_variant() {
        let converted = convert_lopdf_error(lopdf::Error::Syntax("unexpected token".to_string()));
        assert!(matches!(converted, Error::Malformed(_)), "a syntax error is a damaged file, got: {converted:?}");
    }

    #[test]
    fn maps_a_missing_object_to_the_malformed_variant() {
        assert!(matches!(convert_lopdf_error(lopdf::Error::ObjectNotFound((1, 0))), Error::Malformed(_)));
    }

    #[test]
    fn maps_encryption_to_the_unsupported_variant() {
        assert!(matches!(convert_lopdf_error(lopdf::Error::InvalidPassword), Error::Unsupported(_)));
        assert!(matches!(
            convert_lopdf_error(lopdf::Error::Unimplemented("object streams")),
            Error::Unsupported(_)
        ));
    }

    #[test]
    fn preserves_the_original_message_in_the_malformed_variant() {
        let converted = convert_lopdf_error(lopdf::Error::Syntax("unexpected token".to_string()));
        assert!(
            converted.to_string().contains("unexpected token"),
            "the cause must survive translation, got: {converted}"
        );
    }
}
