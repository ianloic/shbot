#!/usr/bin/env bash
# Copyright (C) 2026 Ian McKellar
#
# This program is free software: you can redistribute it and/or modify
# it under the terms of the GNU General Public License as published by
# the Free Software Foundation, either version 3 of the License, or
# (at your option) any later version.
#
# This program is distributed in the hope that it will be useful,
# but WITHOUT ANY WARRANTY; without even the implied warranty of
# MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
# GNU General Public License for more details.
#
# You should have received a copy of the GNU General Public License
# along with this program.  If not, see <https://www.gnu.org/licenses/>.

set -e

# Check if cargo-llvm-cov is installed
if ! command -v cargo-llvm-cov >/dev/null 2>&1; then
    echo "==> cargo-llvm-cov not found, installing..."
    cargo install cargo-llvm-cov
fi


echo "==> Building instrumented daemon binary..."
# Force build the daemon binary so integration tests can spawn it
cargo llvm-cov run --bin shbot -- --help >/dev/null 2>&1 || true

echo "==> Running test suite with llvm-cov coverage instrumentation..."
# Set SHBOT_BIN_PATH so integration tests know exactly where the instrumented binary is
export SHBOT_BIN_PATH="$(pwd)/target/llvm-cov-target/debug/shbot"

# --workspace runs all tests, including integration tests
# We use --no-clean to prevent llvm-cov from wiping the shbot binary we just built
# We generate an html report
cargo llvm-cov --no-clean --workspace --html

echo "==> Coverage HTML report generated in target/llvm-cov/html/index.html"
echo ""
echo "==> Coverage Summary:"
cargo llvm-cov report
