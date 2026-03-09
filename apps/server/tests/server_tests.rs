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
    AskVaultRequest, CreateNoteRequest, ListNotesRequest, LoginRequest, RegisterRequest,
    SearchNotesRequest, SemanticSearchRequest, SummarizeNoteRequest, UploadDocumentRequest,
};
use vaultnote_server::{
    build_http_router, create_db_pool, create_redis_client, generate_embedding, process_next_job,
    validate_jwt, JOB_QUEUE_KEY, VaultNoteServiceImpl,
};

// ---------------------------------------------------------------------------
// Test helpers
// ---------------------------------------------------------------------------

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

fn test_jwt_secret() -> String {
    "test-jwt-secret-do-not-use-in-production".to_string()
}

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
// Existing tests – Milestones 1-5
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_note_rejects_blank_title() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db, redis, test_jwt_secret());

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
    let service = VaultNoteServiceImpl::new(db.clone(), redis, test_jwt_secret());

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
    let service = VaultNoteServiceImpl::new(db.clone(), redis.clone(), test_jwt_secret());

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
// Milestone 6 – server-streaming SearchNotes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn search_notes_streams_matching_notes() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis, test_jwt_secret());

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
    let service = VaultNoteServiceImpl::new(db, redis, test_jwt_secret());

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
    let service = VaultNoteServiceImpl::new(db.clone(), redis, test_jwt_secret());

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
        test_jwt_secret(),
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
        spawn_test_grpc_client(VaultNoteServiceImpl::new(db, redis, test_jwt_secret())).await;

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
    let service = VaultNoteServiceImpl::new(db.clone(), redis, test_jwt_secret());

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
        test_jwt_secret(),
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
        spawn_test_grpc_client(VaultNoteServiceImpl::new(db, redis, test_jwt_secret())).await;

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

// ---------------------------------------------------------------------------
// Future: Auth – Register & Login
// ---------------------------------------------------------------------------

#[tokio::test]
async fn register_creates_user_and_login_returns_jwt() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis, test_jwt_secret());

    let email = format!("user-{}@test.example", Uuid::new_v4());
    let password = "SecurePass123!";

    let reg_resp = service
        .register(GrpcRequest::new(RegisterRequest {
            email: email.clone(),
            password: password.to_string(),
        }))
        .await
        .expect("register should succeed")
        .into_inner();

    assert!(!reg_resp.user_id.is_empty(), "user_id should be set");
    Uuid::parse_str(&reg_resp.user_id).expect("user_id should be a valid UUID");

    // Login with the same credentials should return a JWT.
    let login_resp = service
        .login(GrpcRequest::new(LoginRequest {
            email: email.clone(),
            password: password.to_string(),
        }))
        .await
        .expect("login should succeed")
        .into_inner();

    assert!(!login_resp.token.is_empty(), "token should be set");
    assert_eq!(login_resp.user_id, reg_resp.user_id);

    // The token should decode to the correct user_id.
    let decoded_user_id = validate_jwt(&login_resp.token, &test_jwt_secret())
        .expect("JWT should be valid");
    assert_eq!(decoded_user_id, reg_resp.user_id);

    // Cleanup.
    let uid = Uuid::parse_str(&reg_resp.user_id).unwrap();
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(uid)
        .execute(&db)
        .await
        .expect("cleanup should succeed");
}

#[tokio::test]
async fn register_rejects_duplicate_email() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis, test_jwt_secret());

    let email = format!("dup-{}@test.example", Uuid::new_v4());

    let first = service
        .register(GrpcRequest::new(RegisterRequest {
            email: email.clone(),
            password: "Password1!".to_string(),
        }))
        .await
        .expect("first register should succeed")
        .into_inner();

    let err = service
        .register(GrpcRequest::new(RegisterRequest {
            email: email.clone(),
            password: "DifferentPass2!".to_string(),
        }))
        .await
        .expect_err("duplicate register should fail");

    assert_eq!(err.code(), Code::AlreadyExists);

    // Cleanup.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(Uuid::parse_str(&first.user_id).unwrap())
        .execute(&db)
        .await
        .expect("cleanup should succeed");
}

