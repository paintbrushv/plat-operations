# plat-operations

The Python distribution installs the native `boxscore-exact` command. It provides
checked integer-cent USD calculations, canonical CSV/JSON imports, SQLite
persistence, immutable reports, linked corrections, and reviewed copy migration.
Money uses decimal strings. Legacy `boxscore` floating-point commands are excluded.

```bash
python -m pip install plat-operations==0.1.1
boxscore-exact --version
boxscore-exact --help
```

Candidate wheels target Linux x64, macOS Intel/Apple Silicon, and Windows x64,
with Python 3.11/3.12 clean-install verification. Only passing artifacts are
eligible for publication. This candidate has not yet been published.
Source builds require Rust and use the included Cargo.lock.

See [exact workflow and migration](docs/EXACT_CENTS.md). The code and package
metadata use the repository's existing Apache-2.0 license.
