//! evidence — run the validation stack and write .factory/evidence/latest.json.
//!
//!   evidence [workspace] --scripts DIR [--skip layer,layer] [--only layer,layer] [--schemas DIR]
//!
//! Layers, in order: compose, unit, e2e, serialization (recorded as
//! `write_body_proof`), conformance, lint, json_accounting, live. The four
//! shell wrappers come from --scripts (or $GRAPHOS_FACTORY_CORE_SCRIPTS); the
//! serialization, conformance, lint and json_accounting layers are this
//! binary's own code (serialization in-process, same-run, and non-gating).
//! Exit codes are mapped honestly: 0 pass · 1 fail · 3 not_run · 127 skipped
//! (tool missing). Nothing here can turn a non-zero exit into `pass`.
//! Before any layer runs, the files they read are hashed into `inputs`
//! (`provenance::evidence_inputs`), so a reader can tell, without git,
//! whether the evidence is for the workspace as it is now.

use crate::args::{Args, Flags};
use crate::json::{get, get_obj, get_str, obj, pretty, truthy};
use regex::Regex;
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Command;

// "serialization" runs after "e2e" (never before it): it reads the CURRENT
// run's own e2e layer status/log, already built earlier in this same loop,
// never `.factory/evidence/latest.json` (the same-run evidence contract,
// ADR 0079 Step 2).
const LAYERS: [&str; 8] = [
    "compose",
    "unit",
    "e2e",
    "serialization",
    "conformance",
    "lint",
    "json_accounting",
    "live",
];
const KEEP_RUNS: usize = 3;

fn evidence_key(layer: &str) -> &'static str {
    match layer {
        "compose" => "compose",
        "unit" => "connector_unit",
        "e2e" => "wiremock_e2e",
        "serialization" => "write_body_proof",
        "conformance" => "conformance",
        "lint" => "lint",
        "json_accounting" => "json_accounting",
        _ => "live",
    }
}

struct Run {
    code: i32,
    /// stdout then stderr: the layer's log and the lines its findings read.
    out: String,
    /// stdout alone: where a `--json` subcommand prints its summary. A
    /// failing `--check` prints its message to stderr after the JSON, so
    /// the summary is parsed from here, never from `out`.
    stdout: String,
}

fn run(cmd: &str, args: &[String]) -> Run {
    run_with(Command::new(cmd).args(args))
}

fn run_with(command: &mut Command) -> Run {
    let cmd = command.get_program().to_string_lossy().to_string();
    match command.env("NO_COLOR", "1").output() {
        Ok(o) => Run {
            code: o.status.code().unwrap_or(1),
            out: format!(
                "{}{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            ),
            stdout: String::from_utf8_lossy(&o.stdout).into_owned(),
        },
        Err(e) => Run {
            code: 1,
            out: format!("{}: {}\n", cmd, e),
            stdout: String::new(),
        },
    }
}

/// A `--json` subcommand's summary: the JSON object on its stdout.
fn json_summary(result: &Run) -> Option<Value> {
    let i = result.stdout.find('{')?;
    crate::json::parse(&result.stdout[i..]).ok()
}

/// `pass` when at least one obligation ran and passed and none failed,
/// `fail` when any obligation failed, `not_run` (exit 3) when no obligation
/// applied at all: every one is `Unexecuted` (no schema, selection or
/// inventory to check yet) or `NotApplicable` (no selected operation carries
/// anything this layer checks). "Nothing to check" is never recorded as a
/// pass. `unproven` is never folded into `pass` here either: an obligation
/// only reaches `ObligationStatus::Pass` once every one of its checks is
/// proven.
fn serialization_exit_code(report: &crate::request_serialization::Report) -> i32 {
    use crate::request_serialization::ObligationStatus;
    if report
        .obligations
        .iter()
        .any(|o| matches!(o.status, ObligationStatus::Fail))
    {
        1
    } else if report.obligations.iter().all(|o| {
        matches!(
            o.status,
            ObligationStatus::Unexecuted | ObligationStatus::NotApplicable
        )
    }) {
        3
    } else {
        0
    }
}

fn status_for(code: i32) -> &'static str {
    match code {
        0 => "pass",
        3 => "not_run",
        127 => "skipped",
        _ => "fail",
    }
}

fn set_op(per_op: &mut serde_json::Map<String, Value>, key: &str, layer: &str, status: &str) {
    if let Some(Value::Object(entry)) = per_op.get_mut(key) {
        let prev = entry.get(layer).and_then(Value::as_str).map(str::to_string);
        // fail outranks unchecked, which outranks pass: one unproven e2e
        // case leaves its operation unproven (ADR 0077).
        let next = match (prev.as_deref(), status) {
            (Some("fail"), _) => "fail",
            (Some("unchecked"), "pass") => "unchecked",
            _ => status,
        };
        entry.insert(layer.to_string(), Value::from(next));
    }
}

/// The flags this verb accepts; dispatch refuses any other (ADR 0097).
pub const FLAGS: Flags = Flags {
    boolean: &[],
    valued: &["scripts", "skip", "only", "schemas"],
};

