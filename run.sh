#!/usr/bin/env bash
set -euo pipefail

project_dir="$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)"

if command -v cargo >/dev/null 2>&1; then
    cargo_bin=cargo
elif [[ -x "${HOME:-}/.cargo/bin/cargo" ]]; then
    cargo_bin="${HOME}/.cargo/bin/cargo"
elif [[ -x "${HOME:-}/.cargo/bin/cargo.exe" ]]; then
    cargo_bin="${HOME}/.cargo/bin/cargo.exe"
else
    printf '%s\n' 'Rust is required. Install it from https://rustup.rs and run this script again.' >&2
    exit 1
fi

if [[ $# -eq 0 ]]; then
    set -- setup
fi

exec "$cargo_bin" run --manifest-path "$project_dir/Cargo.toml" --release --locked -- "$@"
