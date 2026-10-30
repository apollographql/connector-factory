//! The graphos target's suite (ADR 0114, Phase 8c): `init --target`
//! records the name, a workspace runs against the target its `skill.name`
//! names, the two `@source` rules this target relaxes, the
//! `supergraph_check` evidence layer, `--help` grouping its commands, and a
//! workspace naming no registered target refused, and `export` (its own
//! module, `export.rs`). One binary, as each suite is (ADR 0061).
//!
//! It runs the `graphos-factory` binary, which registers this target
//! alone (phase 8f), and passes in the public tree, which builds no other.
//! What needs the other product's binary beside this one is the other
//! target's suite's (`targets.rs` there).

mod export;
mod foreign;
mod init_files;
mod supergraph_check;

use graphos_factory_targets::targets::graphos::{self, TARGET};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_graphos-factory"));
    // Nothing from the environment the suite runs in reaches GraphOS: a
    // test that runs the supergraph check sets its own (stubbed) inputs.
    c.env_remove("GRAPHOS_FACTORY_CORE_SCRIPTS")
        .env_remove("GRAPHOS_FACTORY_CORE_BIN")
        .env_remove("GRAPHOS_FACTORY_TARGET_SCRIPTS")
        .env_remove("APOLLO_KEY")
        .env_remove("APOLLO_GRAPH_REF")
        .env_remove("GRAPHOS_FACTORY_GRAPH_REF")
        .env_remove("GRAPHOS_FACTORY_SUPERGRAPH_CHECK")
        .stdin(std::process::Stdio::null());
    c
}

fn run(args: &[&str]) -> (Option<i32>, String, String) {
    let out = bin().args(args).output().unwrap();
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn copy_dir(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

fn repo() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// This target's pilot.
fn graphos_pilot() -> PathBuf {
    repo().join("pilots/graphos/gitea")
}

fn copy_of(pilot: &Path) -> tempfile::TempDir {
    let ws = tempfile::tempdir().unwrap();
    copy_dir(pilot, ws.path());
    ws
}

fn lint_json(dir: &Path) -> Value {
    let (_, stdout, stderr) = run(&["lint", &dir.to_string_lossy(), "--json"]);
    serde_json::from_str(&stdout).unwrap_or_else(|e| panic!("{}: {} {}", e, stdout, stderr))
}

fn findings<'a>(report: &'a Value, rule: &str) -> Vec<&'a Value> {
    report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["rule"] == rule)
        .collect()
}

fn workspace_yaml(dir: &Path) -> String {
    std::fs::read_to_string(dir.join(".factory/workspace.yaml")).unwrap()
}

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/graphos/fixtures")
        .join(name)
}

#[test]
fn the_target_has_the_cores_two_placeholders_and_one_command() {
    assert_eq!(TARGET.name, "graphos-factory");
    assert_eq!(TARGET.placeholders, &["BASE_URL", "AUTH_EXPR"]);
    assert!(TARGET.variables_file_required);
    assert!(TARGET.required_files.is_empty());
    assert!(TARGET.output_files.is_empty());
    let commands: Vec<&str> = TARGET.commands.iter().map(|c| c.name).collect();
    assert_eq!(commands, vec!["export"]);
    assert_eq!(
        TARGET.lint_rules,
        &[
            "foreign-type-without-key",
            "foreign-type-key-field-missing",
            "requires-on-foreign-type",
            "foreign-type-not-object"
        ]
    );
    assert!(TARGET.foreign_types);
    assert!((TARGET.tag_vocabulary)().is_empty());
    assert!(TARGET.export_gate.is_some());
    assert!(TARGET.embedded_schemas.is_empty());
    assert!(TARGET.compose.federation_spec_version.is_none());
    assert_eq!(
        TARGET.compose.link_imports,
        &[
            "@key",
            "@shareable",
            "@requires",
            "@provides",
            "@external",
            "@tag",
            "@inaccessible",
            "@listSize",
            "@cost"
        ]
    );
    assert_eq!(
        TARGET.overridden_rules(),
        vec![
            "multiple-sources",
            "source-name-secondary",
            "commented-source",
            "type-prefix"
        ]
    );
    let layers: Vec<(&str, bool)> = TARGET
        .evidence_layers
        .iter()
        .map(|l| (l.name, l.gating))
        .collect();
    assert_eq!(layers, vec![("supergraph_check", false)]);
    // The binary is named for the target.
    assert_eq!(TARGET.name, graphos::NAME);
}

