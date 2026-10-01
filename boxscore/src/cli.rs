use anyhow::Result;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

use crate::{
    account_review::{export_account_mapping_review, import_account_mapping_review},
    api, ask, calibration, close_readiness,
    config::AppConfig,
    connectors::standardized::{
        collections_unified::ingest_collections_for_lane,
        gl_budget_comparison::ingest_budget_comparison_for_lane,
        gl_transactions::ingest_gl_transactions_for_lane,
        operating_snapshots::ingest_operating_snapshots_for_lane,
        profiler::profile_lane,
        source_registry::{
            inspect_lane, lane_by_key, load_lanes, upsert_lane_config, PropertyLane,
        },
        turn_costs::ingest_turn_costs_for_lane,
        unit_pnl::ingest_unit_pnl_for_lane,
        validator::{self, Severity},
    },
    db, evolution,
    ingest::{self, IngestKind},
    intake, portfolio_demo, questions, synthetic_pms, t12,
    variance::{self, VarianceRequest},
};

#[derive(Debug, Parser)]
#[command(name = "boxscore")]
#[command(about = "Local-first multifamily NOI variance intelligence harness")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Init,
    Ingest {
        #[command(subcommand)]
        command: IngestCommand,
    },
    Analyze {
        #[command(subcommand)]
        command: AnalyzeCommand,
    },
    IngestStandardized {
        #[command(subcommand)]
        command: IngestStandardizedCommand,
    },
    Intake {
        #[command(subcommand)]
        command: IntakeCommand,
    },
    Accounts {
        #[command(subcommand)]
        command: AccountsCommand,
    },
    Questions {
        #[command(subcommand)]
        command: QuestionCommand,
    },
    Gaps {
        #[command(subcommand)]
        command: GapsCommand,
    },
    Demo {
        #[command(subcommand)]
        command: DemoCommand,
    },
    /// Validate a property lane's Standardized/*.csv against data contracts C1-C4.
    Validate {
        /// Lane key (e.g. maplewood, juniper_fund, willow-brook).
        #[arg(long)]
        lane: Option<String>,
        /// Validate every built-in lane.
        #[arg(long)]
        all: bool,
        /// Emit machine-readable JSON instead of the human summary.
        #[arg(long)]
        json: bool,
    },
    CloseReadiness {
        #[arg(long)]
        period: String,
    },
    /// Synthetic-only manager handoff rehearsal; never imports real PMS exports.
    SyntheticPms {
        #[command(subcommand)]
        command: SyntheticPmsCommand,
    },
    /// Persist a finding from the standing agentic report-review into the learning loop.
    ///
    /// Used by the `/report-review` skill: each material finding becomes a gap (with
    /// evidence when a source file is cited); a NOVEL failure class the C1-C5 contracts
    /// would not catch is recorded as a `capability` proposing a new contract Cn.
    RecordFinding {
        /// What to record: a `gap` (a concrete defect found) or a `capability`
        /// (a proposed NEW contract for a failure class the contracts miss).
        #[arg(long, default_value = "gap")]
        kind: String,
        /// Severity of the finding: error | warn | info. Defaults to warn.
        #[arg(long, default_value = "warn")]
        severity: String,
        /// The contract this finding belongs to, e.g. "review:noi_bridge_tieout".
        #[arg(long)]
        contract: String,
        /// Human-readable description of the finding.
        #[arg(long)]
        message: String,
        /// Optional source file the finding was traced to (report or Standardized CSV).
        #[arg(long)]
        source_file: Option<String>,
        /// Optional row number within the source file.
        #[arg(long)]
        source_row: Option<i64>,
        /// Optional proposed fix / new-contract definition.
        #[arg(long)]
        proposed_resolution: Option<String>,
        /// Optional property lane key the finding pertains to (e.g. maplewood).
        #[arg(long)]
        property: Option<String>,
    },
    /// Launch the Boxscore Close Desk terminal workbench.
    Desk {
        #[arg(long)]
        period: Option<String>,
    },
    Capabilities {
        #[command(subcommand)]
        command: CapabilitiesCommand,
    },
    Memories,
    DataSources,
    Sources {
        #[command(subcommand)]
        command: SourcesCommand,
    },
    Tools,
    Server,
    /// Print a trailing-twelve-month income statement as JSON.
    T12 {
        /// Property name (matched case-insensitively).
        #[arg(long)]
        property: String,
        /// End period as YYYY-MM (defaults to the latest period with actuals).
        #[arg(long)]
        period: Option<String>,
    },
    /// Ask a natural-language question about your GL data.
    Ask {
        /// The question to ask, e.g. "what did we pay 7 Kings Landscaping last 6 months".
        question: String,
        /// Send tool results back to the model for a narrative answer (privacy opt-in).
        #[arg(long)]
        narrate: bool,
    },
    /// Record, list, score, and review the harness's calls (predictions/recommendations).
    Calls {
        #[command(subcommand)]
        command: CallsCommand,
    },
    /// Onboard a prospect's property: register a config-driven lane (lanes.json)
    /// rooted at a folder of Yardi Standardized exports, then ingest its CSVs in
    /// one flow. See docs/ONBOARDING.md for the expected files.
    Onboard {
        /// GP / owner display name (e.g. "Bagholder Capital").
        #[arg(long)]
        name: String,
        /// Property display name (e.g. "Vantage at Yieldmore").
        #[arg(long)]
        property: String,
        /// Folder holding the property's exports. Standardized CSVs are read
        /// from `<dir>/Standardized/`.
        #[arg(long)]
        dir: PathBuf,
        /// Lane key (slug). Defaults to a slug of --property.
        #[arg(long)]
        key: Option<String>,
        /// Primary Yardi property id(s) used to attribute GL rows (repeatable).
        #[arg(long = "property-id")]
        property_ids: Vec<String>,
        /// Total unit count hint for the property.
        #[arg(long)]
        units: Option<u32>,
    },
}

#[derive(Debug, Subcommand)]
enum SyntheticPmsCommand {
    Import {
        #[arg(long)]
        boundary: PathBuf,
        #[arg(long)]
        side: String,
        #[arg(long)]
        file: PathBuf,
    },
    Seal {
        #[arg(long)]
        boundary: PathBuf,
        #[arg(long)]
        report_task_run_id: String,
    },
    Status {
        #[arg(long)]
        property: String,
        #[arg(long)]
        period: String,
    },
}

#[derive(Debug, Subcommand)]
enum IngestCommand {
    Property {
        #[arg(long)]
        file: PathBuf,
    },
    GlActuals {
        #[arg(long)]
        file: PathBuf,
    },
    GlBudgets {
        #[arg(long)]
        file: PathBuf,
    },
    RentRoll {
        #[arg(long)]
        file: PathBuf,
    },
    Delinquency {
        #[arg(long)]
        file: PathBuf,
    },
    Leasing {
        #[arg(long)]
        file: PathBuf,
    },
    /// Ingest a turn_costs_summary.csv (turnover / make-ready costs) by file path.
    /// The file must live under a known lane's Standardized/ directory.
    TurnCosts {
        #[arg(long)]
        file: PathBuf,
    },
    /// Ingest a unit_pnl_annual.csv (per-unit P&L) by file path.
    /// The file must live under a known lane's Standardized/ directory.
    UnitPnl {
        #[arg(long)]
        file: PathBuf,
    },
    /// Ingest a gl_monthly_actuals.csv (P&L monthly rollup from the GL parquet).
    MonthlyActuals {
        #[arg(long)]
        property: String,
        #[arg(long)]
        file: std::path::PathBuf,
    },
}

#[derive(Debug, Subcommand)]
enum AnalyzeCommand {
    Variance {
        #[arg(long)]
        property: String,
        #[arg(long)]
        period: String,
    },
}

