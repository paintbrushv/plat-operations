# Invented PMS export fixtures

All files in this directory are synthetic. They do not contain CCAR or TC
accounting records or document an actual Yardi or ResMan export layout.

- `tc_boundary.json`: September 23, 2026 Yardi-to-ResMan handoff.
- `ccar_boundary.json`: September 25, 2026 Yardi-to-Yardi manager handoff.
- Each boundary pins an exact adapter profile, a distinct source namespace for
  each manager, a stable synthetic property ID, and explicit account categories.
- `*_yardi.csv` and `*_resman.csv` deliberately use different, versioned column
  layouts. Record IDs repeat across feeds; their namespaces keep them distinct.
- `tc_outgoing_correction.csv` revises the outgoing September 22 expense from
  $18,000 to $18,500 after the synthetic close is issued.
- `september_budgets.csv` uses Boxscore's current natural-positive expense
  convention. It is a synthetic budget input, not a versioned source adapter.

Expected synthetic NOI: TC $55,000 issued, $54,500 after correction, with a
$57,000 budget. CCAR remains $44,000 against a $46,000 budget. Full source
TB, income statement, AR, collections, and close certification fixtures are
still needed before source parity can be claimed.
