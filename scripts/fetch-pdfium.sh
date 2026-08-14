#!/usr/bin/env bash
# Download the prebuilt Pdfium shared library that opdf-render binds to at runtime.
#
# Pdfium is BSD-3-Clause licensed and is not committed to this repository. The
# release below must match the pdfium_* API feature selected on pdfium-render in
# opdf-render/Cargo.toml.
set -euo pipefail

PDFIUM_RELEASE="chromium/7881"
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
VENDOR_DIR="${REPO_ROOT}/vendor/pdfium"

case "$(uname -s)-$(uname -m)" in
    Linux-x86_64)  ASSET="pdfium-linux-x64.tgz" ;;
    Linux-aarch64) ASSET="pdfium-linux-arm64.tgz" ;;
    Darwin-arm64)  ASSET="pdfium-mac-arm64.tgz" ;;
    Darwin-x86_64) ASSET="pdfium-mac-x64.tgz" ;;
    *) echo "unsupported platform: $(uname -s)-$(uname -m)" >&2; exit 1 ;;
esac

if [ -f "${VENDOR_DIR}/VERSION" ] && grep -q "BUILD=${PDFIUM_RELEASE##*/}" "${VENDOR_DIR}/VERSION"; then
    echo "pdfium ${PDFIUM_RELEASE} already present in ${VENDOR_DIR}"
    exit 0
fi

URL="https://github.com/bblanchon/pdfium-binaries/releases/download/${PDFIUM_RELEASE//\//%2F}/${ASSET}"
echo "downloading ${URL}"
mkdir -p "${VENDOR_DIR}"
curl -sSfL "${URL}" | tar -xz -C "${VENDOR_DIR}"

if [ ! -f "${VENDOR_DIR}/lib/libpdfium.so" ] && [ ! -f "${VENDOR_DIR}/lib/libpdfium.dylib" ]; then
    echo "fetch succeeded but no shared library landed in ${VENDOR_DIR}/lib" >&2
    exit 1
fi

echo "pdfium ${PDFIUM_RELEASE} installed in ${VENDOR_DIR}"