#[derive(Debug, Subcommand)]
enum IngestStandardizedCommand {
    Gl {
        #[arg(long)]
        lane: Option<String>,
        #[arg(long)]
        all: bool,
    },
    Ops {
        #[arg(long)]
        lane: Option<String>,
        #[arg(long)]
        all: bool,
    },
    Collections {
        #[arg(long)]
        lane: Option<String>,
        #[arg(long)]
        all: bool,
    },
    Transactions {
        #[arg(long)]
        lane: Option<String>,
        #[arg(long)]
        all: bool,
        /// Only ingest rows with period >= this YYYY-MM label.
        #[arg(long)]
        since: Option<String>,
    },
    /// Ingest per-lane turn_costs_summary.csv into the turn_costs table.
    TurnCosts {
        #[arg(long)]
        lane: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// Ingest per-lane unit_pnl_annual.csv into the unit_pnl table.
    UnitPnl {
        #[arg(long)]
        lane: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, Subcommand)]
enum IntakeCommand {
    Scan {
        #[arg(long)]
        inbox: PathBuf,
        #[arg(long)]
        period: String,
    },
}

#[derive(Debug, Subcommand)]
enum AccountsCommand {
    Unmapped,
    ExportReview {
        #[arg(long)]
        file: PathBuf,
    },
    ImportReview {
        #[arg(long)]
        file: PathBuf,
    },
    Map {
        #[arg(long)]
        account_code: String,
        #[arg(long)]
        category: String,
        #[arg(long)]
        scope: String,
        #[arg(long)]
        account_name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum QuestionCommand {
    List,
    Answer {
        #[arg(long)]
        id: String,
        #[arg(long)]
        answer: String,
    },
}

#[derive(Debug, Subcommand)]
enum GapsCommand {
    List,
}

#[derive(Debug, Subcommand)]
enum DemoCommand {
    /// Run the portfolio analysis demo over the built-in lanes.
    Portfolio,
    /// Launch the Close Desk TUI against the isolated synthetic demo DB.
    Desk {
        #[arg(long)]
        period: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
enum CapabilitiesCommand {
    List,
}

#[derive(Debug, Subcommand)]
enum SourcesCommand {
    List,
    Inspect {
        #[arg(long)]
        lane: String,
    },
    Profile {
        #[arg(long)]
        lane: Option<String>,
        #[arg(long)]
        all: bool,
    },
}

#[derive(Debug, Subcommand)]
#[allow(clippy::large_enum_variant)]
enum CallsCommand {
    /// Record a new call (skills emit via this; the crate also emits internally).
    ///
    /// When `--decision-kind` is present the call is routed through the
    /// universal decision-capture API (`capture::record`), which derives the
    /// maturation horizon and validates `outcome_mode`/`value_class`.
    Record {
        #[arg(long)]
        property: String,
        #[arg(long)]
        call_type: String,
        #[arg(long)]
        mature_by: String,
        #[arg(long)]
        confidence: Option<f64>,
        /// Domain-specific payload as a JSON object string.
        #[arg(long)]
        payload: String,
        #[arg(long)]
        origin_period: String,
        // ── Decision-capture extensions (flywheel A) ──────────────────────────
        /// Decision kind (e.g. renewal_override, capex, hold_sell). When set,
        /// routes to `capture::record` instead of the legacy insert_call path.
        #[arg(long)]
        decision_kind: Option<String>,
        /// JSON array of entity identifiers this decision pertains to.
        #[arg(long)]
        entities: Option<String>,
        /// How the outcome will be measured: auto | metric_bound | human.
        #[arg(long)]
        outcome_mode: Option<String>,
        /// Value class: value | compliance.
        #[arg(long)]
        value_class: Option<String>,
        /// Whether this decision was acted on (default false).
        #[arg(long, default_value = "false")]
        acted_on: bool,
        /// Source surface (e.g. ask, tui, api).
        #[arg(long)]
        source: Option<String>,
        /// Arbitrary JSON context blob.
        #[arg(long)]
        context: Option<String>,
        /// Override the derived maturation period (YYYY-MM).
        #[arg(long = "mature-by-override")]
        mature_by_override: Option<String>,
    },
    /// List all calls as JSON.
    List,
    /// Score all matured calls for a period (freshness-gated).
    Score {
        #[arg(long)]
        period: String,
    },
    /// Print the harness's batting average per property + call type.
    TrackRecord {
        #[arg(long)]
        property: Option<String>,
        #[arg(long)]
        call_type: Option<String>,
    },
    /// Bootstrap the harness's track record from historical GL.
    ///
    /// Walks `lookback` periods back from `through` (inclusive), running
    /// variance analysis per period (skipping periods with no GL data) and
    /// then scoring any matured calls. Fully idempotent.
    Backfill {
        /// Property name (matched case-insensitively).
        #[arg(long)]
        property: String,
        /// End period as YYYY-MM (most recent period to include).
        #[arg(long)]
        through: String,
        /// Number of additional periods to walk back from `through` (default 11 → 12 months).
        #[arg(long, default_value = "11")]
        lookback: u32,
    },
    /// Backfill the t12_reversion track record from monthly_actuals history.
    BackfillT12 {
        #[arg(long)]
        property: String,
        #[arg(long)]
        through: String,
        /// Start period YYYY-MM (default: earliest history + 12 months).
        #[arg(long)]
        from: Option<String>,
    },
    /// Import per-unit recommendations from a BDDRE/RPCOE CSV as calls (idempotent).
    Import {
        /// Call type: delinquency_risk | renewal_rec.
        #[arg(long)]
        call_type: String,
        /// Property name (matched case-insensitively).
        #[arg(long)]
        property: String,
        /// Analysis period as YYYY-MM (origin period; calls mature the next month).
        #[arg(long)]
        period: String,
        /// Path to the engine output CSV.
        #[arg(long)]
        file: std::path::PathBuf,
    },
    /// Aggregate scored calls by NOI category (which accounts revert/normalize vs not).
    ReversionReport {
        #[arg(long)]
        call_type: String,
        #[arg(long)]
        property: Option<String>,
    },
    /// Run the §6.5 calibration eval: reliability-diagram bins + ECE per engine type.
    ///
    /// Pulls scored value-class calls (confidence, score) for the 4 engine types
    /// and prints per-type reliability bins + ECE as JSON. Use as the honesty gate
    /// before publishing track-record figures.
    CalibrationEval {
        /// Optional property filter (matched case-insensitively). Omit to include all.
        #[arg(long)]
        property: Option<String>,
        /// Optional call-type filter (e.g. noi_diagnosis). Omit to include all 4 types.
        #[arg(long)]
        call_type: Option<String>,
    },
    /// List matured decision calls with outcome_mode != 'auto' still awaiting operator resolution.
    PendingOutcomes,
    /// Flywheel C: derive per-property knowledge from the calls ledger (read-only).
    ///
    /// Computes, per (property, call_type) over scored value-class non-confounded calls:
    /// the property's calibrated posterior vs the pooled/portfolio posterior (flagging a
    /// MATERIAL DIVERGENCE when the property's 90% CI excludes the pooled mean), a recent
    /// vs older regime window, and the data-gated value-vs-compliance / decision-kind
    /// facts (which abstain until those rows exist). Abstains at n_eff < 5.
    PropertyModel {
        /// Property name (matched case-insensitively).
        #[arg(long)]
        property: String,
        /// "Today" period as YYYY-MM for age/decay (defaults to the current month).
        #[arg(long = "as-of")]
        as_of: Option<String>,
    },
    /// Flywheel B surface: contextual recall at the point of decision (read-only).
    ///
    /// Builds a RecallCtx and calls the recall service: prints the calibrated stat plus
    /// age-stamped neighbors (id/summary/score/status/age/similarity), pending ones shown.
    Recall {
        /// Property name (matched case-insensitively).
        #[arg(long)]
        property: String,
        /// Call type to recall over (default: decision).
        #[arg(long)]
        call_type: Option<String>,
        /// Decision kind to weight similarity toward (e.g. renewal_override, capex).
        #[arg(long)]
        decision_kind: Option<String>,
        /// JSON array of entity keys (e.g. ["unit:2BR","account:5120"]).
        #[arg(long)]
        entities: Option<String>,
        /// "Today" period as YYYY-MM for age-stamping (defaults to the current month).
        #[arg(long = "as-of")]
        as_of: Option<String>,
        /// Max neighbors to return (default 5).
        #[arg(long, default_value = "5")]
        k: usize,
    },
    /// Record the operator's qualitative outcome for a decision call.
    Resolve {
        /// The call id to resolve.
        #[arg(long)]
        id: String,
        /// Free-text outcome description from the operator.
        #[arg(long)]
        outcome: String,
        /// Name of the operator resolving the call.
        #[arg(long)]
        by: String,
    },
}

pub async fn run(config: AppConfig) -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Server => api::serve(config).await,
        Command::Sources { command } => run_sources_command(command),
        command => {
            let pool = db::connect(&config.database_url).await?;
            db::init_database(&pool).await?;
            match command {
                Command::Init => {
                    println!("Boxscore database initialized at {}", config.database_url);
                    Ok(())
                }
                Command::Ingest { command } => {
                    // MonthlyActuals has a dedicated fast path (no task_run overhead).
                    if let IngestCommand::MonthlyActuals { property, file } = command {
                        let prop = db::require_property_by_name(&pool, &property).await?;
                        let n = ingest::ingest_monthly_actuals_csv(&pool, &prop.id, &file).await?;
                        println!("{}", serde_json::json!({ "ingested": n }));
                        return Ok(());
                    }
                    let (kind, file) = match command {
                        IngestCommand::Property { file } => (IngestKind::Property, file),
                        IngestCommand::GlActuals { file } => (IngestKind::GlActuals, file),
                        IngestCommand::GlBudgets { file } => (IngestKind::GlBudgets, file),
                        IngestCommand::RentRoll { file } => (IngestKind::RentRoll, file),
                        IngestCommand::Delinquency { file } => (IngestKind::Delinquency, file),
                        IngestCommand::Leasing { file } => (IngestKind::Leasing, file),
                        IngestCommand::TurnCosts { file } => (IngestKind::TurnCosts, file),
                        IngestCommand::UnitPnl { file } => (IngestKind::UnitPnl, file),
                        IngestCommand::MonthlyActuals { .. } => unreachable!(),
                    };
                    let result = ingest::ingest_file(&pool, kind, &file).await?;
                    println!("{}", serde_json::to_string_pretty(&result)?);
                    Ok(())
                }
                Command::Analyze { command } => match command {
                    AnalyzeCommand::Variance { property, period } => {
                        let result = variance::analyze_variance(
                            &pool,
                            VarianceRequest { property, period },
                            &config.report_dir,
                        )
                        .await?;
                        println!("{}", serde_json::to_string_pretty(&result)?);
                        Ok(())
                    }
                },
                Command::IngestStandardized { command } => match command {
                    IngestStandardizedCommand::Gl { lane, all } => {
                        let summaries = ingest_standardized_gl(&pool, lane.as_deref(), all).await?;
                        println!("{}", serde_json::to_string_pretty(&summaries)?);
                        Ok(())
                    }
                    IngestStandardizedCommand::Ops { lane, all } => {
                        let summaries =
                            ingest_standardized_ops(&pool, lane.as_deref(), all).await?;
                        println!("{}", serde_json::to_string_pretty(&summaries)?);
                        Ok(())
                    }
                    IngestStandardizedCommand::Collections { lane, all } => {
                        let summaries =
                            ingest_standardized_collections(&pool, lane.as_deref(), all).await?;
                        println!("{}", serde_json::to_string_pretty(&summaries)?);
                        Ok(())
                    }
                    IngestStandardizedCommand::Transactions { lane, all, since } => {
                        let summaries = ingest_standardized_transactions(
                            &pool,
                            lane.as_deref(),
                            all,
                            since.as_deref(),
                        )
                        .await?;
                        println!("{}", serde_json::to_string_pretty(&summaries)?);
                        Ok(())
                    }
                    IngestStandardizedCommand::TurnCosts { lane, all } => {
                        let summaries =
                            ingest_standardized_turn_costs(&pool, lane.as_deref(), all).await?;
                        println!("{}", serde_json::to_string_pretty(&summaries)?);
                        Ok(())
                    }
                    IngestStandardizedCommand::UnitPnl { lane, all } => {
                        let summaries =
                            ingest_standardized_unit_pnl(&pool, lane.as_deref(), all).await?;
                        println!("{}", serde_json::to_string_pretty(&summaries)?);
                        Ok(())
                    }
                },
                Command::Intake { command } => match command {
                    IntakeCommand::Scan { inbox, period } => {
                        let result = intake::scan_inbox(&inbox, &period, &config.report_dir)?;
                        println!("{}", serde_json::to_string_pretty(&result)?);
                        Ok(())
                    }
                },
                Command::Accounts { command } => match command {
                    AccountsCommand::Unmapped => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(
                                &db::list_unmapped_accounts(&pool).await?
                            )?
                        );
                        Ok(())
                    }
                    AccountsCommand::ExportReview { file } => {
                        let summary = export_account_mapping_review(&pool, &file).await?;
                        println!("{}", serde_json::to_string_pretty(&summary)?);
                        Ok(())
                    }
                    AccountsCommand::ImportReview { file } => {
                        let summary = import_account_mapping_review(&pool, &file).await?;
                        println!("{}", serde_json::to_string_pretty(&summary)?);
                        Ok(())
                    }
                    AccountsCommand::Map {
                        account_code,
                        category,
                        scope,
                        account_name,
                    } => {
                        let id = db::upsert_account_mapping(
                            &pool,
                            db::NewAccountMapping {
                                source_system: "standardized-yardi",
                                property_scope: &scope,
                                account_code: &account_code,
                                account_name: account_name
                                    .as_deref()
                                    .unwrap_or("Operator mapped account"),
                                noi_category: &category,
                                confidence_score: 1.0,
                                status: "approved",
                            },
                        )
                        .await?;
                        db::upsert_memory(
                            &pool,
                            "account_mapping",
                            &scope,
                            &account_code,
                            &format!("{account_code} maps to {category} for standardized-yardi scope {scope}."),
                            1.0,
                            None,
                        )
                        .await?;
                        println!("{}", serde_json::json!({ "id": id, "status": "approved" }));
                        Ok(())
                    }
                },
                Command::Questions { command } => match command {
                    QuestionCommand::List => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&db::list_questions(&pool).await?)?
                        );
                        Ok(())
                    }
                    QuestionCommand::Answer { id, answer } => {
                        questions::answer_question(&pool, &id, &answer).await?;
                        println!("Question answered and memory candidate recorded.");
                        Ok(())
                    }
                },
                Command::Gaps { command } => match command {
                    GapsCommand::List => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&db::list_gaps(&pool).await?)?
                        );
                        Ok(())
                    }
                },
                Command::Demo { command } => match command {
                    DemoCommand::Portfolio => {
                        let result =
                            portfolio_demo::run_portfolio_demo(&pool, &config.report_dir).await?;
                        println!("{}", serde_json::to_string_pretty(&result)?);
                        Ok(())
                    }
                    DemoCommand::Desk { period } => {
                        // Launch the TUI against the isolated synthetic demo DB
                        // (boxscore/demo/demo.db, seeded by ./demo/seed.sh).
                        let demo_db = format!("{}/demo/demo.db", env!("CARGO_MANIFEST_DIR"));
                        if !std::path::Path::new(&demo_db).exists() {
                            eprintln!(
                                "Demo database not found. Seed it first from the boxscore/ \
                                 directory:\n    ./demo/seed.sh"
                            );
                            Ok(())
                        } else {
                            let demo_pool = db::connect(&format!("sqlite://{demo_db}")).await?;
                            db::init_database(&demo_pool).await?;
                            let period = period.unwrap_or_else(crate::tui::current_period);
                            crate::tui::run_desk(demo_pool, period).await
                        }
                    }
                },
                Command::Validate { lane, all, json } => {
                    let exit_error = run_validate(&pool, lane.as_deref(), all, json).await?;
                    if exit_error {
                        std::process::exit(1);
                    }
                    Ok(())
                }
                Command::CloseReadiness { period } => {
                    let result =
                        close_readiness::assess_close_readiness(&pool, &period, &config.report_dir)
                            .await?;
                    println!("{}", serde_json::to_string_pretty(&result)?);
                    Ok(())
                }
                Command::SyntheticPms { command } => {
                    match command {
                        SyntheticPmsCommand::Import {
                            boundary,
                            side,
                            file,
                        } => {
                            let boundary = synthetic_pms::HandoffBoundary::from_path(&boundary)?;
                            let summary =
                                synthetic_pms::import_file(&pool, &boundary, &side, &file).await?;
                            println!("{}", serde_json::to_string_pretty(&summary)?);
                        }
                        SyntheticPmsCommand::Seal {
                            boundary,
                            report_task_run_id,
                        } => {
                            let boundary = synthetic_pms::HandoffBoundary::from_path(&boundary)?;
                            let close = synthetic_pms::seal_synthetic_close(
                                &pool,
                                &boundary,
                                &report_task_run_id,
                            )
                            .await?;
                            println!("{}", serde_json::to_string_pretty(&close)?);
                        }
                        SyntheticPmsCommand::Status { property, period } => {
                            let close =
                                synthetic_pms::synthetic_close(&pool, &property, &period).await?;
                            println!("{}", serde_json::to_string_pretty(&close)?);
                        }
                    }
                    Ok(())
                }
                Command::RecordFinding {
                    kind,
                    severity,
                    contract,
                    message,
                    source_file,
                    source_row,
                    proposed_resolution,
                    property,
                } => {
                    let id = run_record_finding(
                        &pool,
                        RecordFinding {
                            kind: &kind,
                            severity: &severity,
                            contract: &contract,
                            message: &message,
                            source_file: source_file.as_deref(),
                            source_row,
                            proposed_resolution: proposed_resolution.as_deref(),
                            property: property.as_deref(),
                        },
                    )
                    .await?;
                    println!("{}", serde_json::json!({ "kind": kind, "id": id }));
                    Ok(())
                }
                Command::Desk { period } => {
                    let period = period.unwrap_or_else(crate::tui::current_period);
                    crate::tui::run_desk(pool, period).await
                }
                Command::Capabilities { command } => match command {
                    CapabilitiesCommand::List => {
                        println!(
                            "{}",
                            serde_json::to_string_pretty(&db::list_capabilities(&pool).await?)?
                        );
                        Ok(())
                    }
                },
                Command::Memories => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&db::list_memories(&pool).await?)?
                    );
                    Ok(())
                }
                Command::DataSources => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&crate::data_registry::registry())?
                    );
                    Ok(())
                }
                Command::Tools => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&crate::tools::registry())?
                    );
                    Ok(())
                }
                Command::T12 { property, period } => {
                    let stmt = t12::assemble_t12(&pool, &property, period.as_deref()).await?;
                    println!("{}", serde_json::to_string_pretty(&stmt)?);
                    Ok(())
                }
                Command::Ask { question, narrate } => {
                    let provider = crate::model_provider::provider_from_env()
                        .map_err(|e| anyhow::anyhow!("{e}"))?;
                    eprintln!("provider: {}", provider.model());
                    let result = ask::run_ask(&pool, provider.as_ref(), &question, narrate).await?;
                    for section in &result.rendered {
                        println!("{}", section.trim_end());
                    }
                    eprintln!(
                        "{} tool calls · {} in / {} out tokens · model {}",
                        result.tool_calls, result.input_tokens, result.output_tokens, result.model,
                    );
                    Ok(())
                }
                Command::Calls { command } => match command {
                    CallsCommand::Record {
                        property,
                        call_type,
                        mature_by,
                        confidence,
                        payload,
                        origin_period,
                        decision_kind,
                        entities,
                        outcome_mode,
                        value_class,
                        acted_on,
                        source,
                        context,
                        mature_by_override,
                    } => {
                        let prop = db::require_property_by_name(&pool, &property).await?;
                        if let Some(dk) = decision_kind {
                            // Decision-capture path (flywheel A).
                            let req = crate::capture::CaptureReq {
                                property_id: prop.id,
                                origin_period,
                                decision_kind: dk,
                                entities_json: entities.unwrap_or_else(|| "[]".to_string()),
                                outcome_mode: outcome_mode.unwrap_or_else(|| "human".to_string()),
                                value_class: value_class.unwrap_or_else(|| "value".to_string()),
                                confidence,
                                acted_on,
                                accepted_recall: None,
                                source_surface: source.unwrap_or_else(|| "cli".to_string()),
                                context_json: context,
                                mature_by: mature_by_override,
                            };
                            let id = crate::capture::record(&pool, req).await?;
                            println!(
                                "{}",
                                serde_json::json!({ "id": id, "call_type": "decision" })
                            );
                        } else {
                            // Legacy path: explicit call_type + payload + mature_by.
                            let _: serde_json::Value = serde_json::from_str(&payload)
                                .map_err(|e| anyhow::anyhow!("payload is not valid JSON: {e}"))?;
                            let id = db::insert_call(
                                &pool,
                                &prop.id,
                                &origin_period,
                                &call_type,
                                &mature_by,
                                confidence,
                                &payload,
                                None,
                            )
                            .await?;
                            println!(
                                "{}",
                                serde_json::json!({ "id": id, "call_type": call_type })
                            );
                        }
                        Ok(())
                    }
                    CallsCommand::List => {
                        let calls = db::list_calls(&pool).await?;
                        println!("{}", serde_json::to_string_pretty(&calls)?);
                        Ok(())
                    }
                    CallsCommand::Score { period } => {
                        let n = crate::calls::score_due_calls(&pool, &period).await?;
                        println!("{}", serde_json::json!({ "period": period, "scored": n }));
                        Ok(())
                    }
                    CallsCommand::TrackRecord {
                        property,
                        call_type,
                    } => {
                        let mems = db::recent_track_record_memories(&pool, 100).await?;
                        let property_id = match property {
                            Some(p) => Some(db::require_property_by_name(&pool, &p).await?.id),
                            None => None,
                        };
                        let filtered: Vec<_> = mems
                            .into_iter()
                            .filter(|m| {
                                property_id
                                    .as_deref()
                                    .map(|pid| m.key == pid)
                                    .unwrap_or(true)
                            })
                            .filter(|m| call_type.as_deref().map(|t| m.scope == t).unwrap_or(true))
                            .collect();
                        println!("{}", serde_json::to_string_pretty(&filtered)?);
                        Ok(())
                    }
                    CallsCommand::Backfill {
                        property,
                        through,
                        lookback,
                    } => {
                        let (analyzed, scored) = crate::calls::backfill_noi_diagnosis(
                            &pool, &property, &through, lookback,
                        )
                        .await?;
                        println!(
                            "{}",
                            serde_json::json!({
                                "property": property,
                                "through": through,
                                "lookback": lookback,
                                "periods_analyzed": analyzed,
                                "calls_scored": scored,
                            })
                        );
                        Ok(())
                    }
                    CallsCommand::BackfillT12 {
                        property,
                        through,
                        from,
                    } => {
                        let (periods, scored) = crate::calls::backfill_t12_reversion(
                            &pool,
                            &property,
                            &through,
                            from.as_deref(),
                        )
                        .await?;
                        println!(
                            "{}",
                            serde_json::json!({ "property": property, "through": through, "periods_emitted": periods, "calls_scored": scored })
                        );
                        Ok(())
                    }
                    CallsCommand::Import {
                        call_type,
                        property,
                        period,
                        file,
                    } => {
                        let n = crate::calls::import_calls_from_csv(
                            &pool, &call_type, &property, &period, &file,
                        )
                        .await?;
                        println!(
                            "{}",
                            serde_json::json!({ "imported": n, "call_type": call_type })
                        );
                        Ok(())
                    }
                    CallsCommand::ReversionReport {
                        call_type,
                        property,
                    } => {
                        let stats =
                            crate::calls::reversion_report(&pool, &call_type, property.as_deref())
                                .await?;
                        println!("{}", serde_json::to_string_pretty(&stats)?);
                        Ok(())
                    }
                    CallsCommand::PendingOutcomes => {
                        let pending = db::fetch_pending_outcomes(&pool).await?;
                        println!("{}", serde_json::to_string_pretty(&pending)?);
                        Ok(())
                    }
                    CallsCommand::Resolve { id, outcome, by } => {
                        crate::calls::resolve_human(&pool, &id, &outcome, &by).await?;
                        println!("{}", serde_json::json!({ "id": id, "status": "scored" }));
                        Ok(())
                    }
                    CallsCommand::CalibrationEval {
                        property,
                        call_type,
                    } => {
                        let result =
                            run_calibration_eval(&pool, property.as_deref(), call_type.as_deref())
                                .await?;
                        println!("{}", serde_json::to_string_pretty(&result)?);
                        Ok(())
                    }
                    CallsCommand::PropertyModel { property, as_of } => {
                        let today = as_of.unwrap_or_else(|| db::now_iso()[..7].to_string());
                        let model =
                            crate::property_model::build_property_model(&pool, &property, &today)
                                .await?;
                        println!("{}", serde_json::to_string_pretty(&model)?);
                        Ok(())
                    }
                    CallsCommand::Recall {
                        property,
                        call_type,
                        decision_kind,
                        entities,
                        as_of,
                        k,
                    } => {
                        let prop = db::require_property_by_name(&pool, &property).await?;
                        let today = as_of.unwrap_or_else(|| db::now_iso()[..7].to_string());
                        let entity_keys: Vec<String> = match entities {
                            Some(ref s) => serde_json::from_str(s)?,
                            None => Vec::new(),
                        };
                        let ctx = crate::recall::RecallCtx {
                            property_id: prop.id.clone(),
                            call_type: call_type.clone(),
                            decision_kind,
                            entity_keys,
                        };
                        let r = crate::recall::recall(&pool, &ctx, &today, k).await?;
                        let neighbors: Vec<_> = r
                            .neighbors
                            .iter()
                            .map(|n| {
                                serde_json::json!({
                                    "id": n.id,
                                    "summary": n.summary,
                                    "score": n.score,
                                    "status": n.status,
                                    "age_months": n.age_months,
                                    "similarity": n.similarity,
                                })
                            })
                            .collect();
                        let out = serde_json::json!({
                            "property": prop.name,
                            "property_id": prop.id,
                            "call_type": call_type.unwrap_or_else(|| "decision".to_string()),
                            "today_period": today,
                            "stat": {
                                "posterior_mean": r.stat.posterior_mean,
                                "n_eff": r.stat.n_eff,
                                "lo90": r.stat.lo90,
                                "hi90": r.stat.hi90,
                                "abstain": r.stat.abstain,
                            },
                            "neighbors": neighbors,
                        });
                        println!("{}", serde_json::to_string_pretty(&out)?);
                        Ok(())
                    }
                },
                Command::Onboard {
                    name,
                    property,
                    dir,
                    key,
                    property_ids,
                    units,
                } => {
                    let summary = run_onboard(
                        &pool,
                        OnboardRequest {
                            name: &name,
                            property: &property,
                            dir: &dir,
                            key: key.as_deref(),
                            property_ids: &property_ids,
                            units,
                        },
                    )
                    .await?;
                    println!("{}", serde_json::to_string_pretty(&summary)?);
                    Ok(())
                }
                Command::Sources { command } => run_sources_command(command),
                Command::Server => Ok(()),
            }
        }
    }
}

