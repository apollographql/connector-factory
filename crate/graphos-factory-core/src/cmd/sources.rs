//! sources — pin the description documents a workspace is built from.
//!
//!   sources pin [workspace-dir] --path <spec> [--url URL] [--retrieved-at T] [--force --reason R [--decision D-id]] [--json] [--model MODEL]
//!   sources status [workspace-dir] [--json]   (--json: {"sources": [...], "unpinned": [...], "recorded": [...]})
//!   sources refresh [workspace-dir] --path <spec> --from <file> --reason R [--decision D-id] [--url URL] [--retrieved-at T] [--dry-run] [--json] [--model MODEL]
//!
//! `pin` keeps the vendor's bytes as `.factory/sources/<name>.upstream.<ext>`
//! (copied from the working copy the first time, or with --force, which
//! replaces the vendor baseline and therefore needs a --reason: it records a
//! finding in the findings log with both hashes — `source: sources`, `related`
//! naming `--decision` when given (ADR 0113 §3) — and refreshes retrieved_at), detects
//! the document's kind (openapi / swagger) and version, records kind,
//! version, upstream and upstream_sha256 on the matching sources.lock.yaml
//! entry (adding one when the path is new), and acknowledges the working
//! copy's content hash in applied.lock.yaml so a later edit is a hand edit.
//!
//! `status` reports every pinned document: kind, upstream integrity, whether
//! the working copy changed since the lock, and whether the recorded patches
//! still reproduce it. It also reports every other entry of the lock as
//! `recorded` — docs pages, probes and kinds not modelled yet — with its
//! kind, url (a probe's `base_url`), retrieved_at, sha256 (null when none was
//! recorded), the operations it informed (`used_for`, a probe's
//! `operations`), note and credential name, so a reader of `--json` sees
//! every source the workspace was built from, not only the pinned ones.
//!
//! `refresh` takes the vendor's new document (--from, a file fetched by
//! curl or the browser), makes it the upstream copy, replays the entry's
//! `patches[]` over it (crate::refresh: each one re-applied, obsolete or in
//! conflict), writes the working copy from the result, keeps only the
//! re-applied patches on the entry, records a finding in the findings log with
//! both hashes, the origin and every patch's fate (`source: sources`,
//! `related` naming `--decision` when given), acknowledges the new working copy in
//! applied.lock.yaml, and — when `.factory/inventory.json` exists — rebuilds
//! it and prints `inventory diff` against the previous one. It refuses while
//! the working copy carries an uncodified edit (the refresh rewrites the
//! working copy; codify first) or the upstream copy is modified.
//!
//! Exit codes: 0 done; 1 usage; 2 refused or unreadable file (nothing
//! written); 3 done, but a patch is in conflict (dropped, not applied —
//! re-apply its intent by hand and `codify --source`); 4 a write failed
//! after the first file was written (the message lists what was written;
//! `git checkout -- <files>` restores the workspace).

use crate::args::{Args, Flags};
use crate::sources::{
    default_upstream, detect, document_entries, load_document, read_sources_lock, statuses,
    upsert_entry, RecordedSource, SourceStatus, SOURCES_LOCK,
};
use serde_json::Value;
use std::path::Path;

/// The usage text: `sources --help` prints it on stdout (ADR 0086).
pub const USAGE: &str = "usage: sources pin [workspace] --path <spec> [--url URL] [--retrieved-at T] [--force --reason R [--decision D-id]] [--json] [--model MODEL]\n       sources status [workspace] [--json]\n       sources refresh [workspace] --path <spec> --from <file> --reason R [--decision D-id] [--url URL] [--retrieved-at T] [--dry-run] [--json] [--model MODEL]\n  pin --force and refresh record a finding (the findings log, source: sources); --decision names a decision it relates to";

fn usage(msg: &str) -> i32 {
    eprintln!("sources: {}", msg);
    eprintln!("{}", USAGE);
    1
}

pub fn main(argv: &[String]) -> i32 {
    let (sub, rest) = match argv.split_first() {
        Some((s, r)) => (s.as_str(), r.to_vec()),
        None => return usage("pin, status or refresh"),
    };
    match sub {
        "pin" => pin(&rest),
        "status" => status(&rest),
        "refresh" => refresh(&rest),
        other => usage(&format!("unknown subcommand {:?}", other)),
    }
}

/// The flags this verb accepts; dispatch refuses any other (ADR 0097).
pub const PIN_FLAGS: Flags = Flags {
    boolean: &["force", "json"],
    valued: &["path", "url", "retrieved-at", "reason", "model", "decision"],
};

/// `--decision D-id` (numbered or random) on `pin --force` and `refresh`:
/// the decision the finding relates to. `Err` is the usage message for a malformed id.
fn related_decision(args: &Args) -> Result<Vec<String>, String> {
    match args.get("decision") {
        None => Ok(Vec::new()),
        Some(d) if crate::record_log::is_decision_id(d) => Ok(vec![d.to_string()]),
        Some(d) => Err(format!(
            "--decision must look like D-0019 or D-k7m2qx, got {:?}",
            d
        )),
    }
}

