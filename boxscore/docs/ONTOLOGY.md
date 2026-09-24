# Multifamily Ontology

These definitions should remain stable unless changed through an explicit migration or ADR.

- Property: A multifamily asset with a name, market, unit count, ownership entity, and property manager.
- Period: A reporting month in `YYYY-MM` format.
- Account: A GL account with a code, name, and Boxscore category.
- Actual: Booked financial activity for a property and period.
- Budget: Approved expected financial activity for a property and period.
- Variance: Actual minus budget. Positive variance is favorable when it increases NOI; negative variance is unfavorable when it reduces NOI.
- Occupancy: The relationship between occupied units and total available/down/vacant units.
- Physical occupancy: Occupied units divided by occupied, vacant, and down units.
- Economic occupancy: In-place rent divided by market rent.
- Delinquency: Resident balances owed as of a snapshot date.
- Bad debt: Amounts written off or reserved because collection is unlikely.
- Concessions: Rent discounts, credits, or incentives given to residents or prospects.
- Make-ready: Work required to prepare a unit after move-out and before move-in.
- Down units: Units unavailable for lease because of condition, casualty, renovation, or operational hold.
- Leasing funnel: Leads, tours, applications, approvals, move-ins, move-outs, and concessions.
- Renewal: Existing resident agreement to extend occupancy.
- Move-in: Resident takes occupancy.
- Move-out: Resident vacates.
- Turnover: Operational cycle from move-out through make-ready to re-leasing.
- Controllable expense: Expense categories where operating decisions often influence timing or amount, such as payroll, repairs, marketing, administrative spend, and management fees.
- Non-controllable expense: Expense categories typically driven by external contracts, taxes, insurance, utilities, or ownership structure.
- NOI: Net operating income. In v0.1, NOI is calculated as the sum of revenue, contra-revenue, and operating expense GL lines.
