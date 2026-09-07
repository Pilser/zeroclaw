//! Native (.so) plugin host for Firecracker-isolated deployments.
//!
//! Firecracker already provides VM-level isolation, so WASM sandboxing is
//! redundant. Native plugins are `cdylib` Rust crates compiled to `.so` and
//! loaded via `dlopen`. They run with full host privileges and can control
//! the entire agent (config, providers, workspace, MCP, etc.).
//!
//! Design:
//! - `native_path` in manifest (e.g. `native_path = "plugin.so"`) replaces `wasm_path`
//! - Plugins are **always enabled**: discovery ignores `plugins.enabled` gate
//! - **Auto-reload**: file-watcher on `plugins_dir` + mtime check on every call
//! - **Full config access**: `NativePluginContext` exposes `Arc<RwLock<Config>>`
//!   and `set_prop`/`get_prop` for any path, bypassing per-instance scoping.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::SystemTime;

use crate::error::PluginError;
use crate::{PluginCapability, PluginManifest};

// ── Native plugin manifest extension ──────────────────────────────

/// Extension for native plugins: `native_path` is the .so file relative to
/// the plugin directory. If `native_path` is set, `wasm_path` is ignored.
pub fn native_path_for_plugin(plugin_dir: &Path, manifest: &PluginManifest) -> Option<PathBuf> {
    // Check for `native_path` inside the manifest's raw TOML extras if needed.
    // For now, reuse `wasm_path` when it ends with `.so` (back-compat), and
    // also support explicit `native_path` via extra field in PluginManifest.
    if let Some(p) = manifest.native_path.as_deref() {
        return Some(plugin_dir.join(p));
    }
    if let Some(w) = manifest.wasm_path.as_deref() {
        if w.ends_with(".so") {
            return Some(plugin_dir.join(w));
        }
    }
    None
}

// ── Plugin context: full config access ───────────────────────────

/// Context passed to native plugins on init. Gives **full** control over the
/// host config, providers, workspaces, MCP, etc. — no per-instance scoping.
pub struct NativePluginContext {
    /// Shared live config. Plugin can read/write any path.
    pub config: Arc<RwLock<zeroclaw_config::schema::Config>>,
    /// Plugin's own manifest
    pub manifest: PluginManifest,
    /// Plugin directory on disk
    pub plugin_dir: PathBuf,
}

impl NativePluginContext {
    pub fn get_config_value(&self, path: &str) -> anyhow::Result<String> {
        let cfg = self
            .config
            .read()
            .map_err(|_| anyhow::anyhow!("config lock poisoned"))?;
        cfg.get_prop(path)
            .map_err(|e| anyhow::anyhow!(e.to_string()))
    }

    pub fn set_config_value(&self, path: &str, value: &str) -> anyhow::Result<()> {
        let mut cfg = self
            .config
            .write()
            .map_err(|_| anyhow::anyhow!("config lock poisoned"))?;
        cfg.set_prop(path, value)
            .map_err(|e| anyhow::anyhow!(e.to_string()))?;
        cfg.mark_dirty(path);
        Ok(())
    }

    /// Persist dirty paths to disk (calls `save_dirty`).
    pub fn save_config(&self) -> anyhow::Result<()> {
        // save_dirty is async; for native sync context, block on it via futures
        let cfg = self
            .config
            .read()
            .map_err(|_| anyhow::anyhow!("config lock poisoned"))?;
        let path = cfg.config_path.clone();
        drop(cfg);
        // Use blocking runtime to save
        let handle = tokio::runtime::Handle::try_current();
        if let Ok(h) = handle {
            let cfg_clone = Arc::clone(&self.config);
            h.block_on(async move {
                let mut cfg = cfg_clone.write().unwrap();
                // Use try to avoid panic
                let res = cfg.save_dirty().await;
                res.map_err(|e| anyhow::anyhow!(e.to_string()))
            })
        } else {
            // No runtime — spawn one
            let cfg_clone = Arc::clone(&self.config);
            std::thread::spawn(move || {
                let rt = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap();
                rt.block_on(async move {
                    let mut cfg = cfg_clone.write().unwrap();
                    let _ = cfg.save_dirty().await;
                });
            })
            .join()
            .map_err(|_| anyhow::anyhow!("save thread panicked"))?;
            Ok(())
        }
        let _ = path;
        Ok(())
    }

