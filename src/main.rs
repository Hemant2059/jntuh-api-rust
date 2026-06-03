use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Query;
use axum::http::{HeaderName, Method};
use axum::routing::get;
use axum::{Json, Router};
use reqwest::Client;
use serde::Deserialize;
use tokio::signal;
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::compression::CompressionLayer;
use tracing_subscriber::EnvFilter;

mod types;
use types::*;

mod extract_code;
use extract_code::ExamCodes;

mod sem_result;
use sem_result::SemResultService;

mod full_result;
use full_result::AcademicService;

mod all_result;
use all_result::AllResultService;

mod multiple;
use multiple::ClassResultService;

mod notifications;
use notifications::NotificationsService;

mod specific_result;
use specific_result::SpecificResultService;

#[derive(Clone)]
struct AppState {
    client: Client,
    exam_codes: Arc<ExamCodes>,
    sem_results: Arc<SemResultService>,
    academic: Arc<AcademicService>,
    all_results: Arc<AllResultService>,
    class_results: Arc<ClassResultService>,
    notifications: Arc<NotificationsService>,
    specific_results: Arc<SpecificResultService>,
}

fn load_config() -> (String, String) {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8000".to_string());
    let exam_codes_path = std::env::var("EXAM_CODES_PATH").unwrap_or_else(|_| "exam_codes.json".to_string());
    (port, exam_codes_path)
}

