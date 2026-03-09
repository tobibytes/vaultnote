use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use redis::AsyncCommands;
use sqlx::PgPool;
use tokio_stream::StreamExt;
use tonic::{Code, Request as GrpcRequest};
use tower::ServiceExt;
use uuid::Uuid;
use vaultnote_server::proto::vault_note_service_client::VaultNoteServiceClient;
use vaultnote_server::proto::vault_note_service_server::{VaultNoteService, VaultNoteServiceServer};
use vaultnote_server::proto::{
    AskVaultRequest, CreateNoteRequest, ListNotesRequest, SearchNotesRequest,
    UploadDocumentRequest,
};
use vaultnote_server::{build_http_router, create_db_pool, create_redis_client, VaultNoteServiceImpl};

async fn test_db_pool() -> PgPool {
    dotenvy::from_filename("../../.env").ok();
    dotenvy::dotenv().ok();

    let database_url = std::env::var("DATABASE_URL")
        .expect("DATABASE_URL must be set for tests (source ../../.env first)");

    let db = create_db_pool(&database_url)
        .await
        .expect("failed to create test DB pool");

    sqlx::migrate!("./migrations")
        .run(&db)
        .await
        .expect("failed to run migrations");

    db
}

fn test_redis_client() -> redis::Client {
    dotenvy::from_filename("../../.env").ok();
    dotenvy::dotenv().ok();

    let redis_url = std::env::var("REDIS_URL")
        .unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());

    create_redis_client(&redis_url).expect("failed to create test redis client")
}

#[tokio::test]
async fn create_note_rejects_blank_title() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db, redis);

    let result = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: "   ".to_string(),
            content: "content".to_string(),
        }))
        .await;

    let error = result.expect_err("expected invalid_argument for blank title");
    assert_eq!(error.code(), Code::InvalidArgument);
}

#[tokio::test]
async fn create_and_list_notes_round_trip() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis);

    let title = format!("test-note-{}", Uuid::new_v4());
    let content = format!("test-content-{}", Uuid::new_v4());

    let created = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: title.clone(),
            content: content.clone(),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("create_note should return note");

    let listed = service
        .list_notes(GrpcRequest::new(ListNotesRequest {}))
        .await
        .expect("list_notes should succeed")
        .into_inner()
        .notes;

    let matched = listed
        .iter()
        .find(|note| note.id == created.id)
        .expect("created note should be present in list");

    assert_eq!(matched.title, title);
    assert_eq!(matched.content, content);
    assert!(matched.created_at.is_some());
    assert!(matched.updated_at.is_some());

    let created_id = Uuid::parse_str(&created.id).expect("created id should be a UUID");
    sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(created_id)
        .execute(&db)
        .await
        .expect("cleanup delete should succeed");
}

#[tokio::test]
async fn list_notes_served_from_cache_after_first_call() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis.clone());

    let title = format!("cache-test-{}", Uuid::new_v4());

    let created = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: title.clone(),
            content: "cache content".to_string(),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    // First call: populates cache.
    let first = service
        .list_notes(GrpcRequest::new(ListNotesRequest {}))
        .await
        .expect("first list_notes should succeed")
        .into_inner()
        .notes;

    // Verify cache key was written.
    let redis_client = test_redis_client();
    let mut conn = redis_client
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection should succeed");
    let cached: Option<String> = conn
        .get("notes:list")
        .await
        .expect("redis GET should succeed");
    assert!(cached.is_some(), "notes:list cache key should exist after first list_notes call");

    // Second call: served from cache.
    let second = service
        .list_notes(GrpcRequest::new(ListNotesRequest {}))
        .await
        .expect("second list_notes should succeed")
        .into_inner()
        .notes;

    assert_eq!(first.len(), second.len(), "cache should return same results");

    let found = second.iter().any(|n| n.id == created.id);
    assert!(found, "cached list should include our note");

    // create_note should invalidate the cache.
    let title2 = format!("cache-test-2-{}", Uuid::new_v4());
    let created2 = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: title2.clone(),
            content: "second note".to_string(),
        }))
        .await
        .expect("second create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    let after_invalidate: Option<String> = conn
        .get("notes:list")
        .await
        .expect("redis GET should succeed");
    assert!(
        after_invalidate.is_none(),
        "notes:list cache key should be deleted after create_note"
    );

    // Cleanup.
    for id_str in [&created.id, &created2.id] {
        let id = Uuid::parse_str(id_str).expect("id should be UUID");
        sqlx::query("DELETE FROM notes WHERE id = $1")
            .bind(id)
            .execute(&db)
            .await
            .expect("cleanup should succeed");
    }
}