    pub fn get_all_config_paths(&self) -> Vec<String> {
        let cfg = self.config.read().unwrap();
        cfg.prop_fields().into_iter().map(|f| f.name).collect()
    }
}

// ── Native plugin handle (dlopen wrapper) ────────────────────────

pub struct NativePluginHandle {
    _lib: libloading::Library,
    pub manifest: PluginManifest,
    pub plugin_dir: PathBuf,
    pub so_path: PathBuf,
    pub mtime: Option<SystemTime>,
}

impl NativePluginHandle {
    pub unsafe fn load(
        so_path: &Path,
        manifest: PluginManifest,
        plugin_dir: PathBuf,
    ) -> Result<Self, PluginError> {
        let lib = unsafe { libloading::Library::new(so_path) }
            .map_err(|e| PluginError::LoadFailed(format!("dlopen {}: {}", so_path.display(), e)))?;
        let mtime = std::fs::metadata(so_path)
            .ok()
            .and_then(|m| m.modified().ok());
        // Optionally call plugin init symbol if present
        unsafe {
            if let Ok(init) = lib.get::<unsafe extern "C" fn()>(b"zeroclaw_plugin_init") {
                init();
            }
        }
        Ok(Self {
            _lib: lib,
            manifest,
            plugin_dir,
            so_path: so_path.to_path_buf(),
            mtime,
        })
    }

    pub fn needs_reload(&self) -> bool {
        if let Ok(meta) = std::fs::metadata(&self.so_path) {
            if let Ok(mtime) = meta.modified() {
                if let Some(old) = self.mtime {
                    return mtime > old;
                }
            }
        }
        false
    }

    pub fn get_symbol<T>(&self, name: &[u8]) -> Option<libloading::Symbol<T>> {
        unsafe { self._lib.get(name).ok() }
    }
}

// ── Native plugin host with auto-reload ──────────────────────────

pub struct NativePluginHost {
    plugins_dir: PathBuf,
    loaded: HashMap<String, NativePluginHandle>,
    config: Option<Arc<RwLock<zeroclaw_config::schema::Config>>>,
    _watcher: Option<notify::RecommendedWatcher>,
}

impl NativePluginHost {
    pub fn new(
        plugins_dir: &Path,
        config: Option<Arc<RwLock<zeroclaw_config::schema::Config>>>,
    ) -> Result<Self, PluginError> {
        let mut host = Self {
            plugins_dir: plugins_dir.to_path_buf(),
            loaded: HashMap::new(),
            config,
            _watcher: None,
        };
        host.discover()?;
        host.start_watcher()?;
        Ok(host)
    }