pub fn main(argv: &[String]) -> i32 {
    let args = Args::parse(argv, &FLAGS);
    let dir = Path::new(&args.dir()).to_path_buf();
    let scripts = args
        .get("scripts")
        .map(PathBuf::from)
        .or_else(|| crate::env::var("SCRIPTS").map(PathBuf::from));
    let scripts = match scripts {
        Some(s) => s,
        None => {
            eprintln!("evidence: --scripts DIR (or $GRAPHOS_FACTORY_CORE_SCRIPTS) must point at the scripts/ directory, which holds compose.sh, unit.sh, e2e.sh and live.sh");
            return 1;
        }
    };
    // A layer is named by its LAYERS name or by the key latest.json and the
    // console print (`serialization` or `write_body_proof`, `unit` or
    // `connector_unit`); an unknown name is refused rather than silently
    // matching nothing.
    // The active target's layers run after the core's, named as they are
    // recorded under `target_evidence_layers`.
    let target = crate::target::active();
    let target_layer_names: Vec<&str> = target.evidence_layers.iter().map(|l| l.name).collect();
    let split = |name: &str| -> Result<Vec<String>, String> {
        args.get(name)
            .unwrap_or("")
            .split(',')
            .filter(|s| !s.is_empty())
            .map(|s| {
                LAYERS
                    .iter()
                    .find(|l| **l == s || evidence_key(l) == s)
                    .copied()
                    .or_else(|| target_layer_names.iter().copied().find(|l| *l == s))
                    .map(|l| l.to_string())
                    .ok_or_else(|| {
                        format!(
                            "--{} {:?}: unknown layer; one of {}",
                            name,
                            s,
                            LAYERS
                                .iter()
                                .map(|l| evidence_key(l))
                                .chain(target_layer_names.iter().copied())
                                .collect::<Vec<_>>()
                                .join(", ")
                        )
                    })
            })
            .collect()
    };
    let (skip, only) = match (split("skip"), split("only")) {
        (Ok(skip), Ok(only)) => (skip, only),
        (Err(e), _) | (_, Err(e)) => {
            eprintln!("evidence: {}", e);
            return 1;
        }
    };
    let this_exe = std::env::current_exe()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_else(|_| crate::bin_name().to_string());
    let dir_s = dir.to_string_lossy().to_string();

    let workspace = match crate::factory_io::read_to_string(&dir, ".factory/workspace.yaml")
        .map_err(String::from)
        .and_then(|t| crate::yaml::parse(&t).map_err(|e| format!(".factory/workspace.yaml: {}", e)))
    {
        Ok(w) => w,
        Err(e) => {
            eprintln!("evidence: {}", e);
            return 1;
        }
    };
    let selection =
        match crate::factory_io::read_to_string_optional(&dir, ".factory/selection.yaml") {
            Ok(Some(text)) => crate::yaml::parse(&text).unwrap_or(Value::Object(obj())),
            Ok(None) => Value::Object(obj()),
            Err(e) => {
                eprintln!("evidence: {}", e);
                return 1;
            }
        };
    // The inventory's documented error statuses are what the e2e layer's
    // per-status coverage is reconciled against (ADR 0077).
    let inventory =
        match crate::factory_io::read_to_string_optional(&dir, ".factory/inventory.json") {
            Ok(Some(text)) => crate::json::parse(&text).unwrap_or(Value::Object(obj())),
            _ => Value::Object(obj()),
        };
    let included: Vec<(String, Value)> = get_obj(&selection, "operations")
        .into_iter()
        .flatten()
        .filter(|(_, e)| truthy(get(e, "include")))
        .map(|(k, e)| (k.clone(), e.clone()))
        .collect();
    let prefix = get_str(&workspace, "field_prefix").unwrap_or("");
    let mut field_to_op: Vec<(String, String)> = Vec::new();
    let mut entity_ops: Vec<String> = Vec::new();
    for (key, entry) in &included {
        if let Some(name) = get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            field_to_op.push((format!("{}_{}", prefix, name), key.clone()));
        }
        if get(entry, "graphql")
            .and_then(|g| get(g, "entity"))
            .and_then(Value::as_bool)
            == Some(true)
        {
            entity_ops.push(key.clone());
        }
    }
    let ops_in_doc = |text: &str| -> Vec<String> {
        field_to_op
            .iter()
            .filter(|(f, _)| {
                Regex::new(&format!(r"\b{}\b", regex::escape(f)))
                    .unwrap()
                    .is_match(text)
            })
            .map(|(_, k)| k.clone())
            .collect()
    };

    // The commit label is read before any layer runs: the run writes
    // .factory/evidence/ (its log directory, latest.json), and a status taken
    // after that called every run `-dirty`. The evidence directory is also
    // excluded, so a previous run's uncommitted logs do not count either.
    let git_out = |a: &[&str]| {
        Command::new("git")
            .args(a)
            .current_dir(&dir)
            .output()
            .ok()
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .unwrap_or_default()
    };
    let head = git_out(&["rev-parse", "--short", "HEAD"]);
    let dirty = git_out(&[
        "status",
        "--porcelain",
        "--",
        ".",
        ":(exclude).factory/evidence",
    ]);
    let commit = format!(
        "{}{}",
        if head.is_empty() {
            "0000000".to_string()
        } else {
            head
        },
        if dirty.is_empty() { "" } else { "-dirty" }
    );
    // What the layers are about to read, hashed before they run, for the
    // same reason as the commit label. It needs no git. A file that cannot
    // be hashed (a link out of the workspace) leaves `inputs` out and
    // records why as `inputs_error`: the run still records its layers.
    let inputs = crate::provenance::evidence_inputs(&dir);

    let stamp = crate::now_iso();
    const RUNS_ROOT: &str = ".factory/evidence/runs";
    let run_rel = format!("{}/{}", RUNS_ROOT, stamp.replace([':', '.'], "-"));
    let run_dir = match crate::factory_io::create_dir_all(&dir, &run_rel) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("evidence: {}", e);
            return 1;
        }
    };
    let mut olds: Vec<String> = crate::factory_io::read_dir(&dir, RUNS_ROOT)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    olds.sort();
    if olds.len() > KEEP_RUNS {
        for old in &olds[..olds.len() - KEEP_RUNS] {
            let _ = crate::factory_io::remove_dir_all(&dir, &format!("{}/{}", RUNS_ROOT, old));
        }
    }

    let mut layers = obj();
    let mut per_op = obj();
    for (key, _) in &included {
        per_op.insert(key.clone(), Value::Object(obj()));
    }

    // The Apollo Router the e2e layer ran, from e2e.sh's banner line.
    let mut router_version: Option<String> = None;
    let router_re = Regex::new(r"Apollo Router ([\d.]+)").unwrap();

    for layer in LAYERS {
        let ev_key = evidence_key(layer);
        if skip.iter().any(|s| s == layer) || (!only.is_empty() && !only.iter().any(|s| s == layer))
        {
            layers.insert(
                ev_key.into(),
                crate::json::object(vec![
                    ("status", Value::from("skipped")),
                    ("reason", Value::from("skipped by flag")),
                ]),
            );
            continue;
        }
        // Gate the live wrapper on the declared context. An unrecorded
        // context_mode reads as generic, so only a recorded mode or a
        // companion file can hold the wrapper back (ADR 0081).
        let live_context = if layer == "live" {
            let report = crate::context::check(&dir, args.get("schemas").map(Path::new));
            if !report.errors.is_empty() {
                Some(Run {
                    code: 1,
                    out: format!(
                        "live: invalid customer context: {}\n",
                        report.errors.join("; ")
                    ),
                    stdout: String::new(),
                })
            } else if !report.live_ready {
                Some(Run {
                    code: 3,
                    out: format!(
                        "live: not_run — customer context: {}\n",
                        report
                            .blockers
                            .iter()
                            .map(|b| format!("{}: {}; next: {}", b.id, b.reason, b.resolve_with))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                    stdout: String::new(),
                })
            } else {
                None
            }
        } else {
            None
        };
        let (command, result) = if let Some(result) = live_context {
            (
                format!("{} context check {} --phase live", crate::bin_name(), dir_s),
                result,
            )
        } else {
            match layer {
                "compose" | "unit" | "e2e" | "live" => {
                    let script = scripts.join(format!("{}.sh", layer));
                    let mut command = Command::new("bash");
                    command.arg(&script).arg(&dir_s);
                    // The wrappers call back into the binary. Left to
                    // their own resolution they take the bootstrap cache
                    // before PATH, which can hold a far older binary than
                    // the one running this command (ADR 0087): hand them
                    // this one unless the caller named a binary.
                    if crate::env::is_unset("BIN") {
                        command.env(crate::env::name("BIN"), &this_exe);
                    }
                    (format!("{}.sh {}", layer, dir_s), run_with(&mut command))
                }
                "conformance" => (
                    format!("{} validate {} --json", crate::bin_name(), dir_s),
                    run(
                        &this_exe,
                        &["validate".into(), dir_s.clone(), "--json".into()],
                    ),
                ),
                "json_accounting" => (
                    format!(
                        "{} spans json-accounting {} --json --check",
                        crate::bin_name(),
                        dir_s
                    ),
                    run(
                        &this_exe,
                        &[
                            "spans".into(),
                            "json-accounting".into(),
                            dir_s.clone(),
                            "--json".into(),
                            "--check".into(),
                        ],
                    ),
                ),
                "serialization" => {
                    // In-process, same-run: the write-body-proof check reads
                    // this run's own e2e status/log -- already built above,
                    // in this same loop -- never `.factory/evidence/latest.json`
                    // (ADR 0079 Step 2). No subprocess, so its result is
                    // synthesized the same way "conformance" and "lint"
                    // already smuggle a JSON payload through `Run.out`.
                    let evidence_so_far =
                        crate::json::object(vec![("layers", Value::Object(layers.clone()))]);
                    let report = crate::request_serialization::report_with_evidence(
                        &dir,
                        Some(&evidence_so_far),
                    );
                    let code = serialization_exit_code(&report);
                    let stdout =
                        serde_json::to_string(&report).unwrap_or_else(|_| "{}".to_string());
                    // The not_run reason is the log's first line, ahead of the
                    // JSON, so the reason reader never quotes the report.
                    let out = if code == 3 {
                        format!(
                            "serialization: not_run: no obligation applies (no selected write, argument or list to prove)\n{}",
                            stdout
                        )
                    } else {
                        stdout.clone()
                    };
                    (
                        format!("{} serialization (in-process, same-run)", crate::bin_name()),
                        Run { code, stdout, out },
                    )
                }
                _ => {
                    // Lint's evidence rules read latest.json, which this run is
                    // about to rewrite; checking the previous run's file here
                    // would fail every apply that adds an operation.
                    let mut a = vec![
                        "lint".to_string(),
                        dir_s.clone(),
                        "--json".into(),
                        "--skip-evidence".into(),
                    ];
                    if let Some(s) = args.get("schemas") {
                        a.push("--schemas".into());
                        a.push(s.to_string());
                    }
                    (
                        format!(
                            "{} lint {} --json --skip-evidence",
                            crate::bin_name(),
                            dir_s
                        ),
                        run(&this_exe, &a),
                    )
                }
            }
        };
        let log_rel = format!("{}/{}.log", run_rel, layer);
        let log_file = run_dir.join(format!("{}.log", layer));
        let _ = crate::factory_io::write_in_place(&dir, &log_rel, result.out.as_bytes());
        let status = status_for(result.code);
        let mut entry = obj();
        entry.insert("status".into(), Value::from(status));
        entry.insert("command".into(), Value::from(command));
        entry.insert("exit_code".into(), Value::from(result.code));
        entry.insert(
            "log".into(),
            Value::from(crate::json::relative(&dir, &log_file)),
        );
        entry.insert("findings".into(), Value::Array(vec![]));
        let mut findings: Vec<String> = Vec::new();
        let lines: Vec<&str> = result.out.split('\n').collect();

        match layer {
            "compose" => {
                findings = lines
                    .iter()
                    .filter(|l| l.starts_with("compose: "))
                    .map(|l| l.to_string())
                    .collect();
            }
            "unit" => {
                if let Some(summary) = lines.iter().find(|l| l.starts_with("TEST RESULTS:")) {
                    findings.push(summary.trim().to_string());
                    if let Some(m) = Regex::new(r"(\d+)\s+passed;\s+(\d+)\s+failed")
                        .unwrap()
                        .captures(summary)
                    {
                        let p: u64 = m[1].parse().unwrap_or(0);
                        let f: u64 = m[2].parse().unwrap_or(0);
                        entry.insert("cases".into(), Value::from(p + f));
                        entry.insert("failed".into(), Value::from(f));
                    }
                }
                let header =
                    Regex::new(r"^TEST CASE: .* @([A-Za-z_]+)(?:\.([A-Za-z_]+))?").unwrap();
                let fail_re = Regex::new(r"^\[FAIL(URE)?\]").unwrap();
                let mut current: Option<String> = None;
                let mut targets: Vec<(String, &str)> = Vec::new();
                for l in &lines {
                    if let Some(h) = header.captures(l) {
                        let t = match h.get(2) {
                            Some(f) => f.as_str().to_string(),
                            None => format!("type:{}", &h[1]),
                        };
                        if !targets.iter().any(|(n, _)| *n == t) {
                            targets.push((t.clone(), "pass"));
                        }
                        current = Some(t);
                        continue;
                    }
                    if let Some(c) = &current {
                        if fail_re.is_match(l) {
                            if let Some(t) = targets.iter_mut().find(|(n, _)| n == c) {
                                t.1 = "fail";
                            }
                        }
                    }
                    if l.starts_with("FAILURES:") || l.starts_with("TEST RESULTS:") {
                        current = None;
                    }
                }
                for (target, verdict) in targets {
                    if target.starts_with("type:") {
                        for key in &entity_ops {
                            set_op(&mut per_op, key, "unit", verdict);
                        }
                    } else if let Some((_, key)) = field_to_op.iter().find(|(f, _)| *f == target) {
                        set_op(&mut per_op, key, "unit", verdict);
                    }
                }
            }
            "e2e" | "live" => {
                if layer == "e2e" {
                    router_version = lines
                        .iter()
                        .find_map(|l| router_re.captures(l).map(|m| m[1].to_string()));
                }
                let summary_re = Regex::new(&format!(r"^{}: \d+ passed", layer)).unwrap();
                if let Some(summary) = lines.iter().find(|l| summary_re.is_match(l)) {
                    findings.push(summary.trim().to_string());
                    if let Some(m) = Regex::new(r"(\d+) passed, (\d+) failed(?:, (\d+) unproven)?")
                        .unwrap()
                        .captures(summary)
                    {
                        let p: u64 = m[1].parse().unwrap_or(0);
                        let f: u64 = m[2].parse().unwrap_or(0);
                        let u: u64 = m.get(3).and_then(|u| u.as_str().parse().ok()).unwrap_or(0);
                        entry.insert("cases".into(), Value::from(p + f + u));
                        entry.insert("failed".into(), Value::from(f));
                    }
                } else {
                    let prefix_re = Regex::new(&format!(r"^{}: ", layer)).unwrap();
                    findings.extend(
                        lines
                            .iter()
                            .filter(|l| prefix_re.is_match(l))
                            .map(|l| l.trim().to_string()),
                    );
                }
                // `EXCLUDED: <operation> — <reason>`: an operation this layer
                // could not exercise, recorded as such rather than as an
                // anonymous n/a.
                let excl_re = Regex::new(r"^EXCLUDED: (\S+) — (.*)$").unwrap();
                let mut exclusions: Vec<Value> = Vec::new();
                for l in &lines {
                    if let Some(m) = excl_re.captures(l) {
                        set_op(&mut per_op, &m[1], layer, "excluded");
                        exclusions.push(crate::json::object(vec![
                            ("operation", Value::from(&m[1])),
                            ("reason", Value::from(m[2].trim())),
                        ]));
                    }
                }
                if !exclusions.is_empty() {
                    entry.insert("exclusions".into(), Value::Array(exclusions));
                }
                // `EXCLUDED FIELD: <Type>.<field> — <reason>` (ADR 0106): a
                // relationship field has no operations row, so its exclusion
                // is kept as the layer's finding, never as a status.
                findings.extend(
                    lines
                        .iter()
                        .filter(|l| l.starts_with("EXCLUDED FIELD: "))
                        .map(|l| l.trim().to_string()),
                );
                let case_dir =
                    dir.join("tests")
                        .join(if layer == "e2e" { "cases" } else { "live" });
                let case_re = Regex::new(r"^(PASS|FAIL|UNPROVEN): ([A-Za-z0-9_-]+)").unwrap();
                for l in &lines {
                    if let Some(m) = case_re.captures(l) {
                        let f = case_dir.join(format!("{}.graphql", &m[2]));
                        if let Ok(text) = std::fs::read_to_string(&f) {
                            for key in ops_in_doc(&text) {
                                set_op(
                                    &mut per_op,
                                    &key,
                                    layer,
                                    match &m[1] {
                                        "PASS" => "pass",
                                        "UNPROVEN" => "unchecked",
                                        _ => "fail",
                                    },
                                );
                            }
                        }
                    }
                }
                // Per-status error coverage (ADR 0077): `SERVED: <case> <op>
                // <code>` is a status a passing case saw answer that
                // operation's own request. A documented status no such line
                // covers is `not_run`, and the operation is `unchecked` for
                // it: a green operation must not hide an error nobody ran.
                if layer == "e2e" && matches!(status, "pass" | "fail") {
                    let served_re = Regex::new(r"^SERVED: \S+ (\S+) (\d{3})$").unwrap();
                    let served: Vec<(String, String)> = lines
                        .iter()
                        .filter_map(|l| {
                            served_re
                                .captures(l)
                                .map(|m| (m[1].to_string(), m[2].to_string()))
                        })
                        .collect();
                    let mut coverage = obj();
                    let mut gaps: Vec<String> = Vec::new();
                    let mut gap_count = 0;
                    for (key, _) in &included {
                        let documented: Vec<String> =
                            crate::json::get_arr(&inventory, "operations")
                                .into_iter()
                                .flatten()
                                .find(|o| get_str(o, "key") == Some(key.as_str()))
                                .map(crate::cmd::scaffold::documented_error_statuses)
                                .unwrap_or_default()
                                .into_iter()
                                .map(|(s, _)| s)
                                .collect();
                        if documented.is_empty() {
                            continue;
                        }
                        let (executed, not_run): (Vec<String>, Vec<String>) =
                            documented.iter().cloned().partition(|d| {
                                served.iter().any(|(k, c)| {
                                    k == key && crate::cmd::scaffold::status_covers(d, c)
                                })
                            });
                        let strings = |v: &[String]| {
                            Value::Array(v.iter().map(|s| Value::from(s.as_str())).collect())
                        };
                        coverage.insert(
                            key.clone(),
                            crate::json::object(vec![
                                ("documented", strings(&documented)),
                                ("executed", strings(&executed)),
                                ("not_run", strings(&not_run)),
                            ]),
                        );
                        if !not_run.is_empty() {
                            gap_count += not_run.len();
                            gaps.push(format!("{} ({})", key, not_run.join(", ")));
                            let current = per_op
                                .get(key)
                                .and_then(|e| get(e, "e2e"))
                                .and_then(Value::as_str)
                                .map(str::to_string);
                            if current.is_some() {
                                set_op(&mut per_op, key, "e2e", "unchecked");
                            }
                            if let Some(Value::Object(e)) = per_op.get_mut(key) {
                                e.insert(
                                    "note".into(),
                                    Value::from(format!(
                                        "documented error status(es) with no executed case: {}",
                                        not_run.join(", ")
                                    )),
                                );
                            }
                        }
                    }
                    if !coverage.is_empty() {
                        entry.insert("error_coverage".into(), Value::Object(coverage));
                    }
                    if !gaps.is_empty() {
                        findings.push(format!(
                            "e2e: error coverage incomplete — {} documented status(es) on {} operation(s) have no executed case: {}",
                            gap_count,
                            gaps.len(),
                            gaps.join("; ")
                        ));
                    }
                }
            }
            "conformance" => match json_summary(&result) {
                Some(json) => {
                    entry.insert(
                        "oracle".into(),
                        get(&json, "oracle").cloned().unwrap_or(Value::Null),
                    );
                    let counts = get(&json, "counts")
                        .cloned()
                        .unwrap_or(Value::Object(obj()));
                    let c = |k: &str| get(&counts, k).and_then(Value::as_u64).unwrap_or(0);
                    entry.insert(
                        "cases".into(),
                        Value::from(
                            c("pass") + c("fail") + c("unchecked") + c("unmatched") + c("waived"),
                        ),
                    );
                    // An unmatched body is a failure of the layer: a test
                    // asserts a request the spec does not describe.
                    entry.insert("failed".into(), Value::from(c("fail") + c("unmatched")));
                    findings.push(crate::cmd::validate::summary_line(&json));
                    // Per operation the verdicts are validate's own:
                    // pass / fail / unchecked / waived (an unmatched body that
                    // names its operation is already reported as fail).
                    for (key, verdict) in get_obj(&json, "operations").into_iter().flatten() {
                        set_op(
                            &mut per_op,
                            key,
                            "conformance",
                            verdict.as_str().unwrap_or(""),
                        );
                    }
                }
                None => findings.push("validate produced no JSON summary".to_string()),
            },
            "json_accounting" => match json_summary(&result) {
                Some(json) => {
                    findings = crate::json::get_arr(&json, "rows")
                        .into_iter()
                        .flatten()
                        .filter(|r| get_str(r, "status") != Some("accounted"))
                        .map(|r| {
                            format!(
                                "{} [{}.{}] reason={}",
                                get_str(r, "status").unwrap_or(""),
                                get_str(r, "type").unwrap_or(""),
                                get_str(r, "field").unwrap_or(""),
                                get_str(r, "reason").unwrap_or("none"),
                            )
                        })
                        .collect();
                    let mut by_type = crate::json::obj();
                    for t in crate::json::get_arr(&json, "by_type").into_iter().flatten() {
                        if let Some(name) = get_str(t, "type") {
                            by_type.insert(
                                name.to_string(),
                                crate::json::object(vec![
                                    (
                                        "accounted",
                                        Value::from(
                                            crate::json::get(t, "accounted")
                                                .and_then(Value::as_u64)
                                                .unwrap_or(0),
                                        ),
                                    ),
                                    (
                                        "unaccounted",
                                        Value::from(
                                            crate::json::get(t, "unaccounted")
                                                .and_then(Value::as_u64)
                                                .unwrap_or(0),
                                        ),
                                    ),
                                    (
                                        "stale",
                                        Value::from(
                                            crate::json::get(t, "stale")
                                                .and_then(Value::as_u64)
                                                .unwrap_or(0),
                                        ),
                                    ),
                                    (
                                        "recoverable",
                                        Value::from(
                                            crate::json::get(t, "recoverable")
                                                .and_then(Value::as_u64)
                                                .unwrap_or(0),
                                        ),
                                    ),
                                    (
                                        "unresolved",
                                        Value::from(
                                            crate::json::get(t, "unresolved")
                                                .and_then(Value::as_u64)
                                                .unwrap_or(0),
                                        ),
                                    ),
                                ]),
                            );
                        }
                    }
                    entry.insert(
                        "counts".into(),
                        crate::json::object(vec![
                            (
                                "total_fields",
                                Value::from(
                                    crate::json::get(&json, "total_fields")
                                        .and_then(Value::as_u64)
                                        .unwrap_or(0),
                                ),
                            ),
                            (
                                "json_fields",
                                Value::from(
                                    crate::json::get(&json, "json_fields")
                                        .and_then(Value::as_u64)
                                        .unwrap_or(0),
                                ),
                            ),
                            (
                                "accounted",
                                Value::from(
                                    crate::json::get(&json, "accounted")
                                        .and_then(Value::as_u64)
                                        .unwrap_or(0),
                                ),
                            ),
                            (
                                "unaccounted",
                                Value::from(
                                    crate::json::get(&json, "unaccounted")
                                        .and_then(Value::as_u64)
                                        .unwrap_or(0),
                                ),
                            ),
                            (
                                "stale",
                                Value::from(
                                    crate::json::get(&json, "stale")
                                        .and_then(Value::as_u64)
                                        .unwrap_or(0),
                                ),
                            ),
                            (
                                "recoverable",
                                Value::from(
                                    crate::json::get(&json, "recoverable")
                                        .and_then(Value::as_u64)
                                        .unwrap_or(0),
                                ),
                            ),
                            (
                                "unresolved",
                                Value::from(
                                    crate::json::get(&json, "unresolved")
                                        .and_then(Value::as_u64)
                                        .unwrap_or(0),
                                ),
                            ),
                            ("by_type", Value::Object(by_type)),
                        ]),
                    );
                }
                None => findings.push("json-accounting produced no JSON summary".to_string()),
            },
            "serialization" => match crate::json::parse(&result.out) {
                Ok(json) => {
                    let writes = crate::json::get_arr(&json, "writes").into_iter().flatten();
                    let (total, failed_writes) = writes.fold((0u64, 0u64), |(t, f), w| {
                        let proven = get(w, "body_proven").and_then(Value::as_bool) == Some(true);
                        (t + 1, f + u64::from(!proven))
                    });
                    entry.insert("cases".into(), Value::from(total));
                    entry.insert("failed".into(), Value::from(failed_writes));
                    // Writes and reads are summarised apart: a workspace with
                    // no write gap is not shown as failing for reads alone.
                    let count = |k: &str| get(&json, k).and_then(Value::as_u64).unwrap_or(0);
                    entry.insert("write_gaps".into(), Value::from(count("write_gaps")));
                    entry.insert("read_gaps".into(), Value::from(count("read_gaps")));
                    // Arguments the reader could not place and an executed
                    // case did (ADR 0079, executed placement): the rest were
                    // placed by the reader and are the default, not listed.
                    let executed: Vec<Value> = crate::json::get_arr(&json, "placements")
                        .into_iter()
                        .flatten()
                        .filter(|p| get_str(p, "via") == Some("executed"))
                        .cloned()
                        .collect();
                    if !executed.is_empty() {
                        entry.insert("placements".into(), Value::Array(executed));
                    }
                    findings = crate::json::get_arr(&json, "obligations")
                        .into_iter()
                        .flatten()
                        .map(|o| {
                            format!(
                                "{}  {}",
                                get_str(o, "status").unwrap_or(""),
                                get_str(o, "message").unwrap_or("")
                            )
                        })
                        .collect();
                    // This run's per-case verdicts land on the layer that
                    // actually executed the cases (wiremock_e2e), already
                    // built earlier in this same loop -- not on this
                    // layer's own entry (ADR 0079 Step 2's case_proofs).
                    if let Some(case_proofs) = get(&json, "case_proofs") {
                        if let Some(Value::Object(e2e_entry)) = layers.get_mut("wiremock_e2e") {
                            e2e_entry.insert("case_proofs".into(), case_proofs.clone());
                        }
                    }
                }
                Err(_) => findings.push("serialization produced no JSON summary".to_string()),
            },
            _ => match json_summary(&result) {
                Some(json) => {
                    findings = crate::json::get_arr(&json, "findings")
                        .into_iter()
                        .flatten()
                        .map(|f| {
                            format!(
                                "{} [{}] {}",
                                get_str(f, "severity").unwrap_or(""),
                                get_str(f, "rule").unwrap_or(""),
                                get_str(f, "message").unwrap_or("")
                            )
                        })
                        .collect();
                }
                None => findings.push("lint produced no JSON summary".to_string()),
            },
        }
        entry.insert(
            "findings".into(),
            Value::Array(findings.iter().map(|f| Value::from(f.as_str())).collect()),
        );

        if status != "pass" {
            let reason = match status {
                "not_run" => lines
                    .iter()
                    .find(|l| {
                        Regex::new(r"not_run|not set|nothing to run")
                            .unwrap()
                            .is_match(l)
                    })
                    .map(|l| l.trim().to_string())
                    .unwrap_or_else(|| "layer did not run".to_string()),
                "skipped" => lines
                    .iter()
                    .find(|l| Regex::new(r"not installed|not cached").unwrap().is_match(l))
                    .map(|l| l.trim().to_string())
                    .unwrap_or_else(|| "tool missing".to_string()),
                // unit.sh names why it failed where rover's summary cannot:
                // an uncited empty suite, a skipped case (ADR 0046).
                _ if layer == "unit" => lines
                    .iter()
                    .find(|l| l.starts_with("unit: FAIL"))
                    .map(|l| l.trim().to_string())
                    .unwrap_or_else(|| format!("exit {}", result.code)),
                _ if layer == "serialization" && entry.contains_key("write_gaps") => {
                    let n = |k: &str| entry.get(k).and_then(Value::as_u64).unwrap_or(0);
                    format!(
                        "{} write gap{}, {} read gap{} (exit {})",
                        n("write_gaps"),
                        if n("write_gaps") == 1 { "" } else { "s" },
                        n("read_gaps"),
                        if n("read_gaps") == 1 { "" } else { "s" },
                        result.code
                    )
                }
                _ => format!("exit {}", result.code),
            };
            entry.insert("reason".into(), Value::from(reason));
        }
        layers.insert(ev_key.into(), Value::Object(entry));
    }

    let keys: Vec<String> = per_op.keys().cloned().collect();
    for key in keys {
        for (layer, ev) in [
            ("unit", "connector_unit"),
            ("e2e", "wiremock_e2e"),
            ("conformance", "conformance"),
            ("live", "live"),
        ] {
            let has = per_op.get(&key).and_then(|e| get(e, layer)).is_some();
            if has {
                continue;
            }
            let st = layers
                .get(ev)
                .and_then(|l| get_str(l, "status"))
                .map(str::to_string);
            // A unit layer that ran no case at all (unit.sh's "no runnable
            // cases") is not_run as a layer, and applies to no operation.
            let ran_no_case = layers
                .get(ev)
                .and_then(|l| get(l, "cases"))
                .and_then(Value::as_u64)
                == Some(0);
            let fill = match st.as_deref() {
                Some("pass") | Some("fail") => "n/a".to_string(),
                Some("not_run") if ev == "connector_unit" && ran_no_case => "n/a".to_string(),
                Some(other) => other.to_string(),
                None => "not_run".to_string(),
            };
            if let Some(Value::Object(e)) = per_op.get_mut(&key) {
                e.insert(layer.to_string(), Value::from(fill));
            }
        }
    }

    // The target's layers, in its order, each reading the core layers' rows,
    // the per-operation table and the target layers already run. The same
    // statuses apply, and a layer the flags leave out is `skipped`.
    let mut target_layers = obj();
    for layer in target.evidence_layers {
        let selected = !skip.iter().any(|s| s == layer.name)
            && (only.is_empty() || only.iter().any(|s| s == layer.name));
        let mut row = if selected {
            let so_far = crate::json::object(vec![
                ("layers", Value::Object(layers.clone())),
                ("operations", Value::Object(per_op.clone())),
                (
                    "target_evidence_layers",
                    Value::Object(target_layers.clone()),
                ),
            ]);
            match (layer.run)(&crate::target::LayerInput {
                dir: &dir,
                evidence: &so_far,
                scripts: &scripts,
            }) {
                Value::Object(row) => row,
                _ => crate::json::object(vec![
                    ("status", Value::from("fail")),
                    ("reason", Value::from("the layer returned no row")),
                ])
                .as_object()
                .cloned()
                .unwrap_or_default(),
            }
        } else {
            crate::json::object(vec![
                ("status", Value::from("skipped")),
                ("reason", Value::from("skipped by flag")),
            ])
            .as_object()
            .cloned()
            .unwrap_or_default()
        };
        row.insert("target".into(), Value::from(target.name));
        target_layers.insert(layer.name.into(), Value::Object(row));
    }

    let rover = Command::new("rover")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| {
            Regex::new(r"Rover ([\d.]+)")
                .unwrap()
                .captures(&String::from_utf8_lossy(&o.stdout))
                .map(|m| m[1].to_string())
        })
        .unwrap_or_else(|| "unknown".to_string());

    // The router and JVM the e2e layer ran: the router from the banner e2e.sh
    // prints once everything is up (null when the run never got there), the
    // JVM from `java -version` whenever the layer executed at all.
    let router = router_version;
    let e2e_ran = matches!(
        layers
            .get("wiremock_e2e")
            .and_then(|l| get_str(l, "status")),
        Some("pass") | Some("fail")
    );
    let java = if e2e_ran {
        Command::new("java")
            .arg("-version")
            .output()
            .ok()
            .and_then(|o| {
                let text = format!(
                    "{}{}",
                    String::from_utf8_lossy(&o.stderr),
                    String::from_utf8_lossy(&o.stdout)
                );
                Regex::new(r#"version "([^"]+)""#)
                    .unwrap()
                    .captures(&text)
                    .map(|m| m[1].to_string())
            })
    } else {
        None
    };

    let mut evidence = crate::json::object(vec![
        ("contract_version", Value::from(2)),
        ("commit", Value::from(commit.as_str())),
        ("run_at", Value::from(stamp.as_str())),
        (
            "toolchain",
            crate::json::object(vec![
                ("rover", Value::from(rover)),
                (
                    "federation",
                    get(&workspace, "federation_version")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
                (
                    "connect_spec",
                    get(&workspace, "connect_spec")
                        .cloned()
                        .unwrap_or(Value::Null),
                ),
                (
                    "wiremock",
                    Value::from(
                        std::env::var("WIREMOCK_VERSION").unwrap_or_else(|_| "3.13.2".to_string()),
                    ),
                ),
                ("service_factory", Value::from(env!("CARGO_PKG_VERSION"))),
                ("router", router.map(Value::from).unwrap_or(Value::Null)),
                ("java", java.map(Value::from).unwrap_or(Value::Null)),
            ]),
        ),
        ("layers", Value::Object(layers.clone())),
        ("operations", Value::Object(per_op.clone())),
    ]);
    let inputs_line = match &inputs {
        Ok(recorded) => {
            crate::json::set(&mut evidence, "inputs", recorded.clone());
            format!(
                "inputs: {} over {} files",
                crate::provenance::short_digest(get_str(recorded, "digest").unwrap_or("")),
                get_obj(recorded, "files").map_or(0, |f| f.len())
            )
        }
        // Recorded, so a reader tells a run whose files could not be hashed
        // (re-running cannot help until the file is fixed) from evidence
        // written before inputs existed.
        Err(e) => {
            crate::json::set(&mut evidence, "inputs_error", Value::from(e.as_str()));
            format!("inputs: not recorded ({})", e)
        }
    };
    // Absent for a target with no layers, so the core's evidence is unchanged.
    if !target_layers.is_empty() {
        crate::json::set(
            &mut evidence,
            "target_evidence_layers",
            Value::Object(target_layers.clone()),
        );
    }
    let schemas_dir = args.get("schemas").map(PathBuf::from);
    if let Some(schema) = crate::schemas::load("evidence.schema.json", schemas_dir.as_deref()) {
        let errors = crate::jsonschema::validate(&evidence, &schema);
        if !errors.is_empty() {
            for e in &errors {
                eprintln!("  contract: {}", e);
            }
            eprintln!(
                "evidence: refusing to write a file that violates schemas/evidence.schema.json"
            );
            return 1;
        }
    }
    const LATEST: &str = ".factory/evidence/latest.json";
    let out = dir.join(LATEST);
    if let Err(e) = crate::factory_io::write_in_place(&dir, LATEST, pretty(&evidence).as_bytes()) {
        eprintln!("evidence: {}", e);
        return 1;
    }
    let cwd = std::env::current_dir().unwrap_or_default();
    println!(
        "evidence: {} @ {}, {}",
        crate::json::relative(&cwd, &out),
        commit,
        inputs_line
    );
    for (name, l) in layers.iter().chain(target_layers.iter()) {
        let status = get_str(l, "status").unwrap_or("");
        let reason = get_str(l, "reason")
            .map(|r| format!(" {}", r))
            .unwrap_or_default();
        let first = crate::json::get_arr(l, "findings")
            .and_then(|f| f.first())
            .and_then(Value::as_str)
            .map(|f| format!("  {}", f))
            .unwrap_or_default();
        println!("  {:<16} {:<8}{}{}", name, status, reason, first);
    }
    println!("{}", operations_line(&evidence));
    // A relationship field has no row in `operations` (ADR 0094): lint's
    // `link-untested` findings are the only record that one has no test, so
    // the report names them beside the unproven operations.
    let untested_links: Vec<String> = layers
        .get("lint")
        .and_then(|l| crate::json::get_arr(l, "findings"))
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .filter_map(|f| f.strip_prefix("warn [link-untested] "))
        .filter_map(|m| {
            let coordinate = m.split_whitespace().next()?;
            let what = if m.contains("with neither it is not validated") {
                "neither, not validated"
            } else if m.contains("with no unit entry") {
                "no unit entry"
            } else {
                "no e2e case"
            };
            Some(format!("{} ({})", coordinate, what))
        })
        .collect();
    if !untested_links.is_empty() {
        println!(
            "evidence: {} relationship field(s) with no unit entry or no e2e case (lint link-untested): {}",
            untested_links.len(),
            untested_links.join(", ")
        );
    }
    // A target layer fails the run only when the target says it gates.
    let target_failed = target.evidence_layers.iter().any(|l| {
        l.gating && target_layers.get(l.name).and_then(|r| get_str(r, "status")) == Some("fail")
    });
    let code = match gating_exit_code(&layers) {
        0 if target_failed => 1,
        code => code,
    };
    for line in spec_only_lines(&evidence, &dir) {
        println!("{}", line);
    }
    // Last, so the report ends on it; its absence is the only positive
    // signal the report gives.
    for line in not_validated_lines(&evidence) {
        println!("{}", line);
    }
    code
}

/// The per-operation columns of the layers that execute the connector:
/// rover's connector tests, the router against WireMock, the router against
/// the real API. Conformance compares the test fixtures with the spec and
/// executes nothing, so its `pass` is never executed evidence.
pub const EXECUTED_COLUMNS: [&str; 3] = ["unit", "e2e", "live"];

/// Whether an operation's row in `operations` holds executed evidence: a
/// `pass` in one of [`EXECUTED_COLUMNS`]. `evidence`'s report and a
/// target's export gate both read it, so the two never disagree.
pub fn has_executed_evidence(columns: &Value) -> bool {
    EXECUTED_COLUMNS
        .iter()
        .any(|c| get_str(columns, c) == Some("pass"))
}

/// The report's count line over `operations` (`latest.json`'s shape):
/// how many are selected, how many hold executed evidence, and how many an
/// e2e or conformance verdict left `unchecked`.
pub fn operations_line(evidence: &Value) -> String {
    let ops: Vec<&Value> = get_obj(evidence, "operations")
        .into_iter()
        .flatten()
        .map(|(_, v)| v)
        .collect();
    let unchecked = |column: &str| {
        ops.iter()
            .filter(|o| get_str(o, column) == Some("unchecked"))
            .count()
    };
    format!(
        "operations: {} selected; {} with executed evidence (unit, e2e or live pass); {} unchecked at e2e (an unproven case, or a documented status with no case), {} unchecked at conformance",
        ops.len(),
        ops.iter().filter(|o| has_executed_evidence(o)).count(),
        unchecked("e2e"),
        unchecked("conformance"),
    )
}

/// The first line `scaffold` writes into every case it drafts, and the
/// metadata key it writes into every stub. Removing the header does not
/// change the stub, so the two are counted apart.
const SCAFFOLD_CASE_HEADER: &str = "# scaffold:";
const SCAFFOLD_STUB_KEY: &str = "x-scaffold";
/// The metadata key `fixtures` writes into a stub built from a recording.
const RECORDED_STUB_KEY: &str = "x-recorded-from";

/// The report's lines on what the executed layers were measured against.
/// A unit or e2e pass against a stub built from the source document shows
/// the connector agrees with that document, not that the vendor answers the
/// same way: an error inside a 2xx body, another content type or a narrower
/// credential passes every such case. So the report counts, whatever the
/// layers said, the selected operations with no live pass and the e2e stubs
/// recorded from the real API; then the cases and stubs that are still
/// scaffold drafts. The stub count is per workspace: a recording is not yet
/// attributed to the operation it answers.
pub fn spec_only_lines(evidence: &Value, dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    let ops: Vec<&Value> = get_obj(evidence, "operations")
        .into_iter()
        .flatten()
        .map(|(_, v)| v)
        .collect();
    let no_live = ops
        .iter()
        .filter(|o| get_str(o, "live") != Some("pass"))
        .count();
    let files = |sub: &str, ext: &str| -> Vec<PathBuf> {
        let mut paths: Vec<PathBuf> = std::fs::read_dir(dir.join(sub))
            .into_iter()
            .flatten()
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().and_then(|e| e.to_str()) == Some(ext))
            .collect();
        paths.sort();
        paths
    };
    let cases = files("tests/cases", "graphql");
    let drafted_cases = cases
        .iter()
        .filter(|p| {
            std::fs::read_to_string(p)
                .map(|t| t.lines().any(|l| l.starts_with(SCAFFOLD_CASE_HEADER)))
                .unwrap_or(false)
        })
        .count();
    let stubs: Vec<Value> = files("tests/fixtures/mappings", "json")
        .iter()
        .filter_map(|p| std::fs::read_to_string(p).ok())
        .filter_map(|t| crate::json::parse(&t).ok())
        .collect();
    let stub_has = |key: &str| {
        stubs
            .iter()
            .filter(|s| get(s, "metadata").and_then(|m| get(m, key)).is_some())
            .count()
    };
    if no_live > 0 {
        out.push(format!(
            "spec only: {} of {} selected operation(s) have no live pass, and {} of {} e2e stub(s) were recorded from the real API (`fixtures`); a unit or e2e pass against a stub built from the source document shows the connector agrees with that document, not that the vendor answers the same way",
            no_live,
            ops.len(),
            stub_has(RECORDED_STUB_KEY),
            stubs.len(),
        ));
    }
    let drafted_stubs = stub_has(SCAFFOLD_STUB_KEY);
    if drafted_cases > 0 || drafted_stubs > 0 {
        out.push(format!(
            "drafts: {} of {} e2e case(s) still carry the `{}` header and {} of {} stub(s) still carry `metadata.{}`; a case whose header is gone may still answer from a drafted stub",
            drafted_cases,
            cases.len(),
            SCAFFOLD_CASE_HEADER,
            drafted_stubs,
            stubs.len(),
            SCAFFOLD_STUB_KEY,
        ));
    }
    out
}

