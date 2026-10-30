//! export — render a validated subgraph for the user's GraphOS graph and
//! hand off to rover.
//!
//!   export [workspace] --out DIR [--base-url URL] [--json]
//!
//! A gate, a render and a hand-off. The gate refuses a workspace that is not
//! validated: no `.factory/evidence/latest.json`; evidence that is not for
//! the workspace as it is now (`evidence_current`: a file the layers read
//! changed, added or removed since, by the `inputs` digests `evidence`
//! recorded, with no git; for older evidence with no `inputs`, a `-dirty`
//! `commit`, a commit the workspace differs from, or a workspace outside
//! git); a core layer that is not
//! `pass` (`connector_unit` may be `not_run`, the zero-case run whose suites
//! cite their decisions, as the other target's gate allows); a `live` that
//! ran and failed; a selected operation an offline layer left `fail`,
//! `unchecked` or `skipped`, or with no executed evidence (no unit, e2e or
//! live `pass`: conformance executes nothing); a `supergraph_check` that
//! ran against the user's graph and failed, with rover's errors; or a
//! lint error now. There is no override. The render is the core's
//! `render_schema` with the production values (`--base-url`, else
//! `<SERVICE>_BASE_URL`, else `template.yaml`'s `test_default`; `AUTH_EXPR`
//! from `<SERVICE>_AUTH_EXPR`, else `test_default`), and refuses a base URL
//! that is not an absolute `http`/`https` URL with a host, carries userinfo
//! or is this machine, and an `AUTH_EXPR` that is not a static `{$env.NAME}`:
//! GraphOS stores the published schema. Neither a credential nor a URL's
//! userinfo is ever echoed. It writes `DIR/<directory>.graphql` only, never the
//! e2e router config, and refuses a `DIR` inside the workspace. The hand-off
//! names the two rover commands, the credential variables the router process
//! needs, an `override_url` snippet, the router minimum for the workspace's
//! `connect_spec` and the `federation_version` it composed at, and what this
//! export did not verify, each relationship field lint leaves not validated
//! among it.
//!
//! The binary never runs rover and never reads or asks for a GraphOS key:
//! `evidence` records `supergraph_check`, whose script runs rover with the
//! key the user exported.
//!
//! Exit codes: 0 written · 1 refused, or an error · 2 usage.

use graphos_factory_core::args::{Args, Flags};
use graphos_factory_core::json::{get, get_arr, get_obj, get_str};
use graphos_factory_core::lint::{lint_workspace, LintOptions};
use graphos_factory_core::render::{env_override_name, env_var_from_auth_expr, render_schema};
use graphos_factory_core::target::Gate;
use regex::Regex;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Component, Path, PathBuf};

/// The usage text: `export --help` prints it on stdout (ADR 0086).
pub const USAGE: &str =
    "usage: graphos-factory export [workspace] --out DIR [--base-url URL] [--json]
  Renders the validated schema with the production base URL into DIR/<directory>.graphql
  (DIR outside the workspace) and prints the rover commands to publish it. Refuses a
  workspace whose evidence is missing, not passing or not for the workspace as it is now
  (a file the layers read changed since), a base URL that is not an absolute http(s) URL,
  carries userinfo or is local, and an AUTH_EXPR that is not a static {$env.NAME}. Never
  runs rover.
  Exit codes: 0 written, 1 refused or an error, 2 usage.";

/// The command's row in the top-level usage.
pub const SUMMARY: &[&str] = &["[workspace] --out DIR [--base-url URL] [--json]   render the validated subgraph for GraphOS; print the rover hand-off"];

/// The flags this verb accepts; dispatch refuses any other (ADR 0097).
pub const FLAGS: Flags = Flags {
    boolean: &["json"],
    valued: &["out", "base-url"],
};

/// The core layers that must be `pass`, in `evidence`'s order.
const REQUIRED: [&str; 5] = [
    "compose",
    "connector_unit",
    "wiremock_e2e",
    "conformance",
    "lint",
];

/// The core layers that do not gate (`evidence`'s own `NON_GATING_LAYERS`):
/// reported when not `pass`, never a refusal.
const NON_GATING: [&str; 2] = ["write_body_proof", "json_accounting"];

/// The per-operation columns of the offline layers.
const OFFLINE_OPS: [&str; 3] = ["unit", "e2e", "conformance"];

/// The router a `connect_spec` needs (the Connectors version
/// requirements): the minimum, and for `v0.4` the release from which it
/// needs no opt-in. Composition is not a floor here: the hand-off prints
/// the workspace's own `federation_version`, the one the layers composed at.
pub fn minimums(connect_spec: &str) -> Option<(&'static str, Option<&'static str>)> {
    match connect_spec {
        "v0.4" => Some(("2.15.0", Some("2.16.0"))),
        "v0.3" => Some(("2.8.0", None)),
        "v0.2" => Some(("2.3.0", None)),
        "v0.1" => Some(("2.0.0", None)),
        _ => None,
    }
}

fn status<'a>(evidence: &'a Value, layer: &str) -> &'a str {
    get(evidence, "layers")
        .and_then(|l| get(l, layer))
        .and_then(|l| get_str(l, "status"))
        .unwrap_or("missing")
}

fn reason<'a>(row: Option<&'a Value>) -> Option<&'a str> {
    row.and_then(|r| get_str(r, "reason"))
}

