//! What `/_health` answers, and what the deploy script's post-promotion gate
//! (`health_gate_verdict`, scripts/lib/deploy-checks.sh) decides from it, for
//! each state the config source and the runtime can be in.
//!
//! The site's own health block is dispatched through a real runtime: a block
//! declaring a required key, initialized through a config source that answers
//! as each test scripts, so its slot holds what that answer leaves (ready,
//! retrying, failed); and, for the states only a platform knows (published
//! degraded, stale, a failed rebuild, what the last attempt found of D1),
//! impresspress's platform health source, registered the way the Cloudflare
//! entry registers its runtime cache's.
//!
//! Every body is checked for the source's error text and the failing `Init`'s,
//! which must stay out of this public route.

use std::{
    cell::RefCell,
    collections::HashMap,
    sync::{Arc, Mutex},
};

use impresspress_core::runtime_health::{
    set_platform_health_source, BackendCheck, PlatformHealth, PlatformHealthSource, RetryEpisode,
};
use wafer_block::{
    core_types::{ErrorCode, LifecycleEvent, LifecycleType, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar,
};
use wafer_run::{
    ConfigError, ConfigSource, Context, EnvBlockConfig, Wafer, META_RESP_HEADER_PREFIX,
    META_RESP_STATUS,
};
use wafer_site::blocks::health;

/// The block whose config the source answers for.
const DECLARING: &str = "test/declaring";
/// The required key [`DECLARING`] declares.
const KEY: &str = "TEST__DECLARING__REQUIRED";
/// A block that declares no config and whose own `Init` fails transiently.
const RESETTING: &str = "test/resetting";
/// Source error text that must stay out of the public body.
const SOURCE_TEXT: &str = "D1_ERROR: daily row read limit exceeded";
/// [`RESETTING`]'s `Init` error text, which must stay out of it too.
const RESET_TEXT: &str = "D1_ERROR: D1 DB exceeded its CPU time limit and was reset";

/// How the config source answers for [`DECLARING`].
#[derive(Clone, Copy)]
enum Answer {
    Ok,
    Missing,
    Transient,
    SourceFault,
}

/// A config source that answers [`DECLARING`] with the current [`Answer`] and
/// every other block with an empty config. The answer can change between
/// the blocks' `Init` and the `/_health` request.
#[derive(Clone)]
struct ScriptedSource(Arc<Mutex<Answer>>);

impl ScriptedSource {
    fn answer(&self, answer: Answer) {
        *self.0.lock().unwrap() = answer;
    }
}

#[derive(Debug)]
struct SourceErr;

impl std::fmt::Display for SourceErr {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(SOURCE_TEXT)
    }
}

impl std::error::Error for SourceErr {}

#[async_trait::async_trait]
impl ConfigSource for ScriptedSource {
    async fn load_for_block(
        &self,
        block: &str,
        _declared: &[ConfigVar],
    ) -> Result<EnvBlockConfig, ConfigError> {
        if block != DECLARING {
            return Ok(EnvBlockConfig::default());
        }
        let block = block.to_string();
        let answer = *self.0.lock().unwrap();
        match answer {
            Answer::Ok => Ok(EnvBlockConfig::new(HashMap::from([(
                KEY.to_string(),
                "set".to_string(),
            )]))),
            Answer::Missing => Err(ConfigError::MissingRequired {
                block,
                key: KEY.to_string(),
            }),
            // As impresspress's D1 source answers a read that met D1's
            // outage: `DatabaseError::detail_code()`.
            Answer::Transient => Err(ConfigError::Transient {
                block,
                source: Box::new(SourceErr),
                detail_code: Some(wafer_block::wire::database::DATABASE_UNAVAILABLE.to_string()),
            }),
            Answer::SourceFault => Err(ConfigError::SourceFault {
                block,
                source: Box::new(SourceErr),
            }),
        }
    }
}

/// A block declaring one required config key; never called.
struct DeclaringBlock;

