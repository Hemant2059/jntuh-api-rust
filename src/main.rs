use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::Query;
use axum::http::{HeaderName, Method};
use axum::routing::get;
use axum::{Json, Router};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tokio::signal;
use tower_http::cors::{Any, CorsLayer};
use tower_http::request_id::{MakeRequestUuid, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::compression::CompressionLayer;
use tracing_subscriber::EnvFilter;
use utoipa::{IntoParams, OpenApi, ToSchema};
use utoipa_swagger_ui::SwaggerUi;

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

#[derive(Serialize, ToSchema)]
struct RootResponse {
    message: String,
    version: String,
    rust: bool,
}

#[derive(Serialize, ToSchema)]
struct HealthResponse {
    status: String,
}

#[derive(Serialize, ToSchema)]
struct RefreshCodesResponse {
    status: String,
    message: String,
    codes_count: usize,
}

#[derive(Serialize, ToSchema)]
struct RefreshCacheResponse {
    status: String,
    message: String,
}

fn load_config() -> (String, String) {
    let port = std::env::var("PORT").unwrap_or_else(|_| "8000".to_string());
    let exam_codes_path = std::env::var("EXAM_CODES_PATH").unwrap_or_else(|_| "exam_codes.json".to_string());
    (port, exam_codes_path)
}

#[derive(OpenApi)]
#[openapi(
    info(
        title = "JNTUH Results API",
        description = "High-performance API for fetching JNTUH exam results, including semester results, academic history, class results, and notifications.",
        version = "2.0.0",
    ),
    paths(
        root,
        health,
        get_sem_result,
        get_academic_results,
        get_all_results,
        get_class_results,
        get_notifications,
        refresh_codes,
        refresh_cache,
        get_specific_result,
    ),
    components(
        schemas(
            RootResponse,
            HealthResponse,
            RefreshCodesResponse,
            RefreshCacheResponse,
            SubjectResult,
            StudentDetails,
            SemesterResult,
            ExamAttempt,
            SemesterResultData,
            GpaDetails,
            CombinedSemesterResult,
            AcademicResponse,
            SemesterSummary,
            AllResultResponse,
            DetailedExamEntry,
            ClassResultEntry,
            Notification,
        )
    ),
    tags(
        (name = "Results", description = "Semester and academic result endpoints"),
        (name = "Batch", description = "Class/batch result endpoints"),
        (name = "Admin", description = "Admin and maintenance endpoints"),
        (name = "Notifications", description = "JNTUH notification endpoints"),
    ),
)]
struct ApiDoc;

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
        .merge(SwaggerUi::new("/swagger-ui").url("/api-docs/openapi.json", ApiDoc::openapi()))
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

#[utoipa::path(
    get,
    path = "/",
    responses(
        (status = 200, description = "API is running", body = RootResponse),
    )
)]
async fn root() -> Json<RootResponse> {
    Json(RootResponse {
        message: "JNTUH Results API is running".into(),
        version: "2.0".into(),
        rust: true,
    })
}

#[utoipa::path(
    get,
    path = "/health",
    responses(
        (status = 200, description = "Health check", body = HealthResponse),
    )
)]
async fn health() -> Json<HealthResponse> {
    Json(HealthResponse { status: "ok".into() })
}

#[derive(Deserialize, IntoParams)]
struct SemParams {
    htno: String,
    sem: String,
}

#[utoipa::path(
    get,
    path = "/sem",
    params(SemParams),
    responses(
        (status = 200, description = "Semester result with SGPA and history", body = CombinedSemesterResult),
    )
)]
async fn get_sem_result(
    state: axum::extract::State<AppState>,
    Query(params): Query<SemParams>,
) -> Json<CombinedSemesterResult> {
    let result = state.sem_results.get_result(&params.htno, &params.sem).await;
    Json(result)
}

#[derive(Deserialize, IntoParams)]
struct AcademicParams {
    htno: String,
}