/// The gate over `evidence` (`latest.json`'s shape), in the terms
/// `evidence` reports: one reason per required layer that is not `pass`
/// (bar a zero-case `not_run` connector_unit), `live: fail` when the live layer ran
/// and failed, then one per operation an offline layer left `fail`,
/// `unchecked` or `skipped`, and one per operation with no executed
/// evidence (the core's `has_executed_evidence`, which `evidence`'s own
/// report reads too), then `supergraph_check: fail` with each of its
/// findings when the check against the user's graph ran and failed.
pub fn gate(evidence: &Value) -> Gate {
    let mut reasons = Vec::new();
    for key in REQUIRED {
        let s = status(evidence, key);
        // Only the zero-case run (`cases` is 0) is the accepted `not_run`,
        // as the other target's gate has it. A unit layer `--skip` left out
        // or that never reached rover records no `cases` and is held
        // (ADR 0139).
        let zero_case = get(evidence, "layers")
            .and_then(|l| get(l, key))
            .and_then(|l| get(l, "cases"))
            .and_then(Value::as_u64)
            == Some(0);
        if s != "pass" && !(key == "connector_unit" && s == "not_run" && zero_case) {
            let why = reason(get(evidence, "layers").and_then(|l| get(l, key)))
                .map(|r| format!(" ({})", r))
                .unwrap_or_default();
            reasons.push(format!("{}: {}{}", key, s, why));
        }
    }
    if status(evidence, "live") == "fail" {
        reasons.push("live: fail".to_string());
    }
    for (op, columns) in get_obj(evidence, "operations").into_iter().flatten() {
        let note = get_str(columns, "note").unwrap_or("");
        for (column, value) in columns.as_object().into_iter().flatten() {
            let value = value.as_str().unwrap_or("");
            if OFFLINE_OPS.contains(&column.as_str())
                && matches!(value, "fail" | "unchecked" | "skipped")
            {
                let note = if column == "e2e" && !note.is_empty() {
                    format!(" ({})", note)
                } else {
                    String::new()
                };
                reasons.push(format!("{} {}: {}{}", op, column, value, note));
            }
        }
        if !graphos_factory_core::cmd::evidence::has_executed_evidence(columns) {
            reasons.push(format!("{}: no executed evidence at any layer", op));
        }
    }
    // A check against the user's own graph that ran and failed is a real
    // failure: the subgraph does not compose with it, or breaks what the
    // checks hold it to. Not run or skipped is listed, never refused.
    if let Some(row) = get_obj(evidence, "target_evidence_layers")
        .and_then(|t| t.get("supergraph_check"))
        .filter(|r| get_str(r, "status") == Some("fail"))
    {
        reasons.push(format!(
            "supergraph_check: fail ({})",
            reason(Some(row)).unwrap_or("no reason recorded")
        ));
        for f in get_arr(row, "findings")
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            reasons.push(format!("supergraph_check: {}", f));
        }
    }
    Gate {
        pass: reasons.is_empty(),
        reasons,
    }
}

/// The lint rules that leave a relationship field not validated, and what
/// each says is missing (`evidence`'s report names the same ones).
const LINK_RULES: [&str; 3] = [
    "link-untested",
    "link-null-untested",
    "link-live-unaccounted",
];

/// One row per relationship field a lint finding (`(rule, message)`, the
/// message opening with the field's `Type.field`) leaves not validated,
/// then one per field a live `exclusions:` entry names (`evidence`'s live
/// findings): `("link field Type.field", rule, why)`.
pub fn link_fields(evidence: &Value, lint: &[(String, String)]) -> Vec<(String, String, String)> {
    let mut out = Vec::new();
    for (rule, message) in lint {
        if !LINK_RULES.contains(&rule.as_str()) {
            continue;
        }
        let Some(coordinate) = message.split_whitespace().next() else {
            continue;
        };
        let why = match rule.as_str() {
            "link-untested" if message.contains("with neither it is not validated") => {
                "no unit entry and no e2e case"
            }
            "link-untested" if message.contains("with no unit entry") => "no unit entry",
            "link-untested" => "no e2e case",
            "link-null-untested" => "no null-parent e2e case",
            _ => "no live case and no live exclusion",
        };
        out.push((
            format!("link field {}", coordinate),
            rule.clone(),
            why.to_string(),
        ));
    }
    for f in get(evidence, "layers")
        .and_then(|l| get(l, "live"))
        .and_then(|l| get_arr(l, "findings"))
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
    {
        if let Some((coordinate, why)) = f
            .strip_prefix("EXCLUDED FIELD: ")
            .and_then(|r| r.split_once(" \u{2014} "))
        {
            out.push((
                format!("link field {}", coordinate),
                "live-excluded".to_string(),
                why.to_string(),
            ));
        }
    }
    out
}

/// What a passing gate still leaves unverified: a `not_run` connector_unit,
/// a non-gating layer that is not `pass`, a `live` that did not run, every
/// target layer that is not `pass` (`supergraph_check` when the user gave
/// no key, with its reason), every
/// relationship field `link_fields` names from `lint` (findings as
/// `(rule, message)`), and a GraphOS cloud router, which no layer runs.
pub fn not_verified(evidence: &Value, lint: &[(String, String)]) -> Vec<(String, String, String)> {
    let mut out = link_fields(evidence, lint);
    let layer = |key: &str| get(evidence, "layers").and_then(|l| get(l, key));
    let mut push = |name: &str, s: &str, why: String| {
        out.push((name.to_string(), s.to_string(), why));
    };
    if status(evidence, "connector_unit") == "not_run" {
        push(
            "connector_unit",
            "not_run",
            reason(layer("connector_unit"))
                .unwrap_or("no reason recorded")
                .to_string(),
        );
    }
    for key in NON_GATING {
        let s = status(evidence, key);
        if s != "pass" && s != "missing" {
            push(
                key,
                s,
                reason(layer(key)).unwrap_or("non-gating").to_string(),
            );
        }
    }
    let live = status(evidence, "live");
    if live != "pass" {
        push(
            "live",
            live,
            reason(layer("live"))
                .unwrap_or("the subgraph was not run against the real API")
                .to_string(),
        );
    }
    let targets = get_obj(evidence, "target_evidence_layers");
    let supergraph_row = targets.and_then(|t| t.get("supergraph_check"));
    let supergraph = supergraph_row
        .and_then(|r| get_str(r, "status"))
        .unwrap_or("missing");
    if supergraph != "pass" {
        push(
            "supergraph_check",
            supergraph,
            format!(
                "composition with your other subgraphs: {}",
                reason(supergraph_row).unwrap_or("no check against your graph was recorded")
            ),
        );
    }
    for (name, row) in targets.into_iter().flatten() {
        let s = get_str(row, "status").unwrap_or("missing");
        if name != "supergraph_check" && s != "pass" {
            push(
                name,
                s,
                reason(Some(row))
                    .unwrap_or("no reason recorded")
                    .to_string(),
            );
        }
    }
    let router = get(evidence, "toolchain")
        .and_then(|t| get_str(t, "router"))
        .map(|r| format!("router {}", r))
        .unwrap_or_else(|| "router".to_string());
    push(
        "graphos_cloud_router",
        "not_run",
        format!(
            "the e2e layer ran a self-hosted {}; no layer runs a GraphOS cloud router",
            router
        ),
    );
    out
}

