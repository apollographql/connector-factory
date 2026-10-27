//! codify — turn a detected hand edit into something the agent can carry.
//!
//!   codify [workspace-dir] --key K --reason R
//!          [--assert kind=value]… | --pin        # an override, verified by assertions (or pinned bytes)
//!          [--expressed]                         # no override: selection.yaml now expresses the edit
//!          [--decision D-id] [--context TEXT] [--until TEXT] [--expires YYYY-MM-DD]
//!          [--dry-run] [--json] [--model MODEL]
//!   codify [workspace-dir] --source PATH --reason R [--decision D-id] [--context TEXT] [--dry-run] [--json] [--model MODEL]
//!   codify [workspace-dir] --waive TARGET --status unchecked|unmatched --reason R
//!          [--decision D-id] [--context TEXT] [--until TEXT] [--expires YYYY-MM-DD] [--dry-run] [--json]
//!
//! `--waive` accepts a conformance gap (crate::waivers): TARGET is one body
//! (`tests/fixtures/mappings/<case>.json` or `tests/<suite>.connector.yaml#<entry>`)
//! or an operation key (every such body of that operation). `validate` must
//! currently report that target with that status — a waiver for a gap that
//! does not exist is refused — then the `waivers:` entry is written to
//! selection.yaml. Nothing in the lock changes: a waiver is about test
//! bodies, not the schema.
//!
//! `--source` codifies a hand edit to a pinned description document: the
//! structural difference between the vendor's upstream copy and the working
//! copy becomes the entry's `patches` in sources.lock.yaml (JSON Patch, each
//! operation with the reason, `--context` and `--decision` when given), and
//! the working copy's content hash is acknowledged in applied.lock.yaml.
//! Patches
//! already recorded keep their own reason when the same operation is still
//! present; operations no longer in the difference are dropped.
//!
//! What it does, in order: checks the span exists and every assertion holds
//! against its current text (an intent the text does not satisfy is
//! refused); writes or replaces the `overrides:` entry for the key in
//! selection.yaml (never touching the rest of the file); refreshes the key's
//! hash in applied.lock.yaml so the edit stops being reported as a hand
//! edit. With `--expressed` no override is written: the operation must
//! already reconcile without drift, and any previous override for it is
//! dropped because the selection now carries the intent.
//!
//! codify writes no decision (ADR 0113 §3). The entry carries the why:
//! `reason` (required) and `context` (`--context TEXT`, optional);
//! `--decision D-id` (numbered or random) attaches it to a real decision
//! the user or the agent recorded with `decisions add`, and codify warns
//! when no such decision is recorded. An `--expressed` codification leaves
//! no entry, so its `--context`, when given, becomes a finding (`source:
//! codify`).
//!
//! Exit codes: 0 done; 1 usage; 2 refused (no span, a span whose path is a
//! tie the selection does not settle (ADR 0044), failing assertion, the
//! selection does not express the edit, no lock, a file that is unreadable
//! or does not parse, selection.yaml included).

use crate::args::{Args, Flags};
use crate::decisions;
use crate::json::{get, get_arr, get_str};
use crate::spans::{check_assertion, spans, ASSERTION_KINDS};
use serde_json::Value;
use std::path::Path;

/// The selection codify edits in place — `.factory/`, so custody's (ADR 0025).
/// The findings log goes through `crate::findings`, which is custody's too.
const SELECTION: &str = ".factory/selection.yaml";

/// The usage text: `codify --help` prints it on stdout (ADR 0086).
pub const USAGE: &str = "usage: codify [workspace] --key K --reason R [--assert kind=value]… | --pin | --expressed [--decision D-id] [--context TEXT] [--until TEXT] [--expires YYYY-MM-DD] [--dry-run] [--json] [--model MODEL]\n       codify [workspace] --source PATH --reason R [--decision D-id] [--context TEXT] [--dry-run] [--json] [--model MODEL]\n       codify [workspace] --waive TARGET --status unchecked|unmatched --reason R [--decision D-id] [--context TEXT] [--until TEXT] [--expires YYYY-MM-DD] [--dry-run] [--json]";

fn usage(msg: &str) -> i32 {
    eprintln!("codify: {}", msg);
    eprintln!("{}", USAGE);
    1
}

/// Replace (or add, or remove when `entries` is empty) the top-level
/// `overrides:` block of a selection.yaml text, leaving every other line as
/// it was. The block runs from the `overrides:` line to the next line that
/// starts in column 0.
pub fn splice_overrides(text: &str, entries: &[Value]) -> String {
    splice_block(
        text,
        "overrides",
        entries,
        "# Hand edits codified by `graphos-factory-core codify`: the engineer's intent, checked\n# by every reconcile; the agent may change these spans as long as the assertions hold.\n",
    )
}

/// The same for `waivers:` (crate::waivers).
pub fn splice_waivers(text: &str, entries: &[Value]) -> String {
    splice_block(
        text,
        "waivers",
        entries,
        "# Conformance gaps accepted by `graphos-factory-core codify --waive`: bodies the oracle could\n# not judge (unchecked) or could not match (unmatched), each with its reason.\n",
    )
}

