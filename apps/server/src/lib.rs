use anyhow::{Context, Result};
use axum::{extract::State, http::StatusCode, routing::get, Router};
use chrono::{DateTime, Utc};
use prost_types::Timestamp;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tonic::{transport::Server, Request, Response, Status};
use tracing::{error, info};
use uuid::Uuid;

pub mod proto {
    tonic::include_proto!("vaultnote.v1");
}

use proto::vault_note_service_server::{VaultNoteService, VaultNoteServiceServer};
use proto::{
    CreateNoteRequest, CreateNoteResponse, ListNotesRequest, ListNotesResponse, Note, PingRequest,
    PingResponse,
};

#[derive(Clone)]
pub struct VaultNoteServiceImpl {
    db: PgPool,
}

impl VaultNoteServiceImpl {
    pub fn new(db: PgPool) -> Self {
        Self { db }
    }
}

#[derive(sqlx::FromRow)]
struct NoteRow {
    id: Uuid,
    title: String,
    content: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[tonic::async_trait]
impl VaultNoteService for VaultNoteServiceImpl {
    async fn ping(&self, _request: Request<PingRequest>) -> Result<Response<PingResponse>, Status> {
        Ok(Response::new(PingResponse {
            message: "pong".to_string(),
        }))
    }

    async fn create_note(
        &self,
        request: Request<CreateNoteRequest>,
    ) -> Result<Response<CreateNoteResponse>, Status> {
        let input = request.into_inner();
        let title = input.title.trim().to_string();
        let content = input.content;

        if title.is_empty() {
            return Err(Status::invalid_argument("title is required"));
        }

        let row = sqlx::query_as::<_, NoteRow>(
            r#"
            INSERT INTO notes (title, content)
            VALUES ($1, $2)
            RETURNING id, title, content, created_at, updated_at
            "#,
        )
        .bind(title)
        .bind(content)
        .fetch_one(&self.db)
        .await
        .map_err(map_db_error)?;

        Ok(Response::new(CreateNoteResponse {
            note: Some(map_note(row)),
        }))
    }

    async fn list_notes(
        &self,
        _request: Request<ListNotesRequest>,
    ) -> Result<Response<ListNotesResponse>, Status> {
        let rows = sqlx::query_as::<_, NoteRow>(
            r#"
            SELECT id, title, content, created_at, updated_at
            FROM notes
            ORDER BY created_at DESC
            "#,
        )
        .fetch_all(&self.db)
        .await
        .map_err(map_db_error)?;

        let notes = rows.into_iter().map(map_note).collect();
        Ok(Response::new(ListNotesResponse { notes }))
    }
}

#[derive(Clone)]
struct AppState {
    db: PgPool,
}

async fn health_handler() -> &'static str {
    "OK"
}

async fn ready_handler(State(state): State<AppState>) -> (StatusCode, &'static str) {
    match sqlx::query("SELECT 1").execute(&state.db).await {
        Ok(_) => (StatusCode::OK, "READY"),
        Err(err) => {
            error!(error = %err, "readiness probe failed");
            (StatusCode::SERVICE_UNAVAILABLE, "NOT_READY")
        }
    }
}

fn map_db_error(err: sqlx::Error) -> Status {
    error!(error = %err, "database operation failed");
    Status::internal("database operation failed")
}

fn map_note(row: NoteRow) -> Note {
    Note {
        id: row.id.to_string(),
        title: row.title,
        content: row.content,
        created_at: Some(to_timestamp(row.created_at)),
        updated_at: Some(to_timestamp(row.updated_at)),
    }
}

fn to_timestamp(value: DateTime<Utc>) -> Timestamp {
    Timestamp {
        seconds: value.timestamp(),
        nanos: value.timestamp_subsec_nanos() as i32,
    }
}

pub fn build_http_router(db: PgPool) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .with_state(AppState { db })
}

pub async fn create_db_pool(database_url: &str) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(8)
        .connect(database_url)
        .await
        .context("failed to connect to postgres")
}

pub async fn run_servers(db: PgPool, grpc_addr: SocketAddr, http_addr: SocketAddr) -> Result<()> {
    let service = VaultNoteServiceImpl::new(db.clone());
    info!(%grpc_addr, "starting gRPC server");
    let grpc_future = Server::builder()
        .add_service(VaultNoteServiceServer::new(service))
        .serve(grpc_addr);

    info!(%http_addr, "starting HTTP server");
    let app = build_http_router(db);
    let listener = TcpListener::bind(&http_addr).await?;
    let http_future = axum::serve(listener, app);

    tokio::select! {
        res = grpc_future => {
            res?;
        }
        res = http_future => {
            res?;
        }
    }

    Ok(())
}
