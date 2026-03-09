use anyhow::{Context, Result};
use argon2::{
    password_hash::{rand_core::OsRng, PasswordHash, PasswordHasher, PasswordVerifier, SaltString},
    Argon2,
};
use axum::{extract::State, http::StatusCode, routing::get, Router};
use chrono::{DateTime, Utc};
use jsonwebtoken::{decode, encode, Algorithm, DecodingKey, EncodingKey, Header, Validation};
use prost_types::Timestamp;
use redis::aio::MultiplexedConnection;
use redis::AsyncCommands;
use serde::{Deserialize, Serialize};
use sqlx::{postgres::PgPoolOptions, PgPool};
use std::net::SocketAddr;
use std::pin::Pin;
use tokio::net::TcpListener;
use tokio_stream::wrappers::ReceiverStream;
use tokio_stream::{Stream, StreamExt};
use tonic::{transport::Server, Request, Response, Status, Streaming};
use tracing::{error, info, warn};
use uuid::Uuid;

pub mod proto {
    tonic::include_proto!("vaultnote.v1");
}

use proto::vault_note_service_server::{VaultNoteService, VaultNoteServiceServer};
use proto::{
    AskVaultRequest, AskVaultResponse, CreateNoteRequest, CreateNoteResponse, ListNotesRequest,
    ListNotesResponse, LoginRequest, LoginResponse, Note, PingRequest, PingResponse,
    RegisterRequest, RegisterResponse, SearchNotesRequest, SemanticSearchRequest,
    SummarizeNoteRequest, SummarizeNoteResponse, UploadDocumentRequest, UploadDocumentResponse,
};

const NOTES_LIST_CACHE_KEY: &str = "notes:list";
const NOTES_LIST_TTL_SECS: u64 = 60;

/// Key of the Redis list used as the background-job queue.
pub const JOB_QUEUE_KEY: &str = "jobs:queue";

/// Embedding dimension used for all bag-of-words vectors.
const EMBEDDING_DIM: usize = 384;

// ---------------------------------------------------------------------------
// JWT claims
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
struct Claims {
    /// Subject – the user's UUID as a string.
    sub: String,
    /// Expiry (Unix timestamp in seconds).  Using u64 avoids the Y2038
    /// overflow that `usize` would cause on 32-bit targets.
    exp: u64,
}

// ---------------------------------------------------------------------------
// Background-job envelope
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Job {
    EmbedNote { note_id: String },
    ExtractDocumentText { document_id: String },
}

// ---------------------------------------------------------------------------
// Service struct
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct VaultNoteServiceImpl {
    db: PgPool,
    redis: redis::Client,
    jwt_secret: String,
}