/// Replace, add or remove one top-level list block (`<key>:` to the next line
/// in column 0) of a selection.yaml text, leaving every other line as it was.
/// A new block is appended under `header` (a comment).
pub fn splice_block(text: &str, key: &str, entries: &[Value], header: &str) -> String {
    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    let start = lines.iter().position(|l| {
        let t = l.trim_end();
        t == format!("{}:", key)
            || t.starts_with(&format!("{}: ", key))
            || t.starts_with(&format!("{}:#", key))
    });
    let block = if entries.is_empty() {
        String::new()
    } else {
        format!(
            "{}:\n{}",
            key,
            crate::yaml::stringify(&Value::Array(entries.to_vec()), 2)
        )
    };
    match start {
        Some(s) => {
            let mut e = s + 1;
            while e < lines.len() {
                let l = lines[e];
                let first = l.chars().next();
                if !l.trim().is_empty() && first.map(|c| !c.is_whitespace()).unwrap_or(false) {
                    break;
                }
                e += 1;
            }
            // Blank lines at the end of the block stay with what follows.
            let mut end = e;
            while end > s + 1 && lines[end - 1].trim().is_empty() {
                end -= 1;
            }
            let mut out = String::new();
            out.push_str(&lines[..s].concat());
            out.push_str(&block);
            out.push_str(&lines[end..].concat());
            out
        }
        None => {
            if entries.is_empty() {
                return text.to_string();
            }
            let mut out = text.to_string();
            if !out.ends_with('\n') {
                out.push('\n');
            }
            if !out.ends_with("\n\n") {
                out.push('\n');
            }
            out.push_str(header);
            out.push_str(&block);
            out
        }
    }
}

fn schemas_dir(args: &Args) -> Option<&Path> {
    args.get("schemas").map(Path::new)
}

/// Warn (never fail) when `--decision` names an id no decision carries:
/// the entry points at a decision nobody wrote down. An unreadable log
/// warns too, since the id cannot be checked.
fn warn_unrecorded_decision(dir: &Path, args: &Args, id: &str, what: &str) {
    match decisions::load(dir, schemas_dir(args)) {
        Ok(doc) if decisions::find(&doc, id).is_some() => {}
        Ok(_) => eprintln!(
            "codify: warning: the decision log has no decision {} — {}",
            id, what
        ),
        Err(e) => eprintln!(
            "codify: warning: {} cannot be checked against the decision log ({}) — {}",
            id, e, what
        ),
    }
}

/// Refuse (exit 2) a spliced file that would not satisfy its schema: the
/// splice only edits text, so a `context` landing in a version-1 selection,
/// or any other contract break, is caught here before anything is written.
fn refuse_invalid(text: &str, schema: &str, file: &str) -> Option<i32> {
    let value = match crate::yaml::parse(text) {
        Ok(v) => v,
        Err(e) => {
            eprintln!(
                "codify: refusing to write {}: the edited text would not parse ({})",
                file, e
            );
            return Some(2);
        }
    };
    let Some(schema_doc) = crate::schemas::load(schema, None) else {
        eprintln!("codify: refusing to write {}: cannot load {}", file, schema);
        return Some(2);
    };
    let errors = crate::jsonschema::validate(&value, &schema_doc);
    if errors.is_empty() {
        return None;
    }
    eprintln!(
        "codify: refusing to write {}: the edited file would not satisfy {}; nothing was written:",
        file, schema
    );
    for e in &errors {
        eprintln!("  {}{}", file, e);
    }
    Some(2)
}

/// `--context TEXT`, trimmed; `None` when absent or blank.
fn context_arg(args: &Args) -> Option<String> {
    args.get("context")
        .map(str::trim)
        .filter(|c| !c.is_empty())
        .map(str::to_string)
}

/// The finding an `--expressed` codification with `--context` records
/// (ADR 0113 §3): no override is left to carry the context, so the fact
/// that the selection now expresses the edit, and why, is a finding.
pub fn expressed_finding(
    key: &str,
    context: &str,
    reason: &str,
    decision: Option<&str>,
    schema_file: &str,
) -> crate::findings::NewFinding {
    crate::findings::NewFinding {
        title: format!("Hand edit expressed by the selection: {}", key),
        date: crate::today(),
        body: format!(
            "{}\n\n{} selection.yaml now expresses the edit to {} in {}, so there is no override: the next apply reproduces it from the selection.",
            context.trim(),
            reason.trim(),
            key,
            schema_file
        ),
        source: "codify".into(),
        affects: vec![key.to_string()],
        related: decision.map(|d| vec![d.to_string()]).unwrap_or_default(),
        ..Default::default()
    }
}

/// The flags this verb accepts; dispatch refuses any other (ADR 0097).
pub const FLAGS: Flags = Flags {
    boolean: &["expressed", "pin", "dry-run", "json"],
    valued: &[
        "key", "reason", "assert", "decision", "model", "source", "waive", "status", "until",
        "expires", "context", "schemas",
    ],
};