#[async_trait::async_trait]
impl Block for DeclaringBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(DECLARING, "0.1.0", "test/iface@v1", "test")
            .config_keys(vec![ConfigVar::new(KEY, "required", "")])
    }

    async fn lifecycle(&self, _ctx: &dyn Context, _e: LifecycleEvent) -> Result<(), WaferError> {
        Ok(())
    }

    async fn handle(&self, _ctx: &dyn Context, _m: Message, _input: InputStream) -> OutputStream {
        panic!("/_health must not call the blocks it validates");
    }
}

/// A block with no config whose `Init` fails transiently, as one whose own
/// `Init` makes D1 reset does while D1 otherwise answers.
struct ResettingBlock;

#[async_trait::async_trait]
impl Block for ResettingBlock {
    fn info(&self) -> BlockInfo {
        BlockInfo::new(RESETTING, "0.1.0", "test/iface@v1", "test")
    }

    async fn lifecycle(&self, _ctx: &dyn Context, e: LifecycleEvent) -> Result<(), WaferError> {
        match e.event_type {
            LifecycleType::Init => Err(WaferError::new(ErrorCode::Unavailable, RESET_TEXT)),
            _ => Ok(()),
        }
    }

    async fn handle(&self, _ctx: &dyn Context, _m: Message, _input: InputStream) -> OutputStream {
        panic!("/_health must not call the blocks it reports");
    }
}

thread_local! {
    /// What [`platform`] reports: the test's stand-in for the Cloudflare
    /// runtime cache. Tokio tests run on one thread, as an isolate does.
    static PLATFORM: RefCell<PlatformHealth> = RefCell::new(PlatformHealth::default());
}

fn platform() -> PlatformHealth {
    PLATFORM.with(|p| p.borrow().clone())
}

/// Registers [`platform`] reporting `health` for one test, and puts the
/// previous source back on drop.
struct PlatformGuard(Option<PlatformHealthSource>);

impl PlatformGuard {
    fn report(health: PlatformHealth) -> Self {
        PLATFORM.with(|p| *p.borrow_mut() = health);
        Self(set_platform_health_source(Some(platform)))
    }
}

impl Drop for PlatformGuard {
    fn drop(&mut self) {
        set_platform_health_source(self.0.take());
        PLATFORM.with(|p| *p.borrow_mut() = PlatformHealth::default());
    }
}

/// A runtime episode armed now, ending in `retry_in_secs`.
fn episode(retry_in_secs: u64, failed: bool) -> RetryEpisode {
    let now = impresspress_core::util::now_millis();
    RetryEpisode {
        since_ms: now - 120_000,
        retry_at_ms: now + retry_in_secs * 1_000,
        retries: 3,
        failed,
    }
}

/// One `/_health` answer and the gate's verdict on it.
struct Health {
    status: String,
    retry_after: Option<String>,
    body: serde_json::Value,
    verdict: String,
}

/// What `scripts/deploy-cloudflare.sh`'s post-promotion gate decides for
/// this answer.
fn gate_verdict(status: &str, retry_after: Option<&str>, raw_body: &str) -> String {
    let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/lib/deploy-checks.sh");
    let headers = tempfile::NamedTempFile::new().expect("headers file");
    let mut raw_headers = format!("HTTP/2 {status}\r\ncontent-type: application/json\r\n");
    if let Some(seconds) = retry_after {
        raw_headers.push_str(&format!("retry-after: {seconds}\r\n"));
    }
    std::fs::write(headers.path(), raw_headers).expect("write the headers");
    let body = tempfile::NamedTempFile::new().expect("body file");
    std::fs::write(body.path(), raw_body).expect("write the body");
    let out = std::process::Command::new("bash")
        .args([
            "-c",
            r#"source "$1" && health_gate_verdict "$2" "$3" "$4""#,
            "bash",
            lib,
            status,
        ])
        .arg(headers.path())
        .arg(body.path())
        .output()
        .expect("run bash");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout)
        .expect("UTF-8")
        .trim()
        .to_string()
}

