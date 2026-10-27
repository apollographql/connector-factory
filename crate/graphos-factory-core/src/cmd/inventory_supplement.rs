//! inventory supplement — move an inventory's operations that no pinned
//! document declares into a supplement, the OpenAPI 3 document the
//! inventory is then built from beside the pinned ones (ADR 0138).
//!
//!   inventory supplement [workspace] --provenance docs|probe [--out FILE]
//!       [--cite TEMPLATE] [--note TEXT] [--dry-run] [--force] [--json]
//!
//! It writes the document (the operations of that provenance, the shapes
//! only they reach, `$ref`s to the components a pinned document declares)
//! and its `kind: supplement` entry in sources.lock.yaml, then builds from
//! every input and compares the result with the current inventory,
//! operation by operation and shape by shape. It never writes the
//! inventory: `inventory build` does, once the difference is understood.
//! `--cite` fills each operation's `x-source` from `{path}` (the path
//! without its leading `/`), `{operation_id}` and `{method}`.

use crate::args::{Args, Flags};
use crate::inputs::{Input, Loaded, SUPPLEMENT_KIND, SUPPLEMENT_PROVENANCE};
use crate::json::{get, get_arr, get_obj, get_str};
use serde_json::{Map, Value};
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::Path;

pub const USAGE: &str = "usage: inventory supplement [workspace] --provenance docs|probe [--out FILE] [--cite TEMPLATE] [--note TEXT] [--dry-run] [--force] [--json]";

pub const FLAGS: Flags = Flags {
    boolean: &["dry-run", "force", "json"],
    valued: &["provenance", "out", "cite", "note"],
};

const DEFAULT_OUT: &str = "supplement.openapi.json";

fn fail(message: &str) -> i32 {
    eprintln!("inventory supplement: {}", message);
    2
}

/// `#/shapes/X` refs rewritten to the component each names in the document
/// being written: a pinned document's own component, or the supplement's.
fn rewrite_refs(v: &Value, provided: &HashMap<String, String>) -> Value {
    match v {
        Value::Object(o) => {
            let mut out = Map::new();
            for (k, x) in o {
                let x = match (k.as_str(), x.as_str()) {
                    ("$ref", Some(r)) if r.starts_with("#/shapes/") => {
                        let name = &r["#/shapes/".len()..];
                        Value::from(
                            provided
                                .get(name)
                                .cloned()
                                .unwrap_or_else(|| format!("#/components/schemas/{}", name)),
                        )
                    }
                    _ => rewrite_refs(x, provided),
                };
                out.insert(k.clone(), x);
            }
            Value::Object(out)
        }
        Value::Array(a) => Value::Array(a.iter().map(|x| rewrite_refs(x, provided)).collect()),
        other => other.clone(),
    }
}

fn schema_ref(shape_ref: Option<&str>, provided: &HashMap<String, String>) -> Option<Value> {
    shape_ref.map(|r| {
        rewrite_refs(
            &crate::json::object(vec![("$ref", Value::from(r))]),
            provided,
        )
    })
}

