#!/bin/bash
set -euo pipefail

# This script prepares the source package for Launchpad PPA upload.
# It vendors Rust dependencies so Launchpad can build offline.

echo "Preparing source package for Launchpad..."

if [ ! -f "Cargo.toml" ]; then
    echo "Error: Must be run from the project root directory"
    exit 1
fi

# Ensure source-package rules are active
cp debian/control.source debian/control
cp debian/rules.source debian/rules
chmod +x debian/rules

echo "Vendoring Rust dependencies..."
rm -rf vendor .cargo
mkdir -p .cargo
cargo vendor --locked vendor > .cargo/config.toml
python3 debian/sanitize-vendor.py

if ! grep -q '^directory = "vendor"$' .cargo/config.toml; then
    echo "Error: cargo vendor did not generate the expected source replacement config"
    exit 1
fi

echo "Source package preparation complete!"
echo "The package will now build from source on Launchpad"
