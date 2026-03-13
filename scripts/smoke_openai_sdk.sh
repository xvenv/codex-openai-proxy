#!/usr/bin/env bash
set -euo pipefail

BASE_URL="${1:-http://127.0.0.1:8080}"
VENV_DIR="${OPENAI_SDK_SMOKE_VENV:-.venv-openai-sdk}"

python3 -m venv "${VENV_DIR}"
"${VENV_DIR}/bin/python" -m pip install --quiet --upgrade pip
"${VENV_DIR}/bin/python" -m pip install --quiet openai
"${VENV_DIR}/bin/python" scripts/openai_sdk_smoke.py --base-url "${BASE_URL}"
