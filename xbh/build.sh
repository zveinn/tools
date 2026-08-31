#!/bin/bash

# Build script for smallest possible xbh binary

set -e

echo "Building xbh with size optimizations..."

# Build with optimizations
CGO_ENABLED=0 go build \
    -ldflags="-s -w" \
    -trimpath \
    -o xbh

echo "✓ Built successfully"
ls -lh xbh | awk '{print "Binary size:", $5}'

# Check if upx is available for additional compression
if command -v upx &> /dev/null; then
    echo ""
    read -p "UPX is available. Compress binary further? (y/n) " -n 1 -r
    echo
    if [[ $REPLY =~ ^[Yy]$ ]]; then
        echo "Compressing with UPX..."
        upx --best --lzma xbh
        echo "✓ Compressed successfully"
        ls -lh xbh | awk '{print "Final size:", $5}'
    fi
else
    echo ""
    echo "Tip: Install 'upx' for even smaller binaries (50-70% reduction)"
    echo "  Ubuntu/Debian: sudo apt install upx-ucl"
    echo "  Arch: sudo pacman -S upx"
    echo "  macOS: brew install upx"
fi

echo ""
echo "Build complete! Binary: ./xbh"