fn pin(argv: &[String]) -> i32 {
    let args = Args::parse(argv, &PIN_FLAGS);
    let dir = Path::new(&args.dir()).to_path_buf();
    let related = match related_decision(&args) {
        Ok(r) => r,
        Err(e) => return usage(&e),
    };
    let rel = match args.get("path") {
        Some(p) => p.trim_start_matches("./").to_string(),
        None => {
            return usage("--path <spec> is required (the working copy, relative to the workspace)")
        }
    };
    if !crate::sources::inside_workspace(&rel) {
        eprintln!(
            "sources: {} is not inside the workspace — the working copy is committed with the workspace and exported with it, so --path is relative to the workspace root and never climbs out of it",
            rel
        );
        return 2;
    }
    if rel.starts_with(crate::sources::UPSTREAM_DIR) {
        eprintln!(
            "sources: {} is under {}, which is reserved for the vendor copies — a working copy lives elsewhere in the workspace (openapi.json, swagger.json, vendor/…)",
            rel,
            crate::sources::UPSTREAM_DIR
        );
        return 2;
    }
    let bytes = match std::fs::read(dir.join(&rel)) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("sources: {}: {}", rel, e);
            return 2;
        }
    };
    let doc = match load_document(&dir, &rel) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    let (kind, version) = detect(&doc);
    if kind == "unknown" {
        eprintln!(
            "sources: {} declares neither `openapi` nor `swagger`; only description documents are pinned",
            rel
        );
        return 2;
    }

    let lock_text = match crate::factory_io::read_to_string_optional(&dir, SOURCES_LOCK) {
        Ok(t) => t.unwrap_or_default(),
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    let lock = if lock_text.trim().is_empty() {
        None
    } else {
        match crate::yaml::parse(&lock_text) {
            Ok(l) => Some(l),
            Err(e) => {
                eprintln!("sources: {}: {}", SOURCES_LOCK, e);
                return 2;
            }
        }
    };
    let existing = lock
        .as_ref()
        .map(document_entries)
        .unwrap_or_default()
        .into_iter()
        .find(|e| e.path == rel);
    let upstream_rel = existing
        .as_ref()
        .and_then(|e| e.upstream.clone())
        .unwrap_or_else(|| default_upstream(&rel));
    // The entry as it will be recorded, checked against every other entry:
    // the two copies must be distinct files inside the workspace, the vendor
    // copy directly under .factory/sources/ and shared with no one.
    let candidate = crate::sources::SourceEntry {
        kind: kind.to_string(),
        version: Some(version.clone()),
        path: rel.clone(),
        upstream: Some(upstream_rel.clone()),
        upstream_sha256: None,
        patches: vec![],
    };
    // The whole lock, this path's own entries included: two entries for one
    // path is a fault the predicate reports (it skips only the candidate's
    // own record when comparing copies).
    let siblings: Vec<crate::sources::SourceEntry> =
        lock.as_ref().map(document_entries).unwrap_or_default();
    if let Some(fault) = crate::sources::not_followed(&candidate, &siblings) {
        eprintln!(
            "sources: refused — {} {} {}: {} (its default upstream would be {}); fix {} first",
            if existing.is_some() {
                "the entry for"
            } else {
                "pinning"
            },
            rel,
            if existing.is_some() {
                "is not followed".to_string()
            } else {
                "would collide with the lock as it stands".to_string()
            },
            fault.describe(),
            default_upstream(&rel),
            SOURCES_LOCK
        );
        return 2;
    }
    // The vendor copy is `.factory/sources/…` — custody's, by the check
    // above (ADR 0025); nothing here touches it by path. Custody's own
    // answer, not `is_file`'s boolean: a refused vendor copy is not a
    // missing one, and every remedy below ("restore it (git checkout -- …)")
    // is wrong for a file that is right there and was not followed.
    let upstream_present = match crate::factory_io::symlink_metadata(&dir, &upstream_rel) {
        Ok(Some(m)) if m.is_file() => true,
        Ok(None) => false,
        Ok(Some(_)) => {
            eprintln!(
                "sources: refused — {} is not a regular file; the vendor copy lives there and nothing else does: move what is there aside first",
                upstream_rel
            );
            return 2;
        }
        Err(e) => {
            eprintln!(
                "sources: refused — {}; nothing was written, and the vendor copy is not treated as missing",
                e
            );
            return 2;
        }
    };

    let mut copied = false;
    let previous_upstream_sha = existing.as_ref().and_then(|e| e.upstream_sha256.clone());
    // An entry that records upstream_sha256 claims a baseline; moving it —
    // whether the vendor copy is on disk or gone — is a decision.
    let replacing = (upstream_present || previous_upstream_sha.is_some()) && args.has("force");
    let force_reason = match args.get("reason").map(str::trim) {
        Some(r) if !r.is_empty() => Some(r.to_string()),
        _ => None,
    };
    if replacing && force_reason.is_none() {
        return usage(
            "--force replaces the vendor's pinned bytes with the current working copy, which is a decision: give it a --reason (why the baseline changed — a re-fetched vendor document, a new version); an uncodified spec edit is codified with `codify --source`, never forced",
        );
    }
    if !args.has("force") {
        // A recorded baseline is never re-blessed by re-pinning: a modified
        // or missing vendor copy is restored, or replaced on purpose with
        // --force --reason. (An entry with no upstream_sha256 yet — hand
        // written, or from before the copies were distinguished — pins
        // whatever is on disk: that is the migration path.)
        if let Some(expected) = &previous_upstream_sha {
            if !upstream_present {
                eprintln!(
                    "sources: refused — {} is missing but the entry records upstream_sha256 {}; the vendor copy is never re-created from the working copy: restore it (git checkout -- {}), or replace the baseline on purpose with `sources pin --path {} --force --reason R`",
                    upstream_rel, expected, upstream_rel, rel
                );
                return 2;
            }
            let on_disk = match crate::factory_io::read(&dir, &upstream_rel) {
                Ok(b) => crate::patch::bytes_sha256(&b),
                Err(e) => {
                    eprintln!("sources: {}", e);
                    return 2;
                }
            };
            if &on_disk != expected {
                eprintln!(
                    "sources: refused — {} no longer hashes to the recorded upstream_sha256; the vendor copy is never edited: restore it (git checkout -- {}), or replace the baseline on purpose with `sources pin --path {} --force --reason R`",
                    upstream_rel, upstream_rel, rel
                );
                return 2;
            }
        }
    }
    // Whether the vendor copy is written this run. Nothing on disk changes
    // until the new lock text has been built and re-parsed: a refused pin
    // must leave the never-edited artifact exactly as it was.
    let keeping = upstream_present && !args.has("force");
    let upstream_sha = if keeping {
        // A kept vendor copy must be a readable document, or nothing about
        // the spec could ever be checked against it.
        if let Err(e) = load_document(&dir, &upstream_rel) {
            eprintln!(
                "sources: refused — the upstream copy {} does not parse ({}); restore it (git checkout -- {}), or replace the baseline on purpose with `sources pin --path {} --force --reason R`",
                upstream_rel, e, upstream_rel, rel
            );
            return 2;
        }
        match crate::factory_io::read(&dir, &upstream_rel) {
            Ok(b) => crate::patch::bytes_sha256(&b),
            Err(e) => {
                eprintln!("sources: {}", e);
                return 2;
            }
        }
    } else {
        crate::patch::bytes_sha256(&bytes)
    };

    let url = args.get("url").map(str::to_string);
    let retrieved = args.get("retrieved-at").map(str::to_string).or_else(|| {
        if existing.is_none() || replacing {
            Some(crate::now_iso())
        } else {
            None
        }
    });
    // The legacy whole-file hash, when the entry still carries one, follows
    // the bytes it describes.
    let has_legacy_sha = lock
        .as_ref()
        .and_then(|l| crate::json::get_arr(l, "sources"))
        .map(|list| {
            list.iter().any(|s| {
                crate::json::get_str(s, "path") == Some(rel.as_str()) && s.get("sha256").is_some()
            })
        })
        .unwrap_or(false);
    let new_lock_text = match pinned_lock_text(
        &lock_text,
        &PinRecord {
            kind,
            version: &version,
            url: url.as_deref(),
            retrieved_at: retrieved.as_deref(),
            path: &rel,
            upstream: &upstream_rel,
            upstream_sha256: &upstream_sha,
            legacy_sha256: has_legacy_sha && replacing,
        },
    ) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    // The lock text is valid: now the vendor copy, then the lock.
    if !keeping {
        if replacing {
            eprintln!(
                "sources: --force replaces {} with the current working copy; every recorded patch is re-checked by lint",
                upstream_rel
            );
        }
        if let Err(e) = crate::factory_io::create_parent_dir(&dir, &upstream_rel)
            .and_then(|_| crate::factory_io::write_in_place(&dir, &upstream_rel, &bytes))
        {
            eprintln!("sources: {}", e);
            return 2;
        }
        copied = true;
    }
    if let Err(e) = crate::factory_io::create_parent_dir(&dir, SOURCES_LOCK).and_then(|_| {
        crate::factory_io::write_in_place(&dir, SOURCES_LOCK, new_lock_text.as_bytes())
    }) {
        eprintln!("sources: {}", e);
        return 2;
    }

    // Replacing the vendor baseline is recorded as a finding (ADR 0113 §3):
    // both hashes and the reason.
    let mut finding: Option<String> = None;
    if replacing {
        let mut doc = match crate::findings::load(&dir, None) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("sources: {}", e);
                return 2;
            }
        };
        let context = format!(
            "`sources pin --force` replaced the vendor's pinned copy {} (upstream_sha256 {}) with the current working copy (upstream_sha256 {}). Every recorded patch is now measured against the new baseline; `lint` reports the ones that no longer fit.",
            upstream_rel,
            previous_upstream_sha.as_deref().unwrap_or("unrecorded"),
            upstream_sha
        );
        let new = crate::findings::NewFinding {
            title: format!("Upstream replaced: {}", rel),
            date: crate::today(),
            body: format!(
                "{}\n\nReason: {}",
                context,
                force_reason.clone().unwrap_or_default()
            ),
            source: "sources".into(),
            evidence: vec![rel.to_string(), "sources pin --force".to_string()],
            related: related.clone(),
            ..Default::default()
        };
        match crate::findings::add(&mut doc, new)
            .and_then(|id| crate::findings::save(&dir, &doc, None).map(|_| id))
        {
            Ok(id) => finding = Some(id),
            Err(e) => {
                eprintln!("sources: {}", e);
                return 2;
            }
        }
    }

    // Acknowledge the working copy in the applied lock, when there is one.
    let content_sha = crate::patch::canonical_sha256(&doc);
    let mut acknowledged = false;
    let mut provenance_recorded: Option<bool> = None;
    match crate::spans::read_lock(&dir) {
        Ok(Some(mut applied)) => {
            let mut sources = crate::json::get(&applied, "sources")
                .cloned()
                .unwrap_or(Value::Object(crate::json::obj()));
            crate::json::set(&mut sources, &rel, Value::from(content_sha.as_str()));
            crate::json::set(&mut applied, "sources", sources);
            crate::json::set(&mut applied, "written_at", Value::from(crate::now_iso()));
            let provenance_error =
                crate::provenance::refresh_partial(&dir, &mut applied, None, args.get("model"));
            provenance_recorded = Some(provenance_error.is_none());
            if let Some(e) = provenance_error {
                eprintln!(
                    "sources: warning: authoring provenance was omitted because it could not be recorded: {}",
                    e
                );
            }
            if let Err(e) = crate::spans::write_lock(&dir, &applied) {
                eprintln!("sources: {}", e);
                return 2;
            }
            acknowledged = true;
        }
        Ok(None) => {}
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    }

    let summary = crate::json::object(vec![
        ("path", Value::from(rel.as_str())),
        ("kind", Value::from(kind)),
        ("version", Value::from(version.as_str())),
        ("upstream", Value::from(upstream_rel.as_str())),
        ("upstream_sha256", Value::from(upstream_sha.as_str())),
        ("upstream_copied", Value::Bool(copied)),
        (
            "finding",
            finding.clone().map(Value::from).unwrap_or(Value::Null),
        ),
        ("content_sha256", Value::from(content_sha.as_str())),
        ("acknowledged_in_applied_lock", Value::Bool(acknowledged)),
        (
            "provenance_recorded",
            provenance_recorded.map(Value::Bool).unwrap_or(Value::Null),
        ),
    ]);
    if args.has("json") {
        print!("{}", crate::json::pretty(&summary));
    } else {
        println!(
            "sources: pinned {} as {} {} — upstream {}{}; {}",
            rel,
            kind,
            version,
            upstream_rel,
            if copied { " (copied)" } else { " (kept)" },
            if acknowledged {
                "working copy acknowledged in applied.lock.yaml"
            } else {
                "no applied.lock.yaml yet — run `graphos-factory-core lock` after the first apply"
            }
        );
    }
    0
}