/// The 4 engine types tracked by the decision-outcome flywheel.
const VALUE_CLASS_CALL_TYPES: &[&str] = &[
    "noi_diagnosis",
    "t12_reversion",
    "delinquency_risk",
    "renewal_rec",
];

/// §6.5 honesty gate: reliability-diagram bins + ECE for each engine call type.
///
/// Aggregates (confidence, score) pairs across all properties (or a single
/// filtered property) for each of the 4 value-class engine types. Rows where
/// either `confidence` or `score` is `None` are skipped.
async fn run_calibration_eval(
    pool: &sqlx::SqlitePool,
    property_filter: Option<&str>,
    call_type_filter: Option<&str>,
) -> anyhow::Result<serde_json::Value> {
    // Resolve the property set to iterate.
    let properties = if let Some(name) = property_filter {
        vec![db::require_property_by_name(pool, name).await?]
    } else {
        db::list_properties(pool).await?
    };

    let types: Vec<&str> = VALUE_CLASS_CALL_TYPES
        .iter()
        .copied()
        .filter(|t| call_type_filter.map(|f| *t == f).unwrap_or(true))
        .collect();

    let mut out = serde_json::Map::new();
    for call_type in types {
        // Collect (confidence, score) pairs across all relevant properties.
        let mut pairs: Vec<(f64, f64)> = Vec::new();
        for prop in &properties {
            let calls = db::fetch_scored_calls(pool, &prop.id, call_type, 10_000).await?;
            for call in calls {
                if let (Some(conf), Some(score)) = (call.confidence, call.score) {
                    pairs.push((conf, score));
                }
            }
        }
        let ece = calibration::ece(&pairs);
        let bins = calibration::reliability_bins(&pairs, 10);
        out.insert(
            call_type.to_string(),
            serde_json::json!({
                "n_pairs": pairs.len(),
                "ece": ece,
                "bins": bins,
            }),
        );
    }
    Ok(serde_json::Value::Object(out))
}