#[test]
fn init_records_its_name_with_or_without_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let spec = fixture("widgets.openapi.json");
    let spec = spec.to_string_lossy();
    let init = |ws: &Path, extra: &[&str]| {
        let ws = ws.to_string_lossy().into_owned();
        let mut args = vec![
            "init",
            &ws,
            "--name",
            "widget-co",
            "--spec",
            &spec,
            "--created-at",
            "2026-10-01T00:00:00Z",
        ];
        args.extend_from_slice(extra);
        run(&args)
    };
    let ws = dir.path().join("widget-co");
    let (code, stdout, stderr) = init(&ws, &["--target", "graphos-factory"]);
    assert_eq!(code, Some(0), "{} {}", stdout, stderr);
    let text = workspace_yaml(&ws);
    assert!(
        text.contains("skill:\n  name: graphos-factory\n"),
        "{}",
        text
    );
    // The new workspace runs against this target from then on.
    assert_eq!(lint_json(&ws)["target"]["name"], "graphos-factory");

    // Without --target the binary writes its one target.
    let plain = dir.path().join("plain");
    let (code, stdout, stderr) = init(&plain, &[]);
    assert_eq!(code, Some(0), "{} {}", stdout, stderr);
    assert!(
        workspace_yaml(&plain).contains("skill:\n  name: graphos-factory\n"),
        "{}",
        workspace_yaml(&plain)
    );

    // An unknown target is refused, naming the registered one. Nothing is
    // written.
    let other = dir.path().join("other");
    let (code, stdout, stderr) = init(&other, &["--target", "nobody", "--json"]);
    assert_eq!(code, Some(1), "{} {}", stdout, stderr);
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["code"], "target-unknown");
    assert!(
        report["error"]
            .as_str()
            .unwrap()
            .contains("(graphos-factory)"),
        "{}",
        report
    );
    assert!(!other.exists());
}

/// A second `@source` is a warning here, and a commented-out one is not
/// reported. (That the same edit is an error under the other target is
/// that target's suite's.)
#[test]
fn a_second_source_warns_here() {
    let second = "@source(name: \"gitea\", http: { baseURL: \"{{BASE_URL}}\" })\n# was: @source(name: \"old\")\ntype Query";
    let edit = |ws: &Path| {
        let schema = ws.join("gitea.graphql");
        let sdl = std::fs::read_to_string(&schema).unwrap();
        assert!(sdl.contains("type Query"));
        std::fs::write(&schema, sdl.replacen("type Query", second, 1)).unwrap();
    };

    let here = copy_of(&graphos_pilot());
    edit(here.path());
    let report = lint_json(here.path());
    assert_eq!(report["target"]["name"], "graphos-factory");
    let multiple = findings(&report, "multiple-sources");
    assert_eq!(multiple.len(), 1, "{}", report);
    assert_eq!(multiple[0]["severity"], "warn");
    assert_eq!(multiple[0]["origin"], "core");
    assert_eq!(
        multiple[0]["message"],
        "the schema declares 2 @source directives; this target composes with other subgraphs; a second @source is unmodelled, not forbidden"
    );
    assert!(
        findings(&report, "commented-source").is_empty(),
        "{}",
        report
    );
}

/// A second `@source` with its own name: `multiple-sources` and
/// `source-name-secondary` are both warnings here, and the schema has no error from
/// either. (The other target keeps both errors; its suite's.)
#[test]
fn a_second_differently_named_source_is_two_warnings_and_no_error() {
    let here = copy_of(&graphos_pilot());
    let schema = here.path().join("gitea.graphql");
    let sdl = std::fs::read_to_string(&schema).unwrap();
    std::fs::write(
        &schema,
        sdl.replacen(
            "type Query",
            "@source(name: \"gitea_admin\", http: { baseURL: \"{{BASE_URL}}\" })\ntype Query",
            1,
        ),
    )
    .unwrap();
    let report = lint_json(here.path());
    let multiple = findings(&report, "multiple-sources");
    assert_eq!(multiple.len(), 1, "{}", report);
    assert_eq!(multiple[0]["severity"], "warn");
    assert!(findings(&report, "source-name").is_empty(), "{}", report);
    let name = findings(&report, "source-name-secondary");
    assert_eq!(name.len(), 1, "{}", report);
    assert_eq!(name[0]["severity"], "warn");
    assert_eq!(
        name[0]["message"],
        "@source(name: \"gitea_admin\") must equal workspace.service \"gitea\": a second @source is unmodelled, not forbidden; only the first @source's name must equal workspace.service"
    );
    let errors: Vec<&Value> = report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["severity"] == "error")
        .collect();
    assert!(
        errors
            .iter()
            .all(|f| f["rule"] != "source-name-secondary" && f["rule"] != "multiple-sources"),
        "{:?}",
        errors
    );
}

