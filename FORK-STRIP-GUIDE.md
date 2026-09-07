# Zeroclaw Fork Strip Guide — Firecracker Lean Build

> Goal: keep only what Firecracker vMicro needs (agent + provider + mcp + workspace + lean gateway), remove heavy opts, and simplify CI to single `cargo check` + `linux x86_64` release binary.

Source: `Cargo.toml:222`, `crates/zeroclaw-memory/Cargo.toml:features`, `.github/workflows/ci.yml`, `.github/workflows/cross-platform-build-manual.yml`

---

## 1. Can you strip?

**Yes, 70-80% of `ci.yml` is optional.** `Cargo.toml:222` `features.default` is 6 crates only. Everything else is `optional = true`.

| Keep for swarm | Strip candidate | Why strippable |
|---|---|---|
| `agent-runtime` (core loop) | `channels-full` (+15 chats) | Replace with `default-channels` or `channel-webhook` alone (`Cargo.toml:244`) |
| `gateway` (needed for `/api/config` hot patch) | `gateway.embedded-web` / `web/dist` | Dashboard not needed in VM; API still works |
| `plugins-wasm-cranelift` (if you keep WASM) | `plugins-wasm-pulley`, `memory-postgres`, `observability-otel` | Postgres only for `zeroclaw-memory:memory-postgres` (`crates/zeroclaw-memory:11`), OTel heavy |
| `providers.models.*` via `zeroclaw-providers` | `browser-native`, `hardware`, `peripheral-rpi`, `sandbox-*`, `webauthn`, `dev-sim` | Hardware/browser not used headless |
| `mcp` (`crates/zeroclaw-config: mcp.servers`) | `acp-bridge`, `sop-graph`, `evals`, `tauri` | Desktop/ACPs not needed |

**Provider add + model switch stays straightforward after strip:** `providers.models.<type>.<alias>` is in `zeroclaw-config` core, not gated by `gateway`. You keep `zeroclaw-providers` via `agent-runtime`. Changing model is just `PATCH /api/config` `agents.<alias>.model_provider = "openai.gpt4"` (or `providers.models.openai.default.model = "gpt-4o"`). Works with stripped binary.

**Memory `sqlite` is mandatory** (`rusqlite` in `agent-runtime` `Cargo.toml:236`). `memory-postgres` is optional feature — you can drop `dep:postgres` safely. The sqlite file is `<data_dir>/agents/<alias>/memory.db` (`schema.rs:446`), not removable without losing recall.

**Gateway can be slimmed, not removed:** If you remove `gateway` feature entirely you lose `POST /api/config` + `/admin/reload` (`gateway/src/api_config.rs:397` `persist_and_swap`). Keep `gateway` but without `embedded-web` => API-only, 15MB smaller.

## 2. Minimal lean feature set for Firecracker

Create `Cargo.toml` override in fork:

```toml
[features]
default = ["agent-runtime","gateway"] # drop default-channels down to webhook only
default-channels = ["channel-webhook","channel-filesystem"] # not 6 defaults
lean = ["agent-runtime","gateway","plugins-wasm-cranelift"] # + if you need WASM C2
```

Build:
```bash
cargo check --no-default-features --features lean --locked
cargo build --release --no-default-features --features lean --target x86_64-unknown-linux-gnu
# binary ~35MB strip vs 85MB default
```

Or keep CLI-only (no gateway http) for pure wrapper-controlled: `--no-default-features --features agent-runtime` then wrapper edits `config.toml` directly on host mount before VM boot.

## 3. Minimal CI — only `cargo check` + linux binary

Replace `.github/workflows/ci.yml` (60K, 12 jobs, windows/mac matrix) with single-job `ci-lean.yml`:

```yaml
name: Lean CI
on: {push: {branches: [master, feat/*]}, pull_request: {branches: [master]}}
permissions: {contents: read}
jobs:
  check-build:
    runs-on: ubuntu-latest
    timeout-minutes: 25
    steps:
      - uses: actions/checkout@v6
      - uses: dtolnay/rust-toolchain@master
        with: {toolchain: 1.98.0}
      - uses: Swatinem/rust-cache@v2
      - run: cargo fmt --all -- --check
      - run: cargo check --locked --no-default-features --features lean
      - run: cargo build --release --locked --no-default-features --features lean
      - uses: actions/upload-artifact@v4
        with: {name: zeroclaw-linux-x86_64, path: target/release/zeroclaw}
```

**Keep `cargo check` as gate:** catches `Configurable` derive errors (`schema.rs`) before `build`. Remove `clippy`, `doc`, `nextest`, `bench`, `windows-clippy`, `i686`, `blacksmith`, `sbom`, `aur-freshness` jobs - all not needed for fork.

If you want `fmt` as separate gate, keep first 2 steps. Otherwise even `fmt` can be dropped.

**Size win:** `ci.yml:318` matrix `cargo build --profile ci --target ${{matrix.target}}` builds 4 targets (`x86_64`, `aarch64`, `windows`, `i686`) each `~15m`. Lean single `x86_64` is `~6m` + cache hit `~2m`.

## 4. Steps in `Pilser/zeroclaw` fork

```bash
cd /home/vox/.AAAPROJECTS/FPE/AI-plan/zeroclaw
git checkout -b feat/lean-firecracker
# 1. Edit Cargo.toml default features as above
# 2. Write .github/workflows/ci-lean.yml, delete or disable ci.yml via `paths-ignore` or rename to ci.yml.disabled
# 3. Optional: patch `crates/zeroclaw-config/src/schema.rs:19343` `default_config_dir` to `$HOME/.zeroclaw` -> `/data` for VM
git add Cargo.toml .github/workflows/ci-lean.yml
git commit -m "feat: lean firecracker fork - strip gateway mgmt only + lean CI"
git push pilser feat/lean-firecracker
gh workflow run "Lean CI" --repo Pilser/zeroclaw --ref feat/lean-firecracker
gh run watch --repo Pilser/zeroclaw
```

Binary appears in Actions `Artifacts` as `zeroclaw-linux-x86_64` - download to `firecracker` rootfs `/usr/local/bin/zeroclaw` or bake into `Containerfile:18` stage.

## 5. Caveat

Stripping `gateway.embedded-web` removes dashboard but keeps `/api/config` - wrapper must use `curl -H "Authorization: Bearer $TOKEN" http://127.0.0.1:18789/api/config/list`. If you strip `gateway` entirely, control goes via direct `config.toml` mount from host (simpler, no token, no `/admin/reload` pending state).

## 6. Recommended for wrapper managing many vContainers

`lean = agent-runtime + gateway (api-only) + plugins-wasm-cranelift` - covers `mcp`, `providers.models`, `agents`, `workspace` hot `PATCH`, keeps binary ~40MB, CI ~5m. This is the sweet spot between full debug `ci-all` (`Cargo.toml:374`) and kernel-only.
