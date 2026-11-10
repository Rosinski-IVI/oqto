#!/usr/bin/env python3
"""Fail a release build if version declarations or chart metadata diverge."""

from pathlib import Path
import importlib.util
import subprocess
import sys

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib

root = Path(__file__).resolve().parents[2]
cargo = tomllib.loads((root / "backend/Cargo.toml").read_text())
deps = tomllib.loads((root / "dependencies.toml").read_text())
workspace_version = cargo["workspace"]["package"]["version"]
pinned_version = deps["oqto"]["version"]
if workspace_version != pinned_version:
    raise SystemExit(
        f"release version mismatch: Cargo={workspace_version} dependencies.toml={pinned_version}"
    )

pin_spec = importlib.util.spec_from_file_location(
    "sync_platform_pin", root / "scripts/release/sync-platform-pin.py"
)
assert pin_spec and pin_spec.loader
pin_module = importlib.util.module_from_spec(pin_spec)
pin_spec.loader.exec_module(pin_module)
prerelease_pin = pin_module.synchronize('[oqto]\nversion = "0.5.11"\n', "0.5.9-ivi.2")
if 'version = "0.5.9-ivi.2"' not in prerelease_pin:
    raise SystemExit("platform pin helper did not preserve the IVI prerelease version")

preflight = subprocess.run(
    [
        sys.executable,
        "-B",
        str(root / "scripts/release/check-tag-version.py"),
        "--tag",
        f"v{workspace_version}",
    ],
    text=True,
    capture_output=True,
    check=False,
)
if preflight.returncode != 0:
    raise SystemExit(preflight.stderr or preflight.stdout)
print(f"Oqto release version synchronized: {workspace_version}")