/// The keys `sources pin` records on a document's sources.lock.yaml entry.
pub struct PinRecord<'a> {
    pub kind: &'a str,
    pub version: &'a str,
    pub url: Option<&'a str>,
    pub retrieved_at: Option<&'a str>,
    pub path: &'a str,
    pub upstream: &'a str,
    pub upstream_sha256: &'a str,
    /// Also rewrite the legacy whole-file `sha256` to the new upstream hash.
    pub legacy_sha256: bool,
}

/// `lock_text` with the entry for `record.path` written as `sources pin`
/// writes it (added when the path is new; every other line kept), re-parsed
/// before it is returned so a caller never writes a lock that does not read.
/// `init` builds a new workspace's lock through this with an empty
/// `lock_text`, so the two cannot drift.
pub fn pinned_lock_text(lock_text: &str, record: &PinRecord) -> Result<String, String> {
    let mut pairs: Vec<(&str, &str)> = vec![("kind", record.kind), ("version", record.version)];
    if let Some(u) = record.url {
        pairs.push(("url", u));
    }
    if let Some(r) = record.retrieved_at {
        pairs.push(("retrieved_at", r));
    }
    pairs.push(("path", record.path));
    pairs.push(("upstream", record.upstream));
    pairs.push(("upstream_sha256", record.upstream_sha256));
    if record.legacy_sha256 {
        pairs.push(("sha256", record.upstream_sha256));
    }
    let text = upsert_entry(lock_text, record.path, &pairs)?;
    if let Err(e) = crate::yaml::parse(&text) {
        return Err(format!(
            "refusing to write {}: the edited text would not parse ({})",
            SOURCES_LOCK, e
        ));
    }
    Ok(text)
}

