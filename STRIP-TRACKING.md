# Strip Tracking — Fork Lean Progress

> Workflow: `cargo check` only while stripping. `cargo build` commented out in `.github/workflows/ci-lean.yml` until lean set stabilizes.
> Base: `Cargo.toml:222` `features.default` + `crates/zeroclaw-memory/Cargo.toml:features`

## How to use
1. Pick feature from `CANDIDATES` below
2. Comment it out in `Cargo.toml` `default` or dependent crate
3. Push -> `ci-lean.yml` runs `cargo check --features lean` only
4. If check green, move row to `REMOVED` with commit sha
5. If check red, move to `KEPT` with reason
6. When `REMOVED` stable, uncomment `cargo build` in `ci-lean.yml`

## CI Mode
Current: `CHECK ONLY` — `cargo build` line commented in `ci-lean.yml:18`
Next: uncomment after 80% stripped, then verify binary size.

---

### REMOVED (verified via `cargo check` green)

| Date | Feature | Cargo.toml line | Commit | Size delta | Notes |
|------|---------|-----------------|--------|------------|-------|
| | | | | | _add rows as you strip_ |

Example:
| 2026-09-07 | `memory-postgres` | `Cargo.toml:315` | `feat/lean-abc123` | -4MB | postgres dep dropped, sqlite stays |
| 2026-09-07 | `browser-native` | `Cargo.toml:333` | | -18MB | headless VM no browser |

### KEPT (required for Firecracker swarm)

| Feature | Reason to keep |
|---------|----------------|
| `agent-runtime` | core loop + providers + mcp + sqlite |
| `gateway` (api-only, no `embedded-web`) | `PATCH /api/config` hot model/mcp/workspace |
| `channel-webhook` | minimal ingress for wrapper callback |
| `plugins-wasm-cranelift` | only if WASM C2 needed, else can remove later |

### CANDIDATES — To Evaluate (comment out one by one)

| # | Feature | Where | Risk if removed | Priority to strip |
|---|---------|-------|-----------------|-------------------|
| 1 | `channel-email,telegram,discord` | `Cargo.toml:274-307` | lose that chat | HIGH |
| 2 | `channels-full` (15 extra) | `Cargo.toml:250` | lose slack/signal etc | HIGH |
| 3 | `memory-postgres` | `Cargo.toml:315` / `crates/zeroclaw-memory` | lose pg backend | HIGH |
| 4 | `browser-native` | `Cargo.toml:333` | lose browser tool | HIGH |
| 5 | `hardware, peripheral-rpi, probe` | `Cargo.toml:322` | lose pi gpio | HIGH |
| 6 | `sandbox-landlock/bubblewrap` | `Cargo.toml:331` | lose sandbox | MEDIUM — VM already jailed |
| 7 | `webauthn` | `Cargo.toml:334` | lose passkey | HIGH |
| 8 | `observability-otel` | `Cargo.toml:322` | lose otel | HIGH |
| 9 | `observability-prometheus` | `Cargo.toml:318` | lose metrics | LOW — keep if need metrics |
| 10 | `acp-bridge` | `Cargo.toml:265` | lose ACP editor | HIGH |
| 11 | `embedded-web` | `Cargo.toml:335` | lose dashboard | HIGH — keep gateway api |
| 12 | `plugins-wasm-pulley` | `Cargo.toml:362` | lose arm32 jit | HIGH — keep cranelift only |
| 13 | `voice-wake, whatsapp-web` | `Cargo.toml:313` | lose voice | HIGH |

### Log

| Date | Action | `cargo check` | Branch | Runner |
|------|--------|---------------|--------|--------|
| | init lean CI check-only | pending | `feat/lean-firecracker` | `ubuntu-latest` |

---

## Commands

```bash
# local check same as CI
cargo check --locked --no-default-features --features lean
cargo check --locked --no-default-features --features agent-runtime,gateway # without plugins
```
