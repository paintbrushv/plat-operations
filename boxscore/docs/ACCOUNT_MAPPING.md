# Account Mapping

Boxscore uses account mappings to translate property-management-system account codes and descriptions into the stable Boxscore ontology.

## Mapping States

- `approved`: Operator-approved mapping. This takes precedence over deterministic suggestions.
- `suggested`: Rule-based mapping suggestion created by Boxscore when account text strongly matches a known category.
- `unmapped`: No approved mapping or clear suggestion exists. These accounts generate gaps, operator questions, and export-review rows.

## CLI

List unmapped accounts after standardized GL ingestion:

```bash
cargo run -- accounts unmapped
```

Export a review file for suggested and unmapped standardized Yardi accounts:

```bash
cargo run -- accounts export-review --file data/review/account_mappings.csv
```

Review the CSV with an operator. To approve a row, set:

```text
status=approved
reviewed_category=<Boxscore category>
review_notes=<optional operator rationale>
```

Import approved rows:

```bash
cargo run -- accounts import-review --file data/review/account_mappings.csv
```

Approve a mapping:

```bash
cargo run -- accounts map \
  --account-code 5200 \
  --category "Repairs & Maintenance" \
  --scope p101 \
  --account-name "Repairs & Maintenance"
```

## Precedence

1. Approved property-scope mapping.
2. Deterministic text/code suggestion persisted as `suggested`.
3. `Unmapped` category plus review candidate, gap, and operator question.

## Review CSV Contract

The export/import workflow uses:

```text
source_system,property_scope,account_code,account_name,current_category,suggested_category,confidence_score,status,reviewed_category,review_notes
```

- Export includes non-approved rows from `account_mappings`.
- Import ignores rows unless `status=approved`.
- Approved rows must include `reviewed_category`.
- Approved rows upsert `account_mappings`, create `account_mapping` memories, and reclassify existing unmapped GL actual/budget rows for that account/name.
- Non-approved rows remain available for future review.

## Stewardship Rules

- Do not silently force unknown accounts into an NOI category.
- Preserve account code, account name, scope, status, and confidence.
- Manual mappings create durable memories so later analyses improve.
- Capital, debt-service, balance-sheet, and subtotal accounts should remain unmapped until the ontology explicitly supports them.
