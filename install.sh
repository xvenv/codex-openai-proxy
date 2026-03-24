#!/usr/bin/env sh

set -eu

REPO_DIR=$(CDPATH= cd -- "$(dirname -- "$0")" && pwd)
BIN_DIR="${HOME}/.local/bin"
INSTALL_PATH="${BIN_DIR}/codex-openai-proxy"
CONFIG_DIR="${HOME}/.config/codex-proxy"
CONFIG_PATH="${CONFIG_DIR}/config.json"
EXAMPLE_CONFIG="${REPO_DIR}/config/example.config.json"

if ! command -v cargo >/dev/null 2>&1; then
  echo "error: cargo is not installed or not in PATH" >&2
  exit 1
fi

echo "==> building release binary"
cargo build --release --manifest-path "${REPO_DIR}/Cargo.toml"

echo "==> installing binary to ${INSTALL_PATH}"
mkdir -p "${BIN_DIR}"
cp "${REPO_DIR}/target/release/codex-openai-proxy" "${INSTALL_PATH}"
chmod 755 "${INSTALL_PATH}"

mkdir -p "${CONFIG_DIR}"

if [ ! -f "${CONFIG_PATH}" ]; then
  echo "==> creating default config at ${CONFIG_PATH}"
  cp "${EXAMPLE_CONFIG}" "${CONFIG_PATH}"
else
  echo "==> keeping existing config at ${CONFIG_PATH}"
fi

cat <<EOF

Install complete.

Binary:
  ${INSTALL_PATH}

Config:
  ${CONFIG_PATH}

Next:
  1. Review auth_path and port in ${CONFIG_PATH}
  2. Ensure ${BIN_DIR} is in your PATH
  3. Run: codex-openai-proxy
EOF
