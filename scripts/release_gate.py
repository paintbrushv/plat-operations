"""Validate candidate/tag versions and refuse an already published release."""

import argparse
import ast
import json
import urllib.error
import urllib.request
from pathlib import Path

import tomllib

ROOT = Path(__file__).resolve().parents[1]
RUNTIME = {
    "platworks": ("src/platworks/__init__.py", "__version__"),
    "plat-agent": ("src/plat_agent/__init__.py", "__version__"),
    "plat-harness": ("harness/src/plat_harness/__init__.py", "__version__"),
    "plat-costmodel": ("src/plat_costmodel/__init__.py", "__version__"),
    "plat-multifamily-underwriting": ("engine/version.py", "ENGINE_VERSION"),
}


def verify(tag=None, registry=False):
    project = tomllib.loads((ROOT / "pyproject.toml").read_text())["project"]
    name, version = project["name"], project["version"]
    if tag is not None and tag != f"v{version}":
        raise ValueError("Release tag must exactly match the package version")
    if name in RUNTIME:
        filename, constant = RUNTIME[name]
        tree = ast.parse((ROOT / filename).read_text())
        values = [
            ast.literal_eval(n.value)
            for n in tree.body
            if isinstance(n, ast.Assign)
            and any(isinstance(t, ast.Name) and t.id == constant for t in n.targets)
        ]
        if values != [version]:
            raise ValueError("Runtime and package versions differ")
    if name == "plat-operations":
        cargo = tomllib.loads((ROOT / "boxscore/Cargo.toml").read_text())["package"]
        if cargo["version"] != version or cargo["license"] != "Apache-2.0":
            raise ValueError("Cargo version/license differs from Python metadata")
    if registry:
        try:
            with urllib.request.urlopen(
                f"https://pypi.org/pypi/{name}/{version}/json", timeout=30
            ) as response:
                if response.status == 200:
                    raise ValueError(
                        "This version is already published; choose an unused version"
                    )
        except urllib.error.HTTPError as error:
            if error.code != 404:
                raise
    return {
        "name": name,
        "version": version,
        "status": "passed",
        "tag_verified": tag is not None,
        "registry_checked": registry,
    }


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("--tag")
    parser.add_argument("--check-registry", action="store_true")
    args = parser.parse_args()
    print(json.dumps(verify(args.tag, args.check_registry), indent=2))
