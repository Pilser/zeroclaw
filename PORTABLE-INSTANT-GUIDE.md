# Portable Binary + Instant MCP/Provider via Zerowrapper Socket

## 1. Portable binary (run from anywhere)
Lean-native build is already portable:
- `rusqlite/bundled` + `rustls` (no OpenSSL) + `musl` fallback => single `zeroclaw` file
- No `~/.zeroclaw` hardcode: set `ZEROCLAW_CONFIG_DIR=/data/zeroclaw` or run with `--config-dir /tmp/cfg`
- Agentic work stays: `lean-native = agent-runtime+gateway+webhook+filesystem+plugins-native` (`Cargo.toml:234`) keeps LLM loop, tools, MCP client, sqlite memory

Built in `ci-lean.yml` now:
```yaml
cargo build --release --features lean-native --target x86_64-unknown-linux-gnu
# artifact: zeroclaw-linux-x86_64-portable
```
Download: `gh run download --repo Pilser/zeroclaw -n zeroclaw-linux-x86_64-portable`
Run anywhere:
```bash
chmod +x zeroclaw
ZEROCLAW_CONFIG_DIR=/tmp/my-cfg ./zeroclaw daemon --host 127.0.0.1 -p 18789
./zeroclaw --config-dir /tmp/my-cfg agent list
```
Check: `ldd zeroclaw` shows `linux-vdso + libgcc + libc.so.6` (glibc 2.31+). For fully static `musl` add target `x86_64-unknown-linux-musl` + `rustup target add`.

## 2. Why zerowrapper streaming already works over socket
`zerowrapper/config.toml:3` `zeroclaw_socket=/tmp/.zeroclaw/daemon.sock`
`zw-channels/src/zeroclaw_bridge.rs:99` `UnixStream::connect(socket_path)` + `zw-core/src/config.rs:8` streams `PromptStream` events via that socket. No extra gateway needed; binary just needs daemon sock at that path (or `ZEROWRAPPER__ZEROCLAW_SOCKET` env).

## 3. Instant MCP / Provider / Model (fixing "terrible many steps")
Old flow: `config.toml` edit 5 sections + `plugins.entries` + manual `provider` + `model` + restart.

**New native plugin instant (firecracker VM isolation = safe):**
Native plugin gets `NativePluginContext` (`crates/zeroclaw-plugins/src/native.rs:48`) with full `Arc<RwLock<Config>>`:

```rust
// inside your .so plugin (cdylib)
#[no_mangle]
pub extern "C" fn zeroclaw_plugin_init_ctx(ctx: *mut NativePluginContext) {
    let ctx = unsafe { &*ctx };
    // add MCP instantly — no skills dir, no manifest dance
    ctx.set_config_value("mcp.servers.fetch.command", "npx").unwrap();
    ctx.set_config_value("mcp.servers.fetch.args", "[\"-y\",\"@modelcontextprotocol/server-fetch\"]").unwrap();
    ctx.set_config_value("mcp.servers.fetch.transport", "stdio").unwrap();
    // add provider + model instantly
    ctx.set_config_value("providers.models.openai.gpt4.model", "gpt-4o").unwrap();
    ctx.set_config_value("providers.models.openai.gpt4.api_key", "sk-...").unwrap();
    ctx.set_config_value("agents.main.model_provider", "openai.gpt4").unwrap();
    ctx.save_config().unwrap(); // calls save_dirty, marks dirty, persists
}
```

Or without writing a `.so`, use the wrapper directly over the socket (one-shot, no restart):
```bash
# via zeroclaw bridge socket (streaming)
echo '{"jsonrpc":"2.0","id":1,"method":"config/set","params":{"path":"mcp.servers.fetch.command","value":"npx"}}' | socat - UNIX-CONNECT:/tmp/.zeroclaw/daemon.sock
# or via gateway if enabled
curl -X PATCH http://127.0.0.1:18789/api/config \
  -H "Authorization: Bearer $TOKEN" \
  -d '[{"op":"replace","path":"/mcp/servers/fetch/command","value":"npx"}]'
curl -X PATCH http://127.0.0.1:18789/api/config \
  -d '[{"op":"replace","path":"/providers/models/openai/default/model","value":"gpt-4o"}]'
```

After this PR, `PluginsConfig::default` is `enabled:true, auto_discover:true` so no `plugins.enabled` toggle needed; `.so` hot-replaces via `notify` watcher + mtime on next call (`native.rs:272 reload_if_changed`).

**For Firecracker rootfs:** Bake `zeroclaw` + `zerowrapper` + one `instant.so` that on `init_ctx` seeds your fleet's MCP/provider/model from env — instant, no manual steps.
