#!/usr/bin/env bash
set -euo pipefail

cd -- "$(dirname -- "${BASH_SOURCE[0]}")"

if ! command -v cargo >/dev/null 2>&1; then
    if [[ -x "${HOME:-}/.cargo/bin/cargo" ]]; then
        export PATH="${HOME}/.cargo/bin:$PATH"
    else
        printf '%s\n' 'Rust is required. Install it from https://rustup.rs and run this script again.' >&2
        exit 1
    fi
fi
if ! command -v node >/dev/null 2>&1 || ! command -v npm >/dev/null 2>&1; then
    printf '%s\n' 'The optional desktop build requires Node.js 22.12+ and npm.' >&2
    exit 1
fi
node -e 'const [major, minor] = process.versions.node.split(".").map(Number); if (major < 22 || (major === 22 && minor < 12)) { console.error("The optional desktop build requires Node.js 22.12+."); process.exit(1); }'
node -e 'const result = require("node:child_process").spawnSync("rustc", ["--version"], {encoding:"utf8"}); const version = /rustc (\d+)\.(\d+)/.exec(result.stdout || ""); if (result.status !== 0 || !version || Number(version[1]) < 1 || (Number(version[1]) === 1 && Number(version[2]) < 90)) { console.error("The optional desktop build requires Rust 1.90+. Update your active toolchain with rustup update stable."); process.exit(1); }'

# Tauri resolves a relative Cargo target directory from the desktop crate.
# Normalize it here so the output location is unambiguous for callers.
if [[ -n "${CARGO_TARGET_DIR:-}" && "$CARGO_TARGET_DIR" != /* ]]; then
    export CARGO_TARGET_DIR="$PWD/$CARGO_TARGET_DIR"
fi
npm ci --prefix desktop
npm run tauri --prefix desktop -- build --no-bundle -- --locked
target_dir="${CARGO_TARGET_DIR:-$PWD/desktop/src-tauri/target}"
printf '\nBuilt OpenRaid desktop: %s/release/openraid-desktop\n' "$target_dir"
