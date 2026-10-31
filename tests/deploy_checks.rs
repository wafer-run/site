//! The checks `scripts/deploy-cloudflare.sh` runs around a deploy
//! (scripts/lib/deploy-checks.sh), on fixtures, without deploying:
//!
//! - `check_sibling_pins` refuses unless `../wafer-run` and `../impresspress`
//!   are clean checkouts at the pins in `.github/workflows/ci.yml`. The
//!   fixture's ci.yml is this repo's own with only the two pin values
//!   replaced, so the check parses the real file's layout.
//! - `health_gate_verdict` rolls back on any post-promotion `/_health`
//!   answer the health block did not give as a transient outage. The
//!   health block's own answers are checked in tests/health_unavailable.rs.

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

fn verdict(status: &str, body: &str) -> String {
    let file = tempfile::NamedTempFile::new().expect("body file");
    std::fs::write(file.path(), body).expect("write");
    let out = Command::new("bash")
        .args([
            "-c",
            r#"source "$1" && health_gate_verdict "$2" "$3""#,
            "bash",
            LIB,
            status,
        ])
        .arg(file.path())
        .output()
        .expect("run bash");
    assert!(out.status.success(), "{out:?}");
    String::from_utf8(out.stdout)
        .expect("UTF-8")
        .trim()
        .to_string()
}

#[test]
fn only_a_degraded_only_503_keeps_the_new_version() {
    let body = |broken: &str, degraded: &str, failed: &str| {
        format!(r#"{{"ok":[],"broken":{broken},"degraded":{degraded},"failed":{failed}}}"#)
    };
    assert_eq!(verdict("503", &body("[]", r#"["a"]"#, "[]")), "outage");
    // A transient failure next to a deploy-caused one still rolls back.
    assert_eq!(
        verdict("503", &body("[]", r#"["a"]"#, r#"["b"]"#)),
        "rollback"
    );
    assert_eq!(
        verdict(
            "503",
            &body(r#"[{"block":"b","missing_keys":["K"]}]"#, r#"["a"]"#, "[]")
        ),
        "rollback"
    );
    // A 503 naming nothing is not the health block's outage answer.
    assert_eq!(verdict("503", &body("[]", "[]", "[]")), "rollback");
}

#[test]
fn answers_that_are_not_the_health_blocks_roll_back() {
    let degraded = r#"{"ok":[],"broken":[],"degraded":["a"],"failed":[]}"#;
    // The outage body under another status.
    assert_eq!(verdict("500", degraded), "rollback");
    // curl's "000" for no connection, with an empty body.
    assert_eq!(verdict("000", ""), "rollback");
    assert_eq!(verdict("503", "Service Unavailable"), "rollback");
    assert_eq!(verdict("503", r#"{"degraded":["a"]}"#), "rollback");
    assert_eq!(verdict("200", ""), "healthy");
}