pub fn main(argv: &[String]) -> i32 {
    let args = Args::parse(argv, &FLAGS);
    let dir = Path::new(&args.dir()).to_path_buf();
    if let Some(source) = args.get("source") {
        let rel = source.trim_start_matches("./");
        if !crate::sources::inside_workspace(rel) {
            eprintln!(
                "codify: {} is not inside the workspace — a pinned document's working copy is committed with the workspace, so --source is relative to its root and never climbs out of it",
                rel
            );
            return 2;
        }
        return codify_source(&args, &dir, rel);
    }
    if let Some(target) = args.get("waive") {
        return codify_waive(&args, &dir, target);
    }
    let key = match args.get("key") {
        Some(k) => k.to_string(),
        None => return usage("--key is required"),
    };
    let reason = match args.get("reason").map(str::trim) {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => return usage("--reason is required (why the edit was made; it becomes the override's reason and the decision)"),
    };
    let expressed = args.has("expressed");
    let pinned = args.has("pin");
    let dry_run = args.has("dry-run");
    let mut assertions: Vec<(String, String)> = Vec::new();
    for a in args.all("assert") {
        match a.split_once('=') {
            Some((k, v)) if ASSERTION_KINDS.contains(&k.trim()) => {
                assertions.push((k.trim().to_string(), v.to_string()))
            }
            Some((k, _)) => {
                return usage(&format!(
                    "unknown assertion kind {:?}; one of: {}",
                    k,
                    ASSERTION_KINDS.join(", ")
                ))
            }
            None => return usage(&format!("--assert wants kind=value, got {:?}", a)),
        }
    }
    if expressed && (!assertions.is_empty() || pinned) {
        return usage("--expressed records no override; drop --assert/--pin");
    }
    if !expressed && assertions.is_empty() && !pinned {
        return usage("give at least one --assert kind=value (what must stay true), or --pin to freeze the bytes, or --expressed when selection.yaml already carries the edit");
    }
    if let Some(d) = args.get("decision") {
        if !crate::record_log::is_decision_id(d) {
            return usage(&format!(
                "--decision must look like D-0019 or D-k7m2qx, got {:?}",
                d
            ));
        }
    }
    if let Some(e) = args.get("expires") {
        if !regex::Regex::new(r"^\d{4}-\d{2}-\d{2}$")
            .unwrap()
            .is_match(e)
        {
            return usage(&format!("--expires must be YYYY-MM-DD, got {:?}", e));
        }
    }

    // Load the workspace.
    let loaded = match crate::cmd::lock::load(&dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("codify: {}", e);
            return 2;
        }
    };
    let selection_text = match crate::factory_io::read_to_string(&dir, SELECTION) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("codify: {}", e);
            return 2;
        }
    };
    let selection = match crate::yaml::parse(&selection_text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("codify: {}: {}", SELECTION, e);
            return 2;
        }
    };
    // An `--expressed` codification with `--context` records a finding
    // (ADR 0113 §3): read findings.json first, so an unreadable one refuses
    // before anything is written.
    let context = context_arg(&args);
    let mut findings_doc = if expressed && context.is_some() {
        match crate::findings::load(&dir, schemas_dir(&args)) {
            Ok(d) => Some(d),
            Err(e) => {
                eprintln!("codify: {}", e);
                return 2;
            }
        }
    } else {
        None
    };
    let lock = match crate::spans::read_lock(&dir) {
        Ok(Some(l)) => l,
        Ok(None) => {
            eprintln!(
                "codify: no {} — run `graphos-factory-core lock` first (it acknowledges the schema as it stands; codify then records what changes after that)",
                crate::spans::LOCK_FILE
            );
            return 2;
        }
        Err(e) => {
            eprintln!("codify: {}", e);
            return 2;
        }
    };

    let current = spans(&loaded.sdl, loaded.inventory.as_ref(), &loaded.hints);
    let span = match current.iter().find(|s| s.key == key) {
        Some(s) => s,
        None => {
            let near: Vec<&str> = current
                .iter()
                .filter(|s| {
                    s.key
                        .to_lowercase()
                        .contains(&key.to_lowercase().replace("type:", ""))
                })
                .map(|s| s.key.as_str())
                .take(5)
                .collect();
            eprintln!(
                "codify: the schema has no span {:?}{}",
                key,
                if near.is_empty() {
                    String::new()
                } else {
                    format!(" (did you mean: {})", near.join(", "))
                }
            );
            return 2;
        }
    };
    // A span keyed `Query.<f>` only because its path is a tie the selection
    // does not settle: `lock` refuses to record it, and so does codify.
    if let Some(e) = &span.unattributed {
        eprintln!("codify: refused — {}: {}", key, e);
        eprintln!("  {}", crate::spans::SETTLE_TIE);
        return 2;
    }
    if key.contains(":/") {
        let known = loaded
            .inventory
            .as_ref()
            .and_then(|inv| get_arr(inv, "operations"))
            .map(|ops| ops.iter().any(|o| get_str(o, "key") == Some(&key)))
            .unwrap_or(false);
        if !known {
            eprintln!("codify: {} is not in inventory.json", key);
            return 2;
        }
    }

    // `--key` codifies a hand edit. When the span still hashes to what the
    // lock recorded there is no edit, and the decision codify would append
    // ("the engineer edited … by hand; reconcile reported it") would be
    // false. Amending an override the key already has is the one exception:
    // the engineer is rewording its reason or its assertions, not recording
    // a new edit.
    let in_sync = crate::json::get_obj(&lock, "spans")
        .and_then(|spans| spans.get(&key))
        .and_then(Value::as_str)
        == Some(span.sha256.as_str());
    let has_override = crate::reconcile::read_overrides(&selection)
        .iter()
        .any(|o| o.key == key);
    if in_sync && !has_override {
        eprintln!(
            "codify: refused — {} is in sync with {}: there is no hand edit to codify.",
            key,
            crate::spans::LOCK_FILE
        );
        eprintln!("  `--key` records an edit `graphos-factory-core reconcile` or `lock --check` reported. Nothing here differs from what the agent last wrote.");
        eprintln!("  A correction to what the tool *inferred* is a different thing: a wrong shape or response belongs in the pinned description document (`graphos-factory-core codify --source PATH --reason …`), and a judgement — which root property the payload sits under, the root, a name, a tag — belongs in selection.yaml.");
        return 2;
    }

    // Every assertion must hold now.
    let mut failing: Vec<String> = Vec::new();
    for (k, v) in &assertions {
        match check_assertion(k, v, span) {
            Ok(true) => {}
            Ok(false) => failing.push(format!(
                "{} {}",
                k,
                serde_json::to_string(v).unwrap_or_default()
            )),
            Err(e) => failing.push(e),
        }
    }
    if !failing.is_empty() {
        eprintln!(
            "codify: refused — {} assertion{} do{} not hold against the current text of {}:",
            failing.len(),
            if failing.len() == 1 { "" } else { "s" },
            if failing.len() == 1 { "es" } else { "" },
            key
        );
        for f in &failing {
            eprintln!("  FAIL {}", f);
        }
        return 2;
    }

    // --expressed: the selection must already agree with the schema here.
    if expressed && key.contains(":/") {
        let report = match crate::reconcile::reconcile_workspace(&dir, None) {
            Ok(r) => r,
            Err(e) => {
                eprintln!("codify: reconcile: {}", e);
                return 2;
            }
        };
        let mut problems: Vec<String> = Vec::new();
        for section in ["add", "remove", "change"] {
            for item in get_arr(&report, section).into_iter().flatten() {
                if get_str(item, "key") == Some(&key) {
                    let drift: Vec<String> = get_arr(item, "drift")
                        .into_iter()
                        .flatten()
                        .filter_map(|d| get_str(d, "message"))
                        .map(str::to_string)
                        .collect();
                    problems.push(format!(
                        "{}: {}",
                        section,
                        if drift.is_empty() {
                            "the operation is in this section".to_string()
                        } else {
                            drift.join("; ")
                        }
                    ));
                }
            }
        }
        if !problems.is_empty() {
            eprintln!(
                "codify: refused — selection.yaml does not express the edit to {} yet:",
                key
            );
            for p in &problems {
                eprintln!("  {}", p);
            }
            eprintln!("  change the selection (tags, rename, exclude, root, …) until reconcile shows no drift for it, then run codify --expressed again");
            return 2;
        }
    }

    // The override entry: the why is its `reason` and `context`; a
    // `decision:` only when `--decision` names a real decision (ADR 0113 §3).
    let mut entries: Vec<Value> = get_arr(&selection, "overrides")
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|o| get_str(o, "key") != Some(&key))
        .collect();
    let decision_id = args.get("decision").map(str::to_string);
    if !expressed {
        let mut o = crate::json::object(vec![
            ("key", Value::from(key.as_str())),
            ("reason", Value::from(reason.as_str())),
        ]);
        if let Some(d) = &decision_id {
            crate::json::set(&mut o, "decision", Value::from(d.as_str()));
        }
        if let Some(c) = &context {
            crate::json::set(&mut o, "context", Value::from(c.as_str()));
        }
        if !assertions.is_empty() {
            let list: Vec<Value> = assertions
                .iter()
                .map(|(k, v)| crate::json::object(vec![(k.as_str(), Value::from(v.as_str()))]))
                .collect();
            crate::json::set(&mut o, "assert", Value::Array(list));
        }
        if let Some(u) = args.get("until") {
            crate::json::set(&mut o, "until", Value::from(u));
        }
        if let Some(e) = args.get("expires") {
            crate::json::set(&mut o, "expires", Value::from(e));
        }
        entries.push(o);
    }
    let mut new_selection = splice_overrides(&selection_text, &entries);
    if context.is_some() && !expressed {
        // An entry with `context` is a version-2 selection (ADR 0113).
        new_selection = crate::split::bump_selection_version(&new_selection);
    }
    if let Some(code) = refuse_invalid(&new_selection, "selection.schema.json", SELECTION) {
        return code;
    }
    if let Some(d) = &decision_id {
        warn_unrecorded_decision(
            &dir,
            &args,
            d,
            if expressed {
                "the finding relates to a decision that is not recorded"
            } else {
                "the override references a decision that is not recorded"
            },
        );
    }

    // The finding an `--expressed` codification with `--context` leaves.
    let mut finding_id: Option<String> = None;
    if let (Some(doc), Some(c)) = (findings_doc.as_mut(), &context) {
        let new = expressed_finding(
            &key,
            c,
            &reason,
            decision_id.as_deref(),
            &loaded.schema_file,
        );
        match crate::findings::add(doc, new) {
            Ok(id) => finding_id = Some(id),
            Err(e) => {
                eprintln!("codify: {}", e);
                return 2;
            }
        }
    }

    // The lock: this span is now acknowledged.
    let mut new_lock = lock.clone();
    if let Some(Value::Object(map)) = new_lock.get_mut("spans") {
        map.insert(key.clone(), Value::from(span.sha256.as_str()));
    }
    crate::json::set(&mut new_lock, "written_at", Value::from(crate::now_iso()));
    crate::json::set(
        &mut new_lock,
        "written_by",
        Value::from(crate::written_by()),
    );

    let mut summary = crate::json::object(vec![
        ("key", Value::from(key.as_str())),
        ("kind", Value::from(span.kind)),
        (
            "mode",
            Value::from(if expressed {
                "expressed"
            } else if pinned {
                "pinned"
            } else {
                "override"
            }),
        ),
        (
            "decision",
            decision_id
                .as_deref()
                .map(Value::from)
                .unwrap_or(Value::Null),
        ),
        (
            "finding",
            finding_id
                .as_deref()
                .map(Value::from)
                .unwrap_or(Value::Null),
        ),
        ("assertions", Value::from(assertions.len())),
        ("dry_run", Value::Bool(dry_run)),
        (
            "writes",
            Value::Array(
                [
                    Some(".factory/selection.yaml"),
                    finding_id.as_ref().map(|_| crate::record_log::FINDINGS.dir),
                    Some(crate::spans::LOCK_FILE),
                ]
                .into_iter()
                .flatten()
                .map(Value::from)
                .collect(),
            ),
        ),
    ]);

    if dry_run {
        if args.has("json") {
            print!("{}", crate::json::pretty(&summary));
        } else {
            println!(
                "codify (dry run): {} → {}",
                key,
                get_str(&summary, "mode").unwrap_or("")
            );
            if !expressed {
                let last = entries.last().cloned().unwrap_or(Value::Null);
                print!(
                    "--- selection.yaml overrides entry:\n{}",
                    crate::yaml::stringify(&Value::Array(vec![last]), 2)
                );
            }
            if let Some(added) = findings_doc
                .as_ref()
                .and_then(|d| get_arr(d, "findings"))
                .and_then(|a| a.last())
                .filter(|_| finding_id.is_some())
            {
                print!("--- finding record:\n{}", crate::json::pretty(added));
            }
            println!("--- applied.lock.yaml: spans[{}] = {}", key, span.sha256);
        }
        return 0;
    }

    if let Err(e) = crate::factory_io::write_in_place(&dir, SELECTION, new_selection.as_bytes()) {
        eprintln!("codify: {}", e);
        return 2;
    }
    if let (Some(doc), Some(_)) = (&findings_doc, &finding_id) {
        if let Err(e) = crate::findings::save(&dir, doc, schemas_dir(&args)) {
            eprintln!("codify: {}", e);
            return 2;
        }
    }
    let provenance_error =
        crate::provenance::refresh_partial(&dir, &mut new_lock, None, args.get("model"));
    crate::json::set(
        &mut summary,
        "provenance_recorded",
        Value::Bool(provenance_error.is_none()),
    );
    if let Some(e) = &provenance_error {
        eprintln!(
            "codify: warning: authoring provenance was omitted because it could not be recorded: {}",
            e
        );
    }
    if let Err(e) = crate::spans::write_lock(&dir, &new_lock) {
        eprintln!("codify: {}", e);
        return 2;
    }

    if args.has("json") {
        print!("{}", crate::json::pretty(&summary));
    } else {
        let cited = match (&decision_id, &finding_id) {
            (Some(d), Some(f)) => format!(" ({}; finding {} recorded)", d, f),
            (Some(d), None) => format!(" ({})", d),
            (None, Some(f)) => format!(" (finding {} recorded)", f),
            (None, None) => String::new(),
        };
        println!(
            "codify: {} → {}{}; lock refreshed for the span{}",
            key,
            get_str(&summary, "mode").unwrap_or(""),
            cited,
            if expressed {
                "; no override written (the selection expresses it)"
            } else {
                ""
            }
        );
        println!(
            "  commit with: git add .factory && git commit -m \"decide: {}{}\"",
            key,
            decision_id
                .as_deref()
                .map(|d| format!(" — {}", d))
                .unwrap_or_default()
        );
    }
    let _ = get; // (kept for symmetry with sibling commands)
    0
}