/// Whether a base URL's host is this machine: `localhost` (or a
/// `.localhost` name), `host.docker.internal`, or a loopback or unspecified
/// address, an IPv4-mapped IPv6 one included. Not a URL: false
/// (`check_base_url` refuses it first).
pub fn is_local(base_url: &str) -> bool {
    let v4 = |ip: std::net::Ipv4Addr| ip.is_loopback() || ip.is_unspecified();
    match url::Url::parse(base_url)
        .ok()
        .and_then(|u| u.host().map(|h| h.to_owned()))
    {
        Some(url::Host::Domain(d)) => {
            let d = d.trim_end_matches('.').to_ascii_lowercase();
            d == "localhost" || d.ends_with(".localhost") || d == "host.docker.internal"
        }
        Some(url::Host::Ipv4(ip)) => v4(ip),
        Some(url::Host::Ipv6(ip)) => {
            ip.is_loopback() || ip.is_unspecified() || ip.to_ipv4_mapped().is_some_and(v4)
        }
        None => false,
    }
}

/// `value` with anything before an `@` in its authority replaced by
/// `<redacted>`: a base URL is echoed this way, never with its userinfo.
pub fn redact_userinfo(value: &str) -> String {
    let start = value.find("://").map_or(0, |i| i + 3);
    let end = value[start..]
        .find(['/', '?', '#'])
        .map_or(value.len(), |i| start + i);
    match value[start..end].rfind('@') {
        Some(at) => format!("{}<redacted>{}", &value[..start], &value[start + at..]),
        None => value.to_string(),
    }
}

/// Why a rendered base URL cannot be published, as a refusal code: not an
/// absolute `http`/`https` URL with a host (`base-url-invalid`), carrying
/// userinfo (`base-url-userinfo`), or this machine (`base-url-local`).
pub fn check_base_url(base_url: &str) -> Result<(), &'static str> {
    let parsed = match url::Url::parse(base_url) {
        Ok(u)
            if matches!(u.scheme(), "http" | "https")
                && u.host_str().is_some_and(|h| !h.is_empty()) =>
        {
            u
        }
        _ => return Err("base-url-invalid"),
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("base-url-userinfo");
    }
    if is_local(base_url) {
        return Err("base-url-local");
    }
    Ok(())
}