/// One inventory operation as the OpenAPI 3 operation the reader turns
/// back into it, with `x-semantics` / `x-confidence` where the inventory
/// says what the method and the provenance alone would not.
fn operation(
    op: &Value,
    provided: &HashMap<String, String>,
    cite: Option<&str>,
    provenance: &str,
) -> Value {
    let mut out = Map::new();
    let copy = |out: &mut Map<String, Value>, from: &str, to: &str| {
        if let Some(v) = get(op, from).filter(|v| !v.is_null()) {
            out.insert(to.to_string(), v.clone());
        }
    };
    copy(&mut out, "operation_id", "operationId");
    copy(&mut out, "summary", "summary");
    copy(&mut out, "description", "description");
    if get(op, "deprecated") == Some(&Value::Bool(true)) {
        out.insert("deprecated".into(), Value::Bool(true));
    }
    if get_arr(op, "tags").map(|t| !t.is_empty()).unwrap_or(false) {
        copy(&mut out, "tags", "tags");
    }
    copy(&mut out, "security", "security");
    let params: Vec<Value> = get_arr(op, "parameters")
        .into_iter()
        .flatten()
        .map(|p| {
            let mut q = Map::new();
            for k in ["name", "in"] {
                if let Some(v) = get(p, k) {
                    q.insert(k.into(), v.clone());
                }
            }
            if get(p, "required") == Some(&Value::Bool(true)) {
                q.insert("required".into(), Value::Bool(true));
            }
            for k in ["description", "explode", "style"] {
                if let Some(v) = get(p, k).filter(|v| !v.is_null()) {
                    q.insert(k.into(), v.clone());
                }
            }
            let schema = schema_ref(get_str(p, "shape_ref"), provided).unwrap_or_else(|| {
                let mut s = Map::new();
                for k in ["type", "format", "enum", "default", "minimum", "maximum"] {
                    if let Some(v) = get(p, k).filter(|v| !v.is_null()) {
                        s.insert(k.into(), v.clone());
                    }
                }
                Value::Object(s)
            });
            q.insert("schema".into(), schema);
            Value::Object(q)
        })
        .collect();
    if !params.is_empty() {
        out.insert("parameters".into(), Value::Array(params));
    }
    if let Some(rb) = get(op, "request_body").filter(|v| !v.is_null()) {
        let ct = get_str(rb, "content_type").unwrap_or("application/json");
        let mut media = Map::new();
        if let Some(s) = schema_ref(get_str(rb, "shape_ref"), provided) {
            media.insert("schema".into(), s);
        }
        let mut body = Map::new();
        if get(rb, "required") == Some(&Value::Bool(true)) {
            body.insert("required".into(), Value::Bool(true));
        }
        body.insert(
            "content".into(),
            crate::json::object(vec![(ct, Value::Object(media))]),
        );
        out.insert("requestBody".into(), Value::Object(body));
    }
    let mut responses = Map::new();
    let mut response = |r: &Value, fallback_status: &str| {
        let status = get_str(r, "status").unwrap_or(fallback_status).to_string();
        let mut entry = Map::new();
        entry.insert(
            "description".into(),
            get(r, "description")
                .filter(|v| !v.is_null())
                .cloned()
                .unwrap_or_else(|| Value::from("")),
        );
        if let Some(s) = schema_ref(get_str(r, "shape_ref"), provided) {
            let ct = get_str(r, "content_type").unwrap_or("application/json");
            entry.insert(
                "content".into(),
                crate::json::object(vec![(ct, crate::json::object(vec![("schema", s)]))]),
            );
        }
        responses.insert(status, Value::Object(entry));
    };
    if let Some(r) = get(op, "response").filter(|v| !v.is_null()) {
        response(r, "200");
    }
    for e in get_arr(op, "errors").into_iter().flatten() {
        response(e, "default");
    }
    if responses.is_empty() {
        responses.insert(
            "200".into(),
            crate::json::object(vec![("description", Value::from(""))]),
        );
    }
    out.insert("responses".into(), Value::Object(responses));
    let method = get_str(op, "method").unwrap_or("").to_lowercase();
    let derived = if method == "get" || method == "head" {
        "read"
    } else {
        "write"
    };
    if let Some(sem) = get_str(op, "semantics").filter(|s| *s != derived) {
        out.insert("x-semantics".into(), Value::from(sem));
    }
    let max = crate::inputs::confidence(provenance)
        .as_f64()
        .unwrap_or(1.0);
    if let Some(c) = get(op, "confidence").filter(|c| c.as_f64().map(|x| x < max).unwrap_or(false))
    {
        out.insert("x-confidence".into(), c.clone());
    }
    if let Some(t) = cite {
        let path = get_str(op, "path").unwrap_or("");
        out.insert(
            "x-source".into(),
            Value::from(
                t.replace("{path}", path.trim_start_matches('/'))
                    .replace("{operation_id}", get_str(op, "operation_id").unwrap_or(""))
                    .replace(
                        "{method}",
                        &get_str(op, "method").unwrap_or("").to_lowercase(),
                    ),
            ),
        );
    }
    Value::Object(out)
}

/// Field by field, what differs between two records (top-level keys).
fn differing_fields(a: &Value, b: &Value) -> Vec<String> {
    let keys: BTreeSet<&String> = a
        .as_object()
        .into_iter()
        .flat_map(|o| o.keys())
        .chain(b.as_object().into_iter().flat_map(|o| o.keys()))
        .collect();
    keys.into_iter()
        .filter(|k| {
            !crate::json::deep_equal(
                get(a, k).unwrap_or(&Value::Null),
                get(b, k).unwrap_or(&Value::Null),
            )
        })
        .cloned()
        .collect()
}

/// Operation by operation and shape by shape, how a fresh build differs
/// from the current inventory: the moved operations and the supplement's
/// own shapes apart from everything else, because a difference there is the
/// move's, and one elsewhere is an edit to the inventory that no input
/// records.
pub fn compare(
    current: &Value,
    rebuilt: &Value,
    moved: &BTreeSet<String>,
    own_shapes: &BTreeSet<String>,
) -> Value {
    let index = |inv: &Value| -> BTreeMap<String, Value> {
        get_arr(inv, "operations")
            .into_iter()
            .flatten()
            .filter_map(|o| Some((get_str(o, "key")?.to_string(), o.clone())))
            .collect()
    };
    let (a, b) = (index(current), index(rebuilt));
    let missing: Vec<&String> = a.keys().filter(|k| !b.contains_key(*k)).collect();
    let added: Vec<&String> = b.keys().filter(|k| !a.contains_key(*k)).collect();
    let mut by_field: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut elsewhere: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for (k, op) in &a {
        if let Some(other) = b.get(k) {
            let target = if moved.contains(k) {
                &mut by_field
            } else {
                &mut elsewhere
            };
            for f in differing_fields(op, other) {
                target.entry(f).or_default().push(k.clone());
            }
        }
    }
    let shapes = |inv: &Value| get_obj(inv, "shapes").cloned().unwrap_or_default();
    let (sa, sb) = (shapes(current), shapes(rebuilt));
    let shape_missing: Vec<&String> = sa.keys().filter(|k| !sb.contains_key(*k)).collect();
    let shape_added: Vec<&String> = sb.keys().filter(|k| !sa.contains_key(*k)).collect();
    let changed: Vec<&String> = sa
        .iter()
        .filter(|(k, v)| {
            sb.get(*k)
                .map(|w| !crate::json::deep_equal(v, w))
                .unwrap_or(false)
        })
        .map(|(k, _)| k)
        .collect();
    let (shape_changed, shape_elsewhere): (Vec<&String>, Vec<&String>) =
        changed.into_iter().partition(|k| own_shapes.contains(*k));
    let api_changed = differing_fields(
        get(current, "api").unwrap_or(&Value::Null),
        get(rebuilt, "api").unwrap_or(&Value::Null),
    );
    crate::json::object(vec![
        ("operations_missing", serde_json::json!(missing)),
        ("operations_added", serde_json::json!(added)),
        ("operations_changed", serde_json::json!(by_field)),
        ("operations_changed_elsewhere", serde_json::json!(elsewhere)),
        ("shapes_missing", serde_json::json!(shape_missing)),
        ("shapes_added", serde_json::json!(shape_added)),
        ("shapes_changed", serde_json::json!(shape_changed)),
        (
            "shapes_changed_elsewhere",
            serde_json::json!(shape_elsewhere),
        ),
        ("api_changed", serde_json::json!(api_changed)),
    ])
}

fn components_insert(doc: &mut Map<String, Value>, section: &str, value: Value) {
    let c = doc
        .entry("components")
        .or_insert_with(|| Value::Object(Map::new()));
    if let Some(c) = c.as_object_mut() {
        c.insert(section.to_string(), value);
    }
}

/// The API's auth as the security schemes the reader turns back into it,
/// for a supplement that is the workspace's only input; `true` when the
/// API takes no credential (`security: []`). A credential prefix the
/// scheme type does not imply rides as `x-prefix`.
fn security_schemes(api: &Value) -> (Map<String, Value>, bool) {
    let mut out = Map::new();
    let mut none = false;
    for e in get_arr(api, "auth").into_iter().flatten() {
        let kind = get_str(e, "kind").unwrap_or("");
        let name = get_str(e, "scheme_name")
            .map(str::to_string)
            .unwrap_or_else(|| match kind {
                "api_key" => "apiKey".to_string(),
                other => format!("{}Auth", other),
            });
        let mut scheme = match kind {
            "none" => {
                none = true;
                continue;
            }
            "bearer" => serde_json::json!({"type": "http", "scheme": "bearer"}),
            "basic" => serde_json::json!({"type": "http", "scheme": "basic"}),
            "api_key" => match get_str(e, "query_param") {
                Some(q) => serde_json::json!({"type": "apiKey", "in": "query", "name": q}),
                None => {
                    serde_json::json!({"type": "apiKey", "in": "header", "name": get_str(e, "header").unwrap_or("Authorization")})
                }
            },
            "oauth2" => {
                let mut flows = Map::new();
                if let Some(ac) = get(e, "oauth2").and_then(|o| get(o, "authorization_code")) {
                    let scopes: Map<String, Value> = get_arr(ac, "scopes")
                        .into_iter()
                        .flatten()
                        .filter_map(|s| {
                            Some((
                                get_str(s, "name")?.to_string(),
                                get(s, "description").cloned().unwrap_or(Value::from("")),
                            ))
                        })
                        .collect();
                    flows.insert(
                        "authorizationCode".into(),
                        serde_json::json!({
                            "authorizationUrl": get(ac, "authorization_url"),
                            "tokenUrl": get(ac, "token_url"),
                            "scopes": scopes,
                        }),
                    );
                }
                serde_json::json!({"type": "oauth2", "flows": flows})
            }
            _ => continue,
        };
        let implied = match kind {
            "bearer" | "oauth2" => Some("Bearer "),
            "basic" => Some("Basic "),
            _ => None,
        };
        if let Some(p) = get_str(e, "prefix").filter(|p| Some(*p) != implied) {
            crate::json::set(&mut scheme, "x-prefix", Value::from(p));
        }
        out.insert(name, scheme);
    }
    (out, none)
}

fn schemas_count(doc: &Map<String, Value>) -> usize {
    doc.get("components")
        .and_then(|c| get_obj(c, "schemas"))
        .map(|s| s.len())
        .unwrap_or(0)
}

fn is_identical(cmp: &Value) -> bool {
    cmp.as_object()
        .map(|o| {
            o.values().all(|v| match v {
                Value::Array(a) => a.is_empty(),
                Value::Object(m) => m.is_empty(),
                _ => true,
            })
        })
        .unwrap_or(true)
}