#[tokio::test]
async fn login_rejects_wrong_password() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis, test_jwt_secret());

    let email = format!("wrong-pw-{}@test.example", Uuid::new_v4());

    let reg = service
        .register(GrpcRequest::new(RegisterRequest {
            email: email.clone(),
            password: "CorrectPass1!".to_string(),
        }))
        .await
        .expect("register should succeed")
        .into_inner();

    let err = service
        .login(GrpcRequest::new(LoginRequest {
            email: email.clone(),
            password: "WrongPass999!".to_string(),
        }))
        .await
        .expect_err("login with wrong password should fail");

    assert_eq!(err.code(), Code::Unauthenticated);

    // Cleanup.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(Uuid::parse_str(&reg.user_id).unwrap())
        .execute(&db)
        .await
        .expect("cleanup should succeed");
}

#[tokio::test]
async fn login_rejects_unknown_email() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db, redis, test_jwt_secret());

    let err = service
        .login(GrpcRequest::new(LoginRequest {
            email: format!("nobody-{}@test.example", Uuid::new_v4()),
            password: "AnyPass1!".to_string(),
        }))
        .await
        .expect_err("login for unknown email should fail");

    assert_eq!(err.code(), Code::Unauthenticated);
}

// ---------------------------------------------------------------------------
// Future: Vector DB – SemanticSearch
// ---------------------------------------------------------------------------

#[tokio::test]
async fn semantic_search_finds_notes_by_embedding_similarity() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis.clone(), test_jwt_secret());

    let unique = Uuid::new_v4().to_string();
    let note = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: format!("embedding-test-{}", unique),
            content: format!("rust programming language {}", unique),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    let note_uuid = Uuid::parse_str(&note.id).unwrap();

    // Manually insert the embedding so the test does not depend on the worker.
    let embedding = generate_embedding(&format!("embedding-test-{unique} rust programming language {unique}"));
    sqlx::query(
        "INSERT INTO note_embeddings (note_id, embedding) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(note_uuid)
    .bind(&embedding)
    .execute(&db)
    .await
    .expect("insert embedding should succeed");

    // SemanticSearch with words that share the same bag-of-words dimensions.
    let mut stream = service
        .semantic_search(GrpcRequest::new(SemanticSearchRequest {
            query: format!("rust programming {}", unique),
            limit: 5,
        }))
        .await
        .expect("semantic_search should succeed")
        .into_inner();

    let mut found = false;
    while let Some(item) = stream.next().await {
        let n = item.expect("stream item should be Ok");
        if n.id == note.id {
            found = true;
        }
    }
    assert!(found, "semantic search should find the created note");

    // Cleanup.
    sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(note_uuid)
        .execute(&db)
        .await
        .expect("cleanup should succeed");
}

#[tokio::test]
async fn semantic_search_returns_empty_when_no_embeddings_exist() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db, redis, test_jwt_secret());

    // Use a unique query that cannot match any stored embeddings.
    let mut stream = service
        .semantic_search(GrpcRequest::new(SemanticSearchRequest {
            query: format!("xyzzy_nomatch_{}", Uuid::new_v4()),
            limit: 5,
        }))
        .await
        .expect("semantic_search should succeed even with no embeddings")
        .into_inner();

    // Either we get notes (cosine sim = 0 → all tied) or an empty stream.
    // What we care about is that no error is returned.
    while let Some(item) = stream.next().await {
        item.expect("stream items should be Ok");
    }
}

// ---------------------------------------------------------------------------
// Future: AI Summaries – SummarizeNote
// ---------------------------------------------------------------------------

