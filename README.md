# VaultNote

A private, cross-platform notes service. **Rust gRPC + HTTP backend**, **.NET MAUI** mobile client, **Postgres + Redis** for storage and cache — all wired together with a shared `protobuf` contract.

## Architecture

```
┌──────────────────────────┐         ┌────────────────────────────┐
│  .NET MAUI (mobile)      │  gRPC   │  Rust server               │
│  apps/mobile             │ ──────▶ │  apps/server (tonic + axum)│
│  - VaultNote.Proto       │         │  - HTTP /health /ready     │
└──────────────────────────┘         │  - gRPC VaultNoteService   │
                                     └──────┬────────────┬────────┘
                                            │            │
                                       ┌────▼─────┐ ┌────▼─────┐
                                       │ Postgres │ │  Redis   │
                                       └──────────┘ └──────────┘
```

- **`proto/vaultnote/v1/vaultnote.proto`** — single source of truth for the wire contract (Rust + C# generation).
- **`apps/server`** — Rust service exposing both gRPC and a small HTTP surface.
- **`apps/mobile`** — .NET MAUI client targeting iOS / Android / macOS / Windows.
- **`infra/docker-compose.yml`** — local Postgres + Redis for dev.

## What works today

- HTTP `/health` and `/ready` (with a Postgres readiness probe) via `axum`.
- gRPC `Ping`, `CreateNote`, `ListNotes` backed by Postgres (via `sqlx`).
- MAUI `MainPage` with a "Ping Backend" button that calls the gRPC `Ping`.
- SQLx migrations under `apps/server/migrations/`.
- Integration tests at `apps/server/tests/server_tests.rs`:
  - `create_note_rejects_blank_title`
  - `create_and_list_notes_round_trip`
  - `health_and_ready_endpoints_return_ok`

## Roadmap (defined in the proto)

- `SearchNotes` — server-streaming text search
- `UploadDocument` — client-streaming chunked upload, with PDF parsing (`lopdf`)
- `AskVault` — bidirectional streaming "ask anything" session over your notes
- `Register` / `Login` — `argon2` + `jsonwebtoken` auth
- `SemanticSearch` / `SummarizeNote` — vector + LLM features

## Run it locally

```sh
# 1. Start infra
docker compose -f infra/docker-compose.yml up -d

# 2. Apply migrations
cd apps/server
sqlx migrate run

# 3. Start the Rust server (HTTP :8080 + gRPC)
cargo run

# 4. Sanity-check
curl http://127.0.0.1:8080/health   # OK
curl http://127.0.0.1:8080/ready    # READY

# 5. Open VaultNote.sln in Visual Studio / Rider to run the MAUI app.
```

See [`BOOTSTRAP_PROGRESS.md`](./BOOTSTRAP_PROGRESS.md) for the current implementation log.

## Stack

**Server** — Rust 2021 · `tonic` (gRPC) · `axum` (HTTP) · `sqlx` (Postgres) · `redis` · `moka` (in-process cache) · `argon2` · `jsonwebtoken` · `tracing` · `prost` / `prost-build`

**Mobile** — .NET MAUI · generated C# proto stubs

**Infra** — Postgres · Redis · Docker Compose
