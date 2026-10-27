//! A supplement: the agent's own OpenAPI 3 document of what a pinned
//! description lacks, built into one inventory with it (ADR 0138) — the
//! merge and its refusals, a refresh that keeps it, `inventory supplement`
//! moving hand-merged operations into one, and lint's
//! `inventory-not-rebuildable`.

use graphos_factory_core::cmd;
use graphos_factory_core::lint::{lint_workspace, LintOptions};
use serde_json::{json, Value};
use std::path::Path;

fn write(dir: &Path, rel: &str, text: &str) {
    let f = dir.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(f, text).unwrap();
}

fn read(dir: &Path, rel: &str) -> String {
    std::fs::read_to_string(dir.join(rel)).unwrap()
}

fn args(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// The vendor's document: one operation, and the component a supplement
/// operation reuses.
fn spec() -> Value {
    json!({
        "openapi": "3.0.3",
        "info": {"title": "Widget Co", "version": "1.0.0"},
        "servers": [{"url": "https://api.widgets.test"}],
        "components": {"schemas": {
            "Widget": {"type": "object", "properties": {"id": {"type": "string"}, "name": {"type": "string"}}}
        }},
        "paths": {"/widgets": {"get": {
            "operationId": "listWidgets",
            "responses": {"200": {"description": "ok", "content": {"application/json": {"schema": {
                "type": "object", "properties": {"widgets": {"type": "array", "items": {"$ref": "#/components/schemas/Widget"}}}
            }}}}}
        }}}
    })
}

/// What the docs say and the vendor's document lacks: a by-id read that
/// returns the vendor's own `Widget`, and an archive call with its own
/// response shape.
fn supplement() -> Value {
    json!({
        "openapi": "3.1.0",
        "info": {"title": "Widget Co", "version": "1.0.0"},
        "paths": {
            "/widgets/{id}": {"get": {
                "operationId": "getWidget",
                "x-source": "https://docs.widgets.test/reference/get-widget",
                "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}}],
                "responses": {"200": {"description": "", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/Widget"}}}}}
            }},
            "/widgets/{id}/archive": {"post": {
                "operationId": "archiveWidget",
                "x-source": "docs/archive-widget.md",
                "parameters": [{"name": "id", "in": "path", "required": true, "schema": {"type": "string"}}],
                "responses": {"200": {"description": "", "content": {"application/json": {"schema": {"$ref": "#/components/schemas/ArchiveResult"}}}}}
            }}
        },
        "components": {"schemas": {
            "ArchiveResult": {"type": "object", "properties": {"archived": {"type": "boolean"}}}
        }}
    })
}

const SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(name: "widget_co", http: { baseURL: "{{BASE_URL}}" })

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  widget_co_listWidgets: [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "$.widgets { id name }")
}
"#;

const SELECTION: &str = "contract_version: 1\ndefaults:\n  fields: all\noperations:\n  \"get:/widgets\":\n    include: true\n    graphql: { root: query, name: listWidgets }\n";

const WORKSPACE: &str = "contract_version: 1\nservice: widget_co\ndirectory: widget-co\ntype_prefix: Widget_Co\nfield_prefix: widget_co\nskill:\n  name: example\n  version: 0.1.0\nsource_kind: rest\nconnect_spec: v0.3\nfederation_version: \"2.12.0\"\nintake: mixed\ncreated_at: 2026-10-26T00:00:00Z\n";

const SUPPLEMENT_ENTRY: &str = "  - kind: supplement\n    path: supplement.openapi.json\n    provenance: docs\n    note: what the vendor's document lacks, read from its reference pages\n";

/// A workspace pinning the vendor's document, with a supplement beside it.
fn workspace() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    write(d, "widget-co.graphql", SDL);
    write(d, ".factory/workspace.yaml", WORKSPACE);
    write(d, ".factory/selection.yaml", SELECTION);
    write(
        d,
        ".factory/decisions.json",
        r#"{"contract_version":1,"decisions":[]}"#,
    );
    write(
        d,
        "openapi.json",
        &graphos_factory_core::json::pretty(&spec()),
    );
    assert_eq!(
        cmd::sources::main(&args(&[
            "pin",
            d.to_str().unwrap(),
            "--path",
            "openapi.json",
            "--url",
            "https://api.widgets.test/openapi.json",
        ])),
        0
    );
    let lock = read(d, ".factory/sources.lock.yaml");
    write(
        d,
        ".factory/sources.lock.yaml",
        &format!("{}{}", lock, SUPPLEMENT_ENTRY),
    );
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&supplement()),
    );
    write(d, "docs/archive-widget.md", "POST /widgets/{id}/archive\n");
    dir
}

