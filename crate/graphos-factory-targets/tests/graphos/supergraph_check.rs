//! `supergraph_check`: `skills/graphos-factory/scripts/supergraph-check.sh`
//! run by `evidence` on a copy of the pilot, with a stub `rover` on PATH
//! that answers `rover subgraph check --format json` with a fixture. The
//! fixtures under `fixtures/rover-subgraph-check/` are written by hand to
//! rover's documented JSON output (no GraphOS key ran a real check): a pass
//! with operation and lint tasks, an `E029` with two composition build
//! errors, an operation check that failed (`E030`, its tasks beside it),
//! and an error with no check behind it.

use super::export::{current_copy, edit_evidence, export_ok, HOST};
use super::{evidence_with, repo, target_scripts};
use graphos_factory_targets::targets::graphos::{export, supergraph_check};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

const KEY: &str = "service:my-graph:not-a-real-key-0123";
const REF: &str = "my-graph@current";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/graphos/fixtures/rover-subgraph-check")
        .join(name)
}

/// A directory holding a `rover` that records its arguments and the schema
/// it was handed, then prints `$STUB_JSON` and exits `$STUB_EXIT`.
fn rover_stub() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let f = dir.path().join("rover");
    std::fs::write(
        &f,
        r#"#!/usr/bin/env bash
if [ "${1:-}" = --version ]; then echo "Rover 0.41.0"; exit 0; fi
printf '%s\n' "$@" > "$STUB_DIR/args"
while [ $# -gt 0 ]; do
  if [ "$1" = --schema ]; then cp "$2" "$STUB_DIR/schema.graphql"; fi
  shift
done
if [ -n "${STUB_ECHO_KEY:-}" ]; then echo "rover: request with $APOLLO_KEY refused" >&2; fi
cat "$STUB_JSON"
exit "${STUB_EXIT:-0}"
"#,
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
    dir
}

/// `evidence` with the stub on PATH answering `json` with exit `exit`, the
/// two variables set, and `more` on top; the layer's row, the whole
/// evidence and the stub's directory (its `args`, its `schema.graphql`).
fn check_with(
    json: &Path,
    exit: i32,
    more: &[(&str, &str)],
) -> (Value, Value, tempfile::TempDir, Option<i32>) {
    let stub = rover_stub();
    let path = format!(
        "{}:{}",
        stub.path().display(),
        std::env::var("PATH").unwrap_or_default()
    );
    let stub_dir = stub.path().to_string_lossy().into_owned();
    let json = json.to_string_lossy().into_owned();
    let exit = exit.to_string();
    let mut env: Vec<(&str, &str)> = vec![
        ("PATH", &path),
        ("STUB_DIR", &stub_dir),
        ("STUB_JSON", &json),
        ("STUB_EXIT", &exit),
        ("APOLLO_KEY", KEY),
        ("GRAPHOS_FACTORY_GRAPH_REF", REF),
    ];
    env.extend_from_slice(more);
    let (code, evidence, stdout) = evidence_with(&env, &[]);
    let row = evidence["target_evidence_layers"]["supergraph_check"].clone();
    assert!(
        !evidence.to_string().contains(KEY) && !stdout.contains(KEY),
        "the key reached the record: {}",
        evidence
    );
    let schema = graphos_factory_core::schemas::load("evidence.schema.json", None).unwrap();
    let errors = graphos_factory_core::jsonschema::validate(&evidence, &schema);
    assert!(errors.is_empty(), "{:?}", errors);
    (row, evidence, stub, code)
}

fn not_verified(evidence: &Value) -> Vec<(String, String, String)> {
    export::not_verified(evidence, &[])
}

#[test]
fn no_graph_ref_is_not_run_with_what_to_do() {
    let (_, evidence, _) = evidence_with(&[("APOLLO_KEY", KEY)], &[]);
    let row = &evidence["target_evidence_layers"]["supergraph_check"];
    assert_eq!(row["status"], "not_run", "{}", row);
    assert_eq!(row["reason"], NO_GRAPH_REF);
    assert!(row.get("details").is_none(), "{}", row);
}

/// What the layer says when no graph ref was given for the run.
const NO_GRAPH_REF: &str = "supergraph_check: no graph to check against — set GRAPHOS_FACTORY_GRAPH_REF=<graph>@<variant> for this run (the agent asks you first), or GRAPHOS_FACTORY_SUPERGRAPH_CHECK=auto to check against $APOLLO_GRAPH_REF on every run (not_run)";

#[test]
fn an_ambient_apollo_graph_ref_alone_does_not_start_a_check() {
    let (row, _, stub, _) = check_with(
        &fixture("pass.json"),
        0,
        &[("GRAPHOS_FACTORY_GRAPH_REF", ""), ("APOLLO_GRAPH_REF", REF)],
    );
    assert_eq!(row["status"], "not_run", "{}", row);
    assert_eq!(row["reason"], NO_GRAPH_REF);
    assert!(!stub.path().join("args").exists());
}

#[test]
fn auto_checks_against_apollo_graph_ref() {
    let (row, _, stub, _) = check_with(
        &fixture("pass.json"),
        0,
        &[
            ("GRAPHOS_FACTORY_GRAPH_REF", ""),
            ("APOLLO_GRAPH_REF", "other-graph@prod"),
            ("GRAPHOS_FACTORY_SUPERGRAPH_CHECK", "auto"),
        ],
    );
    assert_eq!(row["status"], "pass", "{}", row);
    assert_eq!(row["details"]["mode"], "auto");
    assert_eq!(row["details"]["graph_ref"], "other-graph@prod");
    let args = std::fs::read_to_string(stub.path().join("args")).unwrap();
    assert_eq!(args.lines().nth(2), Some("other-graph@prod"));
}

#[test]
fn an_explicit_graph_ref_wins_over_auto() {
    let (row, _, stub, _) = check_with(
        &fixture("pass.json"),
        0,
        &[
            ("APOLLO_GRAPH_REF", "other-graph@prod"),
            ("GRAPHOS_FACTORY_SUPERGRAPH_CHECK", "auto"),
        ],
    );
    assert_eq!(row["status"], "pass", "{}", row);
    assert_eq!(row["details"]["mode"], "explicit");
    assert_eq!(row["details"]["graph_ref"], REF);
    let args = std::fs::read_to_string(stub.path().join("args")).unwrap();
    assert_eq!(args.lines().nth(2), Some(REF));
}

#[test]
fn a_switch_value_other_than_auto_is_not_run() {
    for bad in ["yes", "AUTO", "1", "explicit"] {
        let (row, _, stub, _) = check_with(
            &fixture("pass.json"),
            0,
            &[("GRAPHOS_FACTORY_SUPERGRAPH_CHECK", bad)],
        );
        assert_eq!(row["status"], "not_run", "{}: {}", bad, row);
        assert_eq!(
            row["reason"],
            "supergraph_check: GRAPHOS_FACTORY_SUPERGRAPH_CHECK is set to something other than auto, its only value — not checked against your supergraph (not_run)"
        );
        assert!(!stub.path().join("args").exists(), "{}", bad);
    }
}

#[test]
fn no_key_is_not_run_and_export_lists_it() {
    let (_, evidence, _) = evidence_with(&[("GRAPHOS_FACTORY_GRAPH_REF", REF)], &[]);
    let row = &evidence["target_evidence_layers"]["supergraph_check"];
    assert_eq!(row["status"], "not_run", "{}", row);
    assert_eq!(
        row["reason"],
        "supergraph_check: APOLLO_KEY is not set — not checked against your supergraph (not_run)"
    );
    assert!(row.get("details").is_none(), "{}", row);
    assert!(!export::gate(&evidence)
        .reasons
        .iter()
        .any(|r| r.starts_with("supergraph_check")));
    let listed: Vec<_> = not_verified(&evidence)
        .into_iter()
        .filter(|(l, _, _)| l == "supergraph_check")
        .collect();
    assert_eq!(listed.len(), 1, "{:?}", listed);
    assert_eq!(listed[0].1, "not_run");
    assert!(
        listed[0].2.contains("APOLLO_KEY is not set"),
        "{:?}",
        listed
    );
}

#[test]
fn a_passing_check_is_recorded_with_the_graph_ref_and_counts() {
    let (row, evidence, stub, _) = check_with(&fixture("pass.json"), 0, &[]);
    assert_eq!(row["status"], "pass", "{}", row);
    assert_eq!(row["exit_code"], 0);
    assert!(row.get("reason").is_none(), "{}", row);
    assert!(row["command"]
        .as_str()
        .unwrap()
        .starts_with("supergraph-check.sh "));
    assert_eq!(
        row["details"],
        json!({
            "graph_ref": REF,
            "mode": "explicit",
            "rover": "0.41.0",
            "subgraph": "gitea",
            "composition": "pass",
            "build_errors": 0,
            "checks": {"operations": "PASSED", "lint": "PASSED"},
            "operations": {"status": "PASSED", "checked": 12, "failing_changes": 0},
            "target_url": "https://studio.apollographql.com/graph/my-graph/variant/current/operationsCheck/1",
        })
    );
    // The flags rover was run with.
    let args = std::fs::read_to_string(stub.path().join("args")).unwrap();
    let args: Vec<&str> = args.lines().collect();
    assert_eq!(&args[..5], ["subgraph", "check", REF, "--name", "gitea"]);
    assert_eq!(args[5], "--schema");
    assert_eq!(&args[7..], ["--format", "json"]);
    // A pass is not listed as unverified, and does not gate.
    assert!(!not_verified(&evidence)
        .iter()
        .any(|(l, _, _)| l == "supergraph_check"));
    assert!(!export::gate(&evidence)
        .reasons
        .iter()
        .any(|r| r.starts_with("supergraph_check")));
}

#[test]
fn build_errors_fail_and_export_refuses_naming_them() {
    let (row, evidence, _, code) = check_with(&fixture("build-errors.json"), 1, &[]);
    assert_eq!(row["status"], "fail", "{}", row);
    assert_eq!(
        row["reason"],
        "supergraph_check: FAIL — composition with my-graph@current: 2 build error(s)"
    );
    let findings: Vec<&str> = row["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .collect();
    assert_eq!(findings.len(), 2, "{:?}", findings);
    assert!(findings[0].starts_with(
        "build error [INVALID_FIELD_SHARING]: Non-shareable field \"Repository.name\""
    ));
    assert!(findings[1].starts_with("build error [SATISFIABILITY_ERROR]: "));
    assert_eq!(row["details"]["composition"], "fail");
    assert_eq!(row["details"]["build_errors"], 2);
    assert_eq!(row["details"]["rover_error_code"], "E029");
    // Non-gating for evidence: the run exits as it does with the layer
    // not_run, the core layers deciding.
    let (baseline, _, _) = evidence_with(&[], &[]);
    assert_eq!(code, baseline);

    // export refuses, naming the errors, on a workspace otherwise passing.
    let reasons = export::gate(&evidence).reasons;
    assert!(reasons.iter().any(|r| r
        == "supergraph_check: fail (supergraph_check: FAIL — composition with my-graph@current: 2 build error(s))"), "{:?}", reasons);
    assert!(reasons
        .iter()
        .any(|r| r.starts_with("supergraph_check: build error [INVALID_FIELD_SHARING]")));
    let ws = current_copy();
    edit_evidence(ws.path(), |e| {
        e["target_evidence_layers"]["supergraph_check"] = row.clone();
    });
    let out = tempfile::tempdir().unwrap();
    let dest = out.path().join("deploy");
    let dest_s = dest.to_string_lossy().into_owned();
    let (code, stdout, stderr) = export_ok(ws.path(), &["--out", &dest_s, "--base-url", HOST]);
    assert_eq!(code, Some(1), "{} {}", stdout, stderr);
    assert!(stderr.contains("not exported"), "{}", stderr);
    assert!(stderr.contains("INVALID_FIELD_SHARING"), "{}", stderr);
    assert!(stderr.contains("SATISFIABILITY_ERROR"), "{}", stderr);
    assert!(!dest.join("gitea.graphql").exists());
}

#[test]
fn a_failed_operation_check_fails_with_the_failing_change() {
    let (row, evidence, _, _) = check_with(&fixture("checks-failed.json"), 1, &[]);
    assert_eq!(row["status"], "fail", "{}", row);
    assert_eq!(
        row["reason"],
        "supergraph_check: FAIL — composes with my-graph@current, but checks did not pass: operations FAILED"
    );
    assert_eq!(
        row["findings"],
        json!(["operations [FIELD_REMOVED]: type `Repository`: field `fullName` removed"])
    );
    assert_eq!(row["details"]["composition"], "pass");
    assert_eq!(
        row["details"]["operations"],
        json!({"status": "FAILED", "checked": 12, "failing_changes": 1})
    );
    assert!(!export::gate(&evidence).pass);
}

#[test]
fn an_error_with_no_check_behind_it_is_not_run() {
    let (row, evidence, _, _) = check_with(&fixture("no-graph.json"), 1, &[]);
    assert_eq!(row["status"], "not_run", "{}", row);
    assert_eq!(
        row["reason"],
        "supergraph_check: rover subgraph check did not run the check: Could not find graph with name \"my-graph\". (E009) — not checked against your supergraph (not_run)"
    );
    assert_eq!(row["details"]["composition"], "not_run");
    assert!(not_verified(&evidence)
        .iter()
        .any(|(l, s, _)| l == "supergraph_check" && s == "not_run"));
}

#[test]
fn rover_missing_is_skipped() {
    // PATH holds bash and nothing else, so no rover can be found however
    // the machine is set up, and nothing reaches GraphOS.
    let bin = tempfile::tempdir().unwrap();
    let bash = std::env::var_os("PATH")
        .and_then(|p| {
            std::env::split_paths(&p)
                .map(|d| d.join("bash"))
                .find(|b| b.is_file())
        })
        .expect("bash on PATH");
    std::os::unix::fs::symlink(bash, bin.path().join("bash")).unwrap();
    let path = bin.path().to_string_lossy().into_owned();
    let (_, evidence, _) = evidence_with(
        &[
            ("PATH", &path),
            ("APOLLO_KEY", KEY),
            ("GRAPHOS_FACTORY_GRAPH_REF", REF),
        ],
        &[],
    );
    let row = &evidence["target_evidence_layers"]["supergraph_check"];
    assert_eq!(row["status"], "skipped", "{}", row);
    assert_eq!(row["exit_code"], 127);
    assert_eq!(row["reason"], "supergraph_check: rover is not installed");
    assert!(not_verified(&evidence)
        .iter()
        .any(|(l, s, _)| l == "supergraph_check" && s == "skipped"));
}

#[test]
fn a_graph_ref_that_is_not_graph_at_variant_is_not_run() {
    for bad in ["--schema@x", "my-graph", "a b@current", "-x@current"] {
        let (row, _, stub, _) = check_with(
            &fixture("pass.json"),
            0,
            &[("GRAPHOS_FACTORY_GRAPH_REF", bad)],
        );
        assert_eq!(row["status"], "not_run", "{}: {}", bad, row);
        assert!(
            row["reason"].as_str().unwrap().starts_with(
                "supergraph_check: GRAPHOS_FACTORY_GRAPH_REF is not <graph>@<variant>"
            ),
            "{}",
            row
        );
        assert!(!stub.path().join("args").exists(), "{}", bad);
    }
}

#[test]
fn the_key_never_reaches_the_record_even_when_rover_echoes_it() {
    let dir = tempfile::tempdir().unwrap();
    let echoed = dir.path().join("echo.json");
    std::fs::write(
        &echoed,
        json!({
            "json_version": "1",
            "data": {"success": false},
            "error": {
                "message": format!("Encountered 1 build error with {}", KEY),
                "code": "E029",
                "details": {"build_errors": [{"message": format!("key {} rejected", KEY), "code": null, "type": "composition"}]}
            }
        })
        .to_string(),
    )
    .unwrap();
    // check_with asserts the key is in neither latest.json nor the report.
    let (row, _, _, _) = check_with(&echoed, 1, &[("STUB_ECHO_KEY", "1")]);
    assert_eq!(row["status"], "fail", "{}", row);
    assert_eq!(
        row["findings"],
        json!(["build error [no code]: key <redacted> rejected"])
    );
}

#[test]
fn an_auth_expr_override_never_reaches_graphos() {
    let (row, _, stub, _) = check_with(
        &fixture("pass.json"),
        0,
        &[
            ("GITEA_AUTH_EXPR", "literal-credential-0123"),
            ("GITEA_BASE_URL", HOST),
        ],
    );
    assert_eq!(row["status"], "pass", "{}", row);
    let sent = std::fs::read_to_string(stub.path().join("schema.graphql")).unwrap();
    assert!(!sent.contains("literal-credential-0123"), "{}", sent);
    assert!(sent.contains("token {$env.GITEA_TOKEN}"), "{}", sent);
    // The production host, when given, is the one sent.
    assert!(sent.contains(&format!("baseURL: \"{}\"", HOST)), "{}", sent);
    assert!(!sent.contains("{{"), "{}", sent);
}

#[test]
fn a_base_url_with_userinfo_is_not_sent() {
    let (row, _, stub, _) = check_with(
        &fixture("pass.json"),
        0,
        &[("GITEA_BASE_URL", "https://user:pw@git.example.com/api/v1")],
    );
    assert_eq!(row["status"], "not_run", "{}", row);
    assert!(
        row["reason"].as_str().unwrap().contains("userinfo"),
        "{}",
        row
    );
    assert!(!stub.path().join("args").exists());
}

#[test]
fn a_base_url_with_a_credential_query_parameter_is_not_sent() {
    let (row, _, stub, _) = check_with(
        &fixture("pass.json"),
        0,
        &[(
            "GITEA_BASE_URL",
            "https://git.example.com/api/v1?access_token=abc",
        )],
    );
    assert_eq!(row["status"], "not_run", "{}", row);
    assert!(
        row["reason"].as_str().unwrap().contains("query parameter"),
        "{}",
        row
    );
    assert!(!stub.path().join("args").exists());
    // The short spellings are credentials too.
    for query in [
        "pwd=abc",
        "pw=abc",
        "user_pw=abc",
        "pw1=abc",
        "cred=abc",
        "jwt=abc",
        "bearer=abc",
    ] {
        let url = format!("https://git.example.com/api/v1?{query}");
        let (row, _, stub, _) = check_with(&fixture("pass.json"), 0, &[("GITEA_BASE_URL", &url)]);
        assert_eq!(row["status"], "not_run", "{query}: {}", row);
        assert!(!stub.path().join("args").exists(), "{query} was sent");
    }
    // A query parameter that names no credential is sent.
    let (row, _, _, _) = check_with(
        &fixture("pass.json"),
        0,
        &[(
            "GITEA_BASE_URL",
            "https://git.example.com/api/v1?api-version=2",
        )],
    );
    assert_eq!(row["status"], "pass", "{}", row);
}

#[test]
fn the_script_is_found_beside_the_core_in_either_layout() {
    // A checkout: the core at the root, the script under skills/.
    let found =
        supergraph_check::script_dir(&repo().join("graphos-factory-core/scripts"), None).unwrap();
    assert_eq!(
        std::fs::canonicalize(found).unwrap(),
        std::fs::canonicalize(target_scripts()).unwrap()
    );
    // An installed skill: its own copy of the core inside it.
    let skill = tempfile::tempdir().unwrap();
    let core = skill.path().join("graphos-factory-core/scripts");
    std::fs::create_dir_all(&core).unwrap();
    std::fs::create_dir_all(skill.path().join("scripts")).unwrap();
    std::fs::write(
        skill.path().join("scripts").join(supergraph_check::SCRIPT),
        "",
    )
    .unwrap();
    let found = supergraph_check::script_dir(&core, None).unwrap();
    assert_eq!(
        std::fs::canonicalize(found).unwrap(),
        std::fs::canonicalize(skill.path().join("scripts")).unwrap()
    );
    // Neither: the layer is skipped and says where it looked.
    let none = tempfile::tempdir().unwrap();
    let err = supergraph_check::script_dir(none.path(), None).unwrap_err();
    assert!(err.contains("not installed"), "{}", err);
    assert_eq!(supergraph_check::row(127, &err, None)["status"], "skipped");
    // The variable wins over both layouts, and names no fallback.
    let named = supergraph_check::script_dir(
        &repo().join("graphos-factory-core/scripts"),
        Some(none.path().as_os_str()),
    );
    assert!(named.unwrap_err().contains(supergraph_check::SCRIPTS_ENV));
}
