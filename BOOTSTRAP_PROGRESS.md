# VaultNote Bootstrap Progress

Date: 2026-03-09

## What is done so far

- Repository scaffold exists with:
  - `apps/mobile/VaultNote.Mobile`
  - `apps/server`
  - `proto/vaultnote/v1`
  - `infra`
  - `scripts`
- `.NET MAUI` app scaffolded and added to solution (`VaultNote.sln`).
- Rust server crate scaffolded with core dependencies (`tonic`, `axum`, `sqlx`, `redis`, etc.).
- Shared proto contract created at `proto/vaultnote/v1/vaultnote.proto` with:
  - `Ping`
  - `CreateNote`
  - `ListNotes`
- Rust protobuf build script added at `apps/server/build.rs`.
- Docker Compose added for Postgres + Redis in `infra/docker-compose.yml`.
- Local environment file created (`.env`) with DB/Redis/gRPC/HTTP addresses.
- SQLx migration created and applied:
  - `apps/server/migrations/20260309175809_init_notes.sql`
- Rust server currently includes:
  - HTTP `/health` endpoint via `axum`
  - HTTP `/ready` endpoint with DB readiness probe
  - gRPC `Ping` implementation
  - gRPC `CreateNote` implementation backed by Postgres
  - gRPC `ListNotes` implementation backed by Postgres
- MAUI `MainPage` now includes a `Ping Backend` button that calls Rust gRPC `Ping` and renders response/error text.
- Added real Rust automated tests at `apps/server/tests/server_tests.rs`:
  - `create_note_rejects_blank_title`
  - `create_and_list_notes_round_trip`
  - `health_and_ready_endpoints_return_ok`
- Rust source module skeleton folders created for continuation:
  - `config`, `grpc`, `http`, `db`, `cache`, `domain`, `app`
- Root `.gitignore` added to ignore build artifacts and local env files.
- Dependency alignment fix applied:
  - `apps/server/Cargo.toml`: `prost-types` changed to `0.13.4` to match tonic/prost generation stack.

## What worked

- `docker compose -f infra/docker-compose.yml up -d`
- Postgres health check (`pg_isready`) and Redis ping (`PONG`)
- `sqlx migrate run` and `sqlx migrate info` (migration is installed)
- `cargo check` for server after prost version fix
- Server startup and HTTP health check (`curl http://127.0.0.1:8080/health` returns `OK`)
- Server readiness check works (`curl http://127.0.0.1:8080/ready` returns `READY`)
- `xcode-select -p` now points to `/Applications/Xcode.app/Contents/Developer`
- MAUI Mac Catalyst build succeeds in this environment with:
  - `dotnet build apps/mobile/VaultNote.Mobile/VaultNote.Mobile.csproj -f net9.0-maccatalyst -p:ValidateXcodeVersion=false -p:EnableCodeSigning=false`
- Updated Rust + MAUI code compiles after Milestone 3/4 slice edits.
- Real Rust tests run via `cargo test --manifest-path apps/server/Cargo.toml`.

## What did not work (or not yet verified)

- `dotnet build apps/mobile/VaultNote.Mobile/VaultNote.Mobile.csproj -f net9.0-maccatalyst`
  - Fails by default due SDK/Xcode mismatch:
  - Installed MacCatalyst pack expects Xcode `26.0` while local Xcode is `26.3`.
- `dotnet build apps/mobile/VaultNote.Mobile/VaultNote.Mobile.csproj -f net9.0-maccatalyst -p:ValidateXcodeVersion=false`
  - Gets past version check but fails codesign due disallowed app bundle xattrs (`com.apple.FinderInfo`) in this workspace location.
- End-to-end MAUI runtime interaction is not fully validated yet (build succeeds; app launch + click-path still pending).
- Redis cache flow for `ListNotes` is not implemented yet.

## What is left

- Permanently align .NET MAUI workload with current Xcode (requires admin/system-level update) so build works without overrides.
- Implement Milestone 3+ backend and UI slice:
  - Launch app and validate MAUI `Ping` click-path against local server
- Add Redis caching for `ListNotes`.
- Extend `/ready` to include Redis readiness when cache integration is added.
- Add MAUI-side unit/integration tests and network-level gRPC integration coverage.

## Continue-from-here commands

### 1) Infra + DB

```bash
docker compose -f infra/docker-compose.yml up -d
cd apps/server
set -a
source ../../.env
set +a
sqlx migrate run
```

### 2) Run backend

```bash
cd /Users/oluwatobiolajide/Desktop/Code/grpc_mobile/vaultnote
set -a
source .env
set +a
cargo run --manifest-path apps/server/Cargo.toml --bin vaultnote-server
```

### 3) Verify HTTP health

```bash
curl http://127.0.0.1:8080/health
```

### 4) Build MAUI on current machine (working command)

```bash
dotnet build apps/mobile/VaultNote.Mobile/VaultNote.Mobile.csproj \
  -f net9.0-maccatalyst \
  -p:ValidateXcodeVersion=false \
  -p:EnableCodeSigning=false
```

### 5) Optional permanent local fix (admin required)

```bash
sudo dotnet workload update
```

### 6) Run real Rust tests

```bash
cd /Users/oluwatobiolajide/Desktop/Code/grpc_mobile/vaultnote/apps/server
set -a
source ../../.env
set +a
cargo test
```