    pub fn discover(&mut self) -> Result<(), PluginError> {
        if !self.plugins_dir.exists() {
            return Ok(());
        }
        for entry in std::fs::read_dir(&self.plugins_dir)?.flatten() {
            let path = entry.path();
            if !path.is_dir() {
                continue;
            }
            let manifest_path = path.join("manifest.toml");
            if !manifest_path.exists() {
                continue;
            }
            let content = match std::fs::read_to_string(&manifest_path) {
                Ok(c) => c,
                Err(_) => continue,
            };
            let manifest: PluginManifest = match toml::from_str(&content) {
                Ok(m) => m,
                Err(_) => continue,
            };
            if let Some(so_path) = native_path_for_plugin(&path, &manifest) {
                if !so_path.exists() {
                    continue;
                }
                // For native plugins, ignore plugins.enabled gate — always enabled
                // Also bypass signature check for firecracker (VM isolation)
                unsafe {
                    match NativePluginHandle::load(&so_path, manifest.clone(), path.clone()) {
                        Ok(handle) => {
                            // Call init with full config context if available
                            if let Some(ref cfg) = self.config {
                                let ctx = NativePluginContext {
                                    config: Arc::clone(cfg),
                                    manifest: manifest.clone(),
                                    plugin_dir: path.clone(),
                                };
                                // If plugin exports zeroclaw_plugin_init_ctx, call it
                                if let Some(init_ctx) = handle.get_symbol::<unsafe extern "C" fn(
                                    *mut NativePluginContext,
                                )>(
                                    b"zeroclaw_plugin_init_ctx"
                                ) {
                                    // Leak ctx for plugin to hold if needed — plugin should not free it
                                    let ctx_ptr = Box::into_raw(Box::new(ctx));
                                    unsafe {
                                        init_ctx(ctx_ptr);
                                    }
                                    // Reclaim box to avoid leak if plugin copied data
                                    unsafe {
                                        let _ = Box::from_raw(ctx_ptr);
                                    }
                                }
                            }
                            self.loaded.insert(manifest.name.clone(), handle);
                        }
                        Err(e) => {
                            ::zeroclaw_log::record!(
                                WARN,
                                ::zeroclaw_log::Event::new(module_path!(), ::zeroclaw_log::Action::Note)
                                    .with_outcome(::zeroclaw_log::EventOutcome::Failure)
                                    .with_attrs(::serde_json::json!({"plugin": manifest.name, "error": format!("{}", e)})),
                                "failed to load native plugin"
                            );
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Check mtimes and reload changed plugins. Called on every tool/channel invocation.
    pub fn reload_if_changed(&mut self) {
        let mut to_reload = Vec::new();
        for (name, handle) in &self.loaded {
            if handle.needs_reload() {
                to_reload.push(name.clone());
            }
        }
        for name in to_reload {
            if let Some(old) = self.loaded.remove(&name) {
                let so_path = old.so_path.clone();
                let manifest = old.manifest.clone();
                let plugin_dir = old.plugin_dir.clone();
                drop(old); // dlclose
                unsafe {
                    if let Ok(new_handle) =
                        NativePluginHandle::load(&so_path, manifest.clone(), plugin_dir.clone())
                    {
                        ::zeroclaw_log::record!(
                            INFO,
                            ::zeroclaw_log::Event::new(
                                module_path!(),
                                ::zeroclaw_log::Action::Note
                            ),
                            &format!("native plugin '{}' auto-reloaded", name)
                        );
                        self.loaded.insert(name, new_handle);
                    }
                }
            }
        }
        // Also discover new plugins added after startup
        let _ = self.discover();
    }

    fn start_watcher(&mut self) -> Result<(), PluginError> {
        use notify::Watcher;
        let dir = self.plugins_dir.clone();
        // Use debouncer for coalescing rapid writes
        // For now, simple watcher that triggers discover on next reload_if_changed
        // Actual async watch is handled via polling in reload_if_changed; this
        // keeps the watcher alive to receive events.
        let mut watcher = notify::recommended_watcher(move |res: Result<notify::Event, _>| {
            if let Ok(event) = res {
                let _ = event;
                // Event received — next reload_if_changed will pick it up
            }
            let _ = dir;
        })
        .map_err(|e| {
            PluginError::Io(std::io::Error::new(
                std::io::ErrorKind::Other,
                e.to_string(),
            ))
        })?;
        if self.plugins_dir.exists() {
            let _ = watcher.watch(&self.plugins_dir, notify::Recursive::Yes);
        }
        self._watcher = Some(watcher);
        Ok(())
    }

    pub fn get(&self, name: &str) -> Option<&NativePluginHandle> {
        self.loaded.get(name)
    }

    pub fn list(&self) -> Vec<String> {
        self.loaded.keys().cloned().collect()
    }

    pub fn is_always_enabled() -> bool {
        true
    }
}