/// Build a runtime whose source answers `at_init` while every block
/// initializes and `at_check` when `/_health` is asked, with
/// [`ResettingBlock`] registered when `resetting`, and dispatch
/// `wafer-site/health` on it.
async fn health_with(at_init: Answer, at_check: Answer, resetting: bool) -> Health {
    let source = ScriptedSource(Arc::new(Mutex::new(at_init)));
    // Only the blocks under test: the blocks the test binary links
    // (impresspress's, wafer-run's web block) would need a storage service.
    let mut wafer = Wafer::builder()
        .config_source(Arc::new(source.clone()))
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("build the runtime");
    wafer
        .register_block(DECLARING, Arc::new(DeclaringBlock))
        .expect("register the declaring block");
    if resetting {
        wafer
            .register_block(RESETTING, Arc::new(ResettingBlock))
            .expect("register the resetting block");
    }
    health::register(&mut wafer).expect("register the health block");
    wafer.seal().await.expect("seal");
    // As a Cloudflare build does before it publishes the runtime.
    wafer.init_all_blocks().await;
    source.answer(at_check);
    let wafer = Arc::new(wafer);

    let response = wafer
        .run_block(health::NAME, Message::new("retrieve"), InputStream::empty())
        .await
        .collect_buffered()
        .await
        .expect("the health block responds");
    let meta = |key: &str| {
        response
            .meta
            .iter()
            .find(|m| m.key == key)
            .map(|m| m.value.clone())
    };
    let header = |name: &str| meta(&format!("{META_RESP_HEADER_PREFIX}{name}"));
    let status = meta(META_RESP_STATUS).expect("the health block sets a status");
    assert_eq!(header("Cache-Control").as_deref(), Some("no-store"));
    let retry_after = header("Retry-After");
    let raw = String::from_utf8(response.body).expect("UTF-8 body");
    for text in [SOURCE_TEXT, RESET_TEXT] {
        assert!(
            !raw.contains(text),
            "failure text must stay out of the body: {raw}"
        );
    }
    let verdict = gate_verdict(&status, retry_after.as_deref(), &raw);
    let body: serde_json::Value = serde_json::from_str(&raw).expect("JSON body");
    // A 503 carries the runtime's Retry-After when it has one.
    assert_eq!(
        retry_after,
        body["runtime"]["retry_in"]
            .as_u64()
            .filter(|_| status == "503")
            .map(|s| s.to_string()),
        "{raw}"
    );
    Health {
        status,
        retry_after,
        body,
        verdict,
    }
}

#[tokio::test]
async fn a_source_that_answers_is_healthy() {
    let h = health_with(Answer::Ok, Answer::Ok, false).await;
    assert_eq!(h.status, "200", "{}", h.body);
    assert_eq!(h.verdict, "healthy");
    assert_eq!(h.retry_after, None);
    assert_eq!(
        h.body,
        serde_json::json!({
            "ok": [DECLARING, health::NAME],
            "broken": [],
            "degraded": [],
            "failed": [],
            "runtime": { "status": "ok" },
        })
    );
}

#[tokio::test]
async fn a_missing_required_key_is_broken_and_rolls_back() {
    let h = health_with(Answer::Missing, Answer::Missing, false).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "rollback");
    assert_eq!(
        h.body["broken"],
        serde_json::json!([{ "block": DECLARING, "missing_keys": [KEY] }])
    );
    // Its Init failed for good, so the runtime reports it too.
    assert_eq!(h.body["runtime"]["status"], "failed");
    assert_eq!(
        h.body["runtime"]["unavailable"],
        serde_json::json!([{ "block": DECLARING, "class": "failed" }])
    );
}

#[tokio::test]
async fn a_source_fault_is_failed_and_rolls_back() {
    let h = health_with(Answer::SourceFault, Answer::SourceFault, false).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "rollback");
    assert_eq!(h.body["failed"], serde_json::json!([DECLARING]));
    assert_eq!(h.body["degraded"], serde_json::json!([]));
    assert_eq!(h.body["runtime"]["status"], "failed");
}

