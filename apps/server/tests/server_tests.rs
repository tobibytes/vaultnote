use axum::{
    body::{to_bytes, Body},
    http::{Request, StatusCode},
};
use sqlx::PgPool;
use tonic::{Code, Request as GrpcRequest};
use tower::ServiceExt;
use uuid::Uuid;
use vaultnote_server::proto::vault_note_service_server::VaultNoteService;
use vaultnote_server::proto::{CreateNoteRequest, ListNotesRequest};
use vaultnote_server::{build_http_router, create_db_pool, VaultNoteServiceImpl};

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

#[tokio::test]
async fn create_note_rejects_blank_title() {
    let db = test_db_pool().await;
    let service = VaultNoteServiceImpl::new(db);

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
    let service = VaultNoteServiceImpl::new(db.clone());

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
async fn health_and_ready_endpoints_return_ok() {
    let db = test_db_pool().await;
    let app = build_http_router(db);

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
