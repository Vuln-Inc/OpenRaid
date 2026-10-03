#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

if command -v cargo >/dev/null 2>&1; then
    cargo_bin=cargo
elif [[ -x "${HOME:-}/.cargo/bin/cargo" ]]; then
    cargo_bin="${HOME}/.cargo/bin/cargo"
else
    printf '%s\n' 'Rust is required. Install it from https://rustup.rs and run this script again.' >&2
    exit 1
fi

"$cargo_bin" build --release --locked
target_dir="${CARGO_TARGET_DIR:-target}"
if [[ "$target_dir" != /* ]]; then
    target_dir="$PWD/$target_dir"
fi
printf '\nBuilt OpenRaid: %s/release/openraid\n' "$target_dir"
