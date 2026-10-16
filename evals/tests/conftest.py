"""Put evals/ on sys.path so the tests import score, report, harness and run
the way the scripts do. No network, no key, no SDK is needed for these tests."""
import json
import sys
from pathlib import Path

import pytest

EVALS = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(EVALS))

FIXTURES = EVALS / "fixtures"


@pytest.fixture
def fixture():
    """Load a committed fixture transcript by name (without .json)."""
    def load(name):
        return json.loads((FIXTURES / f"{name}.json").read_text(encoding="utf-8"))
    return load