fn codify_source(args: &Args, dir: &Path, rel: &str) -> i32 {
    let reason = match args.get("reason").map(str::trim) {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => return usage("--reason is required (why the spec was edited; it becomes every new patch's reason and the decision)"),
    };
    if args.has("key")
        || args.has("assert")
        || args.has("pin")
        || args.has("expressed")
        || args.has("until")
        || args.has("expires")
    {
        return usage("--source takes only --reason, --decision, --context, --dry-run and --json (a source patch has no expiry; say when to revisit it in --reason or --context — nothing checks it)");
    }
    if let Some(d) = args.get("decision") {
        if !crate::record_log::is_decision_id(d) {
            return usage(&format!(
                "--decision must look like D-0019 or D-k7m2qx, got {:?}",
                d
            ));
        }
    }
    let dry_run = args.has("dry-run");

    let lock_text = match crate::factory_io::read_to_string(dir, crate::sources::SOURCES_LOCK) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("codify: {}", e);
            return 2;
        }
    };
    let lock = match crate::yaml::parse(&lock_text) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("codify: {}: {}", crate::sources::SOURCES_LOCK, e);
            return 2;
        }
    };
    let entry = match crate::sources::document_entries(&lock)
        .into_iter()
        .find(|e| e.path == rel)
    {
        Some(e) => e,
        None => {
            eprintln!(
                "codify: {} is not a pinned document in {} — run `graphos-factory-core sources pin --path {}` first",
                rel,
                crate::sources::SOURCES_LOCK,
                rel
            );
            return 2;
        }
    };
    let upstream_rel = entry
        .upstream
        .clone()
        .unwrap_or_else(|| crate::sources::default_upstream(rel));
    {
        let entries = crate::sources::document_entries(&lock);
        if let Some(fault) = crate::sources::not_followed(&entry, &entries) {
            eprintln!(
                "codify: refused — the entry for {} is not followed: {}; fix {} first (nothing is read or written through it)",
                rel,
                fault.describe(),
                crate::sources::SOURCES_LOCK
            );
            return 2;
        }
    }
    if !dir.join(&upstream_rel).exists() {
        eprintln!(
            "codify: no upstream copy at {} — {}",
            upstream_rel,
            if entry.upstream_sha256.is_some() {
                format!(
                    "restore it (git checkout -- {}); it is never re-created from the working copy, or replace the baseline on purpose with `graphos-factory-core sources pin --path {} --force --reason R`",
                    upstream_rel, rel
                )
            } else {
                format!(
                    "run `graphos-factory-core sources pin --path {}` before editing the spec",
                    rel
                )
            }
        );
        return 2;
    }
    let upstream = match crate::sources::load_document(dir, &upstream_rel) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("codify: {}", e);
            return 2;
        }
    };
    if let Some(expected) = &entry.upstream_sha256 {
        // An unreadable upstream is not an empty one: defaulting here would
        // hash `""` and report a refusal as "was modified" (ADR 0025).
        let bytes = match crate::factory_io::read(dir, &upstream_rel) {
            Ok(b) => b,
            Err(e) => {
                eprintln!(
                    "codify: refused — the upstream copy {} cannot be read ({}); restore it (git checkout -- {}) before codifying",
                    upstream_rel, e, upstream_rel
                );
                return 2;
            }
        };
        if &crate::patch::bytes_sha256(&bytes) != expected {
            eprintln!(
                "codify: refused — the upstream copy {} was modified (its bytes no longer hash to upstream_sha256); the vendor copy is never edited: restore it, then edit the working copy",
                upstream_rel
            );
            return 2;
        }
    }
    let working = match crate::sources::load_document(dir, rel) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("codify: {}", e);
            return 2;
        }
    };

    let ops = crate::patch::diff(&upstream, &working);
    let op_values: Vec<Value> = ops.iter().map(|o| o.to_value()).collect();
    match crate::patch::apply(&upstream, &op_values) {
        Ok(replayed) if replayed == working => {}
        Ok(_) => {
            eprintln!(
                "codify: internal error — the computed patches do not reproduce the working copy"
            );
            return 2;
        }
        Err(e) => {
            eprintln!(
                "codify: internal error — the computed patches do not apply: {}",
                e
            );
            return 2;
        }
    }

    let decision_id = args.get("decision").map(str::to_string);
    let context = context_arg(args);
    // Merge: an operation already recorded (same op, path and value) keeps
    // its reason, decision, context and verification; a new one — including
    // a different value at a pointer already patched — takes this reason,
    // and `--decision` / `--context` when given (ADR 0113 §3), and the one
    // it replaces counts as dropped.
    let same_op = |recorded: &Value, op: &crate::patch::Op| -> bool {
        get_str(recorded, "op") == Some(op.op)
            && get_str(recorded, "path") == Some(op.path.as_str())
            && get(recorded, "value") == op.value.as_ref()
    };
    let mut patches: Vec<Value> = Vec::new();
    let mut new_pointers: Vec<String> = Vec::new();
    for op in &ops {
        let previous = entry.patches.iter().find(|p| same_op(p, op));
        let mut v = op.to_value();
        match previous {
            Some(prev) => {
                for k in ["reason", "decision", "context", "verified"] {
                    if let Some(x) = get(prev, k) {
                        crate::json::set(&mut v, k, x.clone());
                    }
                }
            }
            None => {
                crate::json::set(&mut v, "reason", Value::from(reason.as_str()));
                if let Some(d) = &decision_id {
                    crate::json::set(&mut v, "decision", Value::from(d.as_str()));
                }
                if let Some(c) = &context {
                    crate::json::set(&mut v, "context", Value::from(c.as_str()));
                }
                new_pointers.push(format!("{} {}", op.op, op.path));
            }
        }
        patches.push(v);
    }
    let dropped = entry
        .patches
        .iter()
        .filter(|p| !ops.iter().any(|o| same_op(p, o)))
        .count();
    // --reason, --decision and --context land only on a new patch;
    // otherwise nothing carries them.
    let reason_used = !new_pointers.is_empty();
    if let (Some(d), true) = (&decision_id, reason_used) {
        warn_unrecorded_decision(
            dir,
            args,
            d,
            "the patches reference a decision that is not recorded",
        );
    }

    let new_lock_text = match crate::sources::splice_patches(&lock_text, rel, &patches) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("codify: {}", e);
            return 2;
        }
    };
    // The --source path splices sources.lock.yaml, not the selection: it is
    // held to its own schema the same way.
    if let Some(code) = refuse_invalid(
        &new_lock_text,
        "sources-lock.schema.json",
        crate::sources::SOURCES_LOCK,
    ) {
        return code;
    }
    let content_sha = crate::patch::canonical_sha256(&working);
    let applied_exists = dir.join(crate::spans::LOCK_FILE).exists();

    let mut summary = crate::json::object(vec![
        ("source", Value::from(rel)),
        ("upstream", Value::from(upstream_rel.as_str())),
        ("patches", Value::from(patches.len())),
        ("new", Value::from(new_pointers.len())),
        ("dropped", Value::from(dropped)),
        (
            "decision",
            match (&decision_id, reason_used) {
                (Some(d), true) => Value::from(d.as_str()),
                _ => Value::Null,
            },
        ),
        ("reason_used", Value::Bool(reason_used)),
        ("content_sha256", Value::from(content_sha.as_str())),
        (
            "next",
            Value::from(format!(
                "graphos-factory-core inventory build {} && graphos-factory-core reconcile {}",
                crate::inputs::rebuild_target(Path::new(&args.dir()), &args.dir(), &rel),
                args.dir()
            )),
        ),
        (
            "next_reason",
            Value::from(
                "until it is rebuilt, inventory.json describes the document as it was before this edit",
            ),
        ),
        ("dry_run", Value::Bool(dry_run)),
        (
            "writes",
            Value::Array(
                [
                    Some(crate::sources::SOURCES_LOCK),
                    if applied_exists {
                        Some(crate::spans::LOCK_FILE)
                    } else {
                        None
                    },
                ]
                .into_iter()
                .flatten()
                .map(Value::from)
                .collect(),
            ),
        ),
    ]);

    if dry_run {
        if args.has("json") {
            print!("{}", crate::json::pretty(&summary));
        } else {
            println!(
                "codify (dry run): {} → {} patch{} ({} new, {} dropped)",
                rel,
                patches.len(),
                if patches.len() == 1 { "" } else { "es" },
                new_pointers.len(),
                dropped
            );
            for p in &patches {
                println!("  {}", crate::patch::describe(p));
            }
            if applied_exists {
                println!("--- applied.lock.yaml: sources[{}] = {}", rel, content_sha);
            }
        }
        return 0;
    }

    if let Err(e) = crate::factory_io::write_in_place(
        dir,
        crate::sources::SOURCES_LOCK,
        new_lock_text.as_bytes(),
    ) {
        eprintln!("codify: {}", e);
        return 2;
    }
    let mut acknowledged = false;
    let mut provenance_recorded: Option<bool> = None;
    match crate::spans::read_lock(dir) {
        Ok(Some(mut applied)) => {
            let mut sources = get(&applied, "sources")
                .cloned()
                .unwrap_or(Value::Object(crate::json::obj()));
            crate::json::set(&mut sources, rel, Value::from(content_sha.as_str()));
            crate::json::set(&mut applied, "sources", sources);
            crate::json::set(&mut applied, "written_at", Value::from(crate::now_iso()));
            crate::json::set(&mut applied, "written_by", Value::from(crate::written_by()));
            let provenance_error =
                crate::provenance::refresh_partial(dir, &mut applied, None, args.get("model"));
            provenance_recorded = Some(provenance_error.is_none());
            if let Some(e) = provenance_error {
                eprintln!(
                    "codify: warning: authoring provenance was omitted because it could not be recorded: {}",
                    e
                );
            }
            if let Err(e) = crate::spans::write_lock(dir, &applied) {
                eprintln!("codify: {}", e);
                return 2;
            }
            acknowledged = true;
        }
        Ok(None) => eprintln!(
            "codify: note: no {} yet — run `graphos-factory-core lock` after the first apply",
            crate::spans::LOCK_FILE
        ),
        Err(e) => {
            eprintln!("codify: {}", e);
            return 2;
        }
    }

    crate::json::set(
        &mut summary,
        "provenance_recorded",
        provenance_recorded.map(Value::Bool).unwrap_or(Value::Null),
    );

    if args.has("json") {
        print!("{}", crate::json::pretty(&summary));
    } else {
        let decision_clause = match (&decision_id, reason_used) {
            (Some(d), true) => format!("; {}", d),
            (_, true) => String::new(),
            (_, false) if dropped > 0 => "; no new patch, so --reason was not used".to_string(),
            _ => "; no patch changed, so --reason was not used".to_string(),
        };
        println!(
            "codify: {} → {} patch{} in {} ({} new, {} dropped{}); {}",
            rel,
            patches.len(),
            if patches.len() == 1 { "" } else { "es" },
            crate::sources::SOURCES_LOCK,
            new_pointers.len(),
            dropped,
            decision_clause,
            if acknowledged {
                format!("working copy acknowledged in {}", crate::spans::LOCK_FILE)
            } else {
                format!("not acknowledged: no {} yet", crate::spans::LOCK_FILE)
            }
        );
        for p in &patches {
            println!("  {}", crate::patch::describe(p));
        }
        println!(
            "  then rebuild the inventory: graphos-factory-core inventory build {} && graphos-factory-core reconcile {} (until it is rebuilt, inventory.json describes the document as it was before this edit)",
            crate::inputs::rebuild_target(Path::new(&args.dir()), &args.dir(), &rel),
            args.dir()
        );
        if reason_used {
            println!(
                "  commit with: git add .factory {} && git commit -m \"decide: patch {}{}\"",
                rel,
                rel,
                decision_id
                    .as_deref()
                    .map(|d| format!(" — {}", d))
                    .unwrap_or_default()
            );
        } else if dropped > 0 {
            println!(
                "  commit with: git add .factory {} && git commit -m \"decide: patch {} ({} dropped)\"",
                rel, rel, dropped
            );
        } else {
            println!(
                "  commit with: git add .factory {} && git commit -m \"decide: patch {} (re-acknowledged)\"",
                rel, rel
            );
        }
    }
    0
}

