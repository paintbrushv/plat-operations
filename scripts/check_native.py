"""Install a native wheel in a fresh environment and test it outside its source tree."""

import argparse
import hashlib
import json
import os
import sqlite3
import subprocess
import sys
import tempfile
import venv
from pathlib import Path


def verify(wheel):
    with tempfile.TemporaryDirectory(prefix="plat-native-check-") as temporary:
        root = Path(temporary)
        environment = root / "environment"
        venv.create(environment, with_pip=True, symlinks=sys.platform != "win32")
        scripts = environment / ("Scripts" if os.name == "nt" else "bin")
        python = scripts / ("python.exe" if os.name == "nt" else "python")
        binary = scripts / (
            "boxscore-exact.exe" if os.name == "nt" else "boxscore-exact"
        )

        def run(*args, **kwargs):
            return subprocess.run(
                list(map(str, args)),
                check=True,
                cwd=root,
                capture_output=True,
                timeout=120,
                **kwargs,
            ).stdout

        run(python, "-m", "pip", "install", "--no-deps", wheel)
        run(python, "-m", "pip", "check")
        assert run(binary, "--version").strip() == b"boxscore-exact 0.1.1"
        row = {
            "account_code": "4000",
            "account_name": "Rent",
            "category": "rental income",
            "amount": "0.10",
        }
        request = {
            "contract_version": "plat.ops/1",
            "operation": "variance",
            "currency": "USD",
            "expense_convention": "positive_costs",
            "actuals": [row, {**row, "amount": "0.20"}],
            "budgets": [{**row, "amount": "0.29"}],
        }
        payload = json.loads(
            run(binary, "protocol", input=json.dumps(request).encode())
        )
        assert payload["result"]["noi_bridge"]["noi_variance"] == "0.01"
        database = root / "synthetic.sqlite"
        run(binary, "init", "--database", database)
        data = {
            "property": "synthetic",
            "period": "2026-05",
            "unit_count": 1,
            "currency": "USD",
            "expense_convention": "positive_costs",
            "actuals": request["actuals"],
            "budgets": request["budgets"],
            "snapshot": None,
        }
        source = root / "synthetic.json"
        source.write_text(json.dumps(data))
        result = json.loads(
            run(binary, "import", "--database", database, "--input", source)
        )
        report = json.loads(
            run(
                binary,
                "issue",
                "--database",
                database,
                "--revision",
                result["revision_id"],
            )
        )
        assert report["variance"]["noi_bridge"]["actual_noi"] == "0.30"
        with sqlite3.connect(database) as connection:
            assert connection.execute(
                "SELECT DISTINCT typeof(amount_cents) FROM exact_gl"
            ).fetchall() == [("integer",)]
        return {
            "status": "passed",
            "python": sys.version.split()[0],
            "wheel": wheel.name,
            "wheel_sha256": hashlib.sha256(wheel.read_bytes()).hexdigest(),
            "checks": [
                "clean_install",
                "pip_check",
                "version",
                "cent_protocol",
                "sqlite",
                "immutable_issue",
            ],
        }


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument("wheel", type=Path)
    args = parser.parse_args()
    print(json.dumps(verify(args.wheel.resolve()), indent=2))