fn build(d: &Path) -> i32 {
    cmd::inventory::main(&args(&["build", d.to_str().unwrap()]))
}

fn inventory(d: &Path) -> Value {
    graphos_factory_core::json::parse(&read(d, ".factory/inventory.json")).unwrap()
}

fn op<'a>(inv: &'a Value, key: &str) -> &'a Value {
    inv["operations"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["key"] == key)
        .unwrap_or_else(|| panic!("no {}", key))
}

#[test]
fn a_workspace_builds_from_its_pinned_document_and_its_supplement() {
    let dir = workspace();
    let d = dir.path();
    assert_eq!(build(d), 0);
    let inv = inventory(d);
    assert_eq!(inv["operations"].as_array().unwrap().len(), 3);
    let spec_op = op(&inv, "get:/widgets");
    assert_eq!(spec_op["provenance"], "spec");
    assert_eq!(spec_op["confidence"], json!(1));
    let read = op(&inv, "get:/widgets/{id}");
    assert_eq!(read["provenance"], "docs");
    assert_eq!(read["confidence"], json!(0.9));
    // The supplement reuses the vendor's component: one shape, one name.
    assert_eq!(read["response"]["shape_ref"], "#/shapes/Widget");
    assert_eq!(
        op(&inv, "post:/widgets/{id}/archive")["response"]["shape_ref"],
        "#/shapes/ArchiveResult"
    );
    // The first input gives the API's facts.
    assert_eq!(inv["api"]["base_urls"], json!(["https://api.widgets.test"]));
}

#[test]
fn a_single_document_builds_exactly_what_it_always_did() {
    let dir = workspace();
    let d = dir.path();
    let alone = graphos_factory_core::openapi::build_inventory(&spec())
        .unwrap()
        .inventory;
    let (built, _) = graphos_factory_core::inputs::build_files(&[d.join("openapi.json")]).unwrap();
    assert_eq!(built.inventory, alone);
}

#[test]
fn a_supplement_operation_must_name_where_it_was_read() {
    let dir = workspace();
    let d = dir.path();
    let mut s = supplement();
    s["paths"]["/widgets/{id}"]["get"]
        .as_object_mut()
        .unwrap()
        .remove("x-source");
    s["paths"]["/widgets/{id}/archive"]["post"]["x-source"] = json!("docs/missing.md");
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&s),
    );
    let err = graphos_factory_core::inputs::build_workspace(d, None)
        .err()
        .unwrap();
    assert!(
        err.contains("2 operation(s) do not name where they were read"),
        "{}",
        err
    );
    assert!(err.contains("get:/widgets/{id} has no x-source"), "{}", err);
    assert!(
        err.contains("cites docs/missing.md, which is not in the workspace"),
        "{}",
        err
    );
    assert_ne!(build(d), 0);
}

#[test]
fn an_operation_both_inputs_declare_is_refused() {
    let dir = workspace();
    let d = dir.path();
    let mut s = supplement();
    s["paths"]["/widgets"] = json!({"get": {"operationId": "listAgain", "x-source": "https://docs.widgets.test/x", "responses": {"200": {"description": ""}}}});
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&s),
    );
    let err = graphos_factory_core::inputs::build_workspace(d, None)
        .err()
        .unwrap();
    assert!(
        err.contains("get:/widgets is declared by both openapi.json and supplement.openapi.json"),
        "{}",
        err
    );
}