/// The only `@source`, misnamed, stays an error here: the e2e router
/// config keys the source by `workspace.service`, so a renamed one would
/// send e2e to the real host instead of WireMock.
#[test]
fn a_lone_misnamed_source_is_still_an_error() {
    let here = copy_of(&graphos_pilot());
    let schema = here.path().join("gitea.graphql");
    let sdl = std::fs::read_to_string(&schema).unwrap();
    assert!(sdl.contains("  name: \"gitea\"\n"));
    std::fs::write(
        &schema,
        sdl.replacen("  name: \"gitea\"\n", "  name: \"gitea_v2\"\n", 1),
    )
    .unwrap();
    let report = lint_json(here.path());
    let name = findings(&report, "source-name");
    assert_eq!(name.len(), 1, "{}", report);
    assert_eq!(name[0]["severity"], "error");
    assert_eq!(
        name[0]["message"],
        "@source(name: \"gitea_v2\") must equal workspace.service \"gitea\" — rover derives join__Graph from it"
    );
    assert!(findings(&report, "source-name-secondary").is_empty());
}

/// The pilot as committed lints clean against this target, with no
/// target finding (it declares no foreign type) and the core findings only.
#[test]
fn the_pilot_lints_clean_against_this_target() {
    let (code, stdout, stderr) = run(&["lint", &graphos_pilot().to_string_lossy(), "--json"]);
    assert_eq!(code, Some(0), "{} {}", stdout, stderr);
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["errors"], 0, "{}", report);
    assert_eq!(
        report["target"],
        serde_json::json!({
            "name": "graphos-factory",
            "rules": ["foreign-type-without-key", "foreign-type-key-field-missing", "requires-on-foreign-type", "foreign-type-not-object"],
            "overrides": ["multiple-sources", "source-name-secondary", "commented-source", "type-prefix"],
        })
    );
    // Unchanged by the overrides: 0 errors, 2 warnings.
    assert_eq!(report["warnings"], 2, "{}", report);
    assert!(report["findings"]
        .as_array()
        .unwrap()
        .iter()
        .all(|f| f["origin"] == "core"));
    // The pilot is this target's: no file another target adds.
    assert_eq!(
        std::fs::read_dir(graphos_pilot())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.path().extension().is_some_and(|x| x == "yaml"))
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect::<std::collections::BTreeSet<_>>(),
        ["supergraph.yaml", "template.yaml"]
            .iter()
            .map(|s| s.to_string())
            .collect()
    );
    assert!(workspace_yaml(&graphos_pilot()).contains("skill:\n  name: graphos-factory\n"));
}

/// A Federation directive of the target's set, applied without being
/// imported, is drift; an imported one the schema does not apply is not
/// (the pilot imports `@key` and applies none).
#[test]
fn federation_drift_holds_applied_directives_to_the_link() {
    let ws = copy_of(&graphos_pilot());
    assert!(findings(&lint_json(ws.path()), "federation-drift").is_empty());
    let schema = ws.path().join("gitea.graphql");
    let sdl = std::fs::read_to_string(&schema).unwrap();
    std::fs::write(
        &schema,
        sdl.replacen("type Gitea_User {", "type Gitea_User @shareable {", 1),
    )
    .unwrap();
    let report = lint_json(ws.path());
    let drift = findings(&report, "federation-drift");
    assert_eq!(drift.len(), 1, "{}", report);
    assert_eq!(
        drift[0]["message"],
        "the schema applies @shareable but the federation @link does not import it"
    );
}