fn render(statuses: &[SourceStatus]) -> Vec<String> {
    let mut lines = Vec::new();
    for s in statuses {
        let mut notes: Vec<String> = Vec::new();
        if let Some(reason) = &s.misplaced_reason {
            // Nothing else about the entry was looked at, so nothing else is said.
            lines.push(format!(
                "  {} ({}) — {}: {} — fix it in sources.lock.yaml (nothing is read or written through this entry)",
                s.path,
                s.declared_kind,
                if s.outside_workspace {
                    "OUTSIDE THE WORKSPACE"
                } else {
                    "NOT FOLLOWED"
                },
                reason
            ));
            continue;
        }
        if !s.working_present {
            notes.push("working copy missing".into());
        }
        if s.detected_kind != "unknown" && s.detected_kind != s.declared_kind {
            notes.push(format!(
                "declared {} but the document is {} — `graphos-factory-core sources pin --path {}` fixes the record",
                s.declared_kind, s.detected_kind, s.path
            ));
        }
        if !s.upstream_present {
            notes.push(if s.upstream_recorded {
                format!(
                    "no upstream copy ({}) — restore it (git checkout -- {}); it is never re-created from the working copy, or replace the baseline with `graphos-factory-core sources pin --path {} --force --reason R`",
                    s.upstream, s.upstream, s.path
                )
            } else {
                format!(
                    "no upstream copy ({}) — run `graphos-factory-core sources pin --path {}`",
                    s.upstream, s.path
                )
            });
        } else if let Some(e) = &s.upstream_error {
            notes.push(format!(
                "UPSTREAM UNREADABLE ({}) — restore it (git checkout -- {}); until then nothing about the spec can be checked",
                e, s.upstream
            ));
        } else if s.upstream_ok == Some(false) {
            notes.push("UPSTREAM MODIFIED — the vendor copy is never edited; restore it".into());
        }
        if let Some(e) = &s.working_error {
            notes.push(format!("WORKING COPY UNREADABLE — {}", e));
        }
        if s.unlocked() {
            notes.push("not in applied.lock.yaml — run `graphos-factory-core lock`".into());
        } else if s.hand_edit {
            notes.push(if !s.upstream_present && !s.upstream_recorded {
                format!(
                    "HAND EDIT since applied.lock.yaml — pin it first (graphos-factory-core sources pin --path {}), then codify later edits",
                    s.path
                )
            } else if !s.upstream_present || s.upstream_error.is_some() || s.upstream_ok == Some(false) {
                "HAND EDIT since applied.lock.yaml — restore the vendor copy first, then codify it".to_string()
            } else {
                format!(
                    "HAND EDIT since applied.lock.yaml — codify it (graphos-factory-core codify --source {} --reason …)",
                    s.path
                )
            });
        }
        if s.patches_ok == Some(false) {
            notes.push(format!(
                "patches do not reproduce the working copy{}",
                s.patches_error
                    .as_ref()
                    .map(|e| format!(": {}", e))
                    .unwrap_or_default()
            ));
        }
        lines.push(format!(
            "  {} ({} {}) — {} patch{}{}",
            s.path,
            s.declared_kind,
            s.version,
            s.patches,
            if s.patches == 1 { "" } else { "es" },
            if notes.is_empty() {
                "; in sync".to_string()
            } else {
                format!("; {}", notes.join("; "))
            }
        ));
    }
    lines
}

/// The flags this verb accepts; dispatch refuses any other (ADR 0097).
pub const STATUS_FLAGS: Flags = Flags {
    boolean: &["json"],
    valued: &[],
};

fn status(argv: &[String]) -> i32 {
    let args = Args::parse(argv, &STATUS_FLAGS);
    let dir = Path::new(&args.dir()).to_path_buf();
    let applied = match crate::spans::read_lock(&dir) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    let list = match statuses(&dir, applied.as_ref()) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    let unpinned = crate::sources::unpinned(&dir, applied.as_ref());
    let recorded = match crate::sources::recorded(&dir) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    if args.has("json") {
        let v: Vec<Value> = list.iter().map(SourceStatus::to_value).collect();
        print!(
            "{}",
            crate::json::pretty(&crate::json::object(vec![
                ("sources", Value::Array(v)),
                (
                    "unpinned",
                    Value::Array(unpinned.iter().map(|p| Value::from(p.as_str())).collect()),
                ),
                (
                    "recorded",
                    Value::Array(recorded.iter().map(RecordedSource::to_value).collect()),
                ),
            ]))
        );
        return 0;
    }
    if list.is_empty() {
        let has_lock = read_sources_lock(&dir).ok().flatten().is_some();
        println!(
            "sources: {}",
            if has_lock {
                "no pinned documents"
            } else {
                "no .factory/sources.lock.yaml"
            }
        );
    } else {
        println!(
            "sources: {} pinned document{}",
            list.len(),
            if list.len() == 1 { "" } else { "s" }
        );
        for l in render(&list) {
            println!("{}", l);
        }
    }
    for p in &unpinned {
        println!(
            "  UNPINNED: {} is acknowledged in applied.lock.yaml but no entry pins it — restore the entry, or run `graphos-factory-core lock` to stop watching it on purpose",
            p
        );
    }
    if !recorded.is_empty() {
        println!(
            "sources: {} recorded source{} (read, not pinned)",
            recorded.len(),
            if recorded.len() == 1 { "" } else { "s" }
        );
        for l in recorded_lines(&recorded) {
            println!("{}", l);
        }
    }
    0
}