#[test]
fn a_component_is_shared_only_when_both_define_it_alike() {
    let dir = workspace();
    let d = dir.path();
    let mut s = supplement();
    s["components"]["schemas"]["Widget"] = spec()["components"]["schemas"]["Widget"].clone();
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&s),
    );
    assert!(graphos_factory_core::inputs::build_workspace(d, None).is_ok());
    s["components"]["schemas"]["Widget"]["properties"]["colour"] = json!({"type": "string"});
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&s),
    );
    let err = graphos_factory_core::inputs::build_workspace(d, None)
        .err()
        .unwrap();
    assert!(
        err.contains("components.schemas.Widget is defined differently"),
        "{}",
        err
    );
}

#[test]
fn a_path_level_key_cannot_reach_the_other_inputs_operations() {
    let dir = workspace();
    let d = dir.path();
    let mut s = supplement();
    s["paths"]["/widgets"] = json!({
        "parameters": [{"name": "X-Tenant", "in": "header", "schema": {"type": "string"}}],
        "post": {"operationId": "createWidget", "x-source": "https://docs.widgets.test/create", "responses": {"200": {"description": ""}}}
    });
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&s),
    );
    let err = graphos_factory_core::inputs::build_workspace(d, None)
        .err()
        .unwrap();
    assert!(
        err.contains("gives the path its own `parameters`"),
        "{}",
        err
    );
    // Without the path-level key, a new method under the vendor's path is fine.
    s["paths"]["/widgets"]
        .as_object_mut()
        .unwrap()
        .remove("parameters");
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&s),
    );
    let (b, _) = graphos_factory_core::inputs::build_workspace(d, None).unwrap();
    assert_eq!(op(&b.inventory, "post:/widgets")["provenance"], "docs");
}

#[test]
fn a_supplement_is_openapi_3() {
    let dir = workspace();
    let d = dir.path();
    write(
        d,
        "supplement.openapi.json",
        r#"{"swagger": "2.0", "info": {"title": "x", "version": "1"}, "paths": {}}"#,
    );
    let err = graphos_factory_core::inputs::build_workspace(d, None)
        .err()
        .unwrap();
    assert!(
        err.contains("a supplement is an OpenAPI 3 document"),
        "{}",
        err
    );
}

#[test]
fn a_refresh_rebuilds_from_every_input_and_keeps_the_supplement() {
    let dir = workspace();
    let d = dir.path();
    assert_eq!(build(d), 0);
    let fetched = tempfile::tempdir().unwrap();
    let mut newer = spec();
    newer["info"]["version"] = json!("1.1.0");
    newer["paths"]["/widgets"]["get"]["summary"] = json!("List widgets");
    let from = fetched.path().join("openapi.json");
    std::fs::write(&from, graphos_factory_core::json::pretty(&newer)).unwrap();
    let code = cmd::sources::main(&args(&[
        "refresh",
        d.to_str().unwrap(),
        "--path",
        "openapi.json",
        "--from",
        from.to_str().unwrap(),
        "--reason",
        "vendor 1.1.0",
    ]));
    assert_eq!(code, 0);
    let inv = inventory(d);
    assert_eq!(op(&inv, "get:/widgets")["summary"], "List widgets");
    assert_eq!(op(&inv, "get:/widgets/{id}")["provenance"], "docs");
    assert_eq!(inv["operations"].as_array().unwrap().len(), 3);

    // The vendor now documents the by-id read itself: the refresh refuses
    // until the supplement stops declaring it.
    newer["paths"]["/widgets/{id}"] = supplement()["paths"]["/widgets/{id}"].clone();
    std::fs::write(&from, graphos_factory_core::json::pretty(&newer)).unwrap();
    let before = read(d, ".factory/inventory.json");
    let code = cmd::sources::main(&args(&[
        "refresh",
        d.to_str().unwrap(),
        "--path",
        "openapi.json",
        "--from",
        from.to_str().unwrap(),
        "--reason",
        "vendor 1.2.0",
    ]));
    assert_ne!(code, 0);
    assert_eq!(read(d, ".factory/inventory.json"), before);
}