/// Run the native data-contract validator over one or all lanes, persisting
/// findings into Boxscore's learning loop (gaps + evidence + capabilities) and
/// returning `true` when any ERROR was found (so the caller exits non-zero).
async fn run_validate(
    pool: &sqlx::SqlitePool,
    lane: Option<&str>,
    all: bool,
    json: bool,
) -> Result<bool> {
    let lanes: Vec<PropertyLane> = if all {
        load_lanes()
    } else {
        let key = lane.ok_or_else(|| anyhow::anyhow!("provide --lane <key> or --all"))?;
        vec![lane_by_key(key).ok_or_else(|| anyhow::anyhow!("unknown standardized source lane"))?]
    };

    let task_run_id = db::create_task_run(
        pool,
        "data_contract_validation",
        &format!(
            "boxscore validate {}",
            if all {
                "--all".to_string()
            } else {
                format!("--lane {}", lane.unwrap_or(""))
            }
        ),
    )
    .await?;

    let mut results = Vec::new();
    let mut any_error = false;
    let mut total_gaps = 0_usize;
    // Track contract groups that ERROR across more than one lane -> systemic.
    let mut error_contract_counts: std::collections::BTreeMap<String, usize> =
        std::collections::BTreeMap::new();

    for lane in &lanes {
        let result = validator::validate_lane(lane);
        if !result.ok() {
            any_error = true;
        }
        let mut seen_contracts_this_lane: std::collections::BTreeSet<String> =
            std::collections::BTreeSet::new();
        for finding in &result.findings {
            if matches!(finding.severity, Severity::Error | Severity::Warn) {
                let severity = match finding.severity {
                    Severity::Error => "error",
                    Severity::Warn => "warning",
                    Severity::Info => "info",
                };
                let description = format!(
                    "[{}] {} ({}): {}",
                    result.property_name,
                    finding.contract,
                    finding.severity.as_str(),
                    finding.message
                );
                let gap_id = db::insert_gap(
                    pool,
                    &task_run_id,
                    "data_contract_violation",
                    severity,
                    &description,
                    "A standardized-CSV data contract failed; owner-facing RPCOE/BDDRE figures \
                     derived from this lane cannot be trusted until the contract passes.",
                    "Fix the upstream ETL/standardizer so the contract passes; re-run \
                     `boxscore validate` to confirm. Do not publish owner reports for this lane \
                     while the contract is in ERROR.",
                )
                .await?;
                total_gaps += 1;
                db::insert_evidence(
                    pool,
                    db::NewEvidence {
                        task_run_id: &task_run_id,
                        source_type: "data_contract",
                        source_table: "standardized_csv",
                        source_id: Some(&gap_id),
                        source_file: finding.source_file.as_deref(),
                        source_row: finding.source_row,
                        claim: &description,
                    },
                )
                .await?;
                if finding.severity == Severity::Error {
                    seen_contracts_this_lane.insert(finding.contract.clone());
                }
            }
        }
        for contract in seen_contracts_this_lane {
            *error_contract_counts.entry(contract).or_default() += 1;
        }
        results.push(result);
    }

    // Propose a systemic capability when a contract group fails on >1 lane, or
    // any ERROR exists at all (the recurring-failure signal for the evolution loop).
    let systemic: Vec<&String> = error_contract_counts
        .iter()
        .filter(|(_, count)| **count >= 1)
        .map(|(contract, _)| contract)
        .collect();
    if !systemic.is_empty() {
        let recurring: Vec<&String> = error_contract_counts
            .iter()
            .filter(|(_, count)| **count >= 2)
            .map(|(c, _)| c)
            .collect();
        let trigger = if recurring.is_empty() {
            format!(
                "Data-contract ERROR(s) detected on lane(s): {}.",
                results
                    .iter()
                    .filter(|r| !r.ok())
                    .map(|r| r.property_name.clone())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        } else {
            format!(
                "Recurring data-contract ERROR across multiple lanes for contract group(s): {}.",
                recurring
                    .iter()
                    .map(|c| c.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        };
        let proposal = evolution::CapabilityProposal {
            title: "Auto-remediate recurring data-contract violations".to_string(),
            description:
                "When a standardized-CSV data contract fails, trace it to the responsible ETL/standardizer \
                 step and propose (or apply) a targeted fix so the same class of defect stops recurring."
                    .to_string(),
            trigger_reason: trigger,
            expected_value:
                "Stops bad/missing data (footer rows, broken concession flow-through, bucket-less AR, \
                 0/0/0 elasticity) from reaching owner-facing reports; keeps close-readiness honest."
                    .to_string(),
            implementation_hint:
                "Map each contract group (rent_roll/concessions/aged_receivables/rpcoe_elasticity) to its \
                 upstream ETL; add regression fixtures mirroring data_validation.py and re-run on every ETL."
                    .to_string(),
            priority: 1,
        };
        evolution::persist_capabilities(pool, &[proposal]).await?;
    }

    db::complete_task_run(
        pool,
        &task_run_id,
        if any_error { "failed" } else { "completed" },
        None,
        Some(&format!(
            "{} lane(s) validated, {} gap(s) recorded.",
            results.len(),
            total_gaps
        )),
    )
    .await?;

    if json {
        println!("{}", serde_json::to_string_pretty(&results)?);
    } else {
        print_validate_summary(&results, total_gaps);
    }

    Ok(any_error)
}

fn print_validate_summary(results: &[validator::ValidationResult], total_gaps: usize) {
    for result in results {
        println!(
            "== {} [{}] — {} ({} error, {} warn) ==",
            result.property_name,
            result.property_key,
            result.status,
            result.error_count,
            result.warning_count
        );
        for finding in validator::worst_per_contract(result) {
            let loc = match (&finding.source_file, finding.source_row) {
                (Some(f), Some(r)) => format!(" [{f}:{r}]"),
                (Some(f), None) => format!(" [{f}]"),
                _ => String::new(),
            };
            println!(
                "  [{}] {}{}: {}",
                finding.severity.as_str(),
                finding.contract,
                loc,
                finding.message
            );
        }
        println!();
    }
    let any_error = results.iter().any(|r| !r.ok());
    println!(
        "OVERALL: {} — contract set {}, {} gap(s) recorded.",
        if any_error {
            "FAIL — contract violations found"
        } else {
            "PASS"
        },
        validator::CONTRACT_SET_VERSION,
        total_gaps
    );
}

/// Parameters for `boxscore record-finding`, threaded as one struct to keep the
/// argument list manageable.
struct RecordFinding<'a> {
    kind: &'a str,
    severity: &'a str,
    contract: &'a str,
    message: &'a str,
    source_file: Option<&'a str>,
    source_row: Option<i64>,
    proposed_resolution: Option<&'a str>,
    property: Option<&'a str>,
}

/// Persist a finding from the standing agentic report-review into the learning
/// loop, reusing the Phase-2 helpers.
///
/// - `--kind gap` (default): opens a gap (gap_type "agentic_review_finding") under
///   a reusable "agentic_review" task_run, attaching evidence when a source file is
///   cited. Returns the gap id.
/// - `--kind capability`: records a capability_backlog entry proposing a NEW
///   contract `Cn` for a failure class the C1-C5 contracts would not catch.
///   Returns the capability id.
async fn run_record_finding(pool: &sqlx::SqlitePool, f: RecordFinding<'_>) -> Result<String> {
    let severity = match f.severity {
        "error" | "warn" | "info" => f.severity,
        other => {
            return Err(anyhow::anyhow!(
                "invalid --severity {other}; use error|warn|info"
            ))
        }
    };
    let scope = f.property.unwrap_or("portfolio");

    // Reuse a single open agentic_review task_run if one exists, otherwise create
    // one — keeps every finding from one review cycle linked to the same run.
    let task_run_id = reuse_or_create_agentic_review_run(pool).await?;

    match f.kind {
        "gap" => {
            let description = format!("[{scope}] {} — {}", f.contract, f.message);
            let proposed = f.proposed_resolution.unwrap_or(
                "Trace the headline figure back to the Standardized CSV rows that produced it; \
                 fix the upstream ETL/standardizer or report template so the figure ties out, \
                 then re-run the review.",
            );
            let gap_id = db::insert_gap(
                pool,
                &task_run_id,
                "agentic_review_finding",
                severity,
                &description,
                "The standing adversarial report-review found an owner-facing figure that does not \
                 reconcile to its source data; publishing it would mislead owners (this is the same \
                 failure class as the $0-rent 1152-S unit and the $158K footer row).",
                proposed,
            )
            .await?;
            if let Some(source_file) = f.source_file {
                db::insert_evidence(
                    pool,
                    db::NewEvidence {
                        task_run_id: &task_run_id,
                        source_type: "agentic_review",
                        source_table: "report_or_standardized_csv",
                        source_id: Some(&gap_id),
                        source_file: Some(source_file),
                        source_row: f.source_row,
                        claim: &description,
                    },
                )
                .await?;
            }
            db::complete_task_run(
                pool,
                &task_run_id,
                "completed",
                None,
                Some("agentic report-review finding recorded"),
            )
            .await?;
            Ok(gap_id)
        }
        "capability" => {
            // A novel failure class the contracts miss -> propose a new contract Cn.
            let trigger = format!(
                "[{scope}] Agentic report-review found a NOVEL failure class on contract {} that \
                 C1-C5 would not catch: {}",
                f.contract, f.message
            );
            let implementation_hint =
                f.proposed_resolution
                    .map(str::to_string)
                    .unwrap_or_else(|| {
                        format!(
                    "Add a new data contract {} to the native validator (boxscore validate) with a \
                     precise, testable definition mirroring data_validation.py; add a regression \
                     fixture reproducing this finding so the class cannot recur silently.",
                    f.contract
                )
                    });
            let proposal = evolution::CapabilityProposal {
                title: format!("Propose new contract {}", f.contract),
                description: format!(
                    "Codify the agentic report-review finding into a standing contract: {}",
                    f.message
                ),
                trigger_reason: trigger,
                expected_value:
                    "Converts a one-off adversarial catch into a permanent automated gate, so the \
                     same class of owner-facing error is caught on every future reporting cycle."
                        .to_string(),
                implementation_hint,
                priority: match severity {
                    "error" => 1,
                    "warn" => 2,
                    _ => 3,
                },
            };
            let ids = evolution::persist_capabilities(pool, &[proposal]).await?;
            db::complete_task_run(
                pool,
                &task_run_id,
                "completed",
                None,
                Some("agentic report-review capability (new contract proposal) recorded"),
            )
            .await?;
            ids.into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("capability proposal was not persisted"))
        }
        other => Err(anyhow::anyhow!(
            "invalid --kind {other}; use gap|capability"
        )),
    }
}

/// Reuse the most recent still-running `agentic_review` task_run, or create one.
async fn reuse_or_create_agentic_review_run(pool: &sqlx::SqlitePool) -> Result<String> {
    let existing: Option<String> = sqlx::query_scalar(
        "SELECT id FROM task_runs WHERE task_type = 'agentic_review' AND status = 'running' \
         ORDER BY started_at DESC LIMIT 1",
    )
    .fetch_optional(pool)
    .await?;
    if let Some(id) = existing {
        return Ok(id);
    }
    db::create_task_run(
        pool,
        "agentic_review",
        "boxscore record-finding (standing adversarial report-review)",
    )
    .await
}

async fn ingest_standardized_gl(
    pool: &sqlx::SqlitePool,
    lane: Option<&str>,
    all: bool,
) -> Result<Vec<crate::connectors::standardized::StandardizedIngestSummary>> {
    if all {
        let mut summaries = Vec::new();
        for lane in load_lanes() {
            summaries.push(ingest_budget_comparison_for_lane(pool, &lane).await?);
        }
        return Ok(summaries);
    }

    let lane_key = lane.ok_or_else(|| anyhow::anyhow!("provide --lane <key> or --all"))?;
    let lane =
        lane_by_key(lane_key).ok_or_else(|| anyhow::anyhow!("unknown standardized source lane"))?;
    Ok(vec![ingest_budget_comparison_for_lane(pool, &lane).await?])
}

async fn ingest_standardized_ops(
    pool: &sqlx::SqlitePool,
    lane: Option<&str>,
    all: bool,
) -> Result<Vec<crate::connectors::standardized::StandardizedIngestSummary>> {
    if all {
        let mut summaries = Vec::new();
        for lane in load_lanes() {
            summaries.push(ingest_operating_snapshots_for_lane(pool, &lane).await?);
        }
        return Ok(summaries);
    }

    let lane_key = lane.ok_or_else(|| anyhow::anyhow!("provide --lane <key> or --all"))?;
    let lane =
        lane_by_key(lane_key).ok_or_else(|| anyhow::anyhow!("unknown standardized source lane"))?;
    Ok(vec![
        ingest_operating_snapshots_for_lane(pool, &lane).await?,
    ])
}

async fn ingest_standardized_collections(
    pool: &sqlx::SqlitePool,
    lane: Option<&str>,
    all: bool,
) -> Result<Vec<crate::connectors::standardized::StandardizedIngestSummary>> {
    if all {
        let mut summaries = Vec::new();
        for lane in load_lanes() {
            summaries.push(ingest_collections_for_lane(pool, &lane).await?);
        }
        return Ok(summaries);
    }

    let lane_key = lane.ok_or_else(|| anyhow::anyhow!("provide --lane <key> or --all"))?;
    let lane =
        lane_by_key(lane_key).ok_or_else(|| anyhow::anyhow!("unknown standardized source lane"))?;
    Ok(vec![ingest_collections_for_lane(pool, &lane).await?])
}

async fn ingest_standardized_transactions(
    pool: &sqlx::SqlitePool,
    lane: Option<&str>,
    all: bool,
    since: Option<&str>,
) -> Result<Vec<crate::connectors::standardized::StandardizedIngestSummary>> {
    // Normalize the --since label up front so "2025-1" and "2025-01" behave
    // identically and bad labels fail fast.
    let since = since.map(crate::db::normalize_period_label).transpose()?;
    if all {
        let mut summaries = Vec::new();
        for lane in load_lanes() {
            summaries.push(ingest_gl_transactions_for_lane(pool, &lane, since.as_deref()).await?);
        }
        return Ok(summaries);
    }

    let lane_key = lane.ok_or_else(|| anyhow::anyhow!("provide --lane <key> or --all"))?;
    let lane =
        lane_by_key(lane_key).ok_or_else(|| anyhow::anyhow!("unknown standardized source lane"))?;
    Ok(vec![
        ingest_gl_transactions_for_lane(pool, &lane, since.as_deref()).await?,
    ])
}

async fn ingest_standardized_turn_costs(
    pool: &sqlx::SqlitePool,
    lane: Option<&str>,
    all: bool,
) -> Result<Vec<crate::connectors::standardized::StandardizedIngestSummary>> {
    if all {
        let mut summaries = Vec::new();
        for lane in load_lanes() {
            summaries.push(ingest_turn_costs_for_lane(pool, &lane).await?);
        }
        return Ok(summaries);
    }

    let lane_key = lane.ok_or_else(|| anyhow::anyhow!("provide --lane <key> or --all"))?;
    let lane =
        lane_by_key(lane_key).ok_or_else(|| anyhow::anyhow!("unknown standardized source lane"))?;
    Ok(vec![ingest_turn_costs_for_lane(pool, &lane).await?])
}

async fn ingest_standardized_unit_pnl(
    pool: &sqlx::SqlitePool,
    lane: Option<&str>,
    all: bool,
) -> Result<Vec<crate::connectors::standardized::StandardizedIngestSummary>> {
    if all {
        let mut summaries = Vec::new();
        for lane in load_lanes() {
            summaries.push(ingest_unit_pnl_for_lane(pool, &lane).await?);
        }
        return Ok(summaries);
    }

    let lane_key = lane.ok_or_else(|| anyhow::anyhow!("provide --lane <key> or --all"))?;
    let lane =
        lane_by_key(lane_key).ok_or_else(|| anyhow::anyhow!("unknown standardized source lane"))?;
    Ok(vec![ingest_unit_pnl_for_lane(pool, &lane).await?])
}

fn run_sources_command(command: SourcesCommand) -> Result<()> {
    match command {
        SourcesCommand::List => {
            println!("{}", serde_json::to_string_pretty(&load_lanes())?);
        }
        SourcesCommand::Inspect { lane } => {
            let lane = lane_by_key(&lane)
                .ok_or_else(|| anyhow::anyhow!("unknown standardized source lane"))?;
            println!("{}", serde_json::to_string_pretty(&inspect_lane(&lane))?);
        }
        SourcesCommand::Profile { lane, all } => {
            if all {
                let profiles = load_lanes().iter().map(profile_lane).collect::<Vec<_>>();
                println!("{}", serde_json::to_string_pretty(&profiles)?);
            } else {
                let lane_key = lane
                    .as_deref()
                    .ok_or_else(|| anyhow::anyhow!("provide --lane <key> or --all"))?;
                let lane = lane_by_key(lane_key)
                    .ok_or_else(|| anyhow::anyhow!("unknown standardized source lane"))?;
                println!("{}", serde_json::to_string_pretty(&profile_lane(&lane))?);
            }
        }
    }
    Ok(())
}

/// Parameters for `boxscore onboard`, threaded as one struct to keep the
/// argument list manageable.
struct OnboardRequest<'a> {
    name: &'a str,
    property: &'a str,
    dir: &'a std::path::Path,
    key: Option<&'a str>,
    property_ids: &'a [String],
    units: Option<u32>,
}