/// `@tag` is permitted, for GraphOS Contracts, and held to the link like
/// the rest: applied without the import it is drift, imported it is not.
/// The name is the user's; the target checks none (ADR 0123).
#[test]
fn federation_drift_holds_tag_to_the_link() {
    let ws = copy_of(&graphos_pilot());
    let schema = ws.path().join("gitea.graphql");
    let sdl = std::fs::read_to_string(&schema).unwrap();
    // The pilot neither imports nor applies `@tag`.
    assert!(sdl.contains("import: [\"@key\"])"), "{}", sdl);
    assert!(!sdl.contains("@tag"));
    let tagged = sdl.replacen(
        "type Gitea_User {",
        "type Gitea_User @tag(name: \"partner\") {",
        1,
    );
    assert_ne!(tagged, sdl);
    std::fs::write(&schema, &tagged).unwrap();
    let report = lint_json(ws.path());
    let drift = findings(&report, "federation-drift");
    assert_eq!(drift.len(), 1, "{}", report);
    assert_eq!(
        drift[0]["message"],
        "the schema applies @tag but the federation @link does not import it"
    );
    // No tag vocabulary: the name is not checked.
    assert!(findings(&report, "unknown-tag").is_empty(), "{}", report);

    let imported = tagged.replacen("import: [\"@key\"])", "import: [\"@key\", \"@tag\"])", 1);
    assert_ne!(imported, tagged);
    std::fs::write(&schema, imported).unwrap();
    let report = lint_json(ws.path());
    assert!(
        findings(&report, "federation-drift").is_empty(),
        "{}",
        report
    );
    assert!(findings(&report, "unknown-tag").is_empty(), "{}", report);
}

/// `@inaccessible`, `@listSize` and `@cost` are importable too (`SKILL.md`),
/// so each applied without its import is one `federation-drift` finding
/// and imported is none.
#[test]
fn federation_drift_holds_the_other_importable_directives_to_the_link() {
    for (directive, application) in [
        ("@inaccessible", "type Gitea_User @inaccessible {"),
        ("@listSize", "type Gitea_User @listSize(assumedSize: 10) {"),
        ("@cost", "type Gitea_User @cost(weight: 2) {"),
    ] {
        let ws = copy_of(&graphos_pilot());
        let schema = ws.path().join("gitea.graphql");
        let sdl = std::fs::read_to_string(&schema).unwrap();
        assert!(!sdl.contains(directive), "{}", sdl);
        let applied = sdl.replacen("type Gitea_User {", application, 1);
        assert_ne!(applied, sdl);
        std::fs::write(&schema, &applied).unwrap();
        let report = lint_json(ws.path());
        let drift = findings(&report, "federation-drift");
        assert_eq!(drift.len(), 1, "{}: {}", directive, report);
        assert_eq!(
            drift[0]["message"],
            format!(
                "the schema applies {} but the federation @link does not import it",
                directive
            ),
            "{}",
            report
        );

        let imported = applied.replacen(
            "import: [\"@key\"])",
            &format!("import: [\"@key\", \"{}\"])", directive),
            1,
        );
        assert_ne!(imported, applied);
        std::fs::write(&schema, imported).unwrap();
        let report = lint_json(ws.path());
        assert!(
            findings(&report, "federation-drift").is_empty(),
            "{}: {}",
            directive,
            report
        );
    }
}

fn script(dir: &Path, name: &str, body: &str) {
    let f = dir.join(name);
    std::fs::write(&f, format!("#!/usr/bin/env bash\n{}\n", body)).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o755)).unwrap();
}

/// `evidence` on a copy of the pilot, with stub wrappers: what is under
/// test is the recording of this target's layer, not the stack. The target's
/// own script is the real one, found through `GRAPHOS_FACTORY_TARGET_SCRIPTS`.
fn evidence(extra: &[&str]) -> (Option<i32>, Value) {
    let (code, evidence, _) = evidence_with(&[], extra);
    (code, evidence)
}

/// The target's scripts in this tree.
fn target_scripts() -> PathBuf {
    repo().join("skills/graphos-factory/scripts")
}

/// `evidence` as [`evidence`] does, with `env` set on the binary; its exit
/// code, `latest.json` and stdout.
fn evidence_with(env: &[(&str, &str)], extra: &[&str]) -> (Option<i32>, Value, String) {
    let ws = copy_of(&graphos_pilot());
    let scripts = tempfile::tempdir().unwrap();
    let s = scripts.path();
    script(s, "compose.sh", "echo 'compose: pass'; exit 0");
    script(
        s,
        "unit.sh",
        "echo 'TEST RESULTS: SUCCESSFUL 1  passed; 0 failed; 0 skipped'; exit 0",
    );
    script(s, "e2e.sh", "echo 'e2e: 0 passed, 0 failed'; exit 0");
    script(s, "live.sh", "echo 'live: not_run — no credential'; exit 3");
    let ws_s = ws.path().to_string_lossy().into_owned();
    let s_s = s.to_string_lossy().into_owned();
    let mut args = vec!["evidence", &ws_s, "--scripts", &s_s];
    args.extend_from_slice(extra);
    let mut c = bin();
    c.env("GRAPHOS_FACTORY_TARGET_SCRIPTS", target_scripts());
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.args(&args).output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    let text = std::fs::read_to_string(ws.path().join(".factory/evidence/latest.json"))
        .unwrap_or_else(|e| panic!("{}: {} {}", e, stdout, stderr));
    (
        out.status.code(),
        serde_json::from_str(&text).unwrap(),
        stdout,
    )
}