/// The Slack state: operations the agent read from the docs were merged
/// into inventory.json by hand, and nothing names a document for them.
fn hand_merged() -> tempfile::TempDir {
    let dir = workspace();
    let d = dir.path();
    assert_eq!(build(d), 0);
    std::fs::remove_file(d.join("supplement.openapi.json")).unwrap();
    let lock = read(d, ".factory/sources.lock.yaml").replace(SUPPLEMENT_ENTRY, "");
    write(d, ".factory/sources.lock.yaml", &lock);
    dir
}

fn rebuild_findings(d: &Path) -> Vec<String> {
    lint_workspace(
        d,
        &LintOptions {
            schemas_dir: None,
            skip_evidence: true,
            target: &graphos_factory_core::target::BARE,
        },
    )
    .findings
    .into_iter()
    .filter(|f| f.rule == "inventory-not-rebuildable")
    .map(|f| f.message)
    .collect()
}

#[test]
fn lint_names_operations_no_input_declares() {
    let dir = hand_merged();
    let d = dir.path();
    let found = rebuild_findings(d);
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(
        found[0].contains(
            "2 operation(s) no input declares (get:/widgets/{id}, post:/widgets/{id}/archive)"
        ),
        "{}",
        found[0]
    );
}

#[test]
fn lint_names_an_operation_edited_in_place() {
    let dir = workspace();
    let d = dir.path();
    assert_eq!(build(d), 0);
    assert!(rebuild_findings(d).is_empty());
    let mut inv = inventory(d);
    let ops = inv["operations"].as_array_mut().unwrap();
    let o = ops.iter_mut().find(|o| o["key"] == "get:/widgets").unwrap();
    o["summary"] = json!("edited by hand");
    write(
        d,
        ".factory/inventory.json",
        &graphos_factory_core::json::pretty(&inv),
    );
    let found = rebuild_findings(d);
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(
        found[0].contains(
            "1 that differ from what the inputs build (get:/widgets; by field: summary 1)"
        ),
        "{}",
        found[0]
    );
}

#[test]
fn lint_names_a_hand_written_inventory() {
    let dir = workspace();
    let d = dir.path();
    assert_eq!(build(d), 0);
    write(
        d,
        ".factory/sources.lock.yaml",
        "contract_version: 1\nsources:\n  - kind: docs\n    url: https://docs.widgets.test/\n",
    );
    let found = rebuild_findings(d);
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(found[0].contains("3 operation(s), and .factory/sources.lock.yaml names no document or supplement that builds them"), "{}", found[0]);
}

#[test]
fn supplement_moves_hand_merged_operations_and_the_rebuild_reproduces_the_inventory() {
    let dir = hand_merged();
    let d = dir.path();
    let before = read(d, ".factory/inventory.json");
    let code = cmd::inventory::main(&args(&[
        "supplement",
        d.to_str().unwrap(),
        "--provenance",
        "docs",
        "--out",
        "docs.openapi.json",
        "--cite",
        "https://docs.widgets.test/reference/{operation_id}",
    ]));
    assert_eq!(code, 0, "the round trip should be identical");
    let doc: Value = graphos_factory_core::json::parse(&read(d, "docs.openapi.json")).unwrap();
    // The vendor's component is referenced, the supplement's own is copied.
    assert_eq!(
        doc["paths"]["/widgets/{id}"]["get"]["responses"]["200"]["content"]["application/json"]
            ["schema"]["$ref"],
        "#/components/schemas/Widget"
    );
    assert!(doc["components"]["schemas"].get("Widget").is_none());
    assert!(doc["components"]["schemas"].get("ArchiveResult").is_some());
    assert_eq!(
        doc["paths"]["/widgets/{id}/archive"]["post"]["x-source"],
        "https://docs.widgets.test/reference/archiveWidget"
    );
    let lock = read(d, ".factory/sources.lock.yaml");
    assert!(
        lock.contains("kind: supplement")
            && lock.contains("path: docs.openapi.json")
            && lock.contains("provenance: docs"),
        "{}",
        lock
    );
    assert!(rebuild_findings(d).is_empty());
    assert_eq!(build(d), 0);
    assert_eq!(
        graphos_factory_core::json::parse(&read(d, ".factory/inventory.json")).unwrap(),
        graphos_factory_core::json::parse(&before).unwrap()
    );
}