fn codify_waive(args: &Args, dir: &Path, target: &str) -> i32 {
    use crate::waivers::{normalize_where, to_value, Waiver, STATUSES};
    let status = match args.get("status") {
        Some(s) if STATUSES.contains(&s) => s.to_string(),
        Some(s) => {
            return usage(&format!(
                "--status must be one of {} (which result you accept), got {:?}",
                STATUSES.join(", "),
                s
            ))
        }
        None => {
            return usage(&format!(
                "--status is required with --waive: one of {} — which result you accept",
                STATUSES.join(", ")
            ))
        }
    };
    let reason = match args.get("reason").map(str::trim) {
        Some(r) if !r.is_empty() => r.to_string(),
        _ => return usage("--reason is required (why the gap is acceptable; it becomes the waiver's reason and the decision)"),
    };
    if let Some(d) = args.get("decision") {
        if !crate::record_log::is_decision_id(d) {
            return usage(&format!(
                "--decision must look like D-0019 or D-k7m2qx, got {:?}",
                d
            ));
        }
    }
    if let Some(e) = args.get("expires") {
        if !regex::Regex::new(r"^\d{4}-\d{2}-\d{2}$")
            .unwrap()
            .is_match(e)
        {
            return usage(&format!("--expires must be YYYY-MM-DD, got {:?}", e));
        }
    }
    let dry_run = args.has("dry-run");
    let is_operation = regex::Regex::new(r"^(get|put|post|delete|patch|head|options|trace):/")
        .unwrap()
        .is_match(target);
    let target = if is_operation {
        target.to_string()
    } else {
        normalize_where(target)
    };
    let probe = Waiver {
        where_: if is_operation {
            None
        } else {
            Some(target.clone())
        },
        operation: if is_operation {
            Some(target.clone())
        } else {
            None
        },
        status: status.clone(),
        reason: None,
        decision: None,
        context: None,
        until: None,
        expires: None,
    };

    let selection_text = match crate::factory_io::read_to_string(dir, SELECTION) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("codify: {}", e);
            return 2;
        }
    };
    let selection = match crate::yaml::parse(&selection_text) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("codify: {}: {}", SELECTION, e);
            return 2;
        }
    };
    if is_operation {
        let inventory = crate::factory_io::read_to_string(dir, ".factory/inventory.json")
            .ok()
            .and_then(|t| crate::json::parse(&t).ok());
        let known = inventory
            .as_ref()
            .and_then(|inv| get_arr(inv, "operations"))
            .map(|ops| {
                ops.iter()
                    .any(|o| get_str(o, "key") == Some(target.as_str()))
            })
            .unwrap_or(false);
        if !known {
            eprintln!("codify: {} is not in inventory.json", target);
            return 2;
        }
    }

    // The gap must exist now: validate must report that target with that
    // status (or already waived by an earlier waiver for the same target,
    // which this one replaces).
    let report = match crate::cmd::validate::validate_workspace(dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("codify: validate: {}", e);
            return 2;
        }
    };
    let results: Vec<&Value> = get_arr(&report.json, "results")
        .into_iter()
        .flatten()
        .collect();
    let same_target = |w: &Value| {
        get_str(w, "target").map(|x| {
            if is_operation {
                x.to_string()
            } else {
                normalize_where(x)
            }
        }) == Some(target.clone())
            && get_str(w, "status") == Some(status.as_str())
    };
    let matched: Vec<&Value> = results
        .iter()
        .copied()
        .filter(|r| {
            let where_ = get_str(r, "where").unwrap_or("");
            let op = get_str(r, "operation");
            match get_str(r, "status") {
                Some(s) if s == status => probe.matches(where_, op, s),
                Some("waived") => get(r, "waiver").map(same_target).unwrap_or(false),
                _ => false,
            }
        })
        .collect();
    if matched.is_empty() {
        let there: Vec<String> = results
            .iter()
            .filter(|r| {
                let where_ = get_str(r, "where").unwrap_or("");
                if is_operation {
                    get_str(r, "operation") == Some(target.as_str())
                } else {
                    normalize_where(where_) == target
                }
            })
            .map(|r| {
                format!(
                    "{} {}{}",
                    get_str(r, "status").unwrap_or(""),
                    get_str(r, "where").unwrap_or(""),
                    get_str(r, "kind")
                        .map(|k| format!(" [{}]", k))
                        .unwrap_or_default()
                )
            })
            .collect();
        eprintln!(
            "codify: refused — nothing at {} is currently {}; a waiver accepts a gap validate reports, not one that might appear",
            target, status
        );
        if there.is_empty() {
            eprintln!("  validate reports no body at that target at all (check the path or operation key; a unit entry is tests/<suite>.connector.yaml#<entry name>)");
        } else {
            for l in &there {
                eprintln!("  {}", l);
            }
        }
        return 2;
    }
    let mut operations: Vec<String> = matched
        .iter()
        .filter_map(|r| get_str(r, "operation").map(str::to_string))
        .collect();
    operations.sort();
    operations.dedup();
    let mut causes: Vec<String> = matched
        .iter()
        .filter_map(|r| get_str(r, "reason").map(str::to_string))
        .collect();
    causes.sort();
    causes.dedup();

    let decision_id = args.get("decision").map(str::to_string);
    let context = context_arg(args);
    if let Some(d) = &decision_id {
        warn_unrecorded_decision(
            dir,
            args,
            d,
            "the waiver references a decision that is not recorded",
        );
    }

    let mut entries: Vec<Value> = get_arr(&selection, "waivers")
        .cloned()
        .unwrap_or_default()
        .into_iter()
        .filter(|w| {
            let t = get_str(w, "where")
                .map(normalize_where)
                .or_else(|| get_str(w, "operation").map(str::to_string));
            !(t.as_deref() == Some(target.as_str())
                && get_str(w, "status") == Some(status.as_str()))
        })
        .collect();
    let waiver = Waiver {
        reason: Some(reason.clone()),
        decision: decision_id.clone(),
        context: context.clone(),
        until: args.get("until").map(str::to_string),
        expires: args.get("expires").map(str::to_string),
        ..probe
    };
    entries.push(to_value(&waiver));
    let mut new_selection = splice_waivers(&selection_text, &entries);
    if context.is_some() {
        // An entry with `context` is a version-2 selection (ADR 0113).
        new_selection = crate::split::bump_selection_version(&new_selection);
    }
    if let Some(code) = refuse_invalid(&new_selection, "selection.schema.json", SELECTION) {
        return code;
    }
    let summary = crate::json::object(vec![
        ("mode", Value::from("waive")),
        ("target", Value::from(target.as_str())),
        ("status", Value::from(status.as_str())),
        ("matched", Value::from(matched.len())),
        (
            "operations",
            Value::Array(operations.iter().map(|o| Value::from(o.as_str())).collect()),
        ),
        (
            "causes",
            Value::Array(causes.iter().map(|c| Value::from(c.as_str())).collect()),
        ),
        (
            "decision",
            decision_id
                .as_deref()
                .map(Value::from)
                .unwrap_or(Value::Null),
        ),
        ("dry_run", Value::Bool(dry_run)),
    ]);

    if dry_run {
        if args.has("json") {
            print!("{}", crate::json::pretty(&summary));
        } else {
            println!(
                "codify (dry run): waive {} {} bod{} at {}",
                matched.len(),
                status,
                if matched.len() == 1 { "y" } else { "ies" },
                target
            );
            print!(
                "--- selection.yaml waivers entry:\n{}",
                crate::yaml::stringify(&Value::Array(vec![to_value(&waiver)]), 2)
            );
        }
        return 0;
    }
    if let Err(e) = crate::factory_io::write_in_place(dir, SELECTION, new_selection.as_bytes()) {
        eprintln!("codify: {}", e);
        return 2;
    }
    if args.has("json") {
        print!("{}", crate::json::pretty(&summary));
    } else {
        println!(
            "codify: waived {} {} bod{} at {}{}",
            matched.len(),
            status,
            if matched.len() == 1 { "y" } else { "ies" },
            target,
            decision_id
                .as_deref()
                .map(|d| format!(" ({})", d))
                .unwrap_or_default()
        );
        println!(
            "  commit with: git add .factory && git commit -m \"decide: waive {}{}\"",
            target,
            decision_id
                .as_deref()
                .map(|d| format!(" — {}", d))
                .unwrap_or_default()
        );
    }
    0
}
