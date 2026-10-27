//! The inventory's inputs: every document the sources lock names, merged
//! into one OpenAPI 3 model before the one build reads it (ADR 0138).
//!
//! A pinned description document (`kind: openapi` / `swagger`) is the
//! vendor's, corrected by patches. A supplement (`kind: supplement`) is the
//! agent's own OpenAPI 3 document of what the vendor's document lacks, read
//! from its documentation or from probes: it has no upstream copy and is
//! edited directly, and every operation in it names where it was read
//! (`x-source`). Each operation in the built inventory carries the
//! provenance of the input that declared it.

use crate::json::{get, get_arr, get_obj, get_str};
use crate::openapi::Built;
use crate::spec::Format;
use serde_json::Value;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// The sources lock kind of an agent-written document.
pub const SUPPLEMENT_KIND: &str = "supplement";
/// What a supplement may say its operations were read from.
pub const SUPPLEMENT_PROVENANCE: [&str; 2] = ["docs", "probe"];

/// One input to the build, as the sources lock names it.
#[derive(Debug, Clone)]
pub struct Input {
    /// Workspace-relative path (or the path as named on the command line).
    pub path: String,
    /// `openapi`, `swagger`, or `supplement`.
    pub kind: String,
    /// `spec` for a pinned document; a supplement's own `docs` or `probe`.
    pub provenance: String,
    /// The JSON Patch operations a pinned document's entry records.
    pub patches: Vec<Value>,
}

impl Input {
    pub fn is_supplement(&self) -> bool {
        self.kind == SUPPLEMENT_KIND
    }
}

/// The confidence an operation of this provenance carries
/// (`api-discovery.md`: a spec or a probe is 1, documentation 0.9).
pub fn confidence(provenance: &str) -> Value {
    match provenance {
        "docs" => Value::from(0.9),
        _ => Value::from(1),
    }
}

/// The supplement entries of a sources lock, in lock order. An entry with
/// no `path`, or a `provenance` other than `docs` / `probe`, is skipped
/// here and reported by lint against the lock's schema.
pub fn supplement_entries(lock: &Value) -> Vec<Input> {
    get_arr(lock, "sources")
        .into_iter()
        .flatten()
        .filter(|s| get_str(s, "kind") == Some(SUPPLEMENT_KIND))
        .filter_map(|s| {
            let provenance = get_str(s, "provenance")?;
            if !SUPPLEMENT_PROVENANCE.contains(&provenance) {
                return None;
            }
            Some(Input {
                path: get_str(s, "path")?.trim_start_matches("./").to_string(),
                kind: SUPPLEMENT_KIND.to_string(),
                provenance: provenance.to_string(),
                patches: vec![],
            })
        })
        .collect()
}

/// Every input a workspace's inventory is built from: the pinned documents
/// the lock follows, then the supplements, each in lock order. Empty when
/// there is no sources lock or it names neither.
pub fn workspace_inputs(dir: &Path) -> Result<Vec<Input>, String> {
    let lock = match crate::sources::read_sources_lock(dir)? {
        Some(l) => l,
        None => return Ok(vec![]),
    };
    let entries = crate::sources::document_entries(&lock);
    let mut inputs: Vec<Input> = entries
        .iter()
        .filter(|e| crate::sources::not_followed(e, &entries).is_none())
        .map(|e| Input {
            path: e.path.clone(),
            kind: e.kind.clone(),
            provenance: "spec".to_string(),
            patches: e.patches.clone(),
        })
        .collect();
    for s in supplement_entries(&lock) {
        if !crate::sources::inside_workspace(&s.path)
            || s.path.starts_with(crate::sources::UPSTREAM_DIR)
            || s.path.starts_with(".factory/")
        {
            return Err(format!(
                "{}: the supplement {} must be a workspace file outside .factory/",
                crate::sources::SOURCES_LOCK,
                s.path
            ));
        }
        if inputs.iter().any(|i| i.path == s.path) {
            return Err(format!(
                "{}: {} is named twice — one entry per document",
                crate::sources::SOURCES_LOCK,
                s.path
            ));
        }
        inputs.push(s);
    }
    Ok(inputs)
}