impl VaultNoteServiceImpl {
    pub fn new(db: PgPool, redis: redis::Client, jwt_secret: String) -> Self {
        Self {
            db,
            redis,
            jwt_secret,
        }
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

// ---------------------------------------------------------------------------
// DB row types
// ---------------------------------------------------------------------------

#[derive(sqlx::FromRow)]
struct NoteRow {
    id: Uuid,
    title: String,
    content: String,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

#[derive(sqlx::FromRow)]
struct UserRow {
    id: Uuid,
    #[allow(dead_code)]
    email: String,
    password_hash: String,
}

// ---------------------------------------------------------------------------
// Auth helpers
// ---------------------------------------------------------------------------

#[allow(clippy::result_large_err)]
fn hash_password(password: &str) -> Result<String, Status> {
    let salt = SaltString::generate(&mut OsRng);
    Argon2::default()
        .hash_password(password.as_bytes(), &salt)
        .map(|h| h.to_string())
        .map_err(|_| Status::internal("failed to hash password"))
}

fn verify_password(password: &str, hash: &str) -> bool {
    PasswordHash::new(hash)
        .ok()
        .map(|h| {
            Argon2::default()
                .verify_password(password.as_bytes(), &h)
                .is_ok()
        })
        .unwrap_or(false)
}

#[allow(clippy::result_large_err)]
fn generate_jwt(user_id: &str, secret: &str) -> Result<String, Status> {
    let exp = chrono::Utc::now()
        .checked_add_signed(chrono::Duration::hours(24))
        .expect("valid timestamp")
        .timestamp()
        .max(0) as u64;
    encode(
        &Header::default(),
        &Claims {
            sub: user_id.to_string(),
            exp,
        },
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .map_err(|_| Status::internal("failed to generate JWT"))
}

#[allow(clippy::result_large_err)]
pub fn validate_jwt(token: &str, secret: &str) -> Result<String, Status> {
    decode::<Claims>(
        token,
        &DecodingKey::from_secret(secret.as_bytes()),
        &Validation::new(Algorithm::HS256),
    )
    .map(|d| d.claims.sub)
    .map_err(|_| Status::unauthenticated("invalid or expired token"))
}

// ---------------------------------------------------------------------------
// Vector embedding helpers  (bag-of-words, 384-dimensional)
// ---------------------------------------------------------------------------

/// Generates a deterministic, L2-normalised bag-of-words embedding.
/// Each word is mapped to one of the 384 dimensions via an FNV-1a hash and
/// its frequency is accumulated before normalisation.  This intentionally
/// simple approach is a stand-in for a real sentence-embedding model.
pub fn generate_embedding(text: &str) -> Vec<f32> {
    let mut v = vec![0.0f32; EMBEDDING_DIM];
    for raw_word in text.split_whitespace() {
        let word = raw_word
            .trim_matches(|c: char| !c.is_alphanumeric())
            .to_lowercase();
        if word.is_empty() {
            continue;
        }
        // FNV-1a hash → dimension index
        let mut h: u64 = 14_695_981_039_346_656_037;
        for byte in word.bytes() {
            h ^= u64::from(byte);
            h = h.wrapping_mul(1_099_511_628_211);
        }
        v[(h as usize) % EMBEDDING_DIM] += 1.0;
    }
    // L2 normalise
    let norm: f32 = v.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm > 0.0 {
        v.iter_mut().for_each(|x| *x /= norm);
    }
    v
}

/// Cosine similarity between two equal-length vectors.  Returns 0.0 if
/// either vector is all-zeros.
fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    let dot: f32 = a.iter().zip(b.iter()).map(|(x, y)| x * y).sum();
    let norm_a: f32 = a.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = b.iter().map(|x| x * x).sum::<f32>().sqrt();
    if norm_a == 0.0 || norm_b == 0.0 {
        0.0
    } else {
        dot / (norm_a * norm_b)
    }
}

// ---------------------------------------------------------------------------
// PDF text extraction helper
// ---------------------------------------------------------------------------

/// Returns the text extracted from `bytes` if the content appears to be a
/// PDF (starts with `%PDF`), otherwise returns `None`.
pub fn extract_pdf_text(bytes: &[u8]) -> Option<String> {
    if !bytes.starts_with(b"%PDF") {
        return None;
    }
    let doc = lopdf::Document::load_mem(bytes).ok()?;
    let page_numbers: Vec<u32> = doc.get_pages().keys().copied().collect();
    let text = doc.extract_text(&page_numbers).ok()?;
    if text.trim().is_empty() {
        None
    } else {
        Some(text)
    }
}

// ---------------------------------------------------------------------------
// AI summary helper (extractive; swap for LLM call when API key is set)
// ---------------------------------------------------------------------------

fn extractive_summary(title: &str, content: &str, max_sentences: usize) -> String {
    let sentences: Vec<&str> = content
        .split(['.', '!', '?'])
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .take(max_sentences)
        .collect();

    let lead = if sentences.is_empty() {
        content.chars().take(200).collect::<String>()
    } else {
        sentences.join(". ") + "."
    };

    let word_count = content.split_whitespace().count();
    format!("Title: {title}. Summary: {lead} (Word count: {word_count})")
}

// ---------------------------------------------------------------------------
// Background-job helpers
// ---------------------------------------------------------------------------

/// Enqueues a job onto the Redis job queue.  Silently drops errors so that
/// a Redis failure never prevents the primary RPC from succeeding.
async fn enqueue_job(redis: &redis::Client, job: &Job) {
    let Ok(payload) = serde_json::to_string(job) else {
        return;
    };
    if let Ok(mut conn) = redis.get_multiplexed_async_connection().await {
        if let Err(err) = conn.rpush::<_, _, ()>(JOB_QUEUE_KEY, payload).await {
            warn!(error = %err, "failed to enqueue background job");
        }
    }
}

/// Processes a single job from the front of the queue.
/// Returns `true` when a job was dequeued and processed, `false` when the
/// queue was empty, and `None` on a transient error.
pub async fn process_next_job(db: &PgPool, redis: &redis::Client) -> Option<bool> {
    let mut conn = redis.get_multiplexed_async_connection().await.ok()?;
    let payload: Option<String> = conn.lpop(JOB_QUEUE_KEY, None).await.ok()?;
    let payload = match payload {
        Some(p) => p,
        None => return Some(false),
    };

    let job: Job = match serde_json::from_str(&payload) {
        Ok(j) => j,
        Err(err) => {
            warn!(error = %err, "failed to deserialise job payload; discarding");
            return Some(true);
        }
    };

    match job {
        Job::EmbedNote { note_id } => {
            let Ok(note_uuid) = Uuid::parse_str(&note_id) else {
                warn!(%note_id, "invalid UUID in EmbedNote job; skipping");
                return Some(true);
            };

            let row = sqlx::query_as::<_, NoteRow>(
                "SELECT id, title, content, created_at, updated_at FROM notes WHERE id = $1",
            )
            .bind(note_uuid)
            .fetch_optional(db)
            .await;

            match row {
                Ok(Some(note)) => {
                    let embedding =
                        generate_embedding(&format!("{} {}", note.title, note.content));
                    if let Err(err) = sqlx::query(
                        "INSERT INTO note_embeddings (note_id, embedding)
                         VALUES ($1, $2)
                         ON CONFLICT DO NOTHING",
                    )
                    .bind(note_uuid)
                    .bind(&embedding)
                    .execute(db)
                    .await
                    {
                        warn!(error = %err, "failed to store note embedding");
                    }
                }
                Ok(None) => warn!(%note_id, "note not found for EmbedNote job"),
                Err(err) => warn!(error = %err, "DB error in EmbedNote job"),
            }
        }

        Job::ExtractDocumentText { document_id } => {
            let Ok(doc_uuid) = Uuid::parse_str(&document_id) else {
                warn!(%document_id, "invalid UUID in ExtractDocumentText job; skipping");
                return Some(true);
            };

            let content: Option<Vec<u8>> =
                sqlx::query_scalar("SELECT content FROM documents WHERE id = $1")
                    .bind(doc_uuid)
                    .fetch_optional(db)
                    .await
                    .unwrap_or(None);

            if let Some(bytes) = content {
                let extracted = extract_pdf_text(&bytes);
                if let Err(err) = sqlx::query(
                    "UPDATE documents SET extracted_text = $1 WHERE id = $2",
                )
                .bind(extracted)
                .bind(doc_uuid)
                .execute(db)
                .await
                {
                    warn!(error = %err, "failed to store extracted document text");
                }
            }
        }
    }

    Some(true)
}

/// Runs forever, draining the job queue with a short sleep between empty polls.
pub async fn run_job_worker(db: PgPool, redis: redis::Client) {
    info!("background job worker started");
    loop {
        match process_next_job(&db, &redis).await {
            Some(true) => {} // processed a job; try again immediately
            _ => tokio::time::sleep(tokio::time::Duration::from_millis(100)).await,
        }
    }
}

// ---------------------------------------------------------------------------
// gRPC service implementation
// ---------------------------------------------------------------------------

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
        .bind(&title)
        .bind(&content)
        .fetch_one(&self.db)
        .await
        .map_err(map_db_error)?;

        // Invalidate the list cache so the next ListNotes reflects the new note.
        if let Some(mut conn) = self.redis_conn().await {
            if let Err(err) = conn.del::<_, ()>(NOTES_LIST_CACHE_KEY).await {
                warn!(error = %err, "failed to invalidate notes list cache");
            }
        }

        // Enqueue embedding job.
        enqueue_job(
            &self.redis,
            &Job::EmbedNote {
                note_id: row.id.to_string(),
            },
        )
        .await;

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

    // --- Milestone 6: server-streaming SearchNotes ---

    type SearchNotesStream = Pin<Box<dyn Stream<Item = Result<Note, Status>> + Send>>;

    #[allow(clippy::result_large_err)]
    async fn search_notes(
        &self,
        request: Request<SearchNotesRequest>,
    ) -> Result<Response<Self::SearchNotesStream>, Status> {
        let query = request.into_inner().query;
        let pattern = format!("%{}%", query);

        let rows = sqlx::query_as::<_, NoteRow>(
            r#"
            SELECT id, title, content, created_at, updated_at
            FROM notes
            WHERE title ILIKE $1 OR content ILIKE $1
            ORDER BY created_at DESC
            "#,
        )
        .bind(pattern)
        .fetch_all(&self.db)
        .await
        .map_err(map_db_error)?;

        let stream = tokio_stream::iter(rows.into_iter().map(|r| Ok(map_note(r))));
        Ok(Response::new(Box::pin(stream)))
    }

    // --- Milestone 7: client-streaming UploadDocument ---

    async fn upload_document(
        &self,
        request: Request<Streaming<UploadDocumentRequest>>,
    ) -> Result<Response<UploadDocumentResponse>, Status> {
        let mut stream = request.into_inner();

        let mut note_id_str = String::new();
        let mut filename = String::new();
        let mut content: Vec<u8> = Vec::new();

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            if note_id_str.is_empty() {
                note_id_str = chunk.note_id;
                filename = chunk.filename;
            }
            content.extend_from_slice(&chunk.chunk);
        }

        if note_id_str.is_empty() {
            return Err(Status::invalid_argument("note_id is required"));
        }

        let note_id = Uuid::parse_str(&note_id_str)
            .map_err(|_| Status::invalid_argument("note_id must be a valid UUID"))?;

        let size_bytes = content.len() as i64;

        // Attempt eager PDF text extraction; a failure is non-fatal.
        let extracted_text = extract_pdf_text(&content);

        let document_id = sqlx::query_scalar::<_, Uuid>(
            r#"
            INSERT INTO documents (note_id, filename, content, size_bytes, extracted_text)
            VALUES ($1, $2, $3, $4, $5)
            RETURNING id
            "#,
        )
        .bind(note_id)
        .bind(&filename)
        .bind(&content)
        .bind(size_bytes)
        .bind(&extracted_text)
        .fetch_one(&self.db)
        .await
        .map_err(map_db_error)?;

        // Also enqueue a background job so re-extraction can be triggered later.
        enqueue_job(
            &self.redis,
            &Job::ExtractDocumentText {
                document_id: document_id.to_string(),
            },
        )
        .await;

        Ok(Response::new(UploadDocumentResponse {
            document_id: document_id.to_string(),
            bytes_received: size_bytes,
        }))
    }

    // --- Milestone 8: bidirectional AskVault session ---

    type AskVaultStream = Pin<Box<dyn Stream<Item = Result<AskVaultResponse, Status>> + Send>>;

    async fn ask_vault(
        &self,
        request: Request<Streaming<AskVaultRequest>>,
    ) -> Result<Response<Self::AskVaultStream>, Status> {
        let mut stream = request.into_inner();
        let db = self.db.clone();

        let (tx, rx) = tokio::sync::mpsc::channel(32);

        tokio::spawn(async move {
            while let Some(msg) = stream.next().await {
                match msg {
                    Ok(req) => {
                        let question = req.question;
                        if question.is_empty() {
                            continue;
                        }
                        let pattern = format!("%{}%", question);

                        let rows = sqlx::query_as::<_, NoteRow>(
                            r#"
                            SELECT id, title, content, created_at, updated_at
                            FROM notes
                            WHERE title ILIKE $1 OR content ILIKE $1
                            ORDER BY created_at DESC
                            LIMIT 3
                            "#,
                        )
                        .bind(pattern)
                        .fetch_all(&db)
                        .await;

                        match rows {
                            Ok(notes) if notes.is_empty() => {
                                let _ = tx
                                    .send(Ok(AskVaultResponse {
                                        token: "No relevant notes found.".to_string(),
                                    }))
                                    .await;
                            }
                            Ok(notes) => {
                                let _ = tx
                                    .send(Ok(AskVaultResponse {
                                        token: format!(
                                            "Found {} relevant note(s): ",
                                            notes.len()
                                        ),
                                    }))
                                    .await;
                                for note in notes {
                                    let _ = tx
                                        .send(Ok(AskVaultResponse {
                                            token: format!("[{}] ", note.title),
                                        }))
                                        .await;
                                }
                            }
                            Err(err) => {
                                let _ = tx.send(Err(map_db_error(err))).await;
                            }
                        }
                    }
                    Err(err) => {
                        let _ = tx.send(Err(err)).await;
                        break;
                    }
                }
            }
        });

        Ok(Response::new(Box::pin(ReceiverStream::new(rx))))
    }

    // -----------------------------------------------------------------------
    // Future: Auth – Register
    // -----------------------------------------------------------------------

    async fn register(
        &self,
        request: Request<RegisterRequest>,
    ) -> Result<Response<RegisterResponse>, Status> {
        let input = request.into_inner();
        let email = input.email.trim().to_lowercase();
        let password = input.password;

        if email.is_empty() {
            return Err(Status::invalid_argument("email is required"));
        }
        if password.len() < 8 {
            return Err(Status::invalid_argument(
                "password must be at least 8 characters",
            ));
        }

        let password_hash = hash_password(&password)?;

        let user_id = sqlx::query_scalar::<_, Uuid>(
            "INSERT INTO users (email, password_hash) VALUES ($1, $2) RETURNING id",
        )
        .bind(&email)
        .bind(&password_hash)
        .fetch_one(&self.db)
        .await
        .map_err(|err| match &err {
            sqlx::Error::Database(db_err)
                if db_err
                    .constraint()
                    .map(|c| c.contains("email"))
                    .unwrap_or(false) =>
            {
                Status::already_exists("email already registered")
            }
            _ => map_db_error(err),
        })?;

        Ok(Response::new(RegisterResponse {
            user_id: user_id.to_string(),
        }))
    }

    // -----------------------------------------------------------------------
    // Future: Auth – Login
    // -----------------------------------------------------------------------

    async fn login(
        &self,
        request: Request<LoginRequest>,
    ) -> Result<Response<LoginResponse>, Status> {
        let input = request.into_inner();
        let email = input.email.trim().to_lowercase();
        let password = input.password;

        let row = sqlx::query_as::<_, UserRow>(
            "SELECT id, email, password_hash FROM users WHERE email = $1",
        )
        .bind(&email)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| Status::unauthenticated("invalid credentials"))?;

        if !verify_password(&password, &row.password_hash) {
            return Err(Status::unauthenticated("invalid credentials"));
        }

        let token = generate_jwt(&row.id.to_string(), &self.jwt_secret)?;

        Ok(Response::new(LoginResponse {
            token,
            user_id: row.id.to_string(),
        }))
    }

