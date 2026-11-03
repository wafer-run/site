//! The checks `scripts/deploy-cloudflare.sh` runs around a deploy
//! (scripts/lib/deploy-checks.sh), on fixtures, without deploying:
//!
//! - `check_sibling_pins` refuses unless `../wafer-run` and `../impresspress`
//!   are clean checkouts at the pins in `.github/workflows/ci.yml`. The
//!   fixture's ci.yml is this repo's own with only the two pin values
//!   replaced, so the check parses the real file's layout.
//! - `health_gate_verdict` keeps the new version only for the answers an
//!   outage gives, and rolls back on every other non-200, including bodies
//!   the health block never sends; `health_gate_retry_delay` asks once more
//!   after a 503's `Retry-After`. The health block's own answers are checked
//!   against the gate in tests/health_unavailable.rs.

use std::{
    path::{Path, PathBuf},
    process::{Command, Output},
};

const LIB: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/scripts/lib/deploy-checks.sh");
const CI_YML: &str = include_str!("../.github/workflows/ci.yml");

fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args([
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "-c",
            "commit.gpgsign=false",
        ])
        .args(args)
        .output()
        .expect("run git");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8(out.stdout)
        .expect("UTF-8")
        .trim()
        .to_string()
}

/// A git repository at `dir` with two commits; returns their SHAs, oldest
/// first. HEAD is at the second.
fn repo_with_two_commits(dir: &Path) -> (String, String) {
    std::fs::create_dir_all(dir).expect("create the repo dir");
    git(dir, &["init", "-q"]);
    std::fs::write(dir.join("file"), "one").expect("write");
    git(dir, &["add", "file"]);
    git(dir, &["commit", "-q", "-m", "one"]);
    let first = git(dir, &["rev-parse", "HEAD"]);
    std::fs::write(dir.join("file"), "two").expect("write");
    git(dir, &["commit", "-q", "-am", "two"]);
    (first, git(dir, &["rev-parse", "HEAD"]))
}

/// The value of `  NAME: <sha>` in this repo's ci.yml.
fn real_pin(name: &str) -> &'static str {
    let prefix = format!("  {name}: ");
    CI_YML
        .lines()
        .find_map(|l| l.strip_prefix(prefix.as_str()))
        .unwrap_or_else(|| panic!("ci.yml has no {name}"))
        .trim()
}

/// A workspace fixture: `site/` with this repo's ci.yml pinning the HEADs
/// of the sibling repos `wafer-run/` and `impresspress/`.
struct Workspace {
    _root: tempfile::TempDir,
    site: PathBuf,
    wafer_run: PathBuf,
    impresspress: PathBuf,
    /// The commit before each sibling's HEAD (not the pin).
    wafer_run_old: String,
}

impl Workspace {
    fn new() -> Self {
        let root = tempfile::tempdir().expect("tempdir");
        let site = root.path().join("site");
        let wafer_run = root.path().join("wafer-run");
        let impresspress = root.path().join("impresspress");
        let (wafer_run_old, wafer_run_head) = repo_with_two_commits(&wafer_run);
        let (_, impresspress_head) = repo_with_two_commits(&impresspress);

        let ci = CI_YML
            .replace(real_pin("WAFER_RUN_REF"), &wafer_run_head)
            .replace(real_pin("IMPRESSPRESS_REF"), &impresspress_head);
        let workflows = site.join(".github/workflows");
        std::fs::create_dir_all(&workflows).expect("create workflows");
        std::fs::write(workflows.join("ci.yml"), ci).expect("write ci.yml");

        Self {
            _root: root,
            site,
            wafer_run,
            impresspress,
            wafer_run_old,
        }
    }

    fn check(&self) -> Output {
        Command::new("bash")
            .args([
                "-c",
                r#"source "$1" && check_sibling_pins "$2""#,
                "bash",
                LIB,
            ])
            .arg(&self.site)
            .output()
            .expect("run bash")
    }
}

fn stderr(out: &Output) -> String {
    String::from_utf8_lossy(&out.stderr).into_owned()
}

#[test]
fn siblings_at_the_pins_pass() {
    let ws = Workspace::new();
    let out = ws.check();
    assert!(out.status.success(), "{}", stderr(&out));
}

#[test]
fn a_sibling_at_another_commit_is_refused() {
    let ws = Workspace::new();
    git(
        &ws.wafer_run,
        &["checkout", "-q", "--detach", &ws.wafer_run_old],
    );
    let out = ws.check();
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains(&format!(
            "is at {}, but ci.yml pins WAFER_RUN_REF",
            ws.wafer_run_old
        )),
        "{err}"
    );
    assert!(!err.contains("IMPRESSPRESS_REF"), "{err}");
}