#[test]
fn supplement_reports_what_the_rebuild_would_change() {
    let dir = hand_merged();
    let d = dir.path();
    // A value the builder derives, edited in the inventory by hand.
    let mut inv = inventory(d);
    let ops = inv["operations"].as_array_mut().unwrap();
    let o = ops
        .iter_mut()
        .find(|o| o["key"] == "post:/widgets/{id}/archive")
        .unwrap();
    o["support"] = json!("needs_review");
    write(
        d,
        ".factory/inventory.json",
        &graphos_factory_core::json::pretty(&inv),
    );
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_graphos-factory-bare"))
        .args([
            "inventory",
            "supplement",
            d.to_str().unwrap(),
            "--provenance",
            "docs",
            "--cite",
            "https://docs.widgets.test/{path}",
            "--json",
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    let report: Value =
        graphos_factory_core::json::parse(&String::from_utf8_lossy(&out.stdout)).unwrap();
    assert_eq!(report["identical"], json!(false));
    assert_eq!(
        report["difference"]["operations_changed"]["support"],
        json!(["post:/widgets/{id}/archive"])
    );
    // The file and the lock entry are still written, for the agent to settle.
    assert!(d.join("supplement.openapi.json").exists());
}

#[test]
fn supplement_without_cite_leaves_operations_the_build_refuses() {
    let dir = hand_merged();
    let d = dir.path();
    let code = cmd::inventory::main(&args(&[
        "supplement",
        d.to_str().unwrap(),
        "--provenance",
        "docs",
    ]));
    assert_eq!(code, 0);
    let err = graphos_factory_core::inputs::build_workspace(d, None)
        .err()
        .unwrap();
    assert!(
        err.contains("2 operation(s) do not name where they were read"),
        "{}",
        err
    );
}

#[test]
fn a_supplement_states_what_the_method_cannot() {
    let dir = workspace();
    let d = dir.path();
    let mut s = supplement();
    // The page says this GET changes state, and the archive call only reads.
    s["paths"]["/widgets/{id}"]["get"]["x-semantics"] = json!("write");
    s["paths"]["/widgets/{id}/archive"]["post"]["operationId"] = json!("lookupArchive");
    s["paths"]["/widgets/{id}/archive"]["post"]["x-semantics"] = json!("read");
    s["paths"]["/widgets/{id}/archive"]["post"]["x-confidence"] = json!(0.6);
    s["paths"]["/widgets/{id}"]["get"]["x-confidence"] = json!(0.95);
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&s),
    );
    let (b, _) = graphos_factory_core::inputs::build_workspace(d, None).unwrap();
    let get = op(&b.inventory, "get:/widgets/{id}");
    assert_eq!(get["semantics"], "write");
    // Above the provenance's maximum, the maximum stands.
    assert_eq!(get["confidence"], json!(0.9));
    let post = op(&b.inventory, "post:/widgets/{id}/archive");
    assert_eq!(post["semantics"], "read");
    assert!(post.get("read_hint").is_none(), "{}", post);
    assert_eq!(post["confidence"], json!(0.6));
    // A pinned document's x-semantics is not read: only a supplement states it.
    let mut v = spec();
    v["paths"]["/widgets"]["get"]["x-semantics"] = json!("write");
    write(d, "openapi.json", &graphos_factory_core::json::pretty(&v));
    let (b, _) = graphos_factory_core::inputs::build_workspace(d, None).unwrap();
    assert_eq!(op(&b.inventory, "get:/widgets")["semantics"], "read");
}

