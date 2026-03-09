use anyhow::{Context, Result};
use axum::{extract::State, http::StatusCode, routing::get, Router};
use chrono::{DateTime, Utc};
use prost_types::Timestamp;
use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::net::SocketAddr;
use tokio::net::TcpListener;
use tonic::{transport::Server, Request, Response, Status};
use tracing::{error, info, warn};
use uuid::Uuid;

pub mod proto {
    tonic::include_proto!("vaultnote.v1");
}

use proto::vault_note_service_server::{VaultNoteService, VaultNoteServiceServer};
use proto::{
    CreateNoteRequest, CreateNoteResponse, ListNotesRequest, ListNotesResponse, Note, PingRequest,
    PingResponse,
};

const NOTES_LIST_CACHE_KEY: &str = "notes:list";
const NOTES_LIST_TTL_SECS: u64 = 60;

#[derive(Clone)]
pub struct VaultNoteServiceImpl {
    db: PgPool,
    redis: redis::Client,
}

impl VaultNoteServiceImpl {
    pub fn new(db: PgPool, redis: redis::Client) -> Self {
        Self { db, redis }
    }

    async fn redis_conn(&self) -> Option<MultiplexedConnection> {
        match self.redis.get_multiplexed_async_connection().await {
            Ok(conn) => Some(conn),
            Err(err) => {
                warn!(error = %err, "redis connection failed; proceeding without cache");
                None
            }
        }
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

        // Invalidate the list cache so the next ListNotes reflects the new note.
        if let Some(mut conn) = self.redis_conn().await {
            if let Err(err) = conn.del::<_, ()>(NOTES_LIST_CACHE_KEY).await {
                warn!(error = %err, "failed to invalidate notes list cache");
            }
        }

        Ok(Response::new(CreateNoteResponse {
            note: Some(map_note(row)),
        }))
    }

    async fn list_notes(
        &self,
        _request: Request<ListNotesRequest>,
    ) -> Result<Response<ListNotesResponse>, Status> {
        // Try to return from cache first.
        if let Some(mut conn) = self.redis_conn().await {
            match conn.get::<_, Option<String>>(NOTES_LIST_CACHE_KEY).await {
                Ok(Some(cached)) => match serde_json::from_str::<Vec<CachedNote>>(&cached) {
                    Ok(cached_notes) => {
                        let notes = cached_notes.into_iter().map(map_cached_note).collect();
                        return Ok(Response::new(ListNotesResponse { notes }));
                    }
                    Err(err) => {
                        warn!(error = %err, "failed to deserialize cached notes; fetching from DB");
                    }
                },
                Ok(None) => {}
                Err(err) => {
                    warn!(error = %err, "redis GET failed; fetching from DB");
                }
            }
        }

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

        // Populate cache for next request.
        if let Some(mut conn) = self.redis_conn().await {
            let cached: Vec<CachedNote> = rows.iter().map(cached_note_from_row).collect();
            match serde_json::to_string(&cached) {
                Ok(serialized) => {
                    if let Err(err) = conn
                        .set_ex::<_, _, ()>(NOTES_LIST_CACHE_KEY, serialized, NOTES_LIST_TTL_SECS)
                        .await
                    {
                        warn!(error = %err, "failed to cache notes list");
                    }
                }
                Err(err) => {
                    warn!(error = %err, "failed to serialize notes for cache");
                }
            }
        }

        let notes = rows.into_iter().map(map_note).collect();
        Ok(Response::new(ListNotesResponse { notes }))
    }
}

#[derive(Clone)]
struct AppState {
    db: PgPool,
    redis: redis::Client,
}

async fn health_handler() -> &'static str {
    "OK"
}

async fn ready_handler(State(state): State<AppState>) -> (StatusCode, &'static str) {
    let db_ok = sqlx::query("SELECT 1").execute(&state.db).await.is_ok();
    if !db_ok {
        error!("readiness probe failed: database unreachable");
        return (StatusCode::SERVICE_UNAVAILABLE, "NOT_READY");
    }

    let redis_ok = match state.redis.get_multiplexed_async_connection().await {
        Ok(mut conn) => match redis::cmd("PING")
            .query_async::<String>(&mut conn)
            .await
        {
            Ok(r) => r == "PONG",
            Err(err) => {
                error!(error = %err, "readiness probe failed: redis PING error");
                false
            }
        },
        Err(err) => {
            error!(error = %err, "readiness probe failed: redis connection error");
            false
        }
    };

    if !redis_ok {
        error!("readiness probe failed: redis unreachable");
        return (StatusCode::SERVICE_UNAVAILABLE, "NOT_READY");
    }

    (StatusCode::OK, "READY")
}

fn map_db_error(err: sqlx::Error) -> Status {
    error!(error = %err, "database operation failed");
    Status::internal("database operation failed")
}

#[derive(serde::Serialize, serde::Deserialize)]
struct CachedNote {
    id: String,
    title: String,
    content: String,
    created_at_secs: i64,
    created_at_nanos: i32,
    updated_at_secs: i64,
    updated_at_nanos: i32,
}

fn cached_note_from_row(row: &NoteRow) -> CachedNote {
    CachedNote {
        id: row.id.to_string(),
        title: row.title.clone(),
        content: row.content.clone(),
        created_at_secs: row.created_at.timestamp(),
        created_at_nanos: row.created_at.timestamp_subsec_nanos() as i32,
        updated_at_secs: row.updated_at.timestamp(),
        updated_at_nanos: row.updated_at.timestamp_subsec_nanos() as i32,
    }
}

fn map_cached_note(c: CachedNote) -> Note {
    Note {
        id: c.id,
        title: c.title,
        content: c.content,
        created_at: Some(Timestamp {
            seconds: c.created_at_secs,
            nanos: c.created_at_nanos,
        }),
        updated_at: Some(Timestamp {
            seconds: c.updated_at_secs,
            nanos: c.updated_at_nanos,
        }),
    }
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

pub fn build_http_router(db: PgPool, redis: redis::Client) -> Router {
    Router::new()
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .with_state(AppState { db, redis })
}

pub async fn create_db_pool(database_url: &str) -> Result<PgPool> {
    PgPoolOptions::new()
        .max_connections(8)
        .connect(database_url)
        .await
        .context("failed to connect to postgres")
}

pub fn create_redis_client(redis_url: &str) -> Result<redis::Client> {
    redis::Client::open(redis_url).context("failed to create redis client")
}

pub async fn run_servers(
    db: PgPool,
    redis: redis::Client,
    grpc_addr: SocketAddr,
    http_addr: SocketAddr,
) -> Result<()> {
    let service = VaultNoteServiceImpl::new(db.clone(), redis.clone());
    info!(%grpc_addr, "starting gRPC server");
    let grpc_future = Server::builder()
        .add_service(VaultNoteServiceServer::new(service))
        .serve(grpc_addr);

    info!(%http_addr, "starting HTTP server");
    let app = build_http_router(db, redis);
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