#[test]
fn a_sibling_with_modified_files_is_refused() {
    let ws = Workspace::new();
    std::fs::write(ws.impresspress.join("file"), "edited").expect("write");
    let out = ws.check();
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("has uncommitted changes; CI builds IMPRESSPRESS_REF as committed"),
        "{err}"
    );
    assert!(err.contains(" M file"), "{err}");
}

#[test]
fn a_sibling_with_untracked_files_is_refused() {
    let ws = Workspace::new();
    std::fs::write(ws.wafer_run.join("new.rs"), "").expect("write");
    let out = ws.check();
    assert!(!out.status.success());
    assert!(stderr(&out).contains("?? new.rs"), "{}", stderr(&out));
}

#[test]
fn a_missing_sibling_is_refused() {
    let ws = Workspace::new();
    std::fs::remove_dir_all(&ws.impresspress).expect("remove");
    let out = ws.check();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("is not a git checkout (needed at IMPRESSPRESS_REF"),
        "{}",
        stderr(&out)
    );
}

#[test]
fn a_ci_yml_without_a_pin_is_refused() {
    let ws = Workspace::new();
    let ci = ws.site.join(".github/workflows/ci.yml");
    let text = std::fs::read_to_string(&ci).expect("read");
    let without: String = text
        .lines()
        .filter(|l| !l.starts_with("  WAFER_RUN_REF:"))
        .map(|l| format!("{l}\n"))
        .collect();
    std::fs::write(&ci, without).expect("write");
    let out = ws.check();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no 40-hex WAFER_RUN_REF pin"),
        "{}",
        stderr(&out)
    );
}

/// Run `func` from the lib with `status`, a headers file holding
/// `retry_after` (when given) and, for the verdict, a body file.
fn run_gate(func: &str, status: &str, retry_after: Option<&str>, body: Option<&str>) -> String {
    let headers = tempfile::NamedTempFile::new().expect("headers file");
    let mut raw = format!("HTTP/2 {status}\r\ncontent-type: text/plain\r\n");
    if let Some(value) = retry_after {
        raw.push_str(&format!("Retry-After: {value}\r\n"));
    }
    std::fs::write(headers.path(), raw).expect("write");
    let body_file = tempfile::NamedTempFile::new().expect("body file");
    std::fs::write(body_file.path(), body.unwrap_or_default()).expect("write");
    let out = Command::new("bash")
        .args([
            "-c",
            r#"source "$1" && "$2" "$3" "$4" "$5""#,
            "bash",
            LIB,
            func,
            status,
        ])
        .arg(headers.path())
        .arg(body_file.path())
        .output()
        .expect("run bash");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout)
        .expect("UTF-8")
        .trim()
        .to_string()
}

fn verdict(status: &str, retry_after: Option<&str>, body: &str) -> String {
    run_gate("health_gate_verdict", status, retry_after, Some(body))
}

fn retry_delay(status: &str, retry_after: Option<&str>) -> String {
    run_gate("health_gate_retry_delay", status, retry_after, None)
}

/// A `/_health` body: the three config lists and the runtime report.
fn body(broken: &str, degraded: &str, failed: &str, runtime: &str) -> String {
    format!(
        r#"{{"ok":[],"broken":{broken},"degraded":{degraded},"failed":{failed},"runtime":{runtime}}}"#
    )
}

const RT_OK: &str = r#"{"status":"ok"}"#;
const RT_DEGRADED_EMPTY: &str = r#"{"status":"degraded","retry_in":4,"unavailable":[]}"#;
const RT_DEGRADED_LISTED: &str = r#"{"status":"degraded","retry_in":4,"unavailable":[{"block":"r","class":"retrying","retry_in":4}]}"#;
const RT_STALE: &str = r#"{"status":"stale","retry_in":9,"unavailable":[{"block":"r","class":"retrying","retry_in":9}]}"#;
const RT_FAILED: &str = r#"{"status":"failed","retry_in":1,"unavailable":[]}"#;
const BROKEN: &str = r#"[{"block":"b","missing_keys":["K"]}]"#;