/// The D1 outage: config unreachable at Init and still now. The block is
/// retrying in the runtime and degraded in the config lists: an outage, so
/// the gate keeps the version.
#[tokio::test]
async fn an_unreachable_source_is_an_outage() {
    let h = health_with(Answer::Transient, Answer::Transient, false).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "outage");
    assert_eq!(h.body["degraded"], serde_json::json!([DECLARING]));
    assert_eq!(h.body["broken"], serde_json::json!([]));
    assert_eq!(h.body["failed"], serde_json::json!([]));
    assert_eq!(h.body["runtime"]["status"], "degraded");
    let unavailable = &h.body["runtime"]["unavailable"];
    assert_eq!(unavailable[0]["block"], DECLARING, "{}", h.body);
    assert_eq!(unavailable[0]["class"], "retrying", "{}", h.body);
    assert!(unavailable[0]["retry_in"].as_u64().unwrap() >= 1);
    assert!(
        h.retry_after.is_some(),
        "a degraded runtime sets Retry-After"
    );
}

/// The runtime was built while D1 answered; D1 failed since. Every block
/// serves, the config lists show the source failing: an outage.
#[tokio::test]
async fn a_source_that_fails_after_the_build_is_an_outage() {
    let h = health_with(Answer::Ok, Answer::Transient, false).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "outage");
    assert_eq!(h.body["degraded"], serde_json::json!([DECLARING]));
    assert_eq!(h.body["runtime"], serde_json::json!({ "status": "ok" }));
}

/// A block whose own `Init` keeps failing transiently while every block's
/// config answers: the new version's fault (plan I2g), so the gate rolls
/// back rather than calling it an outage.
#[tokio::test]
async fn a_block_retrying_while_config_answers_rolls_back() {
    let h = health_with(Answer::Ok, Answer::Ok, true).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "rollback");
    assert_eq!(h.body["degraded"], serde_json::json!([]));
    assert_eq!(h.body["runtime"]["status"], "degraded");
    assert_eq!(h.body["runtime"]["unavailable"][0]["block"], RESETTING);
    assert_eq!(h.body["runtime"]["unavailable"][0]["class"], "retrying");
}

/// The same block while the config source is failing too: an outage, which
/// may be what makes it retry.
#[tokio::test]
async fn a_block_retrying_while_config_fails_is_an_outage() {
    let h = health_with(Answer::Transient, Answer::Transient, true).await;
    assert_eq!(h.verdict, "outage", "{}", h.body);
}

/// Published degraded with every block serving — its variables read failed
/// after it was published (PR275 review n8) — and no block listed: an outage
/// whatever the config lists hold.
#[tokio::test]
async fn a_runtime_degraded_with_no_block_listed_is_an_outage() {
    let _platform = PlatformGuard::report(PlatformHealth {
        episode: Some(episode(7, false)),
        degraded_since_ms: Some(impresspress_core::util::now_millis()),
        ..PlatformHealth::default()
    });
    let h = health_with(Answer::Ok, Answer::Ok, false).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "outage");
    assert_eq!(h.body["degraded"], serde_json::json!([]));
    assert_eq!(h.body["runtime"]["status"], "degraded");
    assert_eq!(h.body["runtime"]["unavailable"], serde_json::json!([]));
    assert_eq!(h.body["runtime"]["retries"], 3);
    let retry_in = h.body["runtime"]["retry_in"].as_u64().unwrap();
    assert!(
        (1..=7).contains(&retry_in),
        "the window's remainder: {retry_in}"
    );
}

/// Stale: a rebuild failed transiently and the older runtime serves on.
/// That is an outage (or, rarely, a block resetting D1 itself so the
/// rebuild's reads fail too), so the gate keeps the version.
#[tokio::test]
async fn a_stale_runtime_is_an_outage() {
    let _platform = PlatformGuard::report(PlatformHealth {
        episode: Some(episode(30, false)),
        stale_since_ms: Some(impresspress_core::util::now_millis() - 60_000),
        ..PlatformHealth::default()
    });
    let h = health_with(Answer::Ok, Answer::Ok, false).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "outage");
    assert_eq!(h.body["runtime"]["status"], "stale");
    assert!(h.body["runtime"]["since"].is_string(), "{}", h.body);
    assert!(h.retry_after.is_some());
}