#[utoipa::path(
    get,
    path = "/academic",
    params(AcademicParams),
    responses(
        (status = 200, description = "Complete academic history with CGPA across all semesters", body = AcademicResponse),
    )
)]
async fn get_academic_results(
    state: axum::extract::State<AppState>,
    Query(params): Query<AcademicParams>,
) -> Json<AcademicResponse> {
    let result = state.academic.get_full_result(&params.htno).await;
    Json(result)
}

#[derive(Deserialize, IntoParams)]
struct AllResultParams {
    htno: String,
}

#[utoipa::path(
    get,
    path = "/allresult",
    params(AllResultParams),
    responses(
        (status = 200, description = "Detailed results grouped by semester with all exam attempts (regular, supply, RCRV)", body = AllResultResponse),
    )
)]
async fn get_all_results(
    state: axum::extract::State<AppState>,
    Query(params): Query<AllResultParams>,
) -> Json<AllResultResponse> {
    let result = state.all_results.get_all_results(&params.htno).await;
    Json(result)
}

#[derive(Deserialize, IntoParams)]
struct ClassResultParams {
    sem: String,
    start_htno: String,
    end_htno: String,
    #[serde(default = "default_concurrency")]
    concurrency: usize,
}

fn default_concurrency() -> usize { 20 }

#[utoipa::path(
    get,
    path = "/classresult",
    params(ClassResultParams),
    responses(
        (status = 200, description = "Batch results for a class/section within a roll number range", body = HashMap<String, ClassResultEntry>),
    )
)]
async fn get_class_results(
    state: axum::extract::State<AppState>,
    Query(params): Query<ClassResultParams>,
) -> Json<HashMap<String, ClassResultEntry>> {
    let concurrency = params.concurrency.clamp(1, 50);
    let roll_numbers = multiple::generate_roll_numbers(&params.start_htno, &params.end_htno);
    let results = state.class_results.fetch_all_students(&roll_numbers, &params.sem, concurrency).await;
    Json(results)
}

#[derive(Deserialize, IntoParams)]
struct NotificationParams {
    #[serde(default)]
    refresh: bool,
}

#[utoipa::path(
    get,
    path = "/notifications",
    params(NotificationParams),
    responses(
        (status = 200, description = "Latest notifications from JNTUH home page", body = Vec<Notification>),
    )
)]
async fn get_notifications(
    state: axum::extract::State<AppState>,
    Query(params): Query<NotificationParams>,
) -> Json<Vec<Notification>> {
    let result = state.notifications.get_notifications(params.refresh).await;
    Json(result)
}

#[utoipa::path(
    get,
    path = "/refresh-codes",
    responses(
        (status = 200, description = "Refresh exam codes from JNTUH", body = RefreshCodesResponse),
    )
)]
async fn refresh_codes(
    state: axum::extract::State<AppState>,
) -> Json<RefreshCodesResponse> {
    state.exam_codes.refresh(&state.client).await;
    let count = state.exam_codes.total_codes().await;

    Json(RefreshCodesResponse {
        status: "success".into(),
        message: "Exam codes refreshed".into(),
        codes_count: count,
    })
}

#[utoipa::path(
    get,
    path = "/refresh-cache",
    responses(
        (status = 200, description = "Clear cached semester results from memory", body = RefreshCacheResponse),
    )
)]
async fn refresh_cache(
    state: axum::extract::State<AppState>,
) -> Json<RefreshCacheResponse> {
    state.sem_results.clear_cache().await;
    Json(RefreshCacheResponse {
        status: "success".into(),
        message: "Cleared cached results from memory".into(),
    })
}

#[derive(Deserialize, IntoParams)]
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

#[utoipa::path(
    get,
    path = "/specificresult",
    params(SpecificResultParams),
    responses(
        (status = 200, description = "Raw result from JNTUH for a specific exam configuration", body = serde_json::Value),
    )
)]
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
