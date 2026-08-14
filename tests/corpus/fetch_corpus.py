#!/usr/bin/env python3
"""Download the "fetched" tier of the corpus into a gitignored cache.

Usage:
    python3 tests/corpus/fetch_corpus.py            # fetch everything missing
    python3 tests/corpus/fetch_corpus.py --pin       # also print the sha256 to
                                                      # pin in manifest.toml for
                                                      # any freshly-downloaded
                                                      # file

On first run for a given entry, the manifest's sha256 is "PENDING-first-fetch"
and this script downloads unconditionally, then (with --pin) prints the real
hash to record. On every later run, the download is verified against the pinned
hash and rejected if it no longer matches -- an upstream file changing is a
provenance break, not something to fetch silently over.
"""
import argparse
import hashlib
import sys
import urllib.request
from datetime import date
from pathlib import Path

CORPUS_DIR = Path(__file__).parent
CACHE_DIR = CORPUS_DIR / ".cache"
MANIFEST_PATH = CORPUS_DIR / "manifest.toml"


def sha256_of(path: Path) -> str:
    hasher = hashlib.sha256()
    with path.open("rb") as handle:
        for chunk in iter(lambda: handle.read(1 << 20), b""):
            hasher.update(chunk)
    return hasher.hexdigest()


def parse_fetched_entries() -> list[dict[str, str]]:
    import tomllib

    manifest = tomllib.loads(MANIFEST_PATH.read_text())
    return [entry for entry in manifest["entry"] if entry["tier"] == "fetched"]


def fetch(entry: dict[str, str], pin: bool) -> None:
    CACHE_DIR.mkdir(exist_ok=True)
    destination = CACHE_DIR / entry["file"]
    pending = entry["sha256"] == "PENDING-first-fetch"

    if destination.exists() and not pending:
        actual = sha256_of(destination)
        if actual == entry["sha256"]:
            print(f"ok (cached): {entry['file']}")
            return
        print(f"MISMATCH, re-downloading: {entry['file']}", file=sys.stderr)

    print(f"fetching: {entry['file']} <- {entry['source_url']}")
    urllib.request.urlretrieve(entry["source_url"], destination)
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


def main() -> None:
    parser = argparse.ArgumentParser()
    parser.add_argument("--pin", action="store_true", help="print the sha256/date to pin for any first-time download")
    args = parser.parse_args()

    for entry in parse_fetched_entries():
        fetch(entry, args.pin)


if __name__ == "__main__":
    main()