#[tokio::test]
async fn health_and_ready_endpoints_return_ok() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let app = build_http_router(db, redis);

    let health_response = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/health")
                .method("GET")
                .body(Body::empty())
                .expect("failed to build /health request"),
        )
        .await
        .expect("health request should succeed");

    assert_eq!(health_response.status(), StatusCode::OK);
    let health_body = to_bytes(health_response.into_body(), usize::MAX)
        .await
        .expect("failed to read /health body");
    assert_eq!(health_body, "OK");

    let ready_response = app
        .oneshot(
            Request::builder()
                .uri("/ready")
                .method("GET")
                .body(Body::empty())
                .expect("failed to build /ready request"),
        )
        .await
        .expect("ready request should succeed");

    assert_eq!(ready_response.status(), StatusCode::OK);
    let ready_body = to_bytes(ready_response.into_body(), usize::MAX)
        .await
        .expect("failed to read /ready body");
    assert_eq!(ready_body, "READY");
}

// ---------------------------------------------------------------------------
// Helper: spin up an in-process gRPC server and return a connected client.
// ---------------------------------------------------------------------------

async fn spawn_test_grpc_client(
    service: VaultNoteServiceImpl,
) -> VaultNoteServiceClient<tonic::transport::Channel> {
    use tokio_stream::wrappers::TcpListenerStream;

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("failed to bind test listener");
    let addr = format!("http://{}", listener.local_addr().expect("local_addr"));

    tokio::spawn(async move {
        tonic::transport::Server::builder()
            .add_service(VaultNoteServiceServer::new(service))
            .serve_with_incoming(TcpListenerStream::new(listener))
            .await
            .ok();
    });

    VaultNoteServiceClient::connect(addr)
        .await
        .expect("failed to connect test gRPC client")
}

// ---------------------------------------------------------------------------
// Milestone 6 – server-streaming SearchNotes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_notes_streams_matching_notes() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis);

    let unique = Uuid::new_v4().to_string();
    let title = format!("search-target-{}", unique);

    let created = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: title.clone(),
            content: "some body".to_string(),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    let mut stream = service
        .search_notes(GrpcRequest::new(SearchNotesRequest {
            query: unique.clone(),
        }))
        .await
        .expect("search_notes should succeed")
        .into_inner();

    let mut notes = vec![];
    while let Some(item) = stream.next().await {
        notes.push(item.expect("stream item should be Ok"));
    }

    assert!(
        notes.iter().any(|n| n.id == created.id),
        "streamed results should contain the created note"
    );

    // Cleanup.
    sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(Uuid::parse_str(&created.id).unwrap())
        .execute(&db)
        .await
        .expect("cleanup should succeed");
}

#[tokio::test]
async fn search_notes_returns_empty_stream_for_no_match() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db, redis);

    let mut stream = service
        .search_notes(GrpcRequest::new(SearchNotesRequest {
            query: format!("NOMATCHWILLEVER_{}", Uuid::new_v4()),
        }))
        .await
        .expect("search_notes should succeed")
        .into_inner();

    let mut count = 0usize;
    while let Some(item) = stream.next().await {
        item.expect("stream item should be Ok");
        count += 1;
    }

    assert_eq!(count, 0, "no notes should be returned for a non-matching query");
}

// ---------------------------------------------------------------------------
// Milestone 7 – client-streaming UploadDocument
// ---------------------------------------------------------------------------