/// The layers that execute anything, keyed as `layers` records them, each
/// with the name its own wrapper's reasons open with.
const EXECUTED_LAYERS: [(&str, &str); 4] = [
    ("compose", "compose"),
    ("connector_unit", "unit"),
    ("wiremock_e2e", "e2e"),
    ("live", "live"),
];

/// The report's closing `not validated:` lines over `latest.json`'s shape:
/// one when none of compose, connector_unit, wiremock_e2e and live ran
/// (every one `not_run` or `skipped`), with the first one's reason; one when a selected
/// operation holds no executed evidence, naming up to ten. Empty when
/// neither holds; nothing here ever claims a workspace validated.
pub fn not_validated_lines(evidence: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let layers = get(evidence, "layers");
    let rows: Vec<(&str, &str, Option<&Value>)> = EXECUTED_LAYERS
        .iter()
        .map(|(key, short)| (*key, *short, layers.and_then(|l| get(l, key))))
        .collect();
    let none_ran = rows.iter().all(|(_, _, row)| {
        matches!(
            row.and_then(|r| get_str(r, "status")).unwrap_or("not_run"),
            "not_run" | "skipped"
        )
    });
    if none_ran {
        let (key, short, row) = rows[0];
        let status = row.and_then(|r| get_str(r, "status")).unwrap_or("not_run");
        let reason = match row.and_then(|r| get_str(r, "reason")) {
            Some(r) if r.starts_with(&format!("{}:", short)) => r.to_string(),
            Some(r) => format!("{}: {}", key, r),
            None => format!("{}: {}", key, status),
        };
        out.push(format!("not validated: no executed layer ran ({})", reason));
    }
    let missing: Vec<&str> = get_obj(evidence, "operations")
        .into_iter()
        .flatten()
        .filter(|(_, columns)| !has_executed_evidence(columns))
        .map(|(key, _)| key.as_str())
        .collect();
    if !missing.is_empty() {
        const SHOWN: usize = 10;
        let mut named = missing
            .iter()
            .take(SHOWN)
            .copied()
            .collect::<Vec<_>>()
            .join(", ");
        if missing.len() > SHOWN {
            named.push_str(&format!(" and {} more", missing.len() - SHOWN));
        }
        out.push(format!(
            "not validated: {} selected operation(s) have no executed evidence: {}",
            missing.len(),
            named
        ));
    }
    out
}

