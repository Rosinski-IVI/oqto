#!/usr/bin/env python3
"""Fail a release before packaging if tag, Cargo, frontend, or chart versions diverge."""

import argparse
import json
from pathlib import Path
import re

try:
    import tomllib
except ModuleNotFoundError:
    import tomli as tomllib


def chart_scalar(text: str, key: str) -> str:
    match = re.search(
        rf"(?m)^{re.escape(key)}:\s*(?:\"([^\"]*)\"|'([^']*)'|([^#\s]+))\s*(?:#.*)?$",
        text,
    )
    if not match:
        raise ValueError(f"Helm Chart.yaml is missing {key}")
    return next(value for value in match.groups() if value is not None)


def check(
    tag: str,
    cargo_path: Path,
    deps_path: Path,
    package_path: Path,
    chart_path: Path,
) -> None:
    if not re.fullmatch(r"v(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)\.(?:0|[1-9][0-9]*)(?:-[0-9A-Za-z.-]+)?", tag):
        raise ValueError("release requires an explicit v<semver> tag")
    cargo = tomllib.loads(cargo_path.read_text())
    deps = tomllib.loads(deps_path.read_text())
    declared = cargo["workspace"]["package"]["version"]
    pinned = deps["oqto"]["version"]
    frontend = json.loads(package_path.read_text())["version"]
    chart_text = chart_path.read_text()
    chart_version = chart_scalar(chart_text, "version")
    app_version = chart_scalar(chart_text, "appVersion")
    if any(
        version != tag[1:]
        for version in (declared, pinned, frontend, chart_version, app_version)
    ):
        raise ValueError(
            f"release tag {tag} does not match Cargo ({declared}), "
            f"dependency pin ({pinned}), frontend package ({frontend}), "
            f"Helm chart ({chart_version}) and appVersion ({app_version})"
        )


def main() -> None:
    root = Path(__file__).resolve().parents[2]
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--tag", required=True, help="Explicit release tag, e.g. v0.5.0")
    parser.add_argument("--cargo", type=Path, default=root / "backend/Cargo.toml")
    parser.add_argument("--dependencies", type=Path, default=root / "dependencies.toml")
    parser.add_argument("--package", type=Path, default=root / "frontend/package.json")
    parser.add_argument("--chart", type=Path, default=root / "deploy/helm/oqto/Chart.yaml")
    args = parser.parse_args()
    try:
        check(args.tag, args.cargo, args.dependencies, args.package, args.chart)
    except (ValueError, KeyError, OSError, tomllib.TOMLDecodeError) as error:
        parser.exit(1, f"release preflight failed: {error}\n")
    print(f"release tag and declared versions agree: {args.tag}")


if __name__ == "__main__":
    main()