#[tokio::main]
async fn main() {
    let log_format = std::env::var("LOG_FORMAT").unwrap_or_else(|_| "default".to_string());

    if log_format == "json" {
        tracing_subscriber::fmt()
            .json()
            .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
            .init();
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info")))
            .init();
    }

    let (port, exam_codes_path) = load_config();

    let client = Client::builder()
        .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36")
        .cookie_store(true)
        .timeout(Duration::from_secs(30))
        .pool_max_idle_per_host(30)
        .build()
        .expect("Failed to create HTTP client");

    let exam_codes = Arc::new(ExamCodes::new(&exam_codes_path));
    exam_codes.initialize(&client).await;

    let sem_results = Arc::new(SemResultService::new(client.clone(), exam_codes.clone()));
    let academic = Arc::new(AcademicService::new(sem_results.clone()));
    let all_results = Arc::new(AllResultService::new(client.clone(), exam_codes.clone()));
    let class_results = Arc::new(ClassResultService::new(sem_results.clone()));
    let notifications = Arc::new(NotificationsService::new(client.clone()));
    let specific_results = Arc::new(SpecificResultService::new(client.clone()));

    let state = AppState {
        client: client.clone(),
        exam_codes,
        sem_results,
        academic,
        all_results,
        class_results,
        notifications,
        specific_results,
    };

    let cors = CorsLayer::new()
        .allow_methods([Method::GET, Method::OPTIONS])
        .allow_origin(Any)
        .allow_headers(Any);

    let app = Router::new()
        .route("/", get(root))
        .route("/health", get(health))
        .route("/sem", get(get_sem_result))
        .route("/academic", get(get_academic_results))
        .route("/allresult", get(get_all_results))
        .route("/classresult", get(get_class_results))
        .route("/notifications", get(get_notifications))
        .route("/refresh-codes", get(refresh_codes))
        .route("/refresh-cache", get(refresh_cache))
        .route("/specificresult", get(get_specific_result))
        .layer(TimeoutLayer::with_status_code(axum::http::StatusCode::GATEWAY_TIMEOUT, Duration::from_secs(120)))
        .layer(CompressionLayer::new().gzip(true))
        .layer(SetRequestIdLayer::new(HeaderName::from_static("x-request-id"), MakeRequestUuid))
        .layer(cors)
        .with_state(state);

    let addr = format!("0.0.0.0:{}", port);
    tracing::info!("Starting server on {}", addr);

    let listener = tokio::net::TcpListener::bind(&addr).await.unwrap_or_else(|e| {
        tracing::error!("Failed to bind to {}: {}", addr, e);
        std::process::exit(1);
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .unwrap();
}

async fn shutdown_signal() {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install Ctrl+C handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        _ = ctrl_c => tracing::info!("Received Ctrl+C, shutting down"),
        _ = terminate => tracing::info!("Received SIGTERM, shutting down"),
    }
}

async fn root() -> Json<serde_json::Value> {
    Json(serde_json::json!({
        "message": "JNTUH Results API is running",
        "version": "2.0",
        "rust": true
    }))
}

async fn health() -> Json<serde_json::Value> {
    Json(serde_json::json!({"status": "ok"}))
}

#[derive(Deserialize)]
struct SemParams {
    htno: String,
    sem: String,
}

async fn get_sem_result(
    state: axum::extract::State<AppState>,
    Query(params): Query<SemParams>,
) -> Json<CombinedSemesterResult> {
    let result = state.sem_results.get_result(&params.htno, &params.sem).await;
    Json(result)
}

#[derive(Deserialize)]
struct AcademicParams {
    htno: String,
}

async fn get_academic_results(
    state: axum::extract::State<AppState>,
    Query(params): Query<AcademicParams>,
) -> Json<AcademicResponse> {
    let result = state.academic.get_full_result(&params.htno).await;
    Json(result)
}

#[derive(Deserialize)]
struct AllResultParams {
    htno: String,
}

async fn get_all_results(
    state: axum::extract::State<AppState>,
    Query(params): Query<AllResultParams>,
) -> Json<AllResultResponse> {
    let result = state.all_results.get_all_results(&params.htno).await;
    Json(result)
}

#[derive(Deserialize)]
struct ClassResultParams {
    sem: String,
    start_htno: String,
    end_htno: String,
    #[serde(default = "default_concurrency")]
    concurrency: usize,
}

fn default_concurrency() -> usize { 20 }

async fn get_class_results(
    state: axum::extract::State<AppState>,
    Query(params): Query<ClassResultParams>,
) -> Json<HashMap<String, ClassResultEntry>> {
    let concurrency = params.concurrency.clamp(1, 50);
    let roll_numbers = multiple::generate_roll_numbers(&params.start_htno, &params.end_htno);
    let results = state.class_results.fetch_all_students(&roll_numbers, &params.sem, concurrency).await;
    Json(results)
}

#[derive(Deserialize)]
struct NotificationParams {
    #[serde(default)]
    refresh: bool,
}

async fn get_notifications(
    state: axum::extract::State<AppState>,
    Query(params): Query<NotificationParams>,
) -> Json<Vec<Notification>> {
    let result = state.notifications.get_notifications(params.refresh).await;
    Json(result)
}

async fn refresh_codes(
    state: axum::extract::State<AppState>,
) -> Json<serde_json::Value> {
    state.exam_codes.refresh(&state.client).await;
    let count = state.exam_codes.total_codes().await;

    Json(serde_json::json!({
        "status": "success",
        "message": "Exam codes refreshed",
        "codes_count": count
    }))
}

async fn refresh_cache(
    state: axum::extract::State<AppState>,
) -> Json<serde_json::Value> {
    state.sem_results.clear_cache().await;
    Json(serde_json::json!({
        "status": "success",
        "message": "Cleared cached results from memory"
    }))
}

#[derive(Deserialize)]
struct SpecificResultParams {
    exam_code: String,
    etype: String,
    result: String,
    grad: String,
    #[serde(rename = "type")]
    r#type: String,
    degree: String,
    htno: String,
}

async fn get_specific_result(
    state: axum::extract::State<AppState>,
    Query(params): Query<SpecificResultParams>,
) -> Json<serde_json::Value> {
    let result = state.specific_results.get_result(
        &params.exam_code, &params.etype, &params.result, &params.grad,
        &params.r#type, &params.degree, &params.htno,
    ).await;
    Json(result)
}