    // -----------------------------------------------------------------------
    // Future: Vector DB – SemanticSearch
    // -----------------------------------------------------------------------

    type SemanticSearchStream = Pin<Box<dyn Stream<Item = Result<Note, Status>> + Send>>;

    #[allow(clippy::result_large_err)]
    async fn semantic_search(
        &self,
        request: Request<SemanticSearchRequest>,
    ) -> Result<Response<Self::SemanticSearchStream>, Status> {
        let inner = request.into_inner();
        let query = inner.query;
        let limit = if inner.limit > 0 { inner.limit as i64 } else { 10 };

        let query_embedding = generate_embedding(&query);

        // Fetch all stored embeddings alongside their note_ids, then rank by
        // cosine similarity in application code.  For small-to-medium
        // collections this is fast enough; a production system would push the
        // similarity computation to the DB via pgvector.
        #[derive(sqlx::FromRow)]
        struct RawEmbeddingRow {
            note_id: Uuid,
            embedding: Vec<f32>,
        }

        let rows = sqlx::query_as::<_, RawEmbeddingRow>(
            "SELECT note_id, embedding FROM note_embeddings ORDER BY created_at DESC",
        )
        .fetch_all(&self.db)
        .await
        .map_err(map_db_error)?;

        // Rank by cosine similarity and keep the top `limit` results.
        let mut scored: Vec<(f32, Uuid)> = rows
            .into_iter()
            .map(|r| (cosine_similarity(&query_embedding, &r.embedding), r.note_id))
            .collect();
        scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit as usize);

