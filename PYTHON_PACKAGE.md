# plat-operations

The Python distribution installs the native `boxscore-exact` command. It provides
checked integer-cent USD calculations, canonical CSV/JSON imports, SQLite
persistence, immutable reports, linked corrections, and reviewed copy migration.
Money uses decimal strings. Legacy `boxscore` floating-point commands are excluded.

```bash
python -m pip install plat-operations==0.3.0
boxscore-exact --version
boxscore-exact --help
```

Wheels target Linux x64, macOS Intel/Apple Silicon, and Windows x64, with
Python 3.11/3.12 clean-install verification. Only passing artifacts are
published. This is 0.3.0; 0.2.0 was the first version published to PyPI. See the
[release notes](https://github.com/paintbrushv/plat-operations/blob/main/docs/releases/v0.3.0.md).
Source builds require Rust and use the included Cargo.lock.

See [exact workflow and migration](docs/EXACT_CENTS.md). To run the same
command without Python or Rust, use the container image: see
[Docker install](https://github.com/paintbrushv/plat-operations/blob/main/docs/INSTALL.md)
and the [data-flow description](https://github.com/paintbrushv/plat-operations/blob/main/docs/DATA_FLOW.md)
for IT review. The code and package
metadata use the repository's existing Apache-2.0 license.