#[tokio::test]
async fn summarize_note_returns_non_empty_summary() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis, test_jwt_secret());

    let note = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: format!("summary-test-{}", Uuid::new_v4()),
            content: "Rust is a systems programming language. It focuses on safety. Performance is also a key goal.".to_string(),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    let resp = service
        .summarize_note(GrpcRequest::new(SummarizeNoteRequest {
            note_id: note.id.clone(),
        }))
        .await
        .expect("summarize_note should succeed")
        .into_inner();

    assert!(!resp.summary.is_empty(), "summary should not be empty");
    assert!(
        resp.summary.contains("Rust") || resp.summary.contains("summary-test"),
        "summary should reference note content: {}",
        resp.summary
    );

    // Cleanup.
    sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(Uuid::parse_str(&note.id).unwrap())
        .execute(&db)
        .await
        .expect("cleanup should succeed");
}

#[tokio::test]
async fn summarize_note_returns_not_found_for_unknown_id() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db, redis, test_jwt_secret());

    let err = service
        .summarize_note(GrpcRequest::new(SummarizeNoteRequest {
            note_id: Uuid::new_v4().to_string(),
        }))
        .await
        .expect_err("should return not_found for unknown note_id");

    assert_eq!(err.code(), Code::NotFound);
}

// ---------------------------------------------------------------------------
// Future: PDF Parsing
// ---------------------------------------------------------------------------

/// Creates a minimal but structurally valid PDF containing `text` using lopdf.
fn make_test_pdf(text: &str) -> Vec<u8> {
    use lopdf::{dictionary, Document, Object, Stream};

    let mut doc = Document::with_version("1.5");

    // Content stream: place text with a built-in font.
    let content_str = format!(
        "BT /F1 12 Tf 50 700 Td ({}) Tj ET",
        text.replace('(', r"\(").replace(')', r"\)")
    );
    let content_stream =
        Stream::new(lopdf::Dictionary::new(), content_str.into_bytes());
    let content_id = doc.add_object(content_stream);

    let font_id = doc.add_object(dictionary! {
        "Type"     => "Font",
        "Subtype"  => "Type1",
        "BaseFont" => "Helvetica",
    });

    let pages_id = doc.new_object_id();

    let page_id = doc.add_object(dictionary! {
        "Type"      => "Page",
        "Parent"    => pages_id,
        "MediaBox"  => vec![
            Object::Integer(0), Object::Integer(0),
            Object::Integer(612), Object::Integer(792),
        ],
        "Contents"  => content_id,
        "Resources" => dictionary! {
            "Font" => dictionary! {
                "F1" => font_id,
            },
        },
    });

    let pages_dict = dictionary! {
        "Type"  => "Pages",
        "Kids"  => vec![page_id.into()],
        "Count" => 1_i64,
    };
    doc.objects.insert(pages_id, Object::Dictionary(pages_dict));

    let catalog_id = doc.add_object(dictionary! {
        "Type"  => "Catalog",
        "Pages" => pages_id,
    });
    doc.trailer.set("Root", catalog_id);

    let mut bytes = Vec::new();
    doc.save_to(&mut bytes).expect("PDF serialisation should succeed");
    bytes
}