/// `git <args>` in `dir`: its trimmed stdout when it exits 0.
fn git(dir: &Path, args: &[&str]) -> Option<String> {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .ok()?;
    out.status
        .success()
        .then(|| String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// How `evidence_current` found the evidence current.
#[derive(Debug, PartialEq, Eq)]
pub enum Current {
    /// Every file of the recorded `inputs` hashes as it did: the digest,
    /// and how many files it covers.
    Inputs { digest: String, files: usize },
    /// Evidence with no `inputs` (written before they were recorded),
    /// checked by its `commit` against git.
    Commit,
}

/// A stale-evidence refusal: the message, and one reason per file.
#[derive(Debug)]
pub struct Stale {
    pub message: String,
    pub reasons: Vec<String>,
}

/// Whether `evidence` (`latest.json`) is for the workspace as it is now.
/// With `inputs`, the files the layers read are hashed again and compared,
/// with no git at all: each file changed, added to or removed from the
/// input set since is a reason. With `inputs_error` instead, `evidence`
/// could not hash a file, and that reason is the refusal. With neither, the
/// evidence predates input hashing and its `commit` is checked against git
/// (`commit_current`), and the message says so.
pub fn evidence_current(dir: &Path, evidence: &Value) -> Result<Current, Stale> {
    // `evidence` tried and could not hash a file: the commit fallback would
    // ask for a re-run that fails the same way, so the reason is the refusal.
    if let (None, Some(why)) = (get(evidence, "inputs"), get_str(evidence, "inputs_error")) {
        return Err(Stale {
            message: format!(
                "evidence could not hash its inputs ({}): fix that, then re-run evidence",
                why
            ),
            reasons: vec![format!("inputs not recorded: {}", why)],
        });
    }
    let Some(inputs) = get(evidence, "inputs") else {
        let recorded = get_str(evidence, "commit").unwrap_or("0000000");
        return commit_current(dir, recorded)
            .map(|()| Current::Commit)
            .map_err(|m| Stale {
                message: format!(
                    "the evidence predates input hashing, so export checks its commit against git: {}",
                    m
                ),
                reasons: Vec::new(),
            });
    };
    let changes = graphos_factory_core::provenance::evidence_input_changes(dir, inputs).map_err(
        |e| Stale {
            message: format!(
                "cannot compare the files the evidence ran against with the workspace ({}): re-run evidence",
                e
            ),
            reasons: Vec::new(),
        },
    )?;
    if changes.is_empty() {
        return Ok(Current::Inputs {
            digest: get_str(inputs, "digest").unwrap_or("").to_string(),
            files: get_obj(inputs, "files").map_or(0, |f| f.len()),
        });
    }
    let reasons: Vec<String> = changes
        .iter()
        .map(|c| format!("{} {} since evidence ran", c.path, c.change))
        .collect();
    const SHOWN: usize = 10;
    let mut shown = reasons[..reasons.len().min(SHOWN)].join("; ");
    if reasons.len() > SHOWN {
        shown.push_str(&format!(" (+{} more)", reasons.len() - SHOWN));
    }
    Err(Stale {
        message: format!("{}: re-run evidence", shown),
        reasons,
    })
}

/// Whether the evidence's `commit` (`recorded`, `latest.json`'s) is the
/// workspace as it is now, in the form `evidence` writes it: `git rev-parse
/// --short HEAD`, `-dirty` when the workspace, `.factory/evidence` aside,
/// had uncommitted changes. Current when the recorded commit is clean and
/// nothing in the workspace outside `.factory/evidence` differs from it,
/// committed or not: a commit that only adds the evidence keeps it current.
/// A recorded `-dirty` is never current (what it ran against is nowhere),
/// nor is a workspace outside git. The error is the refusal's message.
/// Only evidence with no `inputs` is checked this way.
pub fn commit_current(dir: &Path, recorded: &str) -> Result<(), String> {
    if git(dir, &["rev-parse", "--is-inside-work-tree"]).as_deref() != Some("true") {
        return Err(format!(
            "the workspace is not in a git repository, so export cannot tell what the evidence (recorded at {}) ran against: re-run evidence",
            recorded
        ));
    }
    let head = git(dir, &["rev-parse", "--short", "HEAD"]).unwrap_or_default();
    let dirty = git(
        dir,
        &[
            "status",
            "--porcelain",
            "--",
            ".",
            ":(exclude).factory/evidence",
        ],
    )
    .unwrap_or_default();
    let now = format!(
        "{}{}",
        if head.is_empty() { "0000000" } else { &head },
        if dirty.is_empty() { "" } else { "-dirty" }
    );
    let stale = || {
        Err(format!(
            "evidence was recorded at {}, the workspace is at {}: re-run evidence",
            recorded, now
        ))
    };
    // Only a hex commit reaches git: the value is file content.
    let hex = recorded.len() >= 4
        && recorded.len() <= 64
        && recorded.chars().all(|c| c.is_ascii_hexdigit());
    if !hex || !dirty.is_empty() {
        return stale();
    }
    let full = |rev: &str| {
        git(
            dir,
            &[
                "rev-parse",
                "--verify",
                "--quiet",
                &format!("{}^{{commit}}", rev),
            ],
        )
    };
    match (full(recorded), full("HEAD")) {
        (Some(r), Some(h)) if r == h => Ok(()),
        (Some(r), Some(_)) => {
            let unchanged = std::process::Command::new("git")
                .args([
                    "diff",
                    "--quiet",
                    &r,
                    "--",
                    ".",
                    ":(exclude).factory/evidence",
                ])
                .current_dir(dir)
                .stdin(std::process::Stdio::null())
                .status()
                .is_ok_and(|s| s.code() == Some(0));
            if unchanged {
                Ok(())
            } else {
                stale()
            }
        }
        _ => stale(),
    }
}

/// `path` made absolute and normalized without requiring it to exist: its
/// nearest existing ancestor canonicalized (symlinks resolved), the rest
/// appended, `.` and `..` folded lexically first.
fn absolute(path: &Path) -> PathBuf {
    let joined = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().unwrap_or_default().join(path)
    };
    let mut lexical = PathBuf::new();
    for c in joined.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                lexical.pop();
            }
            other => lexical.push(other.as_os_str()),
        }
    }
    let mut existing = lexical.clone();
    let mut rest = Vec::new();
    while !existing.exists() {
        match existing.file_name() {
            Some(name) => {
                rest.push(name.to_os_string());
                existing.pop();
            }
            None => break,
        }
    }
    let mut out = std::fs::canonicalize(&existing).unwrap_or(existing);
    for name in rest.iter().rev() {
        out.push(name);
    }
    out
}

/// A path as one shell word when it needs quoting, as it is otherwise.
fn shell_word(path: &str) -> String {
    if path
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "/._-+=:,@".contains(c))
    {
        path.to_string()
    } else {
        graphos_factory_core::render::sh_quote(path)
    }
}

struct Refusal {
    code: &'static str,
    message: String,
    reasons: Vec<String>,
    exit: i32,
}

fn refuse(code: &'static str, message: impl Into<String>) -> Refusal {
    Refusal {
        code,
        message: message.into(),
        reasons: Vec::new(),
        exit: 1,
    }
}

