#!/usr/bin/env python3
"""Derive the synthetic corpus fixtures listed in manifest.toml.

Every file this script produces is either a byte-level derivative of
irs_f1040.pdf (public domain, so any derivative is too) or hand-authored from
scratch with no source document. Run from the repository root:

    python3 tests/corpus/generate_synthetic.py

It writes the files into tests/corpus/files/ and prints a ready-to-paste
TOML snippet with the real sha256 of each, to append to manifest.toml.
"""
import hashlib
import re
import subprocess
from pathlib import Path

CORPUS_DIR = Path(__file__).parent
FILES_DIR = CORPUS_DIR / "files"
SOURCE = FILES_DIR / "irs_f1040.pdf"


def build_pdf(objects: list[tuple[int, bytes]]) -> bytes:
    """Assemble a minimal, valid PDF from (object_number, body) pairs.

    Object numbers must be 1..=len(objects), object 1 conventionally the
    catalog. Computes every xref offset itself rather than hand-tracking
    them, so the output is correct regardless of how large the bodies are.
    """
    body = bytearray(b"%PDF-1.7\n%\xe2\xe3\xcf\xd3\n")
    offsets = [0] * (len(objects) + 1)
    for obj_num, content in objects:
        offsets[obj_num] = len(body)
        body += f"{obj_num} 0 obj\n".encode()
        body += content
        body += b"\nendobj\n"
    xref_offset = len(body)
    body += f"xref\n0 {len(objects) + 1}\n".encode()
    body += b"0000000000 65535 f \n"
    for obj_num, _content in objects:
        body += f"{offsets[obj_num]:010d} 00000 n \n".encode()
    body += b"trailer\n"
    body += f"<< /Size {len(objects) + 1} /Root 1 0 R >>\n".encode()
    body += b"startxref\n"
    body += f"{xref_offset}\n".encode()
    body += b"%%EOF"
    return bytes(body)


def sha256_of(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


def generate_truncated() -> Path:
    """Cut irs_f1040.pdf off mid-file: no trailer, no xref, no %%EOF.

    open() must reject this with an error, never panic.
    """
    data = SOURCE.read_bytes()
    truncated = data[:10_000]
    out_path = FILES_DIR / "irs_f1040_truncated_10k.pdf"
    out_path.write_bytes(truncated)
    return out_path


def generate_damaged_xref() -> Path:
    """Corrupt irs_f1040.pdf's cross-reference table in place.

    Locates the byte offset the trailer's `startxref` points at and flips
    every bit in the first 20 bytes there, leaving every object body intact.
    A real-world parser that only trusts the xref table fails outright here;
    one that falls back to a linear object scan (as pdf.js and other
    production tools do) recovers. Track A decides how far to go -- Track E
    only asserts open() does not panic.
    """
    data = bytearray(SOURCE.read_bytes())
    matches = list(re.finditer(rb"startxref\s+(\d+)", bytes(data)))
    if not matches:
        raise RuntimeError("irs_f1040.pdf has no startxref keyword to corrupt")
    offset = int(matches[-1].group(1))
    for index in range(offset, min(offset + 20, len(data))):
        data[index] ^= 0xFF
    out_path = FILES_DIR / "damaged_xref.pdf"
    out_path.write_bytes(bytes(data))
    return out_path


def generate_object_streams() -> Path:
    """Re-save irs_f1040.pdf with compressed cross-reference and object
    streams (PDF 1.5+), via the system qpdf binary.

    Deterministic given a fixed qpdf version. This is the file category the
    "Known gaps" review in contracts.md implicitly assumes Track A must
    parse -- most PDFs produced by modern tooling use object streams.
    """
    out_path = FILES_DIR / "irs_f1040_object_streams.pdf"
    subprocess.run(
        ["qpdf", "--object-streams=generate", str(SOURCE), str(out_path)],
        check=True,
    )
    return out_path


def generate_huge_page_count(page_count: int = 5000) -> Path:
    """A minimal PDF with 5000 blank US Letter pages.

    Entirely synthetic, built with build_pdf() -- no source document, no
    per-page content stream, so the file stays small despite the page count.
    Exercises page-count scaling independently of file-size scaling.
    """
    kids = " ".join(f"{3 + index} 0 R" for index in range(page_count))
    objects: list[tuple[int, bytes]] = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, f"<< /Type /Pages /Kids [{kids}] /Count {page_count} >>".encode()),
    ]
    for index in range(page_count):
        objects.append((3 + index, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>"))
    out_path = FILES_DIR / "huge_page_count.pdf"
    out_path.write_bytes(build_pdf(objects))
    return out_path


def generate_custom_encoding() -> Path:
    """A minimal one-page PDF whose font remaps codes 65/66 ('A'/'B') to
    /copyright and /registered via a /Differences encoding array.

    Hand-authored, no source document -- exercises non-standard font
    encodings, one of the format's genuinely difficult corners.
    """
    stream_content = b"BT /F1 24 Tf 20 100 Td (AB) Tj ET"
    stream_obj = b"<< /Length %d >>\nstream\n" % len(stream_content) + stream_content + b"\nendstream"
    objects: list[tuple[int, bytes]] = [
        (1, b"<< /Type /Catalog /Pages 2 0 R >>"),
        (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>"),
        (3, b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>"),
        (4, b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding << /Type /Encoding /Differences [65 /copyright 66 /registered] >> >>"),
        (5, stream_obj),
    ]
    out_path = FILES_DIR / "custom_encoding.pdf"
    out_path.write_bytes(build_pdf(objects))
    return out_path


def main() -> None:
    generators = [
        generate_truncated,
        generate_damaged_xref,
        generate_object_streams,
        generate_huge_page_count,
        generate_custom_encoding,
    ]
    print("# Paste these entries into tests/corpus/manifest.toml\n")
    for generate in generators:
        out_path = generate()
        print(f'[[entry]]\nfile = "{out_path.name}"\nsha256 = "{sha256_of(out_path)}"\n')


if __name__ == "__main__":
    main()
