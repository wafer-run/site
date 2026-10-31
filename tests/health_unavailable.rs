//! `/_health` answers 503 whenever the config source cannot answer for a
//! block, for both classes of failure wafer-run reports in
//! `ValidationReport::unavailable`: `Transient` (D1 unreachable or over a
//! quota that resets) is `degraded`, `SourceFault` (an error retrying cannot
//! fix) is `failed`. A missing required key stays `broken`.
//!
//! The site's own health block is dispatched through a real runtime whose
//! config source answers per block, so the report it serves is the one
//! wafer-run's validator builds from those answers. Each answer is also put
//! through the deploy script's post-promotion gate
//! (`health_gate_verdict`, scripts/lib/deploy-checks.sh): only the
//! transient case keeps the new version live without rolling back.

use std::{collections::HashMap, sync::Arc};

use wafer_block::{
    core_types::{LifecycleEvent, Message, WaferError},
    streams::{input::InputStream, output::OutputStream},
    Block, BlockInfo, ConfigVar,
};
use wafer_run::{ConfigError, ConfigSource, Context, EnvBlockConfig, Wafer, META_RESP_STATUS};
use wafer_site::blocks::health;

/// The block whose config the source answers for.
const DECLARING: &str = "test/declaring";
/// The required key [`DECLARING`] declares.
const KEY: &str = "TEST__DECLARING__REQUIRED";
/// Source error text that must stay out of the public body.
const SOURCE_TEXT: &str = "D1_ERROR: daily row read limit exceeded";

/// How the config source answers for [`DECLARING`].
#[derive(Clone, Copy)]
enum Answer {
    Ok,
    Missing,
    Transient,
    SourceFault,
}

/// A config source that answers [`DECLARING`] with a fixed [`Answer`] and
/// every other block with an empty config.
struct ScriptedSource(Answer);

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
        match self.0 {
            Answer::Ok => Ok(EnvBlockConfig::new(HashMap::from([(
                KEY.to_string(),
                "set".to_string(),
            )]))),
            Answer::Missing => Err(ConfigError::MissingRequired {
                block,
                key: KEY.to_string(),
            }),
            Answer::Transient => Err(ConfigError::Transient {
                block,
                source: Box::new(SourceErr),
            }),
            Answer::SourceFault => Err(ConfigError::SourceFault {
                block,
                source: Box::new(SourceErr),
            }),
        }
    }
}

/// A block declaring one required config key; never initialised or called.
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

/// What `scripts/deploy-cloudflare.sh`'s post-promotion gate decides for
/// this `/_health` answer.
fn gate_verdict(status: &str, raw_body: &str) -> String {
    let lib = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/lib/deploy-checks.sh");
    let body = tempfile::NamedTempFile::new().expect("body file");
    std::fs::write(body.path(), raw_body).expect("write the body");
    let out = std::process::Command::new("bash")
        .args([
            "-c",
            r#"source "$1" && health_gate_verdict "$2" "$3""#,
            "bash",
            lib,
            status,
        ])
        .arg(body.path())
        .output()
        .expect("run bash");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout)
        .expect("UTF-8")
        .trim()
        .to_string()
}

/// Dispatch `wafer-site/health` on a runtime whose source answers `answer`,
/// returning the response status, JSON body and the deploy gate's verdict.
async fn health_with(answer: Answer) -> (String, serde_json::Value, String) {
    // Only the two blocks under test: the blocks the test binary links
    // (impresspress's, wafer-run's web block) would need a storage service.
    let mut wafer = Wafer::builder()
        .config_source(Arc::new(ScriptedSource(answer)))
        .disable_inventory()
        .disable_lockfile()
        .build()
        .expect("build the runtime");
    wafer
        .register_block(DECLARING, Arc::new(DeclaringBlock))
        .expect("register the declaring block");
    health::register(&mut wafer).expect("register the health block");
    wafer.seal().await.expect("seal");
    let wafer = Arc::new(wafer);

    let response = wafer
        .run_block(health::NAME, Message::new("retrieve"), InputStream::empty())
        .await
        .collect_buffered()
        .await
        .expect("the health block responds");
    let status = response
        .meta
        .iter()
        .find(|m| m.key == META_RESP_STATUS)
        .map(|m| m.value.clone())
        .expect("the health block sets a status");
    let raw = String::from_utf8(response.body).expect("UTF-8 body");
    assert!(
        !raw.contains(SOURCE_TEXT),
        "the source's error text must stay out of the body: {raw}"
    );
    let verdict = gate_verdict(&status, &raw);
    let body = serde_json::from_str(&raw).expect("JSON body");
    (status, body, verdict)
}

#[tokio::test]
async fn a_source_that_answers_is_healthy() {
    let (status, body, verdict) = health_with(Answer::Ok).await;
    assert_eq!(verdict, "healthy", "{body}");
    assert_eq!(status, "200", "{body}");
    assert_eq!(
        body,
        serde_json::json!({
            "ok": [DECLARING, health::NAME],
            "broken": [],
            "degraded": [],
            "failed": [],
        })
    );
}

#[tokio::test]
async fn a_missing_required_key_is_broken() {
    let (status, body, verdict) = health_with(Answer::Missing).await;
    assert_eq!(verdict, "rollback", "{body}");
    assert_eq!(status, "503", "{body}");
    assert_eq!(
        body,
        serde_json::json!({
            "ok": [health::NAME],
            "broken": [{ "block": DECLARING, "missing_keys": [KEY] }],
            "degraded": [],
            "failed": [],
        })
    );
}

#[tokio::test]
async fn an_unreachable_source_is_degraded_and_unhealthy() {
    let (status, body, verdict) = health_with(Answer::Transient).await;
    assert_eq!(verdict, "outage", "{body}");
    assert_eq!(status, "503", "{body}");
    assert_eq!(
        body,
        serde_json::json!({
            "ok": [health::NAME],
            "broken": [],
            "degraded": [DECLARING],
            "failed": [],
        })
    );
}

#[tokio::test]
async fn a_source_fault_is_failed_and_unhealthy() {
    let (status, body, verdict) = health_with(Answer::SourceFault).await;
    assert_eq!(verdict, "rollback", "{body}");
    assert_eq!(status, "503", "{body}");
    assert_eq!(
        body,
        serde_json::json!({
            "ok": [health::NAME],
            "broken": [],
            "degraded": [],
            "failed": [DECLARING],
        })
    );
}