/// The input a document named on the command line is: its sources lock
/// entry when an enclosing workspace pins it, else a plain spec. Returns the
/// workspace root it was found in, for resolving a supplement's citations.
pub fn input_for_file(file: &Path) -> (Input, Option<PathBuf>) {
    let abs = std::fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let plain = Input {
        path: file.to_string_lossy().to_string(),
        kind: String::new(),
        provenance: "spec".to_string(),
        patches: vec![],
    };
    let mut root = abs.parent().map(Path::to_path_buf);
    while let Some(dir) = root {
        if dir.join(crate::sources::SOURCES_LOCK).exists() {
            let lock = match crate::sources::read_sources_lock(&dir) {
                Ok(Some(l)) => l,
                _ => return (plain, None),
            };
            let rel = match abs.strip_prefix(&dir) {
                Ok(r) => r.to_string_lossy().replace('\\', "/"),
                Err(_) => return (plain, None),
            };
            if let Some(s) = supplement_entries(&lock)
                .into_iter()
                .find(|s| s.path == rel)
            {
                return (
                    Input {
                        path: file.to_string_lossy().to_string(),
                        ..s
                    },
                    Some(dir),
                );
            }
            let entries = crate::sources::document_entries(&lock);
            if let Some(e) = entries
                .iter()
                .find(|e| e.path == rel && crate::sources::not_followed(e, &entries).is_none())
            {
                return (
                    Input {
                        path: file.to_string_lossy().to_string(),
                        kind: e.kind.clone(),
                        provenance: "spec".to_string(),
                        patches: e.patches.clone(),
                    },
                    Some(dir),
                );
            }
            return (plain, Some(dir));
        }
        root = dir.parent().map(Path::to_path_buf);
    }
    (plain, None)
}

/// A document read into the OpenAPI 3 model, with the input it came from.
pub struct Loaded {
    pub input: Input,
    pub document: Value,
    pub format: Format,
    pub warnings: Vec<String>,
}

/// Read one input's text into the OpenAPI 3 model. A supplement must be
/// OpenAPI 3 and must cite a source for every operation; `root` resolves a
/// citation that is a workspace path.
pub fn load(input: Input, text: &str, root: Option<&Path>) -> Result<Loaded, String> {
    let loaded = crate::spec::read(text, &input.path)?;
    if input.is_supplement() {
        if !matches!(loaded.format, Format::OpenApi(_)) {
            return Err(format!(
                "{}: a supplement is an OpenAPI 3 document (`openapi: 3.x`), not {}",
                input.path, loaded.format
            ));
        }
        check_citations(&input.path, &loaded.document, root)?;
    }
    Ok(Loaded {
        input,
        document: loaded.document,
        format: loaded.format,
        warnings: loaded.warnings,
    })
}

/// Every operation in a supplement names where it was read: `x-source`, a
/// URL or a workspace path that exists. An uncited operation is a guess
/// nobody can check, so the build refuses it.
fn check_citations(label: &str, doc: &Value, root: Option<&Path>) -> Result<(), String> {
    let mut problems: Vec<String> = Vec::new();
    for (path, item) in get_obj(doc, "paths").into_iter().flatten() {
        for method in crate::http_methods() {
            let Some(op) = get(item, method) else {
                continue;
            };
            let key = format!("{}:{}", method, path);
            match get_str(op, "x-source").map(str::trim) {
                None | Some("") => problems.push(format!("{} has no x-source", key)),
                Some(s) if s.contains("://") => {}
                Some(s) => {
                    // Existence only, never followed: a sample directory
                    // under `.factory/` is a fine citation.
                    let exists = crate::sources::inside_workspace(s)
                        && root
                            .map(|r| std::fs::symlink_metadata(r.join(s)).is_ok())
                            .unwrap_or(true);
                    if !exists {
                        problems.push(format!(
                            "{} cites {}, which is not in the workspace",
                            key, s
                        ));
                    }
                }
            }
        }
    }
    if problems.is_empty() {
        return Ok(());
    }
    let shown: Vec<&str> = problems.iter().take(10).map(String::as_str).collect();
    Err(format!(
        "{}: {} operation(s) do not name where they were read (x-source: a URL, or the workspace path of the pinned page or sample): {}{}",
        label,
        problems.len(),
        shown.join("; "),
        if problems.len() > shown.len() { "; …" } else { "" }
    ))
}

