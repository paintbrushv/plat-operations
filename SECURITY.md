# Security Policy

## Reporting a vulnerability

Please report vulnerabilities privately by opening a GitHub security advisory
("Report a vulnerability" under the Security tab of this repository). Do not open
a public issue for security problems.

## Data confidentiality

Boxscore is local-first by default: no private data leaves the machine unless
explicitly configured. Never commit real GL, rent-roll, receivables, or any
tenant/PII data. `.env`, local databases, private data, and private report
directories are gitignored — keep it that way.

## What this project is NOT

- Not a system that exfiltrates data: there is no telemetry, no remote logging,
  and no default network egress. The HTTP API binds to loopback by default
  (`BOXSCORE_BIND_ADDR=127.0.0.1:3818`).
- Not hardened for multi-tenant or hostile-network deployment. The local HTTP
  API is for a single analyst on one machine.