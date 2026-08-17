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
    /// Alternate URLs serving the same bytes, tried in order by
    /// `fetch_corpus.py` when `source_url` is unreachable.
    ///
    /// Empty for a `"generated"` entry and for any source with no known
    /// mirror. Present because a host being briefly unroutable is not a
    /// provenance change: the pinned `sha256` still governs, whichever URL
    /// the bytes arrived from.
    #[serde(default)]
    pub mirror_urls: Vec<String>,
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
    /// A file is present in the corpus directory that no manifest entry
    /// describes.
    ///
    /// The manifest is the provenance record for every byte the project
    /// redistributes, so an unlisted file is either a licence obligation
    /// nobody recorded or a stray artifact a test wrote into a tracked
    /// directory. Both are defects, and neither is visible to a check that
    /// only walks the manifest.
    #[error("{dir} holds {} file(s) described by no manifest entry -- every corpus file needs a licence and an attribution: {}", files.len(), files.join(", "))]
    Unmanifested {
        /// Every offending file name, sorted.
        files: Vec<String>,
        /// The directory they were found in.
        dir: PathBuf,
    },
    /// The corpus directory itself could not be listed.
    #[error("failed to list the corpus directory {path}: {source}")]
    ReadDir {
        /// The directory that could not be read.
        path: PathBuf,
        /// The underlying I/O failure.
        source: std::io::Error,
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

    /// Verify the manifest and `files_dir` describe exactly the same set of
    /// files, and that each one hashes to the value recorded for it.
    ///
    /// The check runs in both directions deliberately. Walking only the
    /// manifest proves that everything promised is present and unmodified, but
    /// says nothing about a file that is present and *not* promised — an
    /// undeclared specimen with no recorded licence, or a scratch file some
    /// test wrote into this tracked directory and failed to clean up on a
    /// panicking path. Both used to pass.
    pub fn verify_checked_in(&self, files_dir: &Path) -> Result<(), CorpusError> {
        for entry in self.checked_in() {
            let path = files_dir.join(&entry.file);
            let bytes = std::fs::read(&path).map_err(|_source| CorpusError::Missing {
                file: entry.file.clone(),
                path: path.clone(),
            })?;
            let actual = sha256_hex(&bytes);
            if actual != entry.sha256 {
                return Err(CorpusError::HashMismatch {
                    file: entry.file.clone(),
                    expected: entry.sha256.clone(),
                    actual,
                });
            }
        }

        //--- and the other direction: nothing on disk that the manifest does
        //--- not account for ---
        let unmanifested = self.unmanifested_files(files_dir)?;
        if !unmanifested.is_empty() {
            return Err(CorpusError::Unmanifested {
                files: unmanifested,
                dir: files_dir.to_path_buf(),
            });
        }
        Ok(())
    }

    /// File names present in `files_dir` that no checked-in manifest entry
    /// names, sorted so the report is deterministic.
    pub fn unmanifested_files(&self, files_dir: &Path) -> Result<Vec<String>, CorpusError> {
        let listing = std::fs::read_dir(files_dir).map_err(|source| CorpusError::ReadDir {
            path: files_dir.to_path_buf(),
            source,
        })?;

        let mut unmanifested = Vec::new();
        for item in listing {
            let item = item.map_err(|source| CorpusError::ReadDir {
                path: files_dir.to_path_buf(),
                source,
            })?;
            //--- directories are not corpus specimens; only regular files
            //--- carry bytes this project redistributes ---
            if !item.path().is_file() {
                continue;
            }
            let file_name = item.file_name().to_string_lossy().into_owned();
            if !self.checked_in().any(|entry| entry.file == file_name) {
                unmanifested.push(file_name);
            }
        }
        unmanifested.sort();
        Ok(unmanifested)
    }
}

/// Lowercase hex SHA-256 of `bytes`, in the exact form `manifest.toml` records.
///
/// Public because the `fetched` tier is not in git and so cannot be checked by
/// [`CorpusManifest::verify_checked_in`]: the integration test that consumes
/// it has to verify those bytes itself, and must do so the same way.
pub fn sha256_hex(bytes: &[u8]) -> String {
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
    fn the_corpus_directory_holds_nothing_the_manifest_does_not_describe() {
        let manifest = CorpusManifest::load(&manifest_path()).unwrap();
        let unmanifested = manifest.unmanifested_files(&files_dir()).unwrap();
        assert!(
            unmanifested.is_empty(),
            "tests/corpus/files/ contains {} file(s) with no manifest entry, so no recorded licence or attribution: {}",
            unmanifested.len(),
            unmanifested.join(", ")
        );
    }

    /// The reverse direction has to be shown to actually fail, not merely to
    /// pass on a clean tree — a check that never fires is indistinguishable
    /// from the one-directional check it replaced.
    #[test]
    fn an_unmanifested_file_is_rejected() {
        let manifest = CorpusManifest::load(&manifest_path()).unwrap();
        let scratch = tempfile::tempdir().unwrap();

        //--- a directory holding exactly the manifest's files verifies clean ---
        for entry in manifest.checked_in() {
            std::fs::copy(files_dir().join(&entry.file), scratch.path().join(&entry.file)).unwrap();
        }
        manifest.verify_checked_in(scratch.path()).unwrap();

        //--- add one file nobody declared, and it must not ---
        std::fs::write(scratch.path().join("stray_roundtrip_output.pdf"), b"%PDF-1.7\n").unwrap();
        let failure = manifest.verify_checked_in(scratch.path()).unwrap_err();
        assert!(
            matches!(&failure, CorpusError::Unmanifested { files, .. } if files == &["stray_roundtrip_output.pdf".to_string()]),
            "expected an Unmanifested error naming the stray file, got: {failure}"
        );
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