/// The OpenAPI 3 document the inputs make together, and the input index
/// that declared each `method:path`. The first input gives `info`,
/// `servers`, the top-level `security` and every other top-level key;
/// paths and components are merged, and the same operation, or a
/// differently defined component of one name, in two inputs is refused.
pub fn merge(loaded: &[Loaded]) -> Result<(Value, HashMap<String, usize>), String> {
    let mut owner: HashMap<String, usize> = HashMap::new();
    let mut merged = loaded
        .first()
        .map(|l| l.document.clone())
        .ok_or_else(|| "no input to build from".to_string())?;
    for (i, l) in loaded.iter().enumerate() {
        for (path, item) in get_obj(&l.document, "paths").into_iter().flatten() {
            for method in crate::http_methods() {
                if get(item, method).is_some() {
                    let key = format!("{}:{}", method, path);
                    if let Some(&j) = owner.get(&key) {
                        return Err(format!(
                            "{} is declared by both {} and {} — a supplement adds what a document lacks; a correction to a pinned document is a patch to it (codify --source). Remove one",
                            key, loaded[j].input.path, l.input.path
                        ));
                    }
                    owner.insert(key, i);
                }
            }
        }
    }
    for l in loaded.iter().skip(1) {
        let label = &l.input.path;
        if let Some(paths) = get_obj(&l.document, "paths") {
            let target = ensure_object(&mut merged, "paths");
            for (path, item) in paths {
                match target.get_mut(path) {
                    None => {
                        target.insert(path.clone(), item.clone());
                    }
                    Some(existing) => merge_path_item(path, existing, item, label)?,
                }
            }
        }
        if let Some(components) = get_obj(&l.document, "components") {
            for (section, defs) in components {
                let Some(defs) = defs.as_object() else {
                    continue;
                };
                let comps = ensure_object(&mut merged, "components");
                let target = comps
                    .entry(section.clone())
                    .or_insert_with(|| Value::Object(Default::default()));
                let Some(target) = target.as_object_mut() else {
                    return Err(format!("components.{} is not an object", section));
                };
                for (name, def) in defs {
                    match target.get(name) {
                        None => {
                            target.insert(name.clone(), def.clone());
                        }
                        Some(existing) if crate::json::deep_equal(existing, def) => {}
                        Some(_) => {
                            return Err(format!(
                                "components.{}.{} is defined differently in {} and an earlier input — give the supplement's its own name, or $ref the pinned document's",
                                section, name, label
                            ))
                        }
                    }
                }
            }
        }
        if let Some(tags) = get_arr(&l.document, "tags") {
            let target = ensure_object(&mut merged, "")
                .entry("tags".to_string())
                .or_insert_with(|| Value::Array(vec![]));
            if let Some(arr) = target.as_array_mut() {
                for t in tags {
                    let name = get_str(t, "name");
                    if !arr.iter().any(|x| get_str(x, "name") == name) {
                        arr.push(t.clone());
                    }
                }
            }
        }
    }
    Ok((merged, owner))
}

/// `doc[key]` as a mutable object, created when absent (`""` is `doc`).
fn ensure_object<'a>(doc: &'a mut Value, key: &str) -> &'a mut serde_json::Map<String, Value> {
    if !doc.is_object() {
        *doc = Value::Object(Default::default());
    }
    let root = doc.as_object_mut().expect("object");
    if key.is_empty() {
        return root;
    }
    let v = root
        .entry(key.to_string())
        .or_insert_with(|| Value::Object(Default::default()));
    if !v.is_object() {
        *v = Value::Object(Default::default());
    }
    v.as_object_mut().expect("object")
}

/// Two inputs declaring operations under one path: their methods are
/// disjoint (checked by the caller); every path-level key (`parameters`,
/// `summary`, `servers`, …) must agree, because it applies to every
/// operation under the path, the other input's included.
fn merge_path_item(
    path: &str,
    existing: &mut Value,
    item: &Value,
    label: &str,
) -> Result<(), String> {
    let methods = crate::http_methods();
    let (Some(target), Some(source)) = (existing.as_object_mut(), item.as_object()) else {
        return Err(format!("paths.{} is not an object", path));
    };
    for (k, v) in source {
        if methods.contains(&k.as_str()) {
            target.insert(k.clone(), v.clone());
        } else if target
            .get(k)
            .map(|e| !crate::json::deep_equal(e, v))
            .unwrap_or(true)
        {
            return Err(format!(
                "paths.{}: {} gives the path its own `{}`, which would apply to an earlier input's operations too — declare it on the operation instead",
                path, label, k
            ));
        }
    }
    let shared: Vec<String> = target
        .keys()
        .filter(|k| !methods.contains(&k.as_str()))
        .cloned()
        .collect();
    for k in shared {
        if !source.contains_key(&k) {
            return Err(format!(
                "paths.{}: an earlier input gives the path `{}`, which would apply to {}'s operations too — declare it on the operation instead",
                path, k, label
            ));
        }
    }
    Ok(())
}

/// Build one inventory from inputs already read: merged when there is more
/// than one, then each operation and `unresolved` entry stamped with its
/// input's provenance and confidence, then each pinned document's patches
/// marked. One spec input builds exactly what a single document always did.
pub fn build(loaded: Vec<Loaded>) -> Result<(Built, Vec<Format>), String> {
    let formats: Vec<Format> = loaded.iter().map(|l| l.format.clone()).collect();
    let mut warnings: Vec<String> = Vec::new();
    for l in &loaded {
        for w in &l.warnings {
            warnings.push(if loaded.len() > 1 {
                format!("{}: {}", l.input.path, w)
            } else {
                w.clone()
            });
        }
    }
    let single_spec = loaded.len() == 1 && !loaded[0].input.is_supplement();
    let (mut built, owner) = if single_spec {
        (
            crate::openapi::build_inventory(&loaded[0].document)?,
            HashMap::new(),
        )
    } else {
        let (doc, owner) = merge(&loaded)?;
        (crate::openapi::build_inventory(&doc)?, owner)
    };
    warnings.append(&mut built.warnings);
    built.warnings = warnings;
    if !single_spec {
        stamp_provenance(&mut built.inventory, &loaded, &owner);
    }
    for l in &loaded {
        if !l.input.patches.is_empty() {
            crate::cmd::inventory::annotate_with(
                &mut built.inventory,
                &l.input.kind,
                &l.input.patches,
                &built.shape_names,
            );
        }
    }
    Ok((built, formats))
}

/// The operation object `key` (`method:path`) names in a document.
fn raw_operation<'a>(doc: &'a Value, key: &str) -> Option<&'a Value> {
    let (method, path) = key.split_once(':')?;
    get_obj(doc, "paths")?
        .get(path)
        .and_then(|item| get(item, method))
}

/// What a supplement may state about an operation that the OpenAPI model
/// cannot: `x-semantics` (`read` / `write`, when the cited page says a GET
/// changes state or a POST only reads) and `x-confidence` (lower than its
/// provenance's maximum, when the page is vague). Both are read from the
/// supplement, never from a pinned document.
fn supplement_overrides(op: &mut Value, raw: &Value, provenance: &str) {
    if let Some(sem) = get_str(raw, "x-semantics").filter(|s| *s == "read" || *s == "write") {
        crate::json::set(op, "semantics", Value::from(sem));
        // The page settles what the name only hinted at.
        crate::json::remove(op, "read_hint");
    }
    if let Some(c) = get(raw, "x-confidence").and_then(Value::as_f64) {
        let max = confidence(provenance).as_f64().unwrap_or(1.0);
        if c > 0.0 && c < max {
            crate::json::set(
                op,
                "confidence",
                get(raw, "x-confidence").cloned().unwrap_or(Value::Null),
            );
        }
    }
}