/// A workspace with no pinned document: the supplement is its only input,
/// so it carries the API's servers and auth too.
fn discovered(auth: Value, security: Option<Value>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path();
    let mut s = supplement();
    s["servers"] = json!([{"url": "https://api.widgets.test"}]);
    s["paths"]["/widgets/{id}/archive"]["post"]["x-source"] =
        json!("https://docs.widgets.test/archive");
    s["components"]["schemas"]["Widget"] = spec()["components"]["schemas"]["Widget"].clone();
    if !auth.is_null() {
        s["components"]["securitySchemes"] = auth;
    }
    if let Some(sec) = security {
        s["security"] = sec;
    }
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&s),
    );
    write(
        d,
        ".factory/sources.lock.yaml",
        &format!("contract_version: 1\nsources:\n{}", SUPPLEMENT_ENTRY),
    );
    dir
}

#[test]
fn a_supplement_alone_gives_the_apis_auth() {
    let dir = discovered(json!(null), Some(json!([])));
    let (b, _) = graphos_factory_core::inputs::build_workspace(dir.path(), None).unwrap();
    assert_eq!(
        b.inventory["api"]["auth"],
        json!([{"kind": "none", "source": "docs", "confidence": 0.9}])
    );
    let dir = discovered(
        json!({"apiKey": {"type": "apiKey", "in": "header", "name": "Authorization", "x-prefix": "Apikey "}}),
        None,
    );
    let (b, _) = graphos_factory_core::inputs::build_workspace(dir.path(), None).unwrap();
    let auth = &b.inventory["api"]["auth"][0];
    assert_eq!(auth["kind"], "api_key");
    assert_eq!(auth["prefix"], "Apikey ");
    assert_eq!(auth["source"], "docs");
    assert_eq!(
        b.inventory["api"]["base_urls"],
        json!(["https://api.widgets.test"])
    );
}

#[test]
fn supplement_round_trips_a_hand_written_inventory() {
    let dir = discovered(
        json!({"apiKey": {"type": "apiKey", "in": "header", "name": "Authorization", "x-prefix": "Apikey "}}),
        None,
    );
    let d = dir.path();
    let mut s: Value =
        graphos_factory_core::json::parse(&read(d, "supplement.openapi.json")).unwrap();
    s["paths"]["/widgets/{id}"]["get"]["x-semantics"] = json!("write");
    write(
        d,
        "supplement.openapi.json",
        &graphos_factory_core::json::pretty(&s),
    );
    assert_eq!(build(d), 0);
    // Now nothing names a document: the inventory is hand-written.
    std::fs::remove_file(d.join("supplement.openapi.json")).unwrap();
    write(
        d,
        ".factory/sources.lock.yaml",
        "contract_version: 1\nsources:\n  - kind: docs\n    url: https://docs.widgets.test/\n",
    );
    let before = inventory(d);
    let code = cmd::inventory::main(&args(&[
        "supplement",
        d.to_str().unwrap(),
        "--provenance",
        "docs",
        "--cite",
        "https://docs.widgets.test/{operation_id}",
    ]));
    assert_eq!(code, 0, "the round trip should be identical");
    let doc: Value =
        graphos_factory_core::json::parse(&read(d, "supplement.openapi.json")).unwrap();
    assert_eq!(doc["paths"]["/widgets/{id}"]["get"]["x-semantics"], "write");
    assert_eq!(
        doc["components"]["securitySchemes"]["apiKey"]["x-prefix"],
        "Apikey "
    );
    assert_eq!(build(d), 0);
    assert_eq!(inventory(d), before);
}