#[test]
fn an_outage_keeps_the_new_version() {
    // The config source failing transiently, with the runtime serving.
    assert_eq!(
        verdict("503", None, &body("[]", r#"["a"]"#, "[]", RT_OK)),
        "outage"
    );
    // A degraded runtime with its blocks listed, while config fails too.
    assert_eq!(
        verdict(
            "503",
            Some("4"),
            &body("[]", r#"["a"]"#, "[]", RT_DEGRADED_LISTED)
        ),
        "outage"
    );
    // A degraded runtime listing no block (its variables read failed after
    // publish): an outage whatever the config lists hold.
    for degraded in ["[]", r#"["a"]"#] {
        assert_eq!(
            verdict(
                "503",
                Some("4"),
                &body("[]", degraded, "[]", RT_DEGRADED_EMPTY)
            ),
            "outage"
        );
    }
    // Stale, with or without config entries and with blocks listed.
    for degraded in ["[]", r#"["a"]"#] {
        assert_eq!(
            verdict("503", Some("9"), &body("[]", degraded, "[]", RT_STALE)),
            "outage"
        );
    }
    // The Worker's own plain-text 503 (no runtime inside a transient
    // window), and an empty one, carrying Retry-After.
    let edge = "Service temporarily unavailable: a backend this site depends on is \
                not responding. Retry in 12 s. (reference: 0)";
    assert_eq!(verdict("503", Some("12"), edge), "outage");
    assert_eq!(verdict("503", Some("12"), ""), "outage");
}

#[test]
fn a_fault_of_the_new_version_rolls_back() {
    // Config entries the deploy causes, next to anything else.
    for runtime in [RT_OK, RT_DEGRADED_EMPTY, RT_DEGRADED_LISTED, RT_STALE] {
        assert_eq!(
            verdict("503", Some("4"), &body(BROKEN, r#"["a"]"#, "[]", runtime)),
            "rollback",
            "{runtime}"
        );
        assert_eq!(
            verdict(
                "503",
                Some("4"),
                &body("[]", r#"["a"]"#, r#"["b"]"#, runtime)
            ),
            "rollback",
            "{runtime}"
        );
    }
    // A failed runtime, even during an outage.
    for degraded in ["[]", r#"["a"]"#] {
        assert_eq!(
            verdict("503", Some("1"), &body("[]", degraded, "[]", RT_FAILED)),
            "rollback"
        );
    }
    // A block retrying while every block's config answers.
    assert_eq!(
        verdict(
            "503",
            Some("4"),
            &body("[]", "[]", "[]", RT_DEGRADED_LISTED)
        ),
        "rollback"
    );
    // A 503 naming nothing.
    assert_eq!(
        verdict("503", None, &body("[]", "[]", "[]", RT_OK)),
        "rollback"
    );
}

#[test]
fn answers_that_are_not_the_health_blocks_roll_back() {
    let outage = body("[]", r#"["a"]"#, "[]", RT_OK);
    // The outage body under another status.
    assert_eq!(verdict("500", Some("4"), &outage), "rollback");
    // curl's "000" for no connection, with an empty body.
    assert_eq!(verdict("000", None, ""), "rollback");
    // A non-JSON 503 without Retry-After, or with one in another form.
    assert_eq!(verdict("503", None, "Service Unavailable"), "rollback");
    assert_eq!(
        verdict(
            "503",
            Some("Wed, 21 Oct 2026 07:28:00 GMT"),
            "Service Unavailable"
        ),
        "rollback"
    );
    // JSON that is not the health body: the pre-S1 body (no `runtime`), a
    // runtime with no status, a list that is not one.
    assert_eq!(
        verdict(
            "503",
            Some("4"),
            r#"{"ok":[],"broken":[],"degraded":["a"],"failed":[]}"#
        ),
        "rollback"
    );
    assert_eq!(
        verdict("503", Some("4"), &body("[]", r#"["a"]"#, "[]", "{}")),
        "rollback"
    );
    assert_eq!(
        verdict("503", Some("4"), &body("[]", r#""a""#, "[]", RT_OK)),
        "rollback"
    );
    assert_eq!(
        verdict("503", Some("4"), r#"{"degraded":["a"]}"#),
        "rollback"
    );
    assert_eq!(verdict("200", None, ""), "healthy");
}

#[test]
fn only_a_503_with_retry_after_is_asked_again() {
    assert_eq!(retry_delay("503", Some("7")), "7");
    // Bounded: at least 1 s, at most 60 s.
    assert_eq!(retry_delay("503", Some("0")), "1");
    assert_eq!(retry_delay("503", Some("3600")), "60");
    assert_eq!(retry_delay("503", Some("007")), "7");
    // No Retry-After, one in HTTP-date form, or another status: judged as is.
    assert_eq!(retry_delay("503", None), "");
    assert_eq!(
        retry_delay("503", Some("Wed, 21 Oct 2026 07:28:00 GMT")),
        ""
    );
    assert_eq!(retry_delay("500", Some("7")), "");
    assert_eq!(retry_delay("200", Some("7")), "");
}
