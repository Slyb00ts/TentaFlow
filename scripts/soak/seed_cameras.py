#!/usr/bin/env python3
# ============================================================================
# File: scripts/soak/seed_cameras.py — install 4 FakeFile cameras for soak run
# ============================================================================
#
# Status: PLACEHOLDER. The dashboard uses a custom binary WebSocket protocol
# (see tentaflow-core/www/js/protocol/binary-ws-client.js + codec.js + wasm_glue).
# Implementing a full Python client of that protocol is out of scope for the
# soak infra task. For a real 24h acceptance run, seed cameras manually:
#
#   1) Open https://127.0.0.1:18099 in a browser
#   2) Login as admin (credentials from tests/e2e/config-soak.toml)
#   3) TentaVision -> Cameras -> "Add camera" four times with profiles below
#
# Camera profiles (matches tentavision-f1a §17.9 soak workload):
#
#   | # | Name        | Source                          | Resolution  | FPS |
#   |---|-------------|---------------------------------|-------------|-----|
#   | 1 | cam-low     | FakeFile sample_traffic.mp4     |  320x240    |  5  |
#   | 2 | cam-medium  | FakeFile sample_traffic.mp4     |  640x480    | 15  |
#   | 3 | cam-hd      | FakeFile sample_traffic.mp4     | 1280x720    | 30  |
#   | 4 | cam-fhd     | FakeFile sample_traffic.mp4     | 1920x1080   | 30  |
#
# Sample file location (regenerable via M1.W6 procedure):
#   tentaflow-core/assets/test/sample_traffic.mp4
#
# This script currently:
#   - validates that the config file exists and is parseable
#   - writes a short note to --output explaining manual seeding
#   - returns 0 (non-fatal) so run_soak.sh continues
#
# When a Python binary-WS client is available, replace the main() body with
# real seeding logic (login -> install_camera x4 -> verify via list_cameras).

from __future__ import annotations

import argparse
import sys
from pathlib import Path


def main() -> int:
    parser = argparse.ArgumentParser(description="Seed FakeFile cameras for soak run")
    parser.add_argument("--config", required=True, help="Path to tentaflow config TOML")
    parser.add_argument("--output", required=True, help="Where to write a seeding note")
    args = parser.parse_args()

    config_path = Path(args.config)
    if not config_path.is_file():
        print(f"ERROR: config not found: {config_path}", file=sys.stderr)
        return 1

    note = (
        "seed_cameras.py: PLACEHOLDER mode.\n"
        "Binary WebSocket protocol Python client not implemented.\n"
        "Action required: seed 4 FakeFile cameras manually via dashboard.\n"
        "See docs/SOAK_TEST.md section 'Seeding cameras' for the procedure.\n"
    )
    out_path = Path(args.output)
    out_path.parent.mkdir(parents=True, exist_ok=True)
    out_path.write_text(note, encoding="utf-8")
    print(note, end="")
    return 0


if __name__ == "__main__":
    sys.exit(main())