/// One line per recorded source: kind, where it came from, when, whether a
/// hash was recorded, and how many operations it informed.
pub fn recorded_lines(recorded: &[RecordedSource]) -> Vec<String> {
    recorded
        .iter()
        .map(|r| {
            let mut facts: Vec<String> = Vec::new();
            if let Some(at) = &r.retrieved_at {
                facts.push(format!("retrieved {}", at));
            }
            facts.push(match &r.sha256 {
                Some(h) => format!("sha256 {}", h.chars().take(12).collect::<String>()),
                None => "no hash recorded".to_string(),
            });
            facts.push(format!(
                "{} operation{}",
                r.operations.len(),
                if r.operations.len() == 1 { "" } else { "s" }
            ));
            format!(
                "  {} {} ({})",
                r.kind,
                r.url.as_deref().unwrap_or("(no url)"),
                facts.join(", ")
            )
        })
        .collect()
}

/// Lines for `lock --check` and reconcile's text report.
pub fn render_lines(statuses: &[SourceStatus]) -> Vec<String> {
    render(statuses)
}

/// Whether a working copy at `rel` is a YAML file (by extension; anything
/// else is JSON, the reader's default).
fn is_yaml_path(rel: &str) -> bool {
    let lower = rel.to_ascii_lowercase();
    lower.ends_with(".yaml") || lower.ends_with(".yml")
}

/// The working copy's text for a replayed document: the vendor's bytes
/// verbatim when no patch survived and they are already in the working
/// copy's format (the two copies are then the same file), otherwise the
/// document serialised in the working copy's own format — JSON, or YAML
/// by extension. A YAML working copy's comments and layout do not survive
/// a serialisation: the working copy is upstream + patches, nothing else.
fn working_text(rel: &str, upstream_text: &str, doc: &Value, kept: usize) -> Vec<u8> {
    let upstream_is_json = upstream_text.trim_start().starts_with('{');
    if kept == 0 && upstream_is_json != is_yaml_path(rel) {
        return upstream_text.as_bytes().to_vec();
    }
    if is_yaml_path(rel) {
        crate::yaml::stringify(doc, 0).into_bytes()
    } else {
        crate::json::pretty(doc).into_bytes()
    }
}

/// The flags this verb accepts; dispatch refuses any other (ADR 0097).
pub const REFRESH_FLAGS: Flags = Flags {
    boolean: &["dry-run", "json"],
    valued: &[
        "path",
        "from",
        "reason",
        "url",
        "retrieved-at",
        "model",
        "decision",
    ],
};