/// The last rebuild failed for good: the runtime is `failed`, and the gate
/// rolls back.
#[tokio::test]
async fn a_failed_rebuild_rolls_back() {
    let _platform = PlatformGuard::report(PlatformHealth {
        episode: Some(episode(30, true)),
        ..PlatformHealth::default()
    });
    let h = health_with(Answer::Ok, Answer::Ok, false).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "rollback");
    assert_eq!(h.body["runtime"]["status"], "failed");
    assert_eq!(h.body["broken"], serde_json::json!([]));
    assert_eq!(h.body["failed"], serde_json::json!([]));
}

/// What the isolate's last settled attempt found of D1, settled now.
fn backend(answered: bool) -> Option<BackendCheck> {
    let now = impresspress_core::util::now_millis();
    Some(BackendCheck::after(None, answered, now - 1_000))
}

/// impresspress #287's case: D1 failed during a block's `Init` after the
/// variables read answered, so every config read answers from the snapshot
/// and the block is listed — but the last attempt found D1 failing. An
/// outage: the gate keeps the version.
#[tokio::test]
async fn a_block_retrying_while_the_backend_fails_is_an_outage() {
    let _platform = PlatformGuard::report(PlatformHealth {
        episode: Some(episode(4, false)),
        backend: backend(false),
        ..PlatformHealth::default()
    });
    let h = health_with(Answer::Ok, Answer::Ok, true).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "outage");
    assert_eq!(h.body["degraded"], serde_json::json!([]));
    assert_eq!(h.body["runtime"]["status"], "degraded");
    assert_eq!(h.body["runtime"]["backend"]["status"], "failing");
    assert!(
        h.body["runtime"]["backend"]["since"].is_string(),
        "{}",
        h.body
    );
    assert!(
        h.body["runtime"]["backend"]["checked"].is_string(),
        "{}",
        h.body
    );
    assert_eq!(h.body["runtime"]["unavailable"][0]["block"], RESETTING);
}

/// The same block with D1 answering the last attempt: the block fails on
/// its own, the version's fault.
#[tokio::test]
async fn a_block_retrying_while_the_backend_answers_rolls_back() {
    let _platform = PlatformGuard::report(PlatformHealth {
        episode: Some(episode(4, false)),
        backend: backend(true),
        ..PlatformHealth::default()
    });
    let h = health_with(Answer::Ok, Answer::Ok, true).await;
    assert_eq!(h.status, "503", "{}", h.body);
    assert_eq!(h.verdict, "rollback");
    assert_eq!(h.body["runtime"]["backend"]["status"], "ok");
    assert_eq!(h.body["runtime"]["unavailable"][0]["block"], RESETTING);
}

/// A failing backend outranks nothing: a `failed` runtime still rolls back.
#[tokio::test]
async fn a_failed_runtime_rolls_back_whatever_the_backend() {
    let _platform = PlatformGuard::report(PlatformHealth {
        episode: Some(episode(30, true)),
        backend: backend(false),
        ..PlatformHealth::default()
    });
    let h = health_with(Answer::Ok, Answer::Ok, false).await;
    assert_eq!(h.body["runtime"]["status"], "failed", "{}", h.body);
    assert_eq!(h.body["runtime"]["backend"]["status"], "failing");
    assert_eq!(h.verdict, "rollback");
}

/// A healthy runtime's body stays exactly `{"status":"ok"}` under
/// `runtime`, whatever the platform last saw.
#[tokio::test]
async fn an_ok_runtime_carries_no_backend() {
    let _platform = PlatformGuard::report(PlatformHealth {
        backend: backend(true),
        ..PlatformHealth::default()
    });
    let h = health_with(Answer::Ok, Answer::Ok, false).await;
    assert_eq!(h.status, "200", "{}", h.body);
    assert_eq!(h.body["runtime"], serde_json::json!({"status": "ok"}));
}
