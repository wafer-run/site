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
//! - `rollback_to_previous` resolves the version a rollback returns to,
//!   refuses one created before the rollback floor, deploys it, and checks it
//!   went live — run against a stub `npx` that logs every call and answers
//!   wrangler's JSON from fixtures.

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
        run_lib(r#"check_sibling_pins "$1""#, &[self.site.as_os_str()], None)
    }
}

/// Run `script` in bash with the lib sourced: `$0` is the lib, `$1`.. are
/// `args`, and stdin is `stdin` (empty when `None`).
fn run_lib<A: AsRef<std::ffi::OsStr>>(script: &str, args: &[A], stdin: Option<&str>) -> Output {
    run_lib_in(Command::new("bash"), script, args, stdin)
}

/// [`run_lib`] with a prepared `Command` (an environment, a `PATH`).
fn run_lib_in<A: AsRef<std::ffi::OsStr>>(
    mut cmd: Command,
    script: &str,
    args: &[A],
    stdin: Option<&str>,
) -> Output {
    use std::io::Write as _;
    let mut child = cmd
        .args(["-c", &format!(r#"source "$0" && {script}"#), LIB])
        .args(args)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .expect("run bash");
    let mut pipe = child.stdin.take().expect("stdin");
    pipe.write_all(stdin.unwrap_or_default().as_bytes())
        .expect("write stdin");
    drop(pipe);
    child.wait_with_output().expect("wait for bash")
}

fn stdout(out: &Output) -> String {
    String::from_utf8_lossy(&out.stdout).trim().to_string()
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
    let out = run_lib(
        &format!(r#"{func} "$1" "$2" "$3""#),
        &[
            std::ffi::OsStr::new(status),
            headers.path().as_os_str(),
            body_file.path().as_os_str(),
        ],
        None,
    );
    assert!(out.status.success(), "{out:?}");
    stdout(&out)
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

/// A fetch that cannot connect (the gate's retry, say) prints curl's `000`
/// and leaves no earlier answer in the files, so neither the verdict nor the
/// rollback message reads the first 503's headers or body as its own.
#[test]
fn a_fetch_that_cannot_connect_leaves_no_earlier_answer() {
    let headers = tempfile::NamedTempFile::new().expect("headers file");
    let body = tempfile::NamedTempFile::new().expect("body file");
    std::fs::write(headers.path(), "HTTP/2 503\r\nretry-after: 7\r\n").expect("write");
    std::fs::write(body.path(), r#"{"first":"answer"}"#).expect("write");
    // Nothing listens on port 1; curl fails at once.
    let out = run_lib(
        r#"health_fetch "$1" "$2" "$3""#,
        &[
            std::ffi::OsStr::new("http://127.0.0.1:1/_health"),
            headers.path().as_os_str(),
            body.path().as_os_str(),
        ],
        None,
    );
    assert!(out.status.success(), "{out:?}");
    assert_eq!(String::from_utf8_lossy(&out.stdout), "000");
    assert_eq!(std::fs::read_to_string(headers.path()).unwrap(), "");
    assert_eq!(std::fs::read_to_string(body.path()).unwrap(), "");
}

/// impresspress RELEASE.md's `/health` example at IMPRESSPRESS_REF ("`/health`
/// says whether D1 failed the last attempt"), verbatim: the runtime as the
/// health block nests it. Each `backend` rule below varies one field of it.
const RT_RELEASE_FAILING: &str = r#"{"status":"degraded","since":"2026-11-05T10:00:00.000Z","retry_in":4,"retries":3,
 "backend":{"status":"failing","since":"2026-11-05T10:00:00.300Z","checked":"2026-11-05T10:00:04.500Z"},
 "unavailable":[{"block":"wafer-run/auth","class":"retrying","retry_in":4,"since":"2026-11-05T10:00:00.000Z","runtimes":1}]}"#;

/// [`RT_RELEASE_FAILING`] with each `(from, to)` replaced, once each.
fn release_runtime(edits: &[(&str, &str)]) -> String {
    let mut rt = RT_RELEASE_FAILING.to_string();
    for (from, to) in edits {
        assert_eq!(rt.matches(from).count(), 1, "{from} in {rt}");
        rt = rt.replacen(from, to, 1);
    }
    rt
}

const BACKEND_OK: (&str, &str) = (
    r#""backend":{"status":"failing""#,
    r#""backend":{"status":"ok""#,
);
const NOTHING_LISTED: (&str, &str) = (
    r#""unavailable":[{"block":"wafer-run/auth","class":"retrying","retry_in":4,"since":"2026-11-05T10:00:00.000Z","runtimes":1}]"#,
    r#""unavailable":[]"#,
);

/// A 503 health body with `runtime` and empty config lists, judged after the
/// retry (it carries `Retry-After`, as every non-`ok` answer does).
fn backend_verdict(runtime: &str) -> String {
    verdict("503", Some("4"), &body("[]", "[]", "[]", runtime))
}

/// Rule 8, `failing`: D1 could not answer the last attempt — an outage,
/// whatever the runtime lists (the body #282's gate rolled back).
#[test]
fn a_failing_backend_is_an_outage() {
    assert_eq!(backend_verdict(RT_RELEASE_FAILING), "outage");
    assert_eq!(
        backend_verdict(&release_runtime(&[NOTHING_LISTED])),
        "outage"
    );
    assert_eq!(
        backend_verdict(&release_runtime(&[(
            r#""status":"degraded""#,
            r#""status":"stale""#
        )])),
        "outage"
    );
    // Config entries the source could not answer change nothing.
    assert_eq!(
        verdict(
            "503",
            Some("4"),
            &body("[]", r#"["wafer-run/auth"]"#, "[]", RT_RELEASE_FAILING)
        ),
        "outage"
    );
}

/// Rule 6 outranks `backend`: a `failed` runtime rolls back even while D1
/// is failing.
#[test]
fn a_failed_runtime_rolls_back_whatever_the_backend() {
    let failed = (r#""status":"degraded""#, r#""status":"failed""#);
    assert_eq!(backend_verdict(&release_runtime(&[failed])), "rollback");
    assert_eq!(
        backend_verdict(&release_runtime(&[failed, BACKEND_OK])),
        "rollback"
    );
}

/// Rule 8, `ok`: D1 answered, so blocks still listed fail on their own.
#[test]
fn blocks_listed_while_the_backend_answers_roll_back() {
    assert_eq!(backend_verdict(&release_runtime(&[BACKEND_OK])), "rollback");
    // Contention is D1 answering too.
    assert_eq!(
        backend_verdict(&release_runtime(&[
            BACKEND_OK,
            (
                r#""runtimes":1}"#,
                r#""runtimes":1,"detail_code":"database.contended"}"#
            ),
        ])),
        "rollback"
    );
}

/// Rule 7: a config entry the source could not answer, now, is an outage
/// whatever `backend` last saw — even `ok` with blocks listed.
#[test]
fn a_degraded_config_entry_is_an_outage_whatever_the_backend() {
    for runtime in [
        release_runtime(&[BACKEND_OK]),
        RT_RELEASE_FAILING.to_string(),
    ] {
        assert_eq!(
            verdict(
                "503",
                Some("4"),
                &body("[]", r#"["wafer-run/auth"]"#, "[]", &runtime)
            ),
            "outage",
            "{runtime}"
        );
    }
}

/// Rule 8, `ok`: `stale`, or `degraded` with nothing listed, is an outage.
#[test]
fn stale_or_unlisted_while_the_backend_answers_is_an_outage() {
    assert_eq!(
        backend_verdict(&release_runtime(&[
            BACKEND_OK,
            (r#""status":"degraded""#, r#""status":"stale""#),
        ])),
        "outage"
    );
    assert_eq!(
        backend_verdict(&release_runtime(&[BACKEND_OK, NOTHING_LISTED])),
        "outage"
    );
}

/// Rule 8, the rest: a `backend.status` the health block never sends, or an
/// `ok` runtime naming no cause, rolls back.
#[test]
fn other_backend_answers_roll_back() {
    assert_eq!(
        backend_verdict(&release_runtime(&[(
            r#""backend":{"status":"failing""#,
            r#""backend":{"status":"unknown""#,
        )])),
        "rollback"
    );
    assert_eq!(
        backend_verdict(r#"{"status":"ok","backend":{"status":"ok"}}"#),
        "rollback"
    );
}

/// Rule 9: without `backend` (native, an older version) the earlier rules
/// stand — the RELEASE example less its `backend` is a block failing with
/// its config answering.
#[test]
fn without_a_backend_the_earlier_rules_stand() {
    let no_backend = (
        r#"
 "backend":{"status":"failing","since":"2026-11-05T10:00:00.300Z","checked":"2026-11-05T10:00:04.500Z"},"#,
        "",
    );
    assert_eq!(backend_verdict(&release_runtime(&[no_backend])), "rollback");
    assert_eq!(
        verdict(
            "503",
            Some("4"),
            &body(
                "[]",
                r#"["wafer-run/auth"]"#,
                "[]",
                &release_runtime(&[no_backend])
            )
        ),
        "outage"
    );
    assert_eq!(
        backend_verdict(&release_runtime(&[no_backend, NOTHING_LISTED])),
        "outage"
    );
}

/// The lib's `ROLLBACK_FLOOR_CREATED_ON`: when 4c6ff923 was created.
const FLOOR: &str = "2026-10-30T07:27:07.651Z";

fn deployment(created_on: &str, versions: &[(&str, u32)]) -> String {
    let versions: Vec<String> = versions
        .iter()
        .map(|(id, pct)| format!(r#"{{"version_id":"{id}","percentage":{pct}}}"#))
        .collect();
    format!(
        r#"{{"id":"d-{created_on}","source":"wrangler","created_on":"{created_on}","versions":[{}]}}"#,
        versions.join(",")
    )
}

fn deployments(list: &[String]) -> String {
    format!("[{}]", list.join(","))
}

/// `rollback_target_version` with `input` on stdin.
fn rollback_target(input: &str) -> Output {
    run_lib::<&str>("rollback_target_version", &[], Some(input))
}

/// The version at 100% in the most recent deployment before the current
/// one, whatever order wrangler lists them in.
#[test]
fn the_rollback_target_is_the_previous_deployments_full_version() {
    let list = deployments(&[
        deployment("2026-11-01T10:00:00.000000Z", &[("old", 100)]),
        deployment("2026-11-05T10:00:00.000000Z", &[("new", 100)]),
        deployment("2026-11-03T10:00:00.000000Z", &[("previous", 100)]),
    ]);
    let out = rollback_target(&list);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "previous");
}

/// An earlier deployment of the live version itself (a re-promotion) is no
/// rollback: the target is the latest deployment of another version.
#[test]
fn a_deployment_of_the_live_version_is_skipped() {
    let list = deployments(&[
        deployment("2026-11-01T10:00:00.000000Z", &[("previous", 100)]),
        deployment("2026-11-03T10:00:00.000000Z", &[("new", 100)]),
        deployment("2026-11-05T10:00:00.000000Z", &[("new", 100)]),
    ]);
    let out = rollback_target(&list);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "previous");

    let only_live = deployments(&[
        deployment("2026-11-03T10:00:00.000000Z", &[("new", 100)]),
        deployment("2026-11-05T10:00:00.000000Z", &[("new", 100)]),
    ]);
    let out = rollback_target(&only_live);
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("no earlier deployment has another version at 100%"),
        "{}",
        stderr(&out)
    );
}

/// A split deployment (a gradual rollout) has no version to return to; the
/// one before it does.
#[test]
fn a_split_deployment_is_skipped() {
    let list = deployments(&[
        deployment("2026-11-01T10:00:00.000000Z", &[("stable", 100)]),
        deployment("2026-11-03T10:00:00.000000Z", &[("a", 50), ("b", 50)]),
        deployment("2026-11-05T10:00:00.000000Z", &[("new", 100)]),
    ]);
    let out = rollback_target(&list);
    assert!(out.status.success(), "{}", stderr(&out));
    assert_eq!(stdout(&out), "stable");
}

/// A history with nothing before the current deployment (or nothing at
/// all) has no target, said as such.
#[test]
fn an_empty_history_has_no_target() {
    for list in [
        deployments(&[deployment("2026-11-05T10:00:00.000000Z", &[("new", 100)])]),
        "[]".to_string(),
    ] {
        let out = rollback_target(&list);
        assert!(!out.status.success(), "{list}");
        assert_eq!(stdout(&out), "", "{list}");
        assert!(
            stderr(&out).contains("no earlier deployment has another version at 100%"),
            "{list}: {}",
            stderr(&out)
        );
    }
}

/// Output that is not wrangler's JSON array is a parse failure, named as
/// one, not an empty history.
#[test]
fn malformed_deployments_json_is_a_parse_failure() {
    for input in ["not json", "{\"versions\":[]}", ""] {
        let out = rollback_target(input);
        assert!(!out.status.success(), "{input:?}");
        assert_eq!(stdout(&out), "", "{input:?}");
        assert!(
            stderr(&out).contains("could not parse wrangler's deployments list"),
            "{input:?}: {}",
            stderr(&out)
        );
    }
}

/// A stub `npx` for `rollback_to_previous`: logs each call's arguments, one
/// line per call, and answers from files in its directory.
struct Wrangler {
    dir: tempfile::TempDir,
}

impl Wrangler {
    /// `before` and `after` are the deployments lists wrangler returns
    /// before and after the rollback; `versions` the `versions view` answers
    /// by id (`created_on` each); `deploy_exit` what `versions deploy`
    /// exits with.
    fn new(before: &str, after: &str, versions: &[(&str, &str)], deploy_exit: i32) -> Self {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().expect("stub dir");
        let d = dir.path();
        std::fs::write(d.join("deployments-0.json"), before).expect("write");
        std::fs::write(d.join("deployments-1.json"), after).expect("write");
        for (id, created_on) in versions {
            std::fs::write(
                d.join(format!("version-{id}.json")),
                format!(
                    r#"{{"id":"{id}","number":1,"metadata":{{"created_on":"{created_on}","source":"wrangler"}}}}"#
                ),
            )
            .expect("write");
        }
        let stub = format!(
            r#"#!/usr/bin/env bash
d={dir}
echo "$*" >>"$d/calls.log"
[[ "$1" == wrangler@4 ]] || exit 99
case "$2 $3" in
  "deployments list")
    n=$(cat "$d/listed" 2>/dev/null || echo 0)
    echo $((n + 1)) >"$d/listed"
    cat "$d/deployments-$n.json" ;;
  "versions view") cat "$d/version-$4.json" ;;
  "versions deploy") exit {deploy_exit} ;;
  *) exit 98 ;;
esac
"#,
            dir = d.display(),
        );
        let npx = d.join("npx");
        std::fs::write(&npx, stub).expect("write stub");
        std::fs::set_permissions(&npx, std::fs::Permissions::from_mode(0o755)).expect("chmod");
        Self { dir }
    }

    fn rollback(&self) -> Output {
        let mut cmd = Command::new("bash");
        let path = format!(
            "{}:{}",
            self.dir.path().display(),
            std::env::var("PATH").unwrap_or_default()
        );
        cmd.env("PATH", path);
        run_lib_in(
            cmd,
            r#"rollback_to_previous "$1" "$2""#,
            &["wrangler.toml", "post-deploy /_health failed (status: 500)"],
            None,
        )
    }

    fn calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.dir.path().join("calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::to_string)
            .collect()
    }
}

fn history() -> String {
    deployments(&[
        deployment("2026-11-01T10:00:00.000000Z", &[("previous", 100)]),
        deployment("2026-11-05T10:00:00.000000Z", &[("new", 100)]),
    ])
}

fn rolled_back() -> String {
    deployments(&[
        deployment("2026-11-01T10:00:00.000000Z", &[("previous", 100)]),
        deployment("2026-11-05T10:00:00.000000Z", &[("new", 100)]),
        deployment("2026-11-05T10:05:00.000000Z", &[("previous", 100)]),
    ])
}

/// A target newer than the floor is deployed at 100% with `versions
/// deploy` (which never forces changed secrets back), never with `wrangler
/// rollback`, and confirmed live.
#[test]
fn a_rollback_deploys_the_target_and_confirms_it_is_live() {
    let w = Wrangler::new(
        &history(),
        &rolled_back(),
        &[("previous", "2026-11-01T09:59:00.5Z")],
        0,
    );
    let out = w.rollback();
    assert!(out.status.success(), "{}", stderr(&out));
    assert!(
        stdout(&out).contains("Rolled back: previous is live"),
        "{}",
        stdout(&out)
    );
    let calls = w.calls();
    assert!(
        calls.contains(
            &"wrangler@4 versions deploy previous@100% --config wrangler.toml \
              --message post-deploy /_health failed (status: 500) --yes"
                .to_string()
        ),
        "{calls:#?}"
    );
    assert!(!calls.iter().any(|c| c.contains(" rollback")), "{calls:#?}");
}

/// A target created before the floor (before the reset) — by a single
/// millisecond — is refused: nothing is deployed, and the operator is told
/// the new version is still live and to deploy forward from main.
#[test]
fn a_target_older_than_the_floor_is_refused() {
    let w = Wrangler::new(
        &history(),
        &history(),
        &[("previous", "2026-10-30T07:27:07.650999Z")],
        0,
    );
    let out = w.rollback();
    assert!(!out.status.success());
    let err = stderr(&out);
    assert!(
        err.contains("refusing to roll back to previous: it predates the 2026-10-30 reset"),
        "{err}"
    );
    assert!(err.contains(&format!("created before {FLOOR}")), "{err}");
    assert!(err.contains("new is still live"), "{err}");
    assert!(err.contains("Deploy forward from main"), "{err}");
    let calls = w.calls();
    assert!(
        !calls
            .iter()
            .any(|c| c.contains("deploy ") || c.contains(" rollback")),
        "{calls:#?}"
    );
}

/// The floor version itself (4c6ff923, its `created_on` carrying more
/// digits than the floor) may be rolled back to.
#[test]
fn the_floor_version_itself_may_be_rolled_back_to() {
    let w = Wrangler::new(
        &history(),
        &rolled_back(),
        &[("previous", "2026-10-30T07:27:07.651234Z")],
        0,
    );
    let out = w.rollback();
    assert!(out.status.success(), "{}", stderr(&out));
}

/// A target whose creation time cannot be read is refused the same way.
#[test]
fn a_target_that_cannot_be_dated_is_refused() {
    let w = Wrangler::new(&history(), &history(), &[], 0);
    let out = w.rollback();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("could not read when previous was created, so it is refused;"),
        "{}",
        stderr(&out)
    );
    assert!(
        !w.calls().iter().any(|c| c.contains("versions deploy")),
        "{:#?}",
        w.calls()
    );
}

/// wrangler exits 0 when an operator declines its prompt: the deployments
/// are read again, and a rollback that did not happen is reported as one.
#[test]
fn a_rollback_that_did_not_happen_is_reported() {
    let w = Wrangler::new(
        &history(),
        &history(),
        &[("previous", "2026-11-01T09:59:00Z")],
        0,
    );
    let out = w.rollback();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("rollback did not happen; new is still live"),
        "{}",
        stderr(&out)
    );
}

/// A deploy wrangler refuses (secrets changed since the target, say) is a
/// failed rollback, with the new version named as still live.
#[test]
fn a_refused_deploy_is_a_failed_rollback() {
    let w = Wrangler::new(
        &history(),
        &history(),
        &[("previous", "2026-11-01T09:59:00Z")],
        1,
    );
    let out = w.rollback();
    assert!(!out.status.success());
    assert!(
        stderr(&out).contains("the rollback to previous failed; new is still live"),
        "{}",
        stderr(&out)
    );
}
