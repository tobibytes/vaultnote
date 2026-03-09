use anyhow::{Context, Result};
use std::net::SocketAddr;
use tracing_subscriber::EnvFilter;
use vaultnote_server::{create_db_pool, create_redis_client, run_servers};

#[tokio::main]
async fn main() -> Result<()> {
    dotenvy::dotenv().ok();

    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();

    let database_url = std::env::var("DATABASE_URL").context("DATABASE_URL must be set")?;
    let redis_url = std::env::var("REDIS_URL").unwrap_or_else(|_| "redis://127.0.0.1:6379".to_string());
    let grpc_addr: SocketAddr = std::env::var("GRPC_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:50051".to_string())
        .parse()
        .context("invalid GRPC_ADDR")?;
    let http_addr: SocketAddr = std::env::var("HTTP_ADDR")
        .unwrap_or_else(|_| "127.0.0.1:8080".to_string())
        .parse()
        .context("invalid HTTP_ADDR")?;

    let jwt_secret = std::env::var("JWT_SECRET")
        .unwrap_or_else(|_| "change-me-in-production".to_string());

    let db = create_db_pool(&database_url).await?;
    let redis = create_redis_client(&redis_url)?;
    run_servers(db, redis, grpc_addr, http_addr, jwt_secret).await
}