#[tokio::test]
async fn upload_document_accumulates_chunks() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis);

    // Create a note to attach the document to.
    let note = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: format!("doc-owner-{}", Uuid::new_v4()),
            content: "owner note".to_string(),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    let note_id = note.id.clone();

    // Stream two chunks to the server via the in-process transport.
    let mut client = spawn_test_grpc_client(VaultNoteServiceImpl::new(
        db.clone(),
        test_redis_client(),
    ))
    .await;

    let chunk1 = b"Hello, ".to_vec();
    let chunk2 = b"World!".to_vec();
    let expected_bytes = (chunk1.len() + chunk2.len()) as i64;

    let chunks = tokio_stream::iter(vec![
        UploadDocumentRequest {
            note_id: note_id.clone(),
            filename: "hello.txt".to_string(),
            chunk: chunk1,
        },
        UploadDocumentRequest {
            note_id: note_id.clone(),
            filename: String::new(), // subsequent chunks may omit metadata
            chunk: chunk2,
        },
    ]);

    let resp = client
        .upload_document(tonic::Request::new(chunks))
        .await
        .expect("upload_document should succeed")
        .into_inner();

    assert!(!resp.document_id.is_empty(), "document_id should be set");
    assert_eq!(resp.bytes_received, expected_bytes);

    // Cleanup.
    let doc_id = Uuid::parse_str(&resp.document_id).expect("document_id should be UUID");
    sqlx::query("DELETE FROM documents WHERE id = $1")
        .bind(doc_id)
        .execute(&db)
        .await
        .expect("document cleanup should succeed");
    sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(Uuid::parse_str(&note_id).unwrap())
        .execute(&db)
        .await
        .expect("note cleanup should succeed");
}

#[tokio::test]
async fn upload_document_rejects_invalid_note_id() {
    let db = test_db_pool().await;
    let redis = test_redis_client();

    let mut client =
        spawn_test_grpc_client(VaultNoteServiceImpl::new(db, redis)).await;

    let chunks = tokio_stream::iter(vec![UploadDocumentRequest {
        note_id: "not-a-uuid".to_string(),
        filename: "bad.txt".to_string(),
        chunk: b"data".to_vec(),
    }]);

    let err = client
        .upload_document(tonic::Request::new(chunks))
        .await
        .expect_err("should reject invalid note_id");

    assert_eq!(err.code(), Code::InvalidArgument);
}

// ---------------------------------------------------------------------------
// Milestone 8 – bidirectional AskVault
// ---------------------------------------------------------------------------

#[tokio::test]
async fn ask_vault_streams_tokens_for_matching_notes() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis);

    let unique = Uuid::new_v4().to_string();
    let note = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: format!("vault-note-{}", unique),
            content: "vault content".to_string(),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    let mut client = spawn_test_grpc_client(VaultNoteServiceImpl::new(
        db.clone(),
        test_redis_client(),
    ))
    .await;

    let questions = tokio_stream::iter(vec![AskVaultRequest {
        question: unique.clone(),
    }]);

    let mut answer_stream = client
        .ask_vault(tonic::Request::new(questions))
        .await
        .expect("ask_vault should succeed")
        .into_inner();

    let mut tokens = vec![];
    while let Some(resp) = answer_stream.message().await.expect("stream should not error") {
        tokens.push(resp.token);
    }

    assert!(!tokens.is_empty(), "should receive at least one token");
    let full_answer = tokens.join("");
    assert!(
        full_answer.contains("1") || full_answer.contains("Found"),
        "answer should mention found notes: {full_answer}"
    );

    // Cleanup.
    sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(Uuid::parse_str(&note.id).unwrap())
        .execute(&db)
        .await
        .expect("cleanup should succeed");
}

#[tokio::test]
async fn ask_vault_responds_when_no_notes_match() {
    let db = test_db_pool().await;
    let redis = test_redis_client();

    let mut client =
        spawn_test_grpc_client(VaultNoteServiceImpl::new(db, redis)).await;

    let questions = tokio_stream::iter(vec![AskVaultRequest {
        question: format!("NOMATCHWILLEVER_{}", Uuid::new_v4()),
    }]);

    let mut answer_stream = client
        .ask_vault(tonic::Request::new(questions))
        .await
        .expect("ask_vault should succeed")
        .into_inner();

    let mut tokens = vec![];
    while let Some(resp) = answer_stream.message().await.expect("stream should not error") {
        tokens.push(resp.token);
    }

    assert!(!tokens.is_empty(), "should receive at least one token");
    let full_answer = tokens.join("");
    assert!(
        full_answer.contains("No relevant"),
        "answer should say no notes found: {full_answer}"
    );
}
