//! Legacy CSV ingest must fail loudly on a wrong-shaped file and must never
//! leave a partial table behind. Before these tests, a header mismatch
//! "completed" with zero rows and no gap, an unknown category was summed
//! into expenses, and a bad period row deleted the prior load before erroring.

use std::path::{Path, PathBuf};

use boxscore::{
    db,
    ingest::{self, IngestKind},
};

fn sample(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("data/sample")
        .join(name)
}

async fn pool_with_properties() -> sqlx::SqlitePool {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    ingest::ingest_file(&pool, IngestKind::Property, &sample("properties.csv"))
        .await
        .unwrap();
    pool
}

async fn gl_count(pool: &sqlx::SqlitePool) -> i64 {
    sqlx::query_scalar("SELECT COUNT(*) FROM gl_actuals")
        .fetch_one(pool)
        .await
        .unwrap()
}

fn write(dir: &Path, name: &str, body: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, body).unwrap();
    path
}

const GOOD_GL: &str = "property,period,account_code,account_name,category,amount\n\
Oak Ridge,2026-05,4000,Rent,Rental Income,100000\n\
Oak Ridge,2026-05,5000,Payroll,Payroll,35000\n";

#[tokio::test]
async fn wrong_shaped_gl_file_refuses_and_keeps_the_prior_load() {
    let pool = pool_with_properties().await;
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "gl_actuals.csv", GOOD_GL);
    ingest::ingest_file(&pool, IngestKind::GlActuals, &path)
        .await
        .unwrap();
    assert_eq!(gl_count(&pool).await, 2);

    // Same path, vendor-cased header: must refuse, must not delete the load.
    write(
        dir.path(),
        "gl_actuals.csv",
        "Property,Period,Account,Description,Category,Amount\nOak Ridge,2026-05,4000,Rent,Rental Income,1\n",
    );
    let err = ingest::ingest_file(&pool, IngestKind::GlActuals, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(
        err.contains(
            "missing column(s) [property, period, account_code, account_name, category, amount]"
        ),
        "{err}"
    );
    assert!(
        err.contains("Expected header: property,period,account_code,account_name,category,amount"),
        "{err}"
    );
    assert_eq!(
        gl_count(&pool).await,
        2,
        "prior rows must survive a refused file"
    );

    // Header-only file: no rows, but also no silent success.
    write(
        dir.path(),
        "gl_actuals.csv",
        "property,period,account_code\n",
    );
    assert!(ingest::ingest_file(&pool, IngestKind::GlActuals, &path)
        .await
        .is_err());
    assert_eq!(gl_count(&pool).await, 2);
}

#[tokio::test]
async fn ragged_row_refuses_with_its_row_number() {
    let pool = pool_with_properties().await;
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "gl.csv",
        "property,period,account_code,account_name,category,amount\nOak Ridge,2026-05,4000,Rent,Rental Income,100000\nOak Ridge,2026-05,5000,Payroll\n",
    );
    let err = ingest::ingest_file(&pool, IngestKind::GlActuals, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("row 3"), "{err}");
    assert_eq!(gl_count(&pool).await, 0);
}

#[tokio::test]
async fn unknown_category_refuses_instead_of_becoming_an_expense() {
    let pool = pool_with_properties().await;
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "gl.csv",
        &format!("{GOOD_GL}Oak Ridge,2026-05,9999,Total Expenses,Total,146200\n"),
    );
    let err = ingest::ingest_file(&pool, IngestKind::GlActuals, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("row 4"), "{err}");
    assert!(err.contains("\"Total\""), "{err}");
    assert!(
        err.contains("Management Fees"),
        "message lists the allowed set: {err}"
    );
    assert_eq!(
        gl_count(&pool).await,
        0,
        "nothing is written for a refused file"
    );
}

#[tokio::test]
async fn category_case_and_whitespace_are_canonicalized_on_ingest() {
    let pool = pool_with_properties().await;
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "gl.csv",
        "property,period,account_code,account_name,category,amount\n\
Oak Ridge,2026-05,4000,Rent, rental income ,100000\n\
Oak Ridge,2026-05,5000,Payroll,PAYROLL,35000\n\
Oak Ridge,2026-05,2000,Escrow,unmapped,500\n",
    );
    ingest::ingest_file(&pool, IngestKind::GlActuals, &path)
        .await
        .unwrap();
    let stored: Vec<String> =
        sqlx::query_scalar("SELECT category FROM gl_actuals ORDER BY account_code")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(stored, vec!["Unmapped", "Rental Income", "Payroll"]);
}

#[tokio::test]
async fn bad_period_refuses_whole_file_before_any_write() {
    let pool = pool_with_properties().await;
    let dir = tempfile::tempdir().unwrap();
    let path = write(dir.path(), "gl_actuals.csv", GOOD_GL);
    ingest::ingest_file(&pool, IngestKind::GlActuals, &path)
        .await
        .unwrap();
    write(
        dir.path(),
        "gl_actuals.csv",
        &format!("{GOOD_GL}Oak Ridge,May 2026,5100,Admin,Administrative,1200\n"),
    );
    let err = ingest::ingest_file(&pool, IngestKind::GlActuals, &path)
        .await
        .unwrap_err()
        .to_string();
    assert!(err.contains("row 4") && err.contains("YYYY-MM"), "{err}");
    assert_eq!(gl_count(&pool).await, 2, "the earlier load is intact");
}

#[tokio::test]
async fn non_positive_unit_count_is_a_gap_not_a_property() {
    let pool = db::connect("sqlite::memory:").await.unwrap();
    db::init_database(&pool).await.unwrap();
    let dir = tempfile::tempdir().unwrap();
    let path = write(
        dir.path(),
        "properties.csv",
        "name,market,unit_count,owner_entity,property_manager\n\
Good,Austin, 120 ,Owner,PM\n\
Negative,Austin,-5,Owner,PM\n\
Zero,Austin,0,Owner,PM\n",
    );
    let result = ingest::ingest_file(&pool, IngestKind::Property, &path)
        .await
        .unwrap();
    assert_eq!(
        (result.inserted, result.skipped, result.gaps_created),
        (1, 2, 2)
    );
    assert!(db::find_property_by_name(&pool, "Negative")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn snapshot_files_with_missing_columns_refuse() {
    let pool = pool_with_properties().await;
    let dir = tempfile::tempdir().unwrap();
    for (kind, name, header) in [
        (
            IngestKind::RentRoll,
            "rent_roll",
            "property,as_of_date,occupied\n",
        ),
        (
            IngestKind::Delinquency,
            "delinquency",
            "property,as_of_date\n",
        ),
        (IngestKind::Leasing, "leasing", "property,date,leads\n"),
    ] {
        let path = write(
            dir.path(),
            name,
            &format!("{header}Oak Ridge,2026-05-31,1\n"),
        );
        let err = ingest::ingest_file(&pool, kind, &path)
            .await
            .unwrap_err()
            .to_string();
        assert!(err.contains("missing column(s)"), "{name}: {err}");
    }
}
