//! `wafer-site/content` — serves the site's static content (docs, landing
//! page, playground HTML) directly from disk.
//!
//! ## Why not `wafer-run/web`?
//!
//! `wafer-run/web` dispatches reads through the `wafer-run/storage` alias,
//! which `SolobaseBuilder` replaces with `SolobaseStorageBlock`. That wrapper
//! namespaces every folder under the calling block's name
//! (`wafer-run/web/<folder>/<key>`), so the pre-solobase layout of
//! `$CARGO_MANIFEST_DIR/dist/...` stops resolving unless we either copy
//! every file into the namespaced prefix or route requests through a
//! non-namespaced block.
//!
//! Rather than duplicate storage or cross-block shenanigans, this block
//! opens its own [`LocalStorageService`] rooted at the site's `dist/`
//! directory and serves from there. Reads never hit `wafer-run/storage`,
//! so namespacing doesn't apply. The block is scoped to the site binary
//! (no `wafer-run/*` or `suppers-ai/*` name) because it's strictly a
//! local SPA-content helper.

use std::{path::Path, sync::Arc};

use wafer_block_local_storage::service::LocalStorageService;
use wafer_core::interfaces::storage::service::StorageService;
use wafer_run::{
    Block, BlockCategory, BlockInfo, Context, ErrorCode, InputStream, InstanceMode,
    LifecycleEvent, Message, MetaEntry, OutputStream, RuntimeError, Wafer, WaferError,
    META_RESP_CONTENT_TYPE,
};

/// Site content block name. Site-local (not an upstream block) so the
/// `{org}/{name}` convention doesn't collide with anything published.
pub const NAME: &str = "wafer-site/content";

/// Block serving `$dist_root/**` as static files.
pub struct ContentBlock {
    service: Arc<LocalStorageService>,
    /// Folder name passed to the storage service. `LocalStorageService`
    /// joins this under its root, so setting it to `""` reads directly
    /// from `dist_root`.
    folder: String,
    index_file: String,
}

impl ContentBlock {
    pub fn new(dist_root: &str) -> anyhow::Result<Self> {
        let service = LocalStorageService::new(dist_root)
            .map_err(|e| anyhow::anyhow!("LocalStorageService::new({dist_root}): {e:?}"))?;
        Ok(Self {
            service: Arc::new(service),
            folder: String::new(),
            index_file: "index.html".to_string(),
        })
    }

    async fn serve(&self, msg: &Message) -> OutputStream {
        let mut path = msg.path().to_string();
        if path.is_empty() || path == "/" {
            path = format!("/{}", self.index_file);
        }

        let clean = clean_path(&path);
        // Block dotfiles (except `.well-known/*`).
        if clean
            .split('/')
            .any(|seg| seg.starts_with('.') && seg.len() > 1 && seg != ".well-known")
        {
            return not_found();
        }
        let key = clean.trim_start_matches('/');

        // Try exact key → key.html → key/index.html. Mirrors the lookup order
        // used by `wafer-run/web` so clean URLs (e.g. `/docs`) resolve to
        // `docs.html` when no directory exists.
        match self.service.get(&self.folder, key).await {
            Ok((data, info)) => respond(data, content_type(key, &info.content_type)),
            Err(_) if !key.is_empty() && Path::new(key).extension().is_none() => {
                let html_key = format!("{key}.html");
                match self.service.get(&self.folder, &html_key).await {
                    Ok((data, info)) => respond(data, content_type(&html_key, &info.content_type)),
                    Err(_) => {
                        let idx_key = format!("{}/{}", key, self.index_file);
                        match self.service.get(&self.folder, &idx_key).await {
                            Ok((data, info)) => {
                                respond(data, content_type(&idx_key, &info.content_type))
                            }
                            Err(_) => not_found(),
                        }
                    }
                }
            }
            Err(_) => not_found(),
        }
    }
}

#[async_trait::async_trait]
impl Block for ContentBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(
            NAME,
            "0.0.1",
            "http-handler@v1",
            "Site static content server (reads $CARGO_MANIFEST_DIR/dist directly)",
        )
        .instance_mode(InstanceMode::Singleton)
        .category(BlockCategory::Infrastructure)
    }

    async fn handle(&self, _ctx: &dyn Context, msg: Message, _input: InputStream) -> OutputStream {
        let action = msg.action().to_string();
        if !action.is_empty() && action != "retrieve" {
            return OutputStream::error(WaferError {
                code: ErrorCode::Unimplemented,
                message: "Only retrieve action is supported".to_string(),
                meta: vec![],
            });
        }
        self.serve(&msg).await
    }

    async fn lifecycle(
        &self,
        _ctx: &dyn Context,
        _event: LifecycleEvent,
    ) -> std::result::Result<(), WaferError> {
        Ok(())
    }
}

pub fn register(w: &mut Wafer, dist_root: &str) -> anyhow::Result<()> {
    let block = ContentBlock::new(dist_root)?;
    w.register_block(NAME, Arc::new(block))
        .map_err(|e: RuntimeError| anyhow::anyhow!("register {NAME}: {e}"))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn clean_path(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for seg in p.split('/') {
        match seg {
            "" | "." => continue,
            ".." => {
                parts.pop();
            }
            s => parts.push(s),
        }
    }
    format!("/{}", parts.join("/"))
}

fn content_type(key: &str, from_storage: &str) -> String {
    if from_storage.is_empty() || from_storage == "application/octet-stream" {
        guess_mime(key).to_string()
    } else {
        from_storage.to_string()
    }
}

fn guess_mime(path: &str) -> &'static str {
    let ext = Path::new(path)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" => "application/json",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "ttf" => "font/ttf",
        "otf" => "font/otf",
        "txt" | "md" => "text/plain; charset=utf-8",
        "wasm" => "application/wasm",
        _ => "application/octet-stream",
    }
}

fn respond(data: Vec<u8>, ct: String) -> OutputStream {
    OutputStream::respond_with_meta(
        data,
        vec![MetaEntry {
            key: META_RESP_CONTENT_TYPE.to_string(),
            value: ct,
        }],
    )
}

fn not_found() -> OutputStream {
    OutputStream::error(WaferError {
        code: ErrorCode::NotFound,
        message: "Not found".to_string(),
        meta: vec![],
    })
}
