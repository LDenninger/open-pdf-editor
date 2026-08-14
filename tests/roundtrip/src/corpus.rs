//! Loading and validating the corpus provenance manifest at
//! `tests/corpus/manifest.toml`.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use sha2::{Digest, Sha256};

/// One row of `tests/corpus/manifest.toml`, describing a single PDF fixture.
#[derive(Clone, Deserialize, Debug)]
pub struct CorpusEntry {
    /// Bare file name inside `tests/corpus/files/` (`tier = "checked-in"`)
    /// or the name written into `tests/corpus/.cache/` by
    /// `fetch_corpus.py` (`tier = "fetched"`).
    pub file: String,
    /// Lowercase hex SHA-256 of the exact bytes this entry describes.
    pub sha256: String,
    /// Where the file came from: a URL, or the literal string
    /// `"generated"` for a self-authored synthetic fixture.
    pub source_url: String,
    /// The licence covering redistribution of this specific file.
    pub license: String,
    /// Rights holder or author, cross-referenced in `CREDITS.md` when the
    /// licence requires attribution.
    pub attribution: String,
    /// ISO 8601 date the file was fetched or generated.
    pub fetched_on: String,
    /// `"checked-in"` (always present in git) or `"fetched"` (downloaded on
    /// demand, not stored in git).
    pub tier: String,
    /// Free-form category labels, e.g. `["cjk", "acroform"]`, used to select
    /// subsets of the corpus in tests.
    pub tags: Vec<String>,
    /// Why this specimen is in the corpus and what it exercises.
    pub notes: String,
}

/// The parsed manifest: every entry in `tests/corpus/manifest.toml`.
#[derive(Clone, Deserialize, Debug, Default)]
pub struct CorpusManifest {
    /// One row per corpus file.
    #[serde(rename = "entry")]
    pub entries: Vec<CorpusEntry>,
}

/// Error loading or validating the manifest.
#[derive(Debug, thiserror::Error)]
pub enum CorpusError {
    /// The manifest file could not be read from disk.
    #[error("failed to read manifest at {path}: {source}")]
    Io {
        /// The path that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
    },
    /// The manifest is not valid TOML for [`CorpusManifest`].
    #[error("failed to parse manifest: {0}")]
    Parse(#[from] toml::de::Error),
    /// A checked-in entry's bytes do not match the sha256 recorded for it.
    #[error("{file}: manifest records sha256 {expected}, actual file hashes to {actual}")]
    HashMismatch {
        /// The offending file name.
        file: String,
        /// The hash recorded in the manifest.
        expected: String,
        /// The hash actually computed from the file on disk.
        actual: String,
    },
    /// A checked-in entry has no corresponding file on disk.
    #[error("{file}: listed with tier \"checked-in\" but not found at {path}")]
    Missing {
        /// The offending file name.
        file: String,
        /// Where it was expected to be.
        path: PathBuf,
    },
}

impl CorpusManifest {
    /// Read and parse a manifest file.
    pub fn load(manifest_path: &Path) -> Result<Self, CorpusError> {
        let raw = std::fs::read_to_string(manifest_path).map_err(|source| CorpusError::Io {
            path: manifest_path.to_path_buf(),
            source,
        })?;
        Ok(toml::from_str(&raw)?)
    }

    /// Entries tagged `"checked-in"` — always present, usable in the default CI gate.
    pub fn checked_in(&self) -> impl Iterator<Item = &CorpusEntry> {
        self.entries.iter().filter(|entry| entry.tier == "checked-in")
    }

    /// Entries tagged `"fetched"` — large files, present only after
    /// `fetch_corpus.py` has run.
    pub fn fetched(&self) -> impl Iterator<Item = &CorpusEntry> {
        self.entries.iter().filter(|entry| entry.tier == "fetched")
    }

    /// Verify every checked-in file exists under `files_dir` and hashes to
    /// the value recorded for it.
    pub fn verify_checked_in(&self, files_dir: &Path) -> Result<(), CorpusError> {
        for entry in self.checked_in() {
            let path = files_dir.join(&entry.file);
            let bytes = std::fs::read(&path).map_err(|_source| CorpusError::Missing {
                file: entry.file.clone(),
                path: path.clone(),
            })?;
            let actual = hex_sha256(&bytes);
            if actual != entry.sha256 {
                return Err(CorpusError::HashMismatch {
                    file: entry.file.clone(),
                    expected: entry.sha256.clone(),
                    actual,
                });
            }
        }
        Ok(())
    }
}

fn hex_sha256(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest_path() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/manifest.toml")
    }

    fn files_dir() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/files")
    }

    #[test]
    fn every_checked_in_file_matches_its_recorded_hash() {
        let manifest = CorpusManifest::load(&manifest_path()).unwrap();
        manifest.verify_checked_in(&files_dir()).unwrap();
    }

    #[test]
    fn every_entry_declares_a_license_and_attribution() {
        let manifest = CorpusManifest::load(&manifest_path()).unwrap();
        for entry in &manifest.entries {
            assert!(!entry.license.trim().is_empty(), "{} has no license recorded", entry.file);
            assert!(!entry.attribution.trim().is_empty(), "{} has no attribution recorded", entry.file);
        }
    }

    #[test]
    fn every_manifest_entry_declares_a_known_tier() {
        let manifest = CorpusManifest::load(&manifest_path()).unwrap();
        for entry in &manifest.entries {
            assert!(
                entry.tier == "checked-in" || entry.tier == "fetched",
                "{} has unknown tier {:?}",
                entry.file,
                entry.tier
            );
        }
    }
}