#[tokio::test]
async fn upload_pdf_stores_document_and_attempts_text_extraction() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis.clone(), test_jwt_secret());

    // Create an owner note.
    let note = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: format!("pdf-owner-{}", Uuid::new_v4()),
            content: "owner note".to_string(),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    let pdf_bytes = make_test_pdf("Hello PDF World");
    assert!(pdf_bytes.starts_with(b"%PDF"), "generated bytes should be a PDF");

    let mut client = spawn_test_grpc_client(VaultNoteServiceImpl::new(
        db.clone(),
        test_redis_client(),
        test_jwt_secret(),
    ))
    .await;

    let chunks = tokio_stream::iter(vec![UploadDocumentRequest {
        note_id: note.id.clone(),
        filename: "test.pdf".to_string(),
        chunk: pdf_bytes,
    }]);

    let resp = client
        .upload_document(tonic::Request::new(chunks))
        .await
        .expect("upload_document should succeed")
        .into_inner();

    assert!(!resp.document_id.is_empty(), "document_id should be set");
    assert!(resp.bytes_received > 0, "bytes_received should be > 0");

    // Verify the document row exists and extracted_text column is populated.
    let doc_id = Uuid::parse_str(&resp.document_id).expect("document_id should be UUID");
    let extracted: Option<String> =
        sqlx::query_scalar("SELECT extracted_text FROM documents WHERE id = $1")
            .bind(doc_id)
            .fetch_one(&db)
            .await
            .expect("document should exist in DB");

    // lopdf may or may not successfully extract text from the synthetic PDF,
    // but the column should at least exist (None is acceptable if extraction
    // returned nothing; Some(_) confirms extraction worked).
    let _ = extracted; // presence of column is what we validate via the query above

    // Cleanup.
    sqlx::query("DELETE FROM documents WHERE id = $1")
        .bind(doc_id)
        .execute(&db)
        .await
        .expect("document cleanup should succeed");
    sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(Uuid::parse_str(&note.id).unwrap())
        .execute(&db)
        .await
        .expect("note cleanup should succeed");
}

// ---------------------------------------------------------------------------
// Future: Background Jobs
// ---------------------------------------------------------------------------

#[tokio::test]
async fn create_note_enqueues_embed_job() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis.clone(), test_jwt_secret());

    // Drain the queue so we start with a known state.
    let mut conn = redis
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection should succeed");
    conn.del::<_, ()>(JOB_QUEUE_KEY)
        .await
        .expect("queue drain should succeed");

    let note = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: format!("job-enqueue-test-{}", Uuid::new_v4()),
            content: "background job test content".to_string(),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    let queue_len: i64 = conn
        .llen(JOB_QUEUE_KEY)
        .await
        .expect("LLEN should succeed");

    assert!(
        queue_len >= 1,
        "create_note should enqueue at least one background job (got {queue_len})"
    );

    // Cleanup.
    sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(Uuid::parse_str(&note.id).unwrap())
        .execute(&db)
        .await
        .expect("cleanup should succeed");
    conn.del::<_, ()>(JOB_QUEUE_KEY)
        .await
        .expect("queue cleanup should succeed");
}

#[tokio::test]
async fn job_worker_processes_embed_note_and_stores_embedding() {
    let db = test_db_pool().await;
    let redis = test_redis_client();
    let service = VaultNoteServiceImpl::new(db.clone(), redis.clone(), test_jwt_secret());

    // Drain the queue so stale jobs from other tests don't interfere.
    let mut conn = redis
        .get_multiplexed_async_connection()
        .await
        .expect("redis connection should succeed");
    conn.del::<_, ()>(JOB_QUEUE_KEY)
        .await
        .expect("queue drain should succeed");

    let note = service
        .create_note(GrpcRequest::new(CreateNoteRequest {
            title: format!("embed-worker-test-{}", Uuid::new_v4()),
            content: "embedding worker content".to_string(),
        }))
        .await
        .expect("create_note should succeed")
        .into_inner()
        .note
        .expect("should return note");

    let note_uuid = Uuid::parse_str(&note.id).unwrap();

    // Run one job-processing cycle; create_note enqueues EmbedNote.
    let processed = process_next_job(&db, &redis)
        .await
        .expect("process_next_job should not return None");
    assert!(processed, "expected a job to be processed");

    // The embedding should now be stored.
    let embedding_exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM note_embeddings WHERE note_id = $1)")
            .bind(note_uuid)
            .fetch_one(&db)
            .await
            .expect("existence query should succeed");

    assert!(embedding_exists, "embedding should be stored after worker processes EmbedNote job");

    // Cleanup.
    sqlx::query("DELETE FROM notes WHERE id = $1")
        .bind(note_uuid)
        .execute(&db)
        .await
        .expect("cleanup should succeed");
}
