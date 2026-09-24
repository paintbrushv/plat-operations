use axum::{
    extract::{Path, State},
    http::StatusCode,
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use sqlx::SqlitePool;
use std::{net::SocketAddr, path::PathBuf, sync::Arc};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

use crate::{config::AppConfig, db, questions, variance};

#[derive(Clone)]
pub struct ApiState {
    pub pool: SqlitePool,
    pub report_dir: PathBuf,
}

#[derive(Debug, Serialize)]
struct HealthResponse {
    status: &'static str,
    service: &'static str,
}

#[derive(Debug, Deserialize)]
pub struct AnswerQuestionRequest {
    pub answer: String,
}

#[derive(Debug, Serialize)]
struct AnswerQuestionResponse {
    status: &'static str,
}

pub async fn serve(config: AppConfig) -> anyhow::Result<()> {
    let pool = db::connect(&config.database_url).await?;
    db::init_database(&pool).await?;
    let app = router(pool, config.report_dir);
    let addr: SocketAddr = config.bind_addr.parse()?;
    tracing::info!(%addr, "starting Boxscore API server");
    let listener = TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

pub fn router(pool: SqlitePool, report_dir: PathBuf) -> Router {
    let state = Arc::new(ApiState { pool, report_dir });
    Router::new()
        .route("/health", get(health))
        .route("/properties", get(properties))
        .route("/task-runs", get(task_runs))
        .route("/task-runs/:id", get(task_run))
        .route("/analyze/variance", post(analyze_variance))
        .route("/validate", get(validate))
        .route("/gaps", get(gaps))
        .route("/questions", get(questions_list))
        .route("/questions/:id/answer", post(answer_question))
        .route("/capabilities", get(capabilities))
        .route("/memories", get(memories))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok",
        service: "boxscore",
    })
}

async fn properties(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::to_value(
        db::list_properties(&state.pool).await?,
    )?))
}

async fn task_runs(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::to_value(
        db::list_task_runs(&state.pool).await?,
    )?))
}

async fn task_run(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let task = sqlx::query_as::<_, crate::models::TaskRun>("SELECT * FROM task_runs WHERE id = ?")
        .bind(id)
        .fetch_optional(&state.pool)
        .await?;
    match task {
        Some(task) => Ok(Json(serde_json::to_value(task)?)),
        None => Err(ApiError::not_found("task run not found")),
    }
}

async fn analyze_variance(
    State(state): State<Arc<ApiState>>,
    Json(request): Json<variance::VarianceRequest>,
) -> Result<Json<variance::VarianceAnalysisResult>, ApiError> {
    let result = variance::analyze_variance(&state.pool, request, &state.report_dir).await?;
    Ok(Json(result))
}

/// Validate every built-in lane's Standardized/*.csv against the data contracts
/// (read-only; does not persist gaps — use `boxscore validate` for that).
async fn validate() -> Result<Json<serde_json::Value>, ApiError> {
    use crate::connectors::standardized::{source_registry::built_in_lanes, validator};
    let results = built_in_lanes()
        .iter()
        .map(validator::validate_lane)
        .collect::<Vec<_>>();
    Ok(Json(serde_json::to_value(results)?))
}

async fn gaps(State(state): State<Arc<ApiState>>) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::to_value(
        db::list_gaps(&state.pool).await?,
    )?))
}

async fn questions_list(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::to_value(
        db::list_questions(&state.pool).await?,
    )?))
}

async fn answer_question(
    State(state): State<Arc<ApiState>>,
    Path(id): Path<String>,
    Json(request): Json<AnswerQuestionRequest>,
) -> Result<Json<AnswerQuestionResponse>, ApiError> {
    questions::answer_question(&state.pool, &id, &request.answer).await?;
    Ok(Json(AnswerQuestionResponse { status: "answered" }))
}

async fn capabilities(
    State(state): State<Arc<ApiState>>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::to_value(
        db::list_capabilities(&state.pool).await?,
    )?))
}

async fn memories(State(state): State<Arc<ApiState>>) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::to_value(
        db::list_memories(&state.pool).await?,
    )?))
}

#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn not_found(message: &str) -> Self {
        Self {
            status: StatusCode::NOT_FOUND,
            message: message.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({ "error": self.message }));
        (self.status, body).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(value: anyhow::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: value.to_string(),
        }
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(value: sqlx::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: value.to_string(),
        }
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(value: serde_json::Error) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: value.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    #[tokio::test]
    async fn health_returns_ok() {
        let pool = db::connect("sqlite::memory:").await.unwrap();
        db::init_database(&pool).await.unwrap();
        let app = router(pool, PathBuf::from("reports/generated"));
        let response = app
            .oneshot(
                Request::builder()
                    .uri("/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::OK);
    }
}
