#!/usr/bin/env python3
"""Download the "fetched" tier of the corpus into a gitignored cache.

Usage:
    python3 tests/corpus/fetch_corpus.py            # fetch everything missing
    python3 tests/corpus/fetch_corpus.py --strict    # exit non-zero if any
                                                      # specimen is unreachable
    python3 tests/corpus/fetch_corpus.py --pin       # also print the sha256 to
                                                      # pin in manifest.toml for
                                                      # any freshly-downloaded
                                                      # file

Provenance
----------
On first run for a given entry, the manifest's sha256 is "PENDING-first-fetch"
and this script downloads unconditionally, then (with --pin) prints the real
hash to record. On every later run, the download is verified against the pinned
hash and rejected if it no longer matches -- an upstream file changing is a
provenance break, not something to fetch silently over. That check is
unconditional and always fatal.

Availability
------------
Upstream *reachability* is treated differently from upstream *content*. A
specimen host being down, rate-limiting, or routing to a broken mirror is not a
defect in this repository, and taking the whole nightly verification job red for
it hides the results of everything else the job runs. So each entry may list
`mirror_urls` alongside `source_url`; every candidate is tried, with retries,
and only when all of them fail is the specimen skipped.

A skip is never silent. It is reported on stderr, annotated for GitHub Actions,
summarised at the end of the run, and -- because the cached file simply is not
there -- the round-trip test that consumes the fetched tier reports the specimen
as uncovered too. Pass --strict to turn an unreachable specimen back into a
failure, which is what a release check should do.
"""
import argparse
import hashlib
import os
import sys
import time
import urllib.error
import urllib.request
from datetime import date
from pathlib import Path

CORPUS_DIR = Path(__file__).parent
CACHE_DIR = CORPUS_DIR / ".cache"
MANIFEST_PATH = CORPUS_DIR / "manifest.toml"

#--- retry policy: enough to ride out a single flaky mirror or a brief 5xx,
#--- short enough that an entirely dead host does not stall the job ---
ATTEMPTS_PER_URL = 3
BACKOFF_SECONDS = 5
TIMEOUT_SECONDS = 600


#---------------------------------------------------------------------
# Reporting
#---------------------------------------------------------------------


def warn(message: str) -> None:
    """Print a warning, annotated so GitHub Actions surfaces it on the run."""
    print(f"WARNING: {message}", file=sys.stderr)
    if os.environ.get("GITHUB_ACTIONS") == "true":
        print(f"::warning title=Corpus specimen unavailable::{message}")


#---------------------------------------------------------------------
# Hashing
#---------------------------------------------------------------------


def sha256_of(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


#---------------------------------------------------------------------
# Manifest
#---------------------------------------------------------------------


def parse_fetched_entries() -> list[dict]:
    import tomllib

    manifest = tomllib.loads(MANIFEST_PATH.read_text())
    return [entry for entry in manifest["entry"] if entry["tier"] == "fetched"]


def candidate_urls(entry: dict) -> list[str]:
    """Every URL to try for an entry, canonical source first."""
    urls = [entry["source_url"]]
    urls.extend(url for url in entry.get("mirror_urls", []) if url not in urls)
    return urls


#---------------------------------------------------------------------
# Downloading
#---------------------------------------------------------------------


def download_to(url: str, destination: Path) -> None:
    """Fetch `url` into `destination`, via a .part file so a failed or
    truncated transfer never leaves a half-file the next run would trust."""
    partial = destination.with_suffix(destination.suffix + ".part")
    try:
        with urllib.request.urlopen(url, timeout=TIMEOUT_SECONDS) as response, partial.open("wb") as handle:
            while chunk := response.read(1 << 20):
                handle.write(chunk)
        partial.replace(destination)
    finally:
        partial.unlink(missing_ok=True)


def try_download(entry: dict, destination: Path) -> bool:
    """Try every candidate URL in turn. Return True once one succeeds."""
    for url in candidate_urls(entry):
        for attempt in range(1, ATTEMPTS_PER_URL + 1):
            print(f"fetching: {entry['file']} <- {url} (attempt {attempt}/{ATTEMPTS_PER_URL})")
            try:
                download_to(url, destination)
                return True
            except (urllib.error.URLError, TimeoutError, ConnectionError) as failure:
                print(f"  failed: {failure}", file=sys.stderr)
                if attempt < ATTEMPTS_PER_URL:
                    time.sleep(BACKOFF_SECONDS * attempt)
    return False


def fetch(entry: dict, pin: bool) -> bool:
    """Ensure one entry is present and correct in the cache.

    Returns True if the specimen is available afterwards, False if every
    source for it was unreachable. Raises only on a provenance break.
    """
    CACHE_DIR.mkdir(exist_ok=True)
    destination = CACHE_DIR / entry["file"]
    pending = entry["sha256"] == "PENDING-first-fetch"

    if destination.exists() and not pending:
        actual = sha256_of(destination)
        if actual == entry["sha256"]:
            print(f"ok (cached): {entry['file']}")
            return True
        print(f"MISMATCH, re-downloading: {entry['file']}", file=sys.stderr)

    if not try_download(entry, destination):
        warn(
            f"{entry['file']}: every source was unreachable "
            f"({len(candidate_urls(entry))} tried) -- this specimen is NOT covered by this run"
        )
        return False

    actual = sha256_of(destination)

    if not pending and actual != entry["sha256"]:
        destination.unlink()
        raise RuntimeError(
            f"{entry['file']}: downloaded bytes hash to {actual}, "
            f"manifest pins {entry['sha256']} -- upstream file changed, provenance is broken"
        )

    print(f"ok: {entry['file']} sha256={actual}")
    if pin and pending:
        print(f'  -> paste into manifest.toml: sha256 = "{actual}", fetched_on = "{date.today().isoformat()}"')
    return True


#---------------------------------------------------------------------
# Entry point
#---------------------------------------------------------------------


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pin", action="store_true", help="print the sha256/date to pin for any first-time download")
    parser.add_argument("--strict", action="store_true", help="exit non-zero if any specimen could not be fetched")
    args = parser.parse_args()

    entries = parse_fetched_entries()
    unavailable = [entry["file"] for entry in entries if not fetch(entry, args.pin)]

    print(f"\nfetched tier: {len(entries) - len(unavailable)}/{len(entries)} specimens available")
    if not unavailable:
        return

    print(f"unavailable: {', '.join(unavailable)}", file=sys.stderr)
    if args.strict:
        sys.exit(f"--strict: {len(unavailable)} specimen(s) could not be fetched")


if __name__ == "__main__":
    main()