/// `write_body_proof` is not part of the publication gate (ADR 0079 Step
/// 2): it is optional in `evidence.schema.json`, and a `fail` from it must
/// not fail this command's own exit code either -- CI's own "run evidence,
/// fail the job on a non-zero exit" step would otherwise turn every one of
/// its real, previously-invisible findings into a hard CI failure, which is
/// exactly the regression the ADR promised would not happen.
/// `json_accounting` is non-gating for the same reason (ADR 0073,
/// workspace-contract.md § The publication gate): none of the exported
/// workspaces measured for it is clean yet, so its `fail` is a tracked
/// finding, reported as it is, not a failed run.
const NON_GATING_LAYERS: [&str; 2] = ["write_body_proof", "json_accounting"];

fn gating_exit_code(layers: &serde_json::Map<String, Value>) -> i32 {
    // The rest of what leaves a relationship field not validated (ADR
    // 0106): lint's null-parent and live findings, and a live `field:`
    // exclusion, which is named with its reason and never as a pass.
    let lint_findings = |rule: &str| -> Vec<String> {
        let prefix = format!("warn [{}] ", rule);
        layers
            .get("lint")
            .and_then(|l| crate::json::get_arr(l, "findings"))
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .filter_map(|f| f.strip_prefix(prefix.as_str()))
            .filter_map(|m| m.split_whitespace().next().map(str::to_string))
            .collect()
    };
    // One entry per field, its reasons in the order found.
    let mut not_validated: Vec<(String, Vec<String>)> = Vec::new();
    let mut note = |coordinate: &str, why: String| match not_validated
        .iter_mut()
        .find(|(c, _)| c == coordinate)
    {
        Some((_, whys)) => whys.push(why),
        None => not_validated.push((coordinate.to_string(), vec![why])),
    };
    for coordinate in lint_findings("link-null-untested") {
        note(&coordinate, "no null-parent case".to_string());
    }
    for coordinate in lint_findings("link-live-unaccounted") {
        note(&coordinate, "no live case or exclusion".to_string());
    }
    for f in layers
        .get("live")
        .and_then(|l| crate::json::get_arr(l, "findings"))
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some((coordinate, reason)) = f
            .strip_prefix("EXCLUDED FIELD: ")
            .and_then(|r| r.split_once(" — "))
        {
            note(coordinate, format!("excluded live: {}", reason));
        }
    }
    if !not_validated.is_empty() {
        println!(
            "evidence: {} relationship field(s) not validated: {}",
            not_validated.len(),
            not_validated
                .iter()
                .map(|(c, whys)| format!("{} ({})", c, whys.join("; ")))
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let failed = layers
        .iter()
        .filter(|(key, _)| !NON_GATING_LAYERS.contains(&key.as_str()))
        .filter(|(_, l)| get_str(l, "status") == Some("fail"))
        .count();
    i32::from(failed > 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failing_non_gating_layer_alone_does_not_fail_the_run() {
        let layers = crate::json::object(vec![
            (
                "compose",
                crate::json::object(vec![("status", Value::from("pass"))]),
            ),
            (
                "write_body_proof",
                crate::json::object(vec![("status", Value::from("fail"))]),
            ),
        ]);
        assert_eq!(gating_exit_code(layers.as_object().unwrap()), 0);
    }

    /// Nothing to prove is `not_run`, never a pass: a report whose
    /// obligations are all not applicable or unexecuted exits 3, and one
    /// real pass among them makes it 0.
    #[test]
    fn serialization_with_nothing_to_prove_is_not_run() {
        use crate::request_serialization::{Obligation, ObligationStatus, Report};
        let ob = |status| Obligation {
            id: "serialization.x".into(),
            status,
            message: String::new(),
            evidence_ref: None,
            denominator: None,
            gaps: vec![],
        };
        let report = |statuses: Vec<ObligationStatus>| Report {
            writes: vec![],
            obligations: statuses.into_iter().map(ob).collect(),
            case_proofs: Default::default(),
            placements: vec![],
            write_gaps: 0,
            read_gaps: 0,
        };
        use ObligationStatus::*;
        assert_eq!(
            serialization_exit_code(&report(vec![NotApplicable, Unexecuted])),
            3
        );
        assert_eq!(
            serialization_exit_code(&report(vec![NotApplicable, Pass])),
            0
        );
        assert_eq!(serialization_exit_code(&report(vec![Pass, Fail])), 1);
    }

    /// ADR 0073: json_accounting's `fail` is a tracked finding, not a failed
    /// run (workspace-contract.md § The publication gate).
    #[test]
    fn a_failing_json_accounting_alone_does_not_fail_the_run() {
        let layers = crate::json::object(vec![
            (
                "compose",
                crate::json::object(vec![("status", Value::from("pass"))]),
            ),
            (
                "json_accounting",
                crate::json::object(vec![("status", Value::from("fail"))]),
            ),
        ]);
        assert_eq!(gating_exit_code(layers.as_object().unwrap()), 0);
    }

    #[test]
    fn a_failing_gating_layer_still_fails_the_run() {
        let layers = crate::json::object(vec![
            (
                "compose",
                crate::json::object(vec![("status", Value::from("pass"))]),
            ),
            (
                "lint",
                crate::json::object(vec![("status", Value::from("fail"))]),
            ),
            (
                "write_body_proof",
                crate::json::object(vec![("status", Value::from("fail"))]),
            ),
        ]);
        assert_eq!(gating_exit_code(layers.as_object().unwrap()), 1);
    }

    /// The Granola dry run (2026-09-29): a `--json --check` subcommand that
    /// fails prints its JSON on stdout, then its summary on stderr. Read
    /// from stdout and stderr together, the text after the closing brace
    /// made the parse fail and the layer read "produced no JSON summary".
    #[test]
    fn a_failing_check_summary_on_stderr_does_not_hide_the_json_on_stdout() {
        let r = run(
            "sh",
            &[
                "-c".to_string(),
                "printf '{\\n  \"findings\": []\\n}\\n'; echo '1 of 2 fields are not accounted for' >&2; exit 1"
                    .to_string(),
            ],
        );
        assert_eq!(r.code, 1);
        assert_eq!(
            json_summary(&r),
            Some(serde_json::json!({ "findings": [] })),
            "stdout: {:?}",
            r.stdout
        );
        // The stderr message is kept for the layer's log and findings.
        assert!(r.out.contains("1 of 2 fields are not accounted for"));
    }

    /// JSON only on stderr is not a summary: the subcommand's contract is
    /// its stdout.
    #[test]
    fn no_json_on_stdout_is_no_summary() {
        let r = run(
            "sh",
            &[
                "-c".to_string(),
                "echo '{\"on\": \"stderr\"}' >&2; exit 1".to_string(),
            ],
        );
        assert_eq!(json_summary(&r), None);
    }
}