/// Each operation's provenance and confidence from the input that declared
/// it (with a supplement's own overrides), each `unresolved` entry's
/// `source` likewise (an entry's hint is the path, `method path`, or the
/// operation key), and, when a supplement is the first input, the API's
/// auth: its provenance and confidence, `x-prefix` on a scheme as the
/// credential's prefix, and a document-level `security: []` with no
/// scheme as `kind: none`.
fn stamp_provenance(inventory: &mut Value, loaded: &[Loaded], owner: &HashMap<String, usize>) {
    let provenance_of = |key: &str| -> Option<&str> {
        owner.get(key).map(|&i| loaded[i].input.provenance.as_str())
    };
    if let Some(Value::Array(ops)) = inventory.get_mut("operations") {
        for op in ops.iter_mut() {
            let key = get_str(op, "key").unwrap_or("").to_string();
            if let Some(&i) = owner.get(&key) {
                let p = loaded[i].input.provenance.clone();
                crate::json::set(op, "confidence", confidence(&p));
                crate::json::set(op, "provenance", Value::from(p.as_str()));
                if loaded[i].input.is_supplement() {
                    if let Some(raw) = raw_operation(&loaded[i].document, &key) {
                        supplement_overrides(op, raw, &p);
                    }
                }
            }
        }
    }
    if let Some(primary) = loaded.first().filter(|l| l.input.is_supplement()) {
        let schemes =
            get(&primary.document, "components").and_then(|c| get_obj(c, "securitySchemes"));
        let no_auth = schemes.map(|s| s.is_empty()).unwrap_or(true)
            && get_arr(&primary.document, "security")
                .map(|a| a.is_empty())
                .unwrap_or(false);
        let source = Value::from(primary.input.provenance.as_str());
        if let Some(api) = inventory.get_mut("api") {
            if no_auth {
                crate::json::set(
                    api,
                    "auth",
                    Value::Array(vec![crate::json::object(vec![
                        ("kind", Value::from("none")),
                        ("source", source.clone()),
                        ("confidence", confidence(&primary.input.provenance)),
                    ])]),
                );
            } else if let Some(Value::Array(entries)) = api.get_mut("auth") {
                for e in entries.iter_mut() {
                    crate::json::set(e, "source", source.clone());
                    crate::json::set(e, "confidence", confidence(&primary.input.provenance));
                    let prefix = get_str(e, "scheme_name")
                        .and_then(|n| schemes.and_then(|s| s.get(n)))
                        .and_then(|s| get(s, "x-prefix"))
                        .cloned();
                    if let Some(p) = prefix {
                        crate::json::set(e, "prefix", p);
                    }
                }
            }
        }
    }
    if let Some(Value::Array(entries)) = inventory.get_mut("unresolved") {
        for e in entries.iter_mut() {
            let hint = get_str(e, "hint").unwrap_or("").to_string();
            let by_key = provenance_of(&hint).or_else(|| {
                hint.split_once(' ')
                    .and_then(|(m, p)| provenance_of(&format!("{}:{}", m, p)))
            });
            let by_path = || {
                owner
                    .iter()
                    .find(|(k, _)| k.split_once(':').map(|(_, p)| p) == Some(hint.as_str()))
                    .map(|(_, &i)| loaded[i].input.provenance.as_str())
            };
            if let Some(p) = by_key.or_else(by_path) {
                let p = p.to_string();
                crate::json::set(e, "source", Value::from(p));
            }
        }
    }
}

/// What `inventory build` is given to rebuild a workspace after its pinned
/// document `rel` changed: the document itself when it is the workspace's
/// only input (the command as it always was), else the workspace directory
/// (`shown_dir`), so the build reads every input and drops none.
pub fn rebuild_target(dir: &Path, shown_dir: &str, rel: &str) -> String {
    let inputs = workspace_inputs(dir).map(|v| v.len()).unwrap_or(1);
    if inputs > 1 {
        shown_dir.to_string()
    } else if shown_dir == "." {
        rel.to_string()
    } else {
        format!("{}/{}", shown_dir, rel)
    }
}

/// Read every named file and build: each file's provenance and patches come
/// from the sources lock entry of the workspace that pins it.
pub fn build_files(files: &[PathBuf]) -> Result<(Built, Vec<Format>), String> {
    let mut loaded = Vec::new();
    for f in files {
        let text = std::fs::read_to_string(f).map_err(|e| format!("{}: {}", f.display(), e))?;
        let (input, root) = input_for_file(f);
        loaded.push(load(input, &text, root.as_deref())?);
    }
    build(loaded)
}

/// Build the inventory of the workspace at `dir` from every input its
/// sources lock names. `replace` substitutes one input's document (a
/// refresh checking the vendor's new version before writing it).
pub fn build_workspace(
    dir: &Path,
    replace: Option<(&str, Value)>,
) -> Result<(Built, Vec<Format>), String> {
    let inputs = workspace_inputs(dir)?;
    if inputs.is_empty() {
        return Err(format!(
            "{} names no document to build from — pin the vendor's description (sources pin) or add a supplement entry (kind: supplement)",
            crate::sources::SOURCES_LOCK
        ));
    }
    let mut loaded = Vec::new();
    for input in inputs {
        match &replace {
            Some((path, doc)) if *path == input.path => {
                let l = crate::spec::normalise(doc.clone())?;
                loaded.push(Loaded {
                    input,
                    document: l.document,
                    format: l.format,
                    warnings: l.warnings,
                });
            }
            _ => {
                let text = std::fs::read_to_string(dir.join(&input.path))
                    .map_err(|e| format!("{}: {}", input.path, e))?;
                loaded.push(load(input, &text, Some(dir))?);
            }
        }
    }
    build(loaded)
}
