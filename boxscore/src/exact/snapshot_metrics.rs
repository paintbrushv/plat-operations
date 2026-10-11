//! Occupancy, economic occupancy and delinquency over one exact-cent snapshot.
//!
//! Pure functions: a validated [`Snapshot`] and the property's unit count go
//! in, a JSON result and the review reasons it earned come out. Money is only
//! combined with `Money::checked_add` / `checked_sub`, and every rate is a
//! [`Ratio`] of integers (cents for money), never a float. The catalog decides
//! the envelope status from the review reasons.
use super::{error, money::Money, ratio::Ratio, store::Snapshot, Result};
use serde_json::{json, Value};

type Metric = (Value, Vec<String>);

fn cents(money: Money) -> i128 {
    i128::from(money.cents())
}

/// `occupied_units / unit_count`. Down units stay in the denominator, so a
/// property with offline units is not made to look fuller than it is; they are
/// disclosed as an informational review reason.
pub fn occupancy(snapshot: &Snapshot, unit_count: i64) -> Result<Metric> {
    let ratio = Ratio::new(i128::from(snapshot.occupied_units), i128::from(unit_count))?;
    let mut reasons = Vec::new();
    if snapshot.down_units > 0 {
        reasons.push("down_units_present".to_string());
    }
    Ok((
        json!({
            "as_of_date": snapshot.as_of_date,
            "unit_count": unit_count,
            "occupied_units": snapshot.occupied_units,
            "vacant_units": snapshot.vacant_units,
            "down_units": snapshot.down_units,
            "occupancy": ratio,
        }),
        reasons,
    ))
}

/// `in_place_rent_total / market_rent_total`, as a ratio of cents.
pub fn economic_occupancy(snapshot: &Snapshot) -> Result<Metric> {
    let market = snapshot.market_rent_total.ok_or_else(|| {
        error(
            "MISSING_MARKET_RENT",
            "Economic occupancy needs the snapshot's market rent total",
        )
    })?;
    let in_place = snapshot.in_place_rent_total.ok_or_else(|| {
        error(
            "MISSING_IN_PLACE_RENT",
            "Economic occupancy needs the snapshot's in-place rent total",
        )
    })?;
    let ratio = Ratio::new(cents(in_place), cents(market))?;
    let mut reasons = Vec::new();
    if in_place > market {
        reasons.push("in_place_exceeds_market".to_string());
    }
    Ok((
        json!({
            "as_of_date": snapshot.as_of_date,
            "market_rent_total": market,
            "in_place_rent_total": in_place,
            "economic_occupancy": ratio,
        }),
        reasons,
    ))
}

/// Delinquent and prepaid balances, their net, and each as a share of in-place
/// rent. A missing prepaid balance leaves `prepaid_amount`, `net` and the
/// prepaid share null rather than assuming zero.
pub fn delinquency(snapshot: &Snapshot) -> Result<Metric> {
    let delinquent = snapshot.delinquent_amount.ok_or_else(|| {
        error(
            "MISSING_DELINQUENCY",
            "Delinquency needs the snapshot's delinquent amount",
        )
    })?;
    let mut reasons = Vec::new();
    let net = match snapshot.prepaid_amount {
        Some(prepaid) => Some(delinquent.checked_sub(prepaid)?),
        None => {
            reasons.push("missing_prepaid".to_string());
            None
        }
    };
    // A zero in-place total has no meaningful share, so it is reported the
    // same way as a missing one.
    let in_place = snapshot
        .in_place_rent_total
        .filter(|rent| *rent != Money::ZERO);
    let (delinquent_share, prepaid_share) = match in_place {
        Some(rent) => (
            Some(Ratio::new(cents(delinquent), cents(rent))?),
            match snapshot.prepaid_amount {
                Some(prepaid) => Some(Ratio::new(cents(prepaid), cents(rent))?),
                None => None,
            },
        ),
        None => {
            reasons.push("missing_in_place_rent".to_string());
            (None, None)
        }
    };
    Ok((
        json!({
            "as_of_date": snapshot.as_of_date,
            "delinquent_amount": delinquent,
            "prepaid_amount": snapshot.prepaid_amount,
            "net": net,
            "in_place_rent_total": snapshot.in_place_rent_total,
            "delinquent_share_of_in_place_rent": delinquent_share,
            "prepaid_share_of_in_place_rent": prepaid_share,
        }),
        reasons,
    ))
}