        let note_ids: Vec<Uuid> = scored.into_iter().map(|(_, id)| id).collect();

        if note_ids.is_empty() {
            return Ok(Response::new(Box::pin(tokio_stream::empty())));
        }

        // Fetch the actual note rows in a single query.
        // Use ANY($1) to match against the slice of UUIDs.
        let notes = sqlx::query_as::<_, NoteRow>(
            r#"
            SELECT id, title, content, created_at, updated_at
            FROM notes
            WHERE id = ANY($1)
            ORDER BY created_at DESC
            "#,
        )
        .bind(&note_ids as &[Uuid])
        .fetch_all(&self.db)
        .await
        .map_err(map_db_error)?;

        let stream = tokio_stream::iter(notes.into_iter().map(|r| Ok(map_note(r))));
        Ok(Response::new(Box::pin(stream)))
    }

    // -----------------------------------------------------------------------
    // Future: AI Summaries – SummarizeNote
    // -----------------------------------------------------------------------

    async fn summarize_note(
        &self,
        request: Request<SummarizeNoteRequest>,
    ) -> Result<Response<SummarizeNoteResponse>, Status> {
        let note_id_str = request.into_inner().note_id;
        let note_id = Uuid::parse_str(&note_id_str)
            .map_err(|_| Status::invalid_argument("note_id must be a valid UUID"))?;

        let row = sqlx::query_as::<_, NoteRow>(
            "SELECT id, title, content, created_at, updated_at FROM notes WHERE id = $1",
        )
        .bind(note_id)
        .fetch_optional(&self.db)
        .await
        .map_err(map_db_error)?
        .ok_or_else(|| Status::not_found("note not found"))?;

        let summary = extractive_summary(&row.title, &row.content, 3);

        Ok(Response::new(SummarizeNoteResponse { summary }))
    }
}

// ---------------------------------------------------------------------------
// HTTP layer
// ---------------------------------------------------------------------------

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
    jwt_secret: String,
) -> Result<()> {
    let service = VaultNoteServiceImpl::new(db.clone(), redis.clone(), jwt_secret);

    // Spawn the background job worker.
    let worker_db = db.clone();
    let worker_redis = redis.clone();
    tokio::spawn(async move {
        run_job_worker(worker_db, worker_redis).await;
    });

    info!(%grpc_addr, "starting gRPC server");
    let grpc_future = Server::builder()
        .add_service(VaultNoteServiceServer::new(service))
        .serve(grpc_addr);

    info!(%http_addr, "starting HTTP server");
    let app = build_http_router(db, redis);
    let listener = TcpListener::bind(&http_addr).await?;
    let http_future = axum::serve(listener, app);

    tokio::select! {
        res = grpc_future => { res?; }
        res = http_future => { res?; }
    }

    Ok(())
}