/// Machine-readable result of an onboard run.
#[derive(Debug, serde::Serialize)]
struct OnboardSummary {
    owner: String,
    lane_key: String,
    display_name: String,
    config_path: String,
    standardized_path: String,
    /// Expected Standardized files that were NOT found under `<dir>/Standardized/`.
    missing_files: Vec<String>,
    /// One entry per ingest step that ran (skipped steps are omitted).
    ingested: Vec<crate::connectors::standardized::StandardizedIngestSummary>,
    /// Ingest steps skipped because their source file was absent.
    skipped_steps: Vec<String>,
}

/// Slugify a property name into a lane key: lowercase, alphanumerics kept,
/// runs of anything else collapsed to a single `-`, trimmed.
fn slugify(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut prev_dash = false;
    for ch in input.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
            prev_dash = false;
        } else if !prev_dash && !out.is_empty() {
            out.push('-');
            prev_dash = true;
        }
    }
    out.trim_matches('-').to_string()
}

/// Onboard a prospect's property in one flow: register a config-driven lane in
/// `lanes.json` rooted at `--dir`, then ingest whatever Standardized CSVs are
/// present (GL budget bridge, operating snapshots, collections, GL
/// transactions), reusing the existing per-lane ingest machinery. Ingest steps
/// whose source file is absent are reported (not errors) so a partial export
/// still onboards.
async fn run_onboard(pool: &sqlx::SqlitePool, req: OnboardRequest<'_>) -> Result<OnboardSummary> {
    let key = match req.key {
        Some(k) if !k.trim().is_empty() => slugify(k),
        _ => slugify(req.property),
    };
    if key.is_empty() {
        return Err(anyhow::anyhow!(
            "could not derive a lane key from --property {:?}; pass --key explicitly",
            req.property
        ));
    }

    let root_path = std::fs::canonicalize(req.dir).unwrap_or_else(|_| req.dir.to_path_buf());
    let standardized_path = root_path.join("Standardized");
    let raw_data_path = root_path.join("Data");

    let lane = PropertyLane {
        property_key: key.clone(),
        display_name: req.property.to_string(),
        standardized_path: standardized_path.clone(),
        raw_data_path,
        root_path,
        primary_property_ids: req.property_ids.to_vec(),
        unit_count_hint: req.units,
    };

    // Persist the lane into lanes.json (creates the file on first onboard).
    let config_path = upsert_lane_config(&lane)?;

    // Report which expected Standardized files are missing.
    let missing_files: Vec<String> = inspect_lane(&lane)
        .into_iter()
        .filter(|status| !status.exists)
        .map(|status| status.file_name)
        .collect();

    // Run each ingest step only when its required source file(s) exist, so a
    // prospect with a partial export still onboards cleanly.
    let mut ingested = Vec::new();
    let mut skipped_steps = Vec::new();
    let std_dir = &standardized_path;

    if std_dir.join("budget_comparison.csv").exists() {
        ingested.push(ingest_budget_comparison_for_lane(pool, &lane).await?);
    } else {
        skipped_steps.push("gl (budget_comparison.csv)".to_string());
    }

    // Operating snapshots reads rent_roll + aged_receivables + leasing_funnel;
    // require all three before running.
    let ops_files = [
        "rent_roll.csv",
        "aged_receivables.csv",
        "leasing_funnel.csv",
    ];
    if ops_files.iter().all(|f| std_dir.join(f).exists()) {
        ingested.push(ingest_operating_snapshots_for_lane(pool, &lane).await?);
    } else {
        skipped_steps.push("ops (rent_roll/aged_receivables/leasing_funnel)".to_string());
    }

    if std_dir.join("collections_unified.csv").exists() {
        ingested.push(ingest_collections_for_lane(pool, &lane).await?);
    } else {
        skipped_steps.push("collections (collections_unified.csv)".to_string());
    }

    if std_dir.join("gl_transactions.parquet").exists() {
        ingested.push(ingest_gl_transactions_for_lane(pool, &lane, None).await?);
    } else {
        skipped_steps.push("transactions (gl_transactions.parquet)".to_string());
    }

    Ok(OnboardSummary {
        owner: req.name.to_string(),
        lane_key: key,
        display_name: req.property.to_string(),
        config_path: config_path.to_string_lossy().to_string(),
        standardized_path: standardized_path.to_string_lossy().to_string(),
        missing_files,
        ingested,
        skipped_steps,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db;

    #[tokio::test]
    async fn record_finding_inserts_gap_with_evidence() {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();

        let id = run_record_finding(
            &pool,
            RecordFinding {
                kind: "gap",
                severity: "error",
                contract: "review:noi_bridge_tieout",
                message: "RPCOE headline opportunity $158K traces to a footer row, not unit data",
                source_file: Some("data/maplewood/reports/rpcoe_weekly_report.md"),
                source_row: Some(42),
                proposed_resolution: None,
                property: Some("maplewood"),
            },
        )
        .await
        .unwrap();

        let gaps = db::list_gaps(&pool).await.unwrap();
        assert_eq!(gaps.len(), 1);
        let gap = &gaps[0];
        assert_eq!(gap.id, id);
        assert_eq!(gap.gap_type, "agentic_review_finding");
        assert_eq!(gap.severity, "error");
        assert_eq!(gap.status, "open");
        assert!(gap.description.contains("maplewood"));
        assert!(gap.description.contains("review:noi_bridge_tieout"));

        // Evidence linked to the gap and pointing at the cited source file.
        let evidence_count: i64 =
            sqlx::query_scalar("SELECT COUNT(*) FROM evidence_items WHERE source_id = ?")
                .bind(&id)
                .fetch_one(&pool)
                .await
                .unwrap();
        assert_eq!(evidence_count, 1);

        // The task_run was created with the agentic_review type.
        let run_type: String = sqlx::query_scalar("SELECT task_type FROM task_runs WHERE id = ?")
            .bind(&gap.task_run_id)
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(run_type, "agentic_review");
    }

    #[tokio::test]
    async fn record_finding_inserts_capability_for_novel_class() {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();

        let id = run_record_finding(
            &pool,
            RecordFinding {
                kind: "capability",
                severity: "warn",
                contract: "C6:concession_signflip",
                message:
                    "Concession dollars appear with the wrong sign on the BDDRE exposure roll-up",
                source_file: None,
                source_row: None,
                proposed_resolution: None,
                property: None,
            },
        )
        .await
        .unwrap();

        let caps = db::list_capabilities(&pool).await.unwrap();
        assert_eq!(caps.len(), 1);
        let cap = &caps[0];
        assert_eq!(cap.id, id);
        assert_eq!(cap.status, "proposed");
        assert!(cap.title.contains("C6:concession_signflip"));
        assert!(cap.trigger_reason.contains("NOVEL failure class"));
        // warn severity maps to priority 2.
        assert_eq!(cap.priority, 2);

        // No gap should be created for a capability-kind finding.
        assert!(db::list_gaps(&pool).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn record_finding_reuses_open_review_run() {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();

        // Pre-seed an open agentic_review run; both findings should reuse it.
        // (complete_task_run flips status to 'completed', so we re-open between
        // calls to model a single in-progress review cycle.)
        let run_a = reuse_or_create_agentic_review_run(&pool).await.unwrap();
        let run_b = reuse_or_create_agentic_review_run(&pool).await.unwrap();
        assert_eq!(run_a, run_b);
    }

    #[tokio::test]
    async fn record_finding_rejects_bad_kind_and_severity() {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();

        let bad_kind = run_record_finding(
            &pool,
            RecordFinding {
                kind: "memory",
                severity: "warn",
                contract: "review:x",
                message: "m",
                source_file: None,
                source_row: None,
                proposed_resolution: None,
                property: None,
            },
        )
        .await;
        assert!(bad_kind.is_err());

        let bad_sev = run_record_finding(
            &pool,
            RecordFinding {
                kind: "gap",
                severity: "critical",
                contract: "review:x",
                message: "m",
                source_file: None,
                source_row: None,
                proposed_resolution: None,
                property: None,
            },
        )
        .await;
        assert!(bad_sev.is_err());
    }

    #[test]
    fn slugify_produces_clean_lane_keys() {
        assert_eq!(slugify("Vantage at Yieldmore"), "vantage-at-yieldmore");
        assert_eq!(
            slugify("  The Reserve @ Cap-Rate Cove "),
            "the-reserve-cap-rate-cove"
        );
        assert_eq!(slugify("Promote_Pointe!!!"), "promote-pointe");
        assert_eq!(slugify("---"), "");
    }

    /// Serializes onboard tests that mutate the process-global `$BOXSCORE_LANES`.
    static ONBOARD_ENV_GUARD: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Copy a full Standardized fixture set into `<dir>/Standardized/`.
    fn stage_standardized_fixtures(dir: &std::path::Path) {
        let std_dir = dir.join("Standardized");
        std::fs::create_dir_all(&std_dir).unwrap();
        let fixture_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/standardized/maplewood");
        for file in [
            "budget_comparison.csv",
            "rent_roll.csv",
            "aged_receivables.csv",
            "leasing_funnel.csv",
            "collections_unified.csv",
        ] {
            std::fs::copy(fixture_root.join(file), std_dir.join(file)).unwrap();
        }
    }

    // Holding the serialization guard across `.await` is deliberate: it fences
    // the env-set -> onboard -> env-remove window against other env-touching
    // tests in this binary. The runtime is current-thread, so no other task
    // contends for the lock during the await.
    #[allow(clippy::await_holding_lock)]
    #[tokio::test]
    async fn onboard_registers_lane_and_ingests_present_csvs() {
        let _guard = ONBOARD_ENV_GUARD.lock().unwrap_or_else(|e| e.into_inner());

        let pool = db::connect("sqlite::memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();

        let work = tempfile::tempdir().unwrap();
        let prop_dir = work.path().join("bagholder_flats");
        stage_standardized_fixtures(&prop_dir);

        // Point the lane config at an isolated temp file so onboarding never
        // writes the repo's lanes.json.
        let config_file = work.path().join("lanes.json");
        std::env::set_var("BOXSCORE_LANES", &config_file);

        let result = run_onboard(
            &pool,
            OnboardRequest {
                name: "Bagholder Capital",
                property: "Bagholder Flats",
                dir: &prop_dir,
                key: None,
                property_ids: &["s999".to_string()],
                units: Some(120),
            },
        )
        .await;

        // The lane must be registered + queryable before we clear the env, but
        // assert AFTER restoring env so a panic can't leak the override.
        let registered = load_lanes()
            .into_iter()
            .find(|l| l.property_key == "bagholder-flats");
        std::env::remove_var("BOXSCORE_LANES");

        let summary = result.unwrap();
        assert_eq!(summary.lane_key, "bagholder-flats");
        assert_eq!(summary.owner, "Bagholder Capital");
        // gl_transactions.parquet absent -> that step skipped, the other 3 ran.
        assert_eq!(summary.ingested.len(), 3, "gl/ops/collections ingested");
        assert!(summary
            .skipped_steps
            .iter()
            .any(|s| s.contains("transactions")));
        assert!(summary
            .missing_files
            .iter()
            .any(|f| f.contains("gl_transactions") || f == "chart_of_accounts.csv"));

        let lane = registered.expect("onboarded lane present in load_lanes()");
        assert_eq!(lane.display_name, "Bagholder Flats");
        assert_eq!(lane.unit_count_hint, Some(120));
        assert!(lane.primary_property_ids.contains(&"s999".to_string()));

        // The property + GL rows actually landed in the DB.
        let props = db::list_properties(&pool).await.unwrap();
        assert!(props.iter().any(|p| p.name == "Bagholder Flats"));
        let actual_rows: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM gl_actuals")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert!(
            actual_rows > 0,
            "budget_comparison.csv produced gl_actuals rows"
        );
    }
}
