# Security And Privacy

Boxscore is local-first by default.

Principles:

- No private data leaves the machine unless explicitly configured.
- No external model API keys are required for v0.1.
- No secrets in git.
- `.env`, local databases, `data/private/`, and `reports/private/` are gitignored.
- Evidence-aware reporting should show where claims came from.
- Tenant/person-level PII should be minimized, redacted, or avoided in future parser work.
- Capability proposals are auditable and do not self-modify production code.
- Enterprise deployment should add role-based access, secret management, encryption, retention controls, and audit export.