pub fn main(argv: &[String]) -> i32 {
    let args = Args::parse(argv, &FLAGS);
    let dir = Path::new(args.positional.first().map(String::as_str).unwrap_or("."));
    let provenance = match args.get("provenance") {
        Some(p) if SUPPLEMENT_PROVENANCE.contains(&p) => p.to_string(),
        _ => return fail(
            "--provenance docs|probe is required: the operations to move, by where they were read",
        ),
    };
    let out_rel = args
        .get("out")
        .unwrap_or(DEFAULT_OUT)
        .trim_start_matches("./")
        .to_string();
    if !crate::sources::inside_workspace(&out_rel) || out_rel.starts_with(".factory/") {
        return fail("--out must be a workspace path outside .factory/");
    }
    let inventory =
        match crate::factory_io::read_to_string(dir, crate::cmd::inventory::DEFAULT_INVENTORY_REL)
            .map_err(|e| e.to_string())
            .and_then(|t| crate::json::parse(&t))
        {
            Ok(v) => v,
            Err(e) => return fail(&e),
        };
    let ops: Vec<&Value> = get_arr(&inventory, "operations")
        .into_iter()
        .flatten()
        .filter(|o| get_str(o, "provenance") == Some(provenance.as_str()))
        .collect();
    if ops.is_empty() {
        return fail(&format!(
            "the inventory has no operation with provenance {}",
            provenance
        ));
    }
    let inputs = match crate::inputs::workspace_inputs(dir) {
        Ok(i) => i,
        Err(e) => return fail(&e),
    };
    if inputs.iter().any(|i| i.path == out_rel) && !args.has("force") {
        return fail(&format!(
            "{} is already an input; --force replaces it",
            out_rel
        ));
    }
    let others: Vec<Input> = inputs.into_iter().filter(|i| i.path != out_rel).collect();
    // What the other inputs already declare: an operation of theirs is not
    // moved, and a shape of theirs is referenced, never copied.
    let (other_ops, provided): (BTreeSet<String>, HashMap<String, String>) = if others.is_empty() {
        (BTreeSet::new(), HashMap::new())
    } else {
        match build_inputs(dir, &others, None) {
            Ok(b) => {
                let keys = get_arr(&b.inventory, "operations")
                    .into_iter()
                    .flatten()
                    .filter_map(|o| get_str(o, "key").map(str::to_string))
                    .collect();
                let provided = b
                    .shape_names
                    .iter()
                    .map(|(component, shape)| (shape.clone(), component.clone()))
                    .collect();
                (keys, provided)
            }
            Err(e) => return fail(&format!("the other inputs do not build: {}", e)),
        }
    };
    let ops: Vec<&Value> = ops
        .into_iter()
        .filter(|o| !other_ops.contains(get_str(o, "key").unwrap_or("")))
        .collect();
    let all_shapes = get_obj(&inventory, "shapes").cloned().unwrap_or_default();
    let mut starts: Vec<String> = Vec::new();
    for op in &ops {
        let mut refs = vec![get(op, "response"), get(op, "request_body")];
        refs.extend(get_arr(op, "errors").into_iter().flatten().map(Some));
        for r in refs.into_iter().flatten() {
            if let Some(n) = get_str(r, "shape_ref").and_then(|s| s.strip_prefix("#/shapes/")) {
                starts.push(n.to_string());
            }
        }
        for p in get_arr(op, "parameters").into_iter().flatten() {
            if let Some(n) = get_str(p, "shape_ref").and_then(|s| s.strip_prefix("#/shapes/")) {
                starts.push(n.to_string());
            }
        }
    }
    let reached = crate::inventory::shape_closure(starts, &all_shapes);
    let mut schemas = Map::new();
    for name in reached.iter().filter(|n| !provided.contains_key(*n)) {
        if let Some(s) = all_shapes.get(name) {
            schemas.insert(name.clone(), rewrite_refs(s, &provided));
        }
    }
    let mut paths: Map<String, Value> = Map::new();
    for op in &ops {
        let path = get_str(op, "path").unwrap_or("").to_string();
        let method = get_str(op, "method").unwrap_or("").to_lowercase();
        let item = paths
            .entry(path)
            .or_insert_with(|| Value::Object(Map::new()));
        if let Some(o) = item.as_object_mut() {
            o.insert(
                method,
                operation(op, &provided, args.get("cite"), &provenance),
            );
        }
    }
    let api = get(&inventory, "api").cloned().unwrap_or(Value::Null);
    let mut info = Map::new();
    info.insert(
        "title".into(),
        Value::from(get_str(&api, "title").unwrap_or("API")),
    );
    info.insert(
        "version".into(),
        Value::from(get_str(&api, "version").unwrap_or("unversioned")),
    );
    if others.is_empty() {
        if let Some(d) = get(&api, "description").filter(|v| !v.is_null()) {
            info.insert("description".into(), d.clone());
        }
    }
    let mut doc = Map::new();
    doc.insert("openapi".into(), Value::from("3.1.0"));
    doc.insert("info".into(), Value::Object(info));
    if others.is_empty() {
        let servers: Vec<Value> = get_arr(&api, "base_urls")
            .into_iter()
            .flatten()
            .map(|u| crate::json::object(vec![("url", u.clone())]))
            .collect();
        if !servers.is_empty() {
            doc.insert("servers".into(), Value::Array(servers));
        }
        let (schemes, none) = security_schemes(&api);
        if none {
            doc.insert("security".into(), Value::Array(vec![]));
        }
        if !schemes.is_empty() {
            components_insert(&mut doc, "securitySchemes", Value::Object(schemes));
        }
    }
    doc.insert("paths".into(), Value::Object(paths));
    if !schemas.is_empty() {
        components_insert(&mut doc, "schemas", Value::Object(schemas));
    }
    let own_shapes = schemas_count(&doc);
    let doc = Value::Object(doc);
    let uncited = if args.get("cite").is_none() {
        ops.len()
    } else {
        0
    };

    // The proof: build from every input as the workspace would, the new
    // supplement included, and compare with the inventory it replaces.
    let supplement = Input {
        path: out_rel.clone(),
        kind: SUPPLEMENT_KIND.to_string(),
        provenance: provenance.clone(),
        patches: vec![],
    };
    let mut all = others.clone();
    all.push(supplement);
    let cmp = match build_inputs(dir, &all, Some((&out_rel, doc.clone()))) {
        Ok(b) => {
            let moved: BTreeSet<String> = ops
                .iter()
                .filter_map(|o| get_str(o, "key").map(str::to_string))
                .collect();
            let own: BTreeSet<String> = get(&doc, "components")
                .and_then(|c| get_obj(c, "schemas"))
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default();
            compare(&inventory, &b.inventory, &moved, &own)
        }
        Err(e) => {
            return fail(&format!(
                "the inputs with this supplement do not build: {}",
                e
            ))
        }
    };
    let identical = is_identical(&cmp);

    if !args.has("dry-run") {
        let target = dir.join(&out_rel);
        if target.exists() && !args.has("force") {
            return fail(&format!("{} exists; --force replaces it", out_rel));
        }
        if let Err(e) = std::fs::write(&target, crate::json::pretty(&doc)) {
            return fail(&format!("{}: {}", out_rel, e));
        }
        let lock_text =
            crate::factory_io::read_to_string_optional(dir, crate::sources::SOURCES_LOCK)
                .ok()
                .flatten()
                .unwrap_or_default();
        let mut pairs = vec![
            ("kind", SUPPLEMENT_KIND),
            ("provenance", provenance.as_str()),
        ];
        if let Some(n) = args.get("note") {
            pairs.push(("note", n));
        }
        let updated = match crate::sources::upsert_entry(&lock_text, &out_rel, &pairs) {
            Ok(t) => t,
            Err(e) => return fail(&e),
        };
        if let Err(e) =
            crate::factory_io::write_in_place(dir, crate::sources::SOURCES_LOCK, updated.as_bytes())
        {
            return fail(&e.to_string());
        }
    }
    if args.has("json") {
        println!(
            "{}",
            crate::json::pretty(&crate::json::object(vec![
                ("supplement", Value::from(out_rel.as_str())),
                ("provenance", Value::from(provenance.as_str())),
                ("operations", Value::from(ops.len())),
                ("shapes", Value::from(own_shapes)),
                ("uncited", Value::from(uncited)),
                ("written", Value::Bool(!args.has("dry-run"))),
                ("identical", Value::Bool(identical)),
                ("difference", cmp),
            ]))
        );
    } else {
        println!(
            "{}{}: {} {} operation(s), {} shape(s) of their own{}",
            if args.has("dry-run") {
                "would write "
            } else {
                "wrote "
            },
            out_rel,
            ops.len(),
            provenance,
            own_shapes,
            if args.has("dry-run") {
                String::new()
            } else {
                format!(" and its {} entry", crate::sources::SOURCES_LOCK)
            }
        );
        if uncited > 0 {
            println!("  {} operation(s) have no x-source yet; `inventory build` refuses them until each names the page or sample it was read from (or rerun with --cite TEMPLATE)", uncited);
        }
        if identical {
            println!("round trip: building from every input reproduces the inventory exactly");
        } else {
            println!("round trip: building from every input differs from the inventory:");
            for (k, label) in [
                ("operations_missing", "operations it no longer has"),
                ("operations_added", "operations it would gain"),
                ("shapes_missing", "shapes it no longer has"),
                ("shapes_added", "shapes it would gain"),
                ("shapes_changed", "of the supplement's shapes differ"),
                (
                    "shapes_changed_elsewhere",
                    "other shapes differ (an edit to the inventory no input records)",
                ),
                ("api_changed", "api fields that differ"),
            ] {
                let list: Vec<&str> = get_arr(&cmp, k)
                    .into_iter()
                    .flatten()
                    .filter_map(Value::as_str)
                    .collect();
                if !list.is_empty() {
                    println!(
                        "  {} {}: {}{}",
                        list.len(),
                        label,
                        list.iter().take(8).cloned().collect::<Vec<_>>().join(", "),
                        if list.len() > 8 { ", …" } else { "" }
                    );
                }
            }
            for (k, whose) in [
                ("operations_changed", "moved"),
                (
                    "operations_changed_elsewhere",
                    "other (an edit to the inventory no input records)",
                ),
            ] {
                for (field, keys) in get_obj(&cmp, k).into_iter().flatten() {
                    let keys: Vec<&str> = keys
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .collect();
                    println!(
                        "  `{}` differs on {} {} operation(s): {}{}",
                        field,
                        keys.len(),
                        whose,
                        keys.iter().take(6).cloned().collect::<Vec<_>>().join(", "),
                        if keys.len() > 6 { ", …" } else { "" }
                    );
                }
            }
            println!("  Each difference is a fact an input should state (edit the supplement, or patch the pinned document: codify --source), a judgement the old inventory carried (selection.yaml), or a value the builder now derives. Settle them before `inventory build` replaces the inventory.");
        }
    }
    if identical {
        0
    } else {
        1
    }
}

/// Build from inputs, one of them (`replace`) given as a document already
/// in hand; citations are not checked here (the report says what is
/// uncited), everything else is exactly what `inventory build` does.
fn build_inputs(
    dir: &Path,
    inputs: &[Input],
    replace: Option<(&str, Value)>,
) -> Result<crate::openapi::Built, String> {
    let mut loaded = Vec::new();
    for input in inputs {
        let doc = match &replace {
            Some((p, d)) if *p == input.path => d.clone(),
            _ => crate::spec::load(
                &std::fs::read_to_string(dir.join(&input.path))
                    .map_err(|e| format!("{}: {}", input.path, e))?,
                &input.path,
            )?,
        };
        let l = crate::spec::normalise(doc)?;
        loaded.push(Loaded {
            input: input.clone(),
            document: l.document,
            format: l.format,
            warnings: l.warnings,
        });
    }
    crate::inputs::build(loaded).map(|(b, _)| b)
}