fn refresh(argv: &[String]) -> i32 {
    let args = Args::parse(argv, &REFRESH_FLAGS);
    let dir = Path::new(&args.dir()).to_path_buf();
    let dry_run = args.has("dry-run");
    let related = match related_decision(&args) {
        Ok(r) => r,
        Err(e) => return usage(&e),
    };
    let rel = match args.get("path") {
        Some(p) => p.trim_start_matches("./").to_string(),
        None => {
            return usage("--path <spec> is required (the working copy, relative to the workspace)")
        }
    };
    let from = match args.get("from") {
        Some(f) => std::path::PathBuf::from(f),
        None => {
            return usage(
                "--from <file> is required: the vendor's new document as fetched (curl -fsSL URL -o FILE); refresh never fetches itself",
            )
        }
    };
    if !crate::sources::inside_workspace(&rel) || rel.starts_with(crate::sources::UPSTREAM_DIR) {
        eprintln!(
            "sources: {} is not a working copy inside the workspace (never absolute, never `..`, never under {})",
            rel,
            crate::sources::UPSTREAM_DIR
        );
        return 2;
    }
    // Moving the vendor baseline is a decision, as it is for `pin --force`:
    // the reason is the decision's text, so it is required and kept to one
    // line for a tidy `resolution.decision` in decisions.json.
    let reason = match args.get("reason").map(str::trim) {
        Some(r) if !r.is_empty() => r.split_whitespace().collect::<Vec<_>>().join(" "),
        _ => {
            return usage(
                "--reason R is required: replacing the vendor baseline is a decision, and R is its text (why the document was re-fetched — a new version, a changelog entry)",
            )
        }
    };

    // The lock and the entry, as pin and codify read them.
    let lock_text = match crate::factory_io::read_to_string(&dir, SOURCES_LOCK) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    let lock = match crate::yaml::parse(&lock_text) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("sources: {}: {}", SOURCES_LOCK, e);
            return 2;
        }
    };
    let entries = document_entries(&lock);
    let entry = match entries.iter().find(|e| e.path == rel) {
        Some(e) => e.clone(),
        None => {
            eprintln!(
                "sources: {} is not a pinned document in {} — run `graphos-factory-core sources pin --path {}` first",
                rel, SOURCES_LOCK, rel
            );
            return 2;
        }
    };
    if let Some(fault) = crate::sources::not_followed(&entry, &entries) {
        eprintln!(
            "sources: refused — the entry for {} is not followed: {}; fix {} first (nothing is read or written through it)",
            rel,
            fault.describe(),
            SOURCES_LOCK
        );
        return 2;
    }
    let upstream_rel = crate::sources::resolved_upstream(&entry);
    let previous_sha = match &entry.upstream_sha256 {
        Some(s) => s.clone(),
        None => {
            eprintln!(
                "sources: refused — the entry for {} records no upstream_sha256, so there is no baseline to move; pin it first (`graphos-factory-core sources pin --path {}`)",
                rel, rel
            );
            return 2;
        }
    };
    let old_upstream_bytes = match crate::factory_io::read(&dir, &upstream_rel) {
        Ok(b) => b,
        Err(e) => {
            eprintln!(
                "sources: refused — the upstream copy {} cannot be read ({}); restore it (git checkout -- {}) before refreshing",
                upstream_rel, e, upstream_rel
            );
            return 2;
        }
    };
    if crate::patch::bytes_sha256(&old_upstream_bytes) != previous_sha {
        eprintln!(
            "sources: refused — {} no longer hashes to the recorded upstream_sha256; the vendor copy is never edited: restore it (git checkout -- {}) before refreshing",
            upstream_rel, upstream_rel
        );
        return 2;
    }
    let old_upstream = match load_document(&dir, &upstream_rel) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("sources: refused — the upstream copy does not parse: {}", e);
            return 2;
        }
    };
    let working = match load_document(&dir, &rel) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("sources: refused — the working copy does not parse: {}", e);
            return 2;
        }
    };
    // The refresh rewrites the working copy from upstream + patches, so an
    // edit the patches do not describe would be lost: codify it first.
    match crate::patch::apply(&old_upstream, &entry.patches) {
        Ok(replayed) if replayed == working => {}
        Ok(_) => {
            eprintln!(
                "sources: refused — {} carries an edit its patches do not describe (the refresh rewrites the working copy from the new upstream plus the patches, and would lose it); codify it first: graphos-factory-core codify --source {} --reason R",
                rel, rel
            );
            return 2;
        }
        Err(e) => {
            eprintln!(
                "sources: refused — the recorded patches do not apply to the current upstream ({}); `graphos-factory-core sources status` names the fix",
                e
            );
            return 2;
        }
    }

    // The vendor's new document.
    let from_bytes = match std::fs::read(&from) {
        Ok(b) => b,
        Err(e) => {
            eprintln!("sources: --from {}: {}", from.display(), e);
            return 2;
        }
    };
    let same_file = |a: &Path, b: &Path| -> bool {
        match (std::fs::canonicalize(a), std::fs::canonicalize(b)) {
            (Ok(x), Ok(y)) => x == y,
            _ => false,
        }
    };
    if same_file(&from, &dir.join(&rel)) {
        eprintln!(
            "sources: refused — --from names the working copy {}; a refresh takes the vendor's new document, and an edit to the working copy is codified (`graphos-factory-core codify --source {} --reason R`), never refreshed in",
            rel, rel
        );
        return 2;
    }
    let new_sha = crate::patch::bytes_sha256(&from_bytes);
    let from_text = match std::str::from_utf8(&from_bytes) {
        Ok(t) => t,
        Err(_) => {
            eprintln!("sources: --from {}: not UTF-8 text", from.display());
            return 2;
        }
    };
    let new_upstream = match crate::spec::load(from_text, &from.to_string_lossy()) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("sources: --from {}: {}", from.display(), e);
            return 2;
        }
    };
    let (kind, version) = detect(&new_upstream);
    if kind == "unknown" {
        eprintln!(
            "sources: --from {} declares neither `openapi` nor `swagger`; only description documents are pinned",
            from.display()
        );
        return 2;
    }
    if new_sha == previous_sha {
        if args.has("json") {
            print!(
                "{}",
                crate::json::pretty(&crate::json::object(vec![
                    ("path", Value::from(rel.as_str())),
                    ("upstream", Value::from(upstream_rel.as_str())),
                    ("upstream_sha256", Value::from(new_sha.as_str())),
                    ("changed", Value::Bool(false)),
                ]))
            );
        } else {
            println!(
                "sources: {} is already pinned at this document (upstream_sha256 {}); nothing to refresh",
                rel, new_sha
            );
        }
        return 0;
    }

    // Replay.
    let replay = crate::refresh::replay(&new_upstream, &entry.patches);
    let kept = replay.kept();
    let reapplied = replay.count(crate::refresh::Fate::Reapplied);
    let obsolete = replay.count(crate::refresh::Fate::Obsolete);
    let conflicts = replay.count(crate::refresh::Fate::Conflict);
    let new_working_bytes = working_text(&rel, from_text, &replay.document, kept.len());
    // The written text must read back as the replayed document, or the
    // patches recorded next would not reproduce the working copy.
    let new_working_text = String::from_utf8_lossy(&new_working_bytes);
    match crate::spec::load(&new_working_text, &rel) {
        Ok(d) if d == replay.document => {}
        Ok(_) => {
            eprintln!(
                "sources: internal error — the working copy as written would not read back as the replayed document"
            );
            return 2;
        }
        Err(e) => {
            eprintln!(
                "sources: internal error — the working copy as written does not parse: {}",
                e
            );
            return 2;
        }
    }
    let content_sha = crate::patch::canonical_sha256(&replay.document);

    // The lock text, built and re-parsed before anything is written.
    let retrieved = args
        .get("retrieved-at")
        .map(str::to_string)
        .unwrap_or_else(crate::now_iso);
    let url = args.get("url").map(str::to_string);
    let has_legacy_sha = crate::json::get_arr(&lock, "sources")
        .map(|list| {
            list.iter().any(|s| {
                crate::json::get_str(s, "path") == Some(rel.as_str()) && s.get("sha256").is_some()
            })
        })
        .unwrap_or(false);
    let mut pairs: Vec<(&str, &str)> = vec![("kind", kind), ("version", version.as_str())];
    if let Some(u) = &url {
        pairs.push(("url", u.as_str()));
    }
    pairs.push(("retrieved_at", retrieved.as_str()));
    pairs.push(("path", rel.as_str()));
    pairs.push(("upstream", upstream_rel.as_str()));
    pairs.push(("upstream_sha256", new_sha.as_str()));
    if has_legacy_sha {
        pairs.push(("sha256", new_sha.as_str()));
    }
    let new_lock_text = match upsert_entry(&lock_text, &rel, &pairs)
        .and_then(|t| crate::sources::splice_patches(&t, &rel, &kept))
    {
        Ok(t) => t,
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    if let Err(e) = crate::yaml::parse(&new_lock_text) {
        eprintln!(
            "sources: refusing to write {}: the edited text would not parse ({})",
            SOURCES_LOCK, e
        );
        return 2;
    }

    // The inventory before the refresh, and the one the replayed document
    // describes — built now, before anything is written, so a vendor
    // document the reader rejects (a dangling `$ref`, an external one) is a
    // refusal with nothing written, in a dry run and a real one alike.
    let inventory_rel = crate::cmd::inventory::DEFAULT_INVENTORY_REL;
    let previous_inventory = match crate::factory_io::read_to_string_optional(&dir, inventory_rel) {
        Ok(Some(text)) => match crate::json::parse(&text) {
            Ok(v) => Some(v),
            Err(e) => {
                eprintln!(
                    "sources: refused — {} does not parse ({}); rebuild it before refreshing",
                    inventory_rel, e
                );
                return 2;
            }
        },
        Ok(None) => None,
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    // Whether or not the workspace has an inventory yet: a document the
    // reader cannot build one from is never made the working copy.
    // The inventory is built from every input the lock names (ADR 0138):
    // the vendor's new document stands in for the working copy, beside any
    // other pinned document and supplement, so a refresh never drops what
    // they declare and refuses an operation both now declare.
    let prebuilt = match crate::inputs::build_workspace(&dir, Some((&rel, replay.document.clone())))
        .map(|(b, _)| b)
    {
        Ok(b) => b.inventory,
        Err(e) => {
            eprintln!(
                "sources: refused — the inventory cannot be built from the refreshed document ({}); nothing was written. The vendor's new document has a defect the reader cannot pass: fix it in a copy, or refresh from a version that reads",
                e
            );
            return 2;
        }
    };

    // The finding: both hashes and every patch's fate, so a dropped
    // patch's reason survives in findings.json (ADR 0113 §3).
    let mut findings_doc = match crate::findings::load(&dir, None) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("sources: {}", e);
            return 2;
        }
    };
    let finding_id = crate::findings::new_id(&findings_doc);
    // The document's origin, as a committed file should record it: the URL
    // when given, else the fetched file's name — never a local path.
    let from_label = url.clone().unwrap_or_else(|| {
        from.file_name()
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| from.to_string_lossy().to_string())
    });
    let mut decision_context = format!(
        "`sources refresh` replaced the vendor's pinned copy {} (upstream_sha256 {}) with {} (upstream_sha256 {}; {} {}{}). Of {} recorded patch{}, {} re-applied and kept on the entry, {} obsolete (the vendor's document now says what the patch produced; dropped), {} in conflict (the vendor changed the target; dropped and not applied — re-apply the intent by hand and codify it). The working copy was rewritten from the new upstream plus the re-applied patches{}.",
        upstream_rel,
        previous_sha,
        from_label,
        new_sha,
        kind,
        version,
        if kind != entry.kind {
            format!(", previously {}", entry.kind)
        } else {
            String::new()
        },
        entry.patches.len(),
        if entry.patches.len() == 1 { "" } else { "es" },
        reapplied,
        obsolete,
        conflicts,
        if reapplied > 0 {
            "; their `verified` evidence was recorded against the previous baseline"
        } else {
            ""
        }
    );
    if !replay.outcomes.is_empty() {
        decision_context.push_str("\nPatches:\n");
        for line in crate::refresh::report_lines(&replay) {
            decision_context.push_str("- ");
            decision_context.push_str(&line);
            decision_context.push('\n');
        }
    }
    let mut evidence = vec![rel.to_string()];
    if previous_inventory.is_some() {
        evidence.push(crate::cmd::inventory::DEFAULT_INVENTORY_REL.to_string());
    }
    evidence.push(format!("sources refresh --from {}", from_label));
    let finding_record = crate::findings::NewFinding {
        title: format!("Upstream refreshed: {}", rel),
        date: crate::today(),
        body: format!("{}\n\nReason: {}", decision_context.trim_end(), reason),
        source: "sources".into(),
        evidence,
        related: related.clone(),
        ..Default::default()
    };

    let mut writes: Vec<&str> = vec![
        upstream_rel.as_str(),
        rel.as_str(),
        SOURCES_LOCK,
        crate::record_log::FINDINGS.dir,
    ];
    let applied_exists = dir.join(crate::spans::LOCK_FILE).exists();
    if previous_inventory.is_some() {
        writes.push(crate::cmd::inventory::DEFAULT_INVENTORY_REL);
    }
    if applied_exists {
        writes.push(crate::spans::LOCK_FILE);
    }

    let mut inventory_diff: Option<Value> = previous_inventory
        .as_ref()
        .map(|before| crate::inventory::diff_inventories(before, &prebuilt));
    let mut acknowledged = false;
    let mut provenance_recorded: Option<bool> = None;
    if !dry_run {
        // From the first write on, a failure is exit 4 with the list of what
        // was written — never 2, which promises nothing was.
        let mut written: Vec<&str> = Vec::new();
        let json = args.has("json");
        let incomplete = |written: &[&str], what: &str, e: &dyn std::fmt::Display| -> i32 {
            eprintln!(
                "sources: INCOMPLETE — {} ({}); written so far: {}. Restore the workspace (git checkout -- {}) and refresh again",
                what,
                e,
                written.join(", "),
                written.join(" ")
            );
            if json {
                print!(
                    "{}",
                    crate::json::pretty(&crate::json::object(vec![
                        ("path", Value::from(rel.as_str())),
                        ("exit", Value::from(4)),
                        ("incomplete", Value::from(format!("{} ({})", what, e))),
                        (
                            "written",
                            Value::Array(written.iter().map(|w| Value::from(*w)).collect()),
                        ),
                    ]))
                );
            }
            4
        };
        if let Err(e) = crate::factory_io::create_parent_dir(&dir, &upstream_rel) {
            return incomplete(
                &written,
                &format!("{} could not be written", upstream_rel),
                &e,
            );
        }
        // The first write may have truncated the file before failing: from
        // here on the workspace is not what it was.
        written.push(upstream_rel.as_str());
        if let Err(e) = crate::factory_io::write_in_place(&dir, &upstream_rel, &from_bytes) {
            return incomplete(
                &written,
                &format!("{} could not be written", upstream_rel),
                &e,
            );
        }
        if let Err(e) = std::fs::write(dir.join(&rel), &new_working_bytes) {
            return incomplete(&written, &format!("{} could not be written", rel), &e);
        }
        written.push(rel.as_str());
        if let Err(e) =
            crate::factory_io::write_in_place(&dir, SOURCES_LOCK, new_lock_text.as_bytes())
        {
            return incomplete(
                &written,
                &format!("{} could not be written", SOURCES_LOCK),
                &e,
            );
        }
        written.push(SOURCES_LOCK);
        if let Err(e) = crate::findings::add_with_id(&mut findings_doc, &finding_id, finding_record)
        {
            return incomplete(&written, "the finding could not be recorded", &e);
        }
        if let Err(e) = crate::findings::save(&dir, &findings_doc, None) {
            return incomplete(&written, "the finding could not be written", &e);
        }
        written.push(crate::record_log::FINDINGS.dir);
        if let Some(before) = &previous_inventory {
            // The same inventory, now with the patch marks the files on disk
            // let the builder see; the pre-built one stands in if that
            // reading fails (it has already been proven buildable).
            let after = match crate::inputs::build_workspace(&dir, None) {
                Ok((b, _)) => b.inventory,
                Err(e) => {
                    eprintln!(
                        "sources: warning: the inventory was rebuilt without patch marks ({}); run `graphos-factory-core inventory build {}` to add them",
                        e, crate::inputs::rebuild_target(&dir, ".", &rel)
                    );
                    prebuilt.clone()
                }
            };
            if let Err(e) = crate::factory_io::write_in_place(
                &dir,
                inventory_rel,
                crate::json::pretty(&after).as_bytes(),
            ) {
                return incomplete(
                    &written,
                    &format!("{} could not be written", inventory_rel),
                    &e,
                );
            }
            written.push(inventory_rel);
            inventory_diff = Some(crate::inventory::diff_inventories(before, &after));
        }
        // Record the applied source and provenance once, after the inventory
        // write, so the hashes describe the final refresh output.
        match crate::spans::read_lock(&dir) {
            Ok(Some(mut applied)) => {
                let mut sources = crate::json::get(&applied, "sources")
                    .cloned()
                    .unwrap_or(Value::Object(crate::json::obj()));
                crate::json::set(&mut sources, &rel, Value::from(content_sha.as_str()));
                crate::json::set(&mut applied, "sources", sources);
                crate::json::set(&mut applied, "written_at", Value::from(crate::now_iso()));
                let provenance_error =
                    crate::provenance::refresh_partial(&dir, &mut applied, None, args.get("model"));
                provenance_recorded = Some(provenance_error.is_none());
                if let Some(e) = provenance_error {
                    eprintln!(
                        "sources: warning: authoring provenance was omitted because it could not be recorded: {}",
                        e
                    );
                }
                if let Err(e) = crate::spans::write_lock(&dir, &applied) {
                    return incomplete(
                        &written,
                        &format!("{} could not be written", crate::spans::LOCK_FILE),
                        &e,
                    );
                }
                written.push(crate::spans::LOCK_FILE);
                acknowledged = true;
            }
            Ok(None) => {}
            Err(e) => {
                return incomplete(
                    &written,
                    &format!("{} could not be read", crate::spans::LOCK_FILE),
                    &e,
                )
            }
        }
    }

    let code = if conflicts > 0 { 3 } else { 0 };
    let summary = crate::json::object(vec![
        ("path", Value::from(rel.as_str())),
        ("upstream", Value::from(upstream_rel.as_str())),
        ("from", Value::from(from.to_string_lossy().as_ref())),
        ("kind", Value::from(kind)),
        ("version", Value::from(version.as_str())),
        (
            "previous_upstream_sha256",
            Value::from(previous_sha.as_str()),
        ),
        ("upstream_sha256", Value::from(new_sha.as_str())),
        ("content_sha256", Value::from(content_sha.as_str())),
        ("finding", Value::from(finding_id.as_str())),
        (
            "patches",
            crate::json::object(vec![
                ("recorded", Value::from(entry.patches.len())),
                ("reapplied", Value::from(reapplied)),
                ("obsolete", Value::from(obsolete)),
                ("conflicts", Value::from(conflicts)),
                (
                    "outcomes",
                    Value::Array(replay.outcomes.iter().map(|o| o.to_value()).collect()),
                ),
            ]),
        ),
        ("inventory", inventory_diff.clone().unwrap_or(Value::Null)),
        ("acknowledged_in_applied_lock", Value::Bool(acknowledged)),
        (
            "provenance_recorded",
            provenance_recorded.map(Value::Bool).unwrap_or(Value::Null),
        ),
        ("dry_run", Value::Bool(dry_run)),
        (
            "writes",
            Value::Array(writes.iter().map(|w| Value::from(*w)).collect()),
        ),
        ("exit", Value::from(code)),
    ]);
    if args.has("json") {
        print!("{}", crate::json::pretty(&summary));
        return code;
    }
    println!(
        "sources{}: refreshed {} — upstream {} {} → {} ({} {}){}",
        if dry_run { " (dry run)" } else { "" },
        rel,
        upstream_rel,
        &previous_sha[..12.min(previous_sha.len())],
        &new_sha[..12.min(new_sha.len())],
        kind,
        version,
        if dry_run {
            format!(
                "; would record a finding ({} here; the real run draws its own id)",
                finding_id
            )
        } else {
            format!("; finding {} recorded", finding_id)
        }
    );
    println!(
        "  {} recorded patch{}: {} re-applied, {} obsolete, {} in conflict",
        entry.patches.len(),
        if entry.patches.len() == 1 { "" } else { "es" },
        reapplied,
        obsolete,
        conflicts
    );
    for line in crate::refresh::report_lines(&replay) {
        println!("  {}", line);
    }
    if conflicts > 0 {
        println!(
            "  a conflicting patch is dropped and not applied: re-apply its intent to {} by hand, then `graphos-factory-core codify --source {} --reason R` (its reason is in {})",
            rel, rel, finding_id
        );
    }
    match &inventory_diff {
        Some(d) => {
            let n = |k: &str| crate::json::get_arr(d, k).map(|a| a.len()).unwrap_or(0);
            println!(
                "  inventory: {} added, {} removed, {} changed{}",
                n("added"),
                n("removed"),
                n("changed"),
                if dry_run {
                    " (not written)"
                } else {
                    " (.factory/inventory.json rebuilt)"
                }
            );
            for k in crate::json::get_arr(d, "added").into_iter().flatten() {
                println!("    added:   {}", k.as_str().unwrap_or(""));
            }
            for k in crate::json::get_arr(d, "removed").into_iter().flatten() {
                println!("    removed: {}", k.as_str().unwrap_or(""));
            }
            for c in crate::json::get_arr(d, "changed").into_iter().flatten() {
                let fields: Vec<&str> = crate::json::get_arr(c, "fields")
                    .map(|f| f.iter().filter_map(Value::as_str).collect())
                    .unwrap_or_default();
                println!(
                    "    changed: {} ({})",
                    crate::json::get_str(c, "key").unwrap_or(""),
                    fields.join(", ")
                );
            }
            for e in crate::json::get_arr(d, "api").into_iter().flatten() {
                println!("    {}", crate::inventory::api_change_line(e));
            }
        }
        None => println!(
            "  no {} to diff — run `graphos-factory-core inventory build {}`",
            crate::cmd::inventory::DEFAULT_INVENTORY_REL,
            crate::inputs::rebuild_target(&dir, ".", &rel)
        ),
    }
    if dry_run {
        println!(
            "  nothing written (--dry-run); would write: {}",
            writes.join(", ")
        );
    } else {
        println!(
            "  {}; next: graphos-factory-core reconcile {} — take the diff to the user, update selection.yaml, then apply",
            if acknowledged {
                "working copy acknowledged in applied.lock.yaml"
            } else {
                "no applied.lock.yaml yet — run `graphos-factory-core lock` after the first apply"
            },
            args.dir()
        );
    }
    code
}