fn run(args: &Args) -> Result<Value, Refusal> {
    let out = args.get("out").ok_or(Refusal {
        code: "usage",
        message: USAGE.to_string(),
        reasons: Vec::new(),
        exit: 2,
    })?;
    let dir = PathBuf::from(args.dir());
    let ws_abs = std::fs::canonicalize(&dir)
        .map_err(|e| refuse("error", format!("{}: {}", dir.display(), e)))?;
    let workspace =
        graphos_factory_core::factory_io::read_to_string(&dir, ".factory/workspace.yaml")
            .map_err(String::from)
            .and_then(|t| graphos_factory_core::yaml::parse(&t))
            .map_err(|e| refuse("error", e))?;
    let service = get_str(&workspace, "service")
        .ok_or_else(|| refuse("error", "workspace.yaml has no service"))?
        .to_string();
    let directory = get_str(&workspace, "directory")
        .ok_or_else(|| refuse("error", "workspace.yaml has no directory"))?
        .to_string();
    let connect_spec = get_str(&workspace, "connect_spec")
        .unwrap_or("")
        .to_string();
    let federation_version = match get(&workspace, "federation_version") {
        Some(Value::String(v)) => Some(v.clone()),
        Some(Value::Number(n)) => Some(n.to_string()),
        _ => None,
    };

    // The rendered copy carries the production host, which the workspace
    // never records: inside it, it is an untracked file beside the schema
    // that the next `git add` commits.
    let out_abs = absolute(Path::new(out));
    if out_abs.starts_with(&ws_abs) {
        return Err(refuse(
            "out-inside-workspace",
            format!(
                "--out {} is inside the workspace {}: the rendered copy holds the production host, which the workspace does not record, and would sit there as an untracked file beside the schema; write it outside the workspace",
                out_abs.display(),
                ws_abs.display()
            ),
        ));
    }

    // The gate: evidence first, then lint on the files as they are now.
    let evidence_text = graphos_factory_core::factory_io::read_to_string_optional(
        &dir,
        ".factory/evidence/latest.json",
    )
    .map_err(|e| refuse("error", String::from(e)))?
    .ok_or_else(|| {
        refuse(
            "no-evidence",
            "no .factory/evidence/latest.json: the workspace has not been validated, so it is not exported; run `graphos-factory evidence` first",
        )
    })?;
    let evidence = graphos_factory_core::json::parse(&evidence_text)
        .map_err(|e| refuse("error", format!(".factory/evidence/latest.json: {}", e)))?;
    // The evidence is for the workspace as it is now, or it gates nothing.
    let recorded = get_str(&evidence, "commit")
        .unwrap_or("0000000")
        .to_string();
    let current = evidence_current(&dir, &evidence).map_err(|s| Refusal {
        code: "evidence-stale",
        message: s.message,
        reasons: s.reasons,
        exit: 1,
    })?;
    let mut reasons = gate(&evidence).reasons;
    let linted = lint_workspace(
        &dir,
        &LintOptions {
            schemas_dir: None,
            skip_evidence: false,
            target: graphos_factory_core::target::active(),
        },
    );
    for f in linted.findings.iter().filter(|f| f.severity == "error") {
        if f.rule == "auth-test-default" {
            // Its message quotes the test default, which may be the
            // credential itself: the rule and the file only.
            reasons.push(format!(
                "lint: [{}] {}: AUTH_EXPR.test_default must be a complete {{$env.NAME}} expression, got <redacted>",
                f.rule,
                f.file.as_deref().unwrap_or("template.yaml")
            ));
        } else {
            reasons.push(format!("lint: [{}] {}", f.rule, f.message));
        }
    }
    let lint_findings: Vec<(String, String)> = linted
        .findings
        .iter()
        .map(|f| (f.rule.clone(), f.message.clone()))
        .collect();
    if !reasons.is_empty() {
        return Err(Refusal {
            code: "not-validated",
            message: format!(
                "the workspace is not validated, so it is not exported: {}; fix it and re-run `graphos-factory evidence`",
                reasons.join("; ")
            ),
            reasons,
            exit: 1,
        });
    }

    // The render: production values in, the core's substitution.
    let template = dir.join("template.yaml");
    let variables: Vec<Value> = if template.exists() {
        get_arr(
            &graphos_factory_core::yaml::parse_file(&template).map_err(|e| refuse("error", e))?,
            "variables",
        )
        .cloned()
        .unwrap_or_default()
    } else {
        Vec::new()
    };
    let declared = |name: &str| variables.iter().find(|v| get_str(v, "name") == Some(name));
    let mut env: HashMap<String, String> = std::env::vars().collect();
    let base_env = env_override_name(&service, "BASE_URL");
    let auth_env = env_override_name(&service, "AUTH_EXPR");
    let from_env = |env: &HashMap<String, String>, name: &str| {
        env.get(name).filter(|v| !v.is_empty()).cloned()
    };
    let base_from = match args.get("base-url") {
        Some(url) => {
            if declared("BASE_URL").is_none() {
                return Err(refuse(
                    "error",
                    "template.yaml declares no BASE_URL, so --base-url has nothing to render",
                ));
            }
            env.insert(base_env.clone(), url.to_string());
            "--base-url".to_string()
        }
        None if from_env(&env, &base_env).is_some() => base_env.clone(),
        None => "template.yaml test_default".to_string(),
    };
    if let Some(auth) = declared("AUTH_EXPR") {
        let (value, source) = match from_env(&env, &auth_env) {
            Some(v) => (Some(v), auth_env.clone()),
            None => (
                get_str(auth, "test_default").map(str::to_string),
                "template.yaml test_default".to_string(),
            ),
        };
        if env_var_from_auth_expr(value.as_deref()).is_none() {
            // The value is never echoed: it may be the credential itself.
            return Err(refuse(
                "auth-expr-literal",
                format!(
                    "AUTH_EXPR (from {}) is not a static {{$env.NAME}} expression; GraphOS stores the published schema, so a literal credential would be published with it. Set {} to {{$env.NAME}} and give the router process NAME",
                    source, auth_env
                ),
            ));
        }
    }
    let (_, sdl) = graphos_factory_core::reconcile::read_schema_file(&dir, &workspace)
        .map_err(|e| refuse("error", e))?;
    let rendered = render_schema(&sdl, &variables, &service, &env)
        .map_err(|e| refuse("error", e))?
        .sdl;

    let sources = graphos_factory_core::graphql::directives(&rendered, "source");
    let name_re = Regex::new(r#"name\s*:\s*"([^"]*)""#).unwrap();
    let base_re = Regex::new(r#"baseURL\s*:\s*"([^"]*)""#).unwrap();
    let source_names: Vec<String> = sources
        .iter()
        .filter_map(|s| name_re.captures(&s.args).map(|m| m[1].to_string()))
        .collect();
    let base_urls: Vec<String> = sources
        .iter()
        .filter_map(|s| base_re.captures(&s.args).map(|m| m[1].to_string()))
        .collect();
    for url in &base_urls {
        let shown = redact_userinfo(url);
        let message = match check_base_url(url) {
            Ok(()) => continue,
            Err(code @ "base-url-invalid") => (code, format!(
                "the base URL {} (from {}) is not an absolute http or https URL with a host. Pass --base-url with the production host, for example https://api.example.com/v1 (or set {})",
                shown, base_from, base_env
            )),
            Err(code @ "base-url-userinfo") => (code, format!(
                "the base URL {} (from {}) carries userinfo: GraphOS stores the published schema, so a credential in the URL would be published with it. Pass the host without it and send the credential through AUTH_EXPR",
                shown, base_from
            )),
            Err(code) => (code, format!(
                "the base URL {} (from {}) is this machine: the test default is for the local layers. Pass --base-url with the production host (or set {})",
                shown, base_from, base_env
            )),
        };
        return Err(refuse(message.0, message.1));
    }
    let env_re = Regex::new(r"\$env\.([A-Za-z_][A-Za-z0-9_]*)").unwrap();
    let mut credentials: Vec<String> = Vec::new();
    for d in sources
        .iter()
        .chain(graphos_factory_core::graphql::directives(&rendered, "connect").iter())
    {
        for m in env_re.captures_iter(&d.args) {
            if !credentials.contains(&m[1].to_string()) {
                credentials.push(m[1].to_string());
            }
        }
    }

    // The write: DIR/<directory>.graphql, truncated in place.
    std::fs::create_dir_all(&out_abs)
        .map_err(|e| refuse("error", format!("{}: {}", out_abs.display(), e)))?;
    let file = out_abs.join(format!("{}.graphql", directory));
    if std::fs::symlink_metadata(&file).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(refuse(
            "error",
            format!(
                "{} is a symlink; refusing to write through it",
                file.display()
            ),
        ));
    }
    std::fs::write(&file, &rendered)
        .map_err(|e| refuse("error", format!("{}: {}", file.display(), e)))?;
    let file_s = file.display().to_string();
    let schema_arg = shell_word(&file_s);

    let keys: Vec<String> = source_names
        .iter()
        .map(|s| format!("{}.{}", directory, s))
        .collect();
    let mut router_yaml = String::from("connectors:\n  sources:\n");
    for key in &keys {
        router_yaml.push_str(&format!(
            "    {}:\n      override_url: \"${{env.{}}}\"\n",
            key, base_env
        ));
    }
    let (router_min, router_no_opt_in) = match minimums(&connect_spec) {
        Some((r, o)) => (Value::from(r), o.map_or(Value::Null, Value::from)),
        None => (Value::Null, Value::Null),
    };
    let passed: Vec<&str> = REQUIRED
        .iter()
        .copied()
        .filter(|k| status(&evidence, k) == "pass")
        .collect();
    let operations = get_obj(&evidence, "operations").map_or(0, |o| o.len());
    Ok(json!({
        "service": service,
        "subgraph": directory,
        "out": file_s,
        "base_url": base_urls.first(),
        "base_url_from": base_from,
        "connect_spec": connect_spec,
        "router_min": router_min,
        "router_no_opt_in_from": router_no_opt_in,
        "federation_version": federation_version,
        "evidence_commit": recorded,
        "evidence_checked": match current { Current::Inputs { .. } => "inputs", Current::Commit => "commit" },
        "evidence_digest": match &current {
            Current::Inputs { digest, .. } => Value::from(graphos_factory_core::provenance::short_digest(digest)),
            Current::Commit => Value::Null,
        },
        "evidence_files": match current { Current::Inputs { files, .. } => Value::from(files), Current::Commit => Value::Null },
        "credential_env": credentials,
        "sources": keys,
        "rover": {
            "check": format!("rover subgraph check <GRAPH_REF> --name {} --schema {}", directory, schema_arg),
            "publish": format!("rover subgraph publish <GRAPH_REF> --name {} --schema {} --routing-url http://localhost", directory, schema_arg),
        },
        "router_yaml": router_yaml,
        "gate": {"passed": passed, "operations": operations},
        "not_verified": not_verified(&evidence, &lint_findings)
            .into_iter()
            .map(|(layer, status, why)| json!({"layer": layer, "status": status, "reason": why}))
            .collect::<Vec<_>>(),
    }))
}

fn print_handoff(r: &Value) {
    let s = |k: &str| get_str(r, k).unwrap_or("");
    println!(
        "export: wrote {} ({} from {})",
        s("out"),
        s("base_url"),
        s("base_url_from")
    );
    let passed: Vec<&str> = get(r, "gate")
        .and_then(|g| get_arr(g, "passed"))
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    let against = match get_str(r, "evidence_digest") {
        Some(digest) => format!(
            "evidence inputs {} ({} files) unchanged since it ran",
            digest,
            get(r, "evidence_files")
                .and_then(Value::as_u64)
                .unwrap_or(0)
        ),
        None => format!(
            "evidence at {} (it predates input hashing; checked against git)",
            s("evidence_commit")
        ),
    };
    println!(
        "gate: {} pass; {} selected operation(s) with executed evidence; {}",
        passed.join(", "),
        get(r, "gate")
            .and_then(|g| get(g, "operations"))
            .and_then(Value::as_u64)
            .unwrap_or(0),
        against
    );
    println!("\nPublish it yourself; this binary never contacts GraphOS (rover uses your own credentials):");
    println!(
        "  {}",
        get(r, "rover")
            .and_then(|v| get_str(v, "check"))
            .unwrap_or("")
    );
    println!(
        "  {}",
        get(r, "rover")
            .and_then(|v| get_str(v, "publish"))
            .unwrap_or("")
    );
    println!(
        "  (a first publish needs --routing-url; a connectors-only subgraph is never called at it)"
    );
    let creds: Vec<&str> = get_arr(r, "credential_env")
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect();
    match (get_str(r, "router_min"), get_str(r, "router_no_opt_in_from")) {
        (Some(router), Some(from)) => println!(
            "\nRouter: connect/{} needs Apollo Router {} or later, with no opt-in from {}",
            s("connect_spec"),
            router,
            from
        ),
        (Some(router), None) => println!(
            "\nRouter: connect/{} needs Apollo Router {} or later",
            s("connect_spec"),
            router
        ),
        _ => println!(
            "\nRouter: connect_spec {:?} has no known minimum; check the Connectors version requirements",
            s("connect_spec")
        ),
    }
    match get_str(r, "federation_version") {
        Some(v) => println!("  Composition: validated at federation_version {}", v),
        None => println!("  Composition: workspace.yaml records no federation_version"),
    }
    if creds.is_empty() {
        println!("  The schema reads no $env credential.");
    } else {
        println!("  The router process needs: {}", creds.join(", "));
    }
    println!("  To move the host per environment, in the router's YAML:");
    for line in s("router_yaml").lines() {
        println!("    {}", line);
    }
    println!("\nNot verified by this export:");
    for row in get_arr(r, "not_verified").into_iter().flatten() {
        println!(
            "  {}: {} ({})",
            get_str(row, "layer").unwrap_or(""),
            get_str(row, "status").unwrap_or(""),
            get_str(row, "reason").unwrap_or("")
        );
    }
}

pub fn main(argv: &[String]) -> i32 {
    let args = Args::parse(argv, &FLAGS);
    match run(&args) {
        Ok(report) => {
            if args.has("json") {
                print!("{}", graphos_factory_core::json::pretty(&report));
            } else {
                print_handoff(&report);
            }
            0
        }
        Err(r) => {
            eprintln!("export: {}", r.message);
            if args.has("json") {
                print!(
                    "{}",
                    graphos_factory_core::json::pretty(&json!({
                        "error": r.message,
                        "code": r.code,
                        "reasons": r.reasons,
                        "exit": r.exit,
                    }))
                );
            }
            r.exit
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn passing() -> Value {
        json!({
            "toolchain": {"router": "2.17.0"},
            "layers": {
                "compose": {"status": "pass"},
                "connector_unit": {"status": "pass", "cases": 3},
                "wiremock_e2e": {"status": "pass"},
                "write_body_proof": {"status": "pass"},
                "conformance": {"status": "pass"},
                "lint": {"status": "pass"},
                "json_accounting": {"status": "pass"},
                "live": {"status": "not_run", "reason": "no credential"}
            },
            "operations": {
                "get:/a": {"unit": "pass", "e2e": "pass", "conformance": "pass", "live": "not_run"}
            },
            "target_evidence_layers": {
                "supergraph_check": {"status": "not_run", "reason": "not built", "target": "graphos-factory"}
            }
        })
    }

    #[test]
    fn a_passing_run_passes_and_names_what_it_did_not_verify() {
        assert_eq!(
            gate(&passing()),
            Gate {
                pass: true,
                reasons: vec![]
            }
        );
        let layers: Vec<String> = not_verified(&passing(), &[])
            .into_iter()
            .map(|(l, s, _)| format!("{}: {}", l, s))
            .collect();
        assert_eq!(
            layers,
            vec![
                "live: not_run",
                "supergraph_check: not_run",
                "graphos_cloud_router: not_run"
            ]
        );
    }

    #[test]
    fn a_layer_not_passing_is_named_with_its_reason_and_not_run_unit_passes() {
        let mut e = passing();
        e["layers"]["wiremock_e2e"] = json!({"status": "skipped", "reason": "java missing"});
        e["layers"]["connector_unit"] =
            json!({"status": "not_run", "reason": "D-0001", "cases": 0});
        e["layers"].as_object_mut().unwrap().remove("lint");
        e["layers"]["live"] = json!({"status": "fail", "reason": "401"});
        assert_eq!(
            gate(&e).reasons,
            vec![
                "wiremock_e2e: skipped (java missing)",
                "lint: missing",
                "live: fail"
            ]
        );
        assert!(not_verified(&e, &[])
            .iter()
            .any(|(l, s, w)| l == "connector_unit" && s == "not_run" && w == "D-0001"));
    }

    #[test]
    fn a_not_run_unit_layer_without_a_zero_case_count_is_held() {
        // Left out by `--skip unit`, or stopped before rover: no `cases`.
        let mut e = passing();
        e["layers"]["connector_unit"] = json!({
            "status": "not_run",
            "reason": graphos_factory_core::cmd::evidence::EXCLUDED_BY_FLAG
        });
        assert_eq!(
            gate(&e).reasons,
            vec!["connector_unit: not_run (excluded by --only/--skip)"]
        );
    }

    #[test]
    fn an_operation_without_executed_evidence_is_named() {
        let mut e = passing();
        e["operations"]["get:/b"] =
            json!({"unit": "not_run", "e2e": "n/a", "conformance": "n/a", "live": "not_run"});
        e["operations"]["get:/a"]["e2e"] = "unchecked".into();
        e["operations"]["get:/a"]["note"] = "404 not executed".into();
        // Conformance executes nothing: its pass alone is not evidence.
        e["operations"]["get:/c"] =
            json!({"unit": "n/a", "e2e": "n/a", "conformance": "pass", "live": "not_run"});
        assert_eq!(
            gate(&e).reasons,
            vec![
                "get:/a e2e: unchecked (404 not executed)",
                "get:/b: no executed evidence at any layer",
                "get:/c: no executed evidence at any layer"
            ]
        );
    }

    #[test]
    fn local_hosts_are_local() {
        for u in [
            "http://127.0.0.1:3000/api/v1",
            "http://localhost:8080",
            "http://LOCALHOST/x",
            "http://[::1]:80/",
            "http://0.0.0.0/",
            "http://127.1.2.3/",
            "http://api.localhost/",
            "http://0x7f000001/",
            "http://2130706433/",
            "http://localhost./",
        ] {
            assert!(is_local(u), "{}", u);
            assert_eq!(check_base_url(u), Err("base-url-local"), "{}", u);
        }
        for u in [
            "https://git.example.com/api/v1",
            "http://10.0.0.1/",
            "https://api.localhost-corp.com/v1",
        ] {
            assert!(!is_local(u), "{}", u);
            assert_eq!(check_base_url(u), Ok(()), "{}", u);
        }
    }

    #[test]
    fn a_base_url_without_a_scheme_is_invalid() {
        assert_eq!(
            check_base_url("localhost:3000/api"),
            Err("base-url-invalid")
        );
    }

    #[test]
    fn an_ip_and_port_without_a_scheme_is_invalid() {
        assert_eq!(
            check_base_url("127.0.0.1:3000/api"),
            Err("base-url-invalid")
        );
    }

    #[test]
    fn text_that_is_not_a_url_is_invalid() {
        assert_eq!(check_base_url("not a url"), Err("base-url-invalid"));
        assert!(!is_local("not a url"));
    }

    #[test]
    fn a_scheme_other_than_http_or_https_is_invalid() {
        assert_eq!(
            check_base_url("ftp://git.example.com/api"),
            Err("base-url-invalid")
        );
        assert_eq!(check_base_url("file:///srv/api"), Err("base-url-invalid"));
    }

    #[test]
    fn an_ipv4_mapped_loopback_is_local() {
        assert!(is_local("http://[::ffff:127.0.0.1]:3000/api/v1"));
        assert!(is_local("http://[::ffff:0.0.0.0]/"));
        assert!(!is_local("http://[::ffff:10.0.0.1]/"));
    }

    #[test]
    fn host_docker_internal_is_local() {
        assert!(is_local("http://host.docker.internal:3000/api/v1"));
        assert!(is_local("http://HOST.DOCKER.INTERNAL./"));
        assert_eq!(
            check_base_url("http://host.docker.internal:3000/api/v1"),
            Err("base-url-local")
        );
    }

    #[test]
    fn userinfo_is_refused_and_redacted() {
        assert_eq!(
            check_base_url("https://user:s3cret@git.example.com/api/v1"),
            Err("base-url-userinfo")
        );
        assert_eq!(
            check_base_url("https://token@git.example.com/"),
            Err("base-url-userinfo")
        );
        assert_eq!(
            redact_userinfo("https://user:s3cret@git.example.com/api/v1"),
            "https://<redacted>@git.example.com/api/v1"
        );
        // Not a URL, still never echoed.
        assert_eq!(redact_userinfo("user:s3cret@host/x"), "<redacted>@host/x");
        assert_eq!(
            redact_userinfo("https://git.example.com/a@b"),
            "https://git.example.com/a@b"
        );
    }

    #[test]
    fn minimums_follow_the_connect_spec() {
        assert_eq!(minimums("v0.4"), Some(("2.15.0", Some("2.16.0"))));
        assert_eq!(minimums("v0.3"), Some(("2.8.0", None)));
        assert_eq!(minimums("v0.2"), Some(("2.3.0", None)));
        assert_eq!(minimums("v0.1"), Some(("2.0.0", None)));
        assert_eq!(minimums("v9"), None);
    }

    #[test]
    fn link_fields_are_named_not_verified() {
        let lint = vec![
            ("link-untested".to_string(), "T.a is a relationship field with no unit entry and no e2e case; with neither it is not validated, whatever ...".to_string()),
            ("link-untested".to_string(), "T.b is a relationship field with no e2e case (a tests/cases/*.graphql ...); the field is half tested".to_string()),
            ("link-null-untested".to_string(), "T.c reads {$this.fk} and fk is nullable, ...".to_string()),
            ("link-live-unaccounted".to_string(), "T.d is a relationship field no live case selects ...".to_string()),
            ("pagination-bounds-unknown".to_string(), "get:/x: ...".to_string()),
        ];
        let mut e = passing();
        e["layers"]["live"]["findings"] =
            json!(["EXCLUDED FIELD: T.e \u{2014} the vendor has no sandbox"]);
        let rows: Vec<String> = not_verified(&e, &lint)
            .into_iter()
            .map(|(l, s, w)| format!("{}: {} ({})", l, s, w))
            .collect();
        assert_eq!(
            rows[..5],
            [
                "link field T.a: link-untested (no unit entry and no e2e case)",
                "link field T.b: link-untested (no e2e case)",
                "link field T.c: link-null-untested (no null-parent e2e case)",
                "link field T.d: link-live-unaccounted (no live case and no live exclusion)",
                "link field T.e: live-excluded (the vendor has no sandbox)",
            ]
        );
        // They are reported, never a refusal.
        assert!(gate(&e).pass);
    }
}