#[test]
fn evidence_records_supergraph_check_not_run_under_target_evidence_layers() {
    let (code, evidence) = evidence(&[]);
    let layers = evidence["target_evidence_layers"].as_object().unwrap();
    let names: Vec<&String> = layers.keys().collect();
    assert_eq!(names, vec!["supergraph_check"], "{}", evidence);
    // No graph ref for the run: the script says what to set and exits 3.
    let row = &evidence["target_evidence_layers"]["supergraph_check"];
    assert_eq!(row["status"], "not_run", "{}", row);
    assert_eq!(row["target"], "graphos-factory");
    assert_eq!(row["exit_code"], 3);
    assert_eq!(
        row["reason"],
        "supergraph_check: no graph to check against — set GRAPHOS_FACTORY_GRAPH_REF=<graph>@<variant> for this run (the agent asks you first), or GRAPHOS_FACTORY_SUPERGRAPH_CHECK=auto to check against $APOLLO_GRAPH_REF on every run (not_run)"
    );
    // The six core layers are all there, and not_run is not a failure.
    for core in [
        "compose",
        "connector_unit",
        "wiremock_e2e",
        "conformance",
        "lint",
        "live",
    ] {
        assert!(evidence["layers"].get(core).is_some(), "{}", core);
    }
    assert_ne!(code, None);
    let schema = graphos_factory_core::schemas::load("evidence.schema.json", None).unwrap();
    let errors = graphos_factory_core::jsonschema::validate(&evidence, &schema);
    assert!(errors.is_empty(), "{:?}", errors);

    // Left out by a flag, it is not_run with that reason, never skipped
    // (the tool is not missing) or pass.
    let (_, evidence) = evidence_skipping();
    let row = &evidence["target_evidence_layers"]["supergraph_check"];
    assert_eq!(row["status"], "not_run");
    assert_eq!(row["reason"], "excluded by --only/--skip");
}

fn evidence_skipping() -> (Option<i32>, Value) {
    evidence(&["--skip", "supergraph_check"])
}

#[test]
fn help_lists_this_target_under_its_name() {
    let (code, stdout, _) = run(&["--help"]);
    assert_eq!(code, Some(0));
    let at = |needle: &str| {
        stdout
            .find(needle)
            .unwrap_or_else(|| panic!("{} missing: {}", needle, stdout))
    };
    assert!(at("\n  error-statuses ") < at("\n  [target graphos-factory]\n"));
    assert!(at("\n  [target graphos-factory]\n") < at("\n  export "));
    assert!(stdout.ends_with("\n  version\n"), "{}", stdout);
    // init's own usage names the flag that picks one.
    let (code, stdout, _) = run(&["init", "--help"]);
    assert_eq!(code, Some(0));
    assert!(stdout.contains("--target NAME"), "{}", stdout);
}

/// A workspace whose `skill.name` names no registered target is refused
/// before the command runs, with the names this binary serves.
#[test]
fn a_workspace_naming_an_unknown_target_is_refused() {
    let ws = copy_of(&graphos_pilot());
    let file = ws.path().join(".factory/workspace.yaml");
    let text = std::fs::read_to_string(&file).unwrap();
    std::fs::write(
        &file,
        text.replace("name: graphos-factory", "name: nobody-factory"),
    )
    .unwrap();
    let dir = ws.path().to_string_lossy().into_owned();
    for args in [
        vec!["lint", dir.as_str()],
        vec!["validate", dir.as_str()],
        vec!["reconcile", dir.as_str()],
        vec!["decisions", "list", dir.as_str()],
    ] {
        let (code, stdout, stderr) = run(&args);
        assert_eq!(code, Some(1), "{:?}: {} {}", args, stdout, stderr);
        assert!(
            stderr.contains("\"nobody-factory\"")
                && stderr.contains("(registered: graphos-factory)"),
            "{:?}: {}",
            args,
            stderr
        );
    }
    let (code, stdout, _) = run(&["lint", &dir, "--json"]);
    assert_eq!(code, Some(1));
    let report: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(report["code"], "target-unknown");
    assert_eq!(report["exit"], 1);
    // A command with no workspace still runs, and names the binary.
    let (code, stdout, _) = run(&["version"]);
    assert_eq!(code, Some(0));
    assert!(stdout.starts_with("graphos-factory "), "{}", stdout);
}
