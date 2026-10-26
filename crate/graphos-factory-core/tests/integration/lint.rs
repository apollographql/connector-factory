use graphos_factory_core::lint::{
    body_mapping, lint_workspace, wiring, BodyMapping, LintOptions, LintResult,
};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::Path;

const SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

scalar Widget_Co_JSON

type Widget_Co_Widget {
  id: ID
  name: String
  "Free-form vendor metadata; the spec documents no properties."
  metadata: Widget_Co_JSON
}

type Query {
  widget_co_listWidgets(limit: Int): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id name metadata")
}
"#;

const TEMPLATE: &str = "variables:\n  - name: BASE_URL\n    description: \"Base URL for the Widget Co API\"\n    test_default: \"https://api.widgets.test\"\n  - name: AUTH_EXPR\n    description: \"Complete Connectors authentication expression\"\n    test_default: \"{$env.WIDGET_CO_TOKEN}\"\n";

const WORKSPACE: &str = "contract_version: 1\nservice: widget_co\ndirectory: widget-co\ntype_prefix: Widget_Co\nfield_prefix: widget_co\nskill:\n  name: example\n  version: 0.1.0\nsource_kind: rest\nconnect_spec: v0.3\nfederation_version: \"2.12.0\"\nintake: spec\ncreated_at: 2026-09-08T00:00:00Z\ncontext_mode: generic\n";

const SELECTION: &str = "contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\noperations:\n  \"get:/widgets\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: query\n      name: listWidgets\n";

fn inventory() -> Value {
    json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [{"key": "get:/widgets", "operation_id": "listWidgets", "method": "GET", "path": "/widgets", "semantics": "read", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null}],
        "shapes": {},
        "unresolved": []
    })
}

fn evidence() -> Value {
    json!({
        "contract_version": 2, "commit": "a1b2c3d", "run_at": "2026-09-08T15:00:00Z",
        "toolchain": {"rover": "0.40.0", "federation": "2.12.0", "connect_spec": "v0.3"},
        "layers": {
            "compose": {"status": "pass"}, "connector_unit": {"status": "pass", "cases": 1, "failed": 0}, "wiremock_e2e": {"status": "skipped", "reason": "docker unavailable"},
            "conformance": {"status": "pass", "oracle": "openapi.json"}, "lint": {"status": "pass"}, "live": {"status": "not_run", "reason": "no credential"}
        },
        "operations": {"get:/widgets": {"unit": "pass", "e2e": "skipped", "conformance": "pass", "live": "not_run"}}
    })
}

/// `None` removes a default file.
fn make_workspace(overrides: HashMap<&str, Option<String>>) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let mut files: Vec<(&str, Option<String>)> = vec![
        ("widget-co.graphql", Some(SDL.to_string())),
        ("template.yaml", Some(TEMPLATE.to_string())),
        (".factory/workspace.yaml", Some(WORKSPACE.to_string())),
        (".factory/selection.yaml", Some(SELECTION.to_string())),
        (".factory/inventory.json", Some(graphos_factory_core::json::pretty(&inventory()))),
        (".factory/evidence/latest.json", Some(graphos_factory_core::json::pretty(&evidence()))),
        ("tests/cases/list_widgets.graphql", Some("query { widget_co_listWidgets(limit: 2) { id name metadata } }\n".to_string())),
        ("tests/fixtures/mappings/list_widgets.json", Some(json!({"request": {"method": "GET", "urlPath": "/widgets"}, "response": {"status": 200, "jsonBody": {"widgets": []}}}).to_string())),
        ("tests/widget-co.connector.yaml", Some(concat!(
            "tests:\n  - name: \"list\"\n    target: \"Query.widget_co_listWidgets\"\n    apiResponseBody: |\n      {\"widgets\": []}\n",
            "    expect:\n      connectorRequest:\n        method: GET\n        url: https://api.widgets.test/widgets\n        headers:\n          authorization: Bearer test-token\n      connectorResponse: |\n        {\"widgets\": []}\n",
        ).to_string())),
    ];
    for (rel, content) in overrides {
        match files.iter_mut().find(|(r, _)| *r == rel) {
            Some(entry) => entry.1 = content,
            None => files.push((rel, content)),
        }
    }
    // The applied lock matches whatever schema is being written, unless a
    // test supplies its own (to stage a hand edit).
    if !files
        .iter()
        .any(|(r, _)| *r == ".factory/applied.lock.yaml")
    {
        let sdl = files
            .iter()
            .find(|(r, _)| *r == "widget-co.graphql")
            .and_then(|(_, c)| c.clone())
            .unwrap_or_default();
        let inv = files
            .iter()
            .find(|(r, _)| *r == ".factory/inventory.json")
            .and_then(|(_, c)| c.clone())
            .and_then(|c| graphos_factory_core::json::parse(&c).ok());
        let spans = graphos_factory_core::spans::spans(&sdl, inv.as_ref(), &Default::default());
        let lock = graphos_factory_core::spans::lock_document("widget-co.graphql", &spans);
        files.push((
            ".factory/applied.lock.yaml",
            Some(graphos_factory_core::yaml::stringify(&lock, 0)),
        ));
    }
    for (rel, content) in files {
        if let Some(text) = content {
            let file = dir.path().join(rel);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(file, text).unwrap();
        }
    }
    dir
}

fn lint(overrides: Vec<(&str, Option<String>)>) -> LintResult {
    let dir = make_workspace(overrides.into_iter().collect());
    lint_workspace(
        dir.path(),
        &LintOptions {
            schemas_dir: None,
            skip_evidence: false,
            target: &graphos_factory_core::target::BARE,
        },
    )
}

fn rules(result: &LintResult) -> Vec<String> {
    result.findings.iter().map(|f| f.rule.clone()).collect()
}

fn s(x: &str) -> Option<String> {
    Some(x.to_string())
}

#[test]
fn a_well_formed_workspace_lints_clean() {
    let r = lint(vec![]);
    assert!(r.findings.is_empty(), "{:?}", r.findings);
}

#[test]
fn a_second_source_is_an_error() {
    let r = lint(vec![("widget-co.graphql", s(&SDL.replace("scalar Widget_Co_JSON", "@source(name: \"widget_co_admin\", http: { baseURL: \"{{BASE_URL}}\" })\n\nscalar Widget_Co_JSON")))]);
    assert!(rules(&r).contains(&"multiple-sources".to_string()));
}

#[test]
fn a_commented_out_source_is_an_error() {
    let r = lint(vec![(
        "widget-co.graphql",
        s(&SDL.replace(
            "scalar Widget_Co_JSON",
            "# was: @source(name: \"widget_co_admin\")\nscalar Widget_Co_JSON",
        )),
    )]);
    assert!(rules(&r).contains(&"commented-source".to_string()));
}

#[test]
fn the_source_name_must_equal_the_workspace_service() {
    let r = lint(vec![(
        "widget-co.graphql",
        s(&SDL.replace("name: \"widget_co\"", "name: \"widgetCo\"")),
    )]);
    assert!(rules(&r).contains(&"source-name".to_string()));
}

#[test]
fn an_unknown_placeholder_is_an_error() {
    let r = lint(vec![
        (
            "widget-co.graphql",
            s(&SDL.replace("\"Bearer {{AUTH_EXPR}}\"", "\"Bearer {{VENDOR_TOKEN}}\"")),
        ),
        (
            "template.yaml",
            s(&TEMPLATE.replace("AUTH_EXPR", "VENDOR_TOKEN")),
        ),
    ]);
    assert!(rules(&r).contains(&"unknown-placeholder".to_string()));
}

#[test]
fn token_var_is_refused_as_legacy_flat_auth() {
    let r = lint(vec![
        (
            "widget-co.graphql",
            s(&SDL.replace("{{AUTH_EXPR}}", "{{TOKEN_VAR}}")),
        ),
        (
            "template.yaml",
            s(&TEMPLATE
                .replace("AUTH_EXPR", "TOKEN_VAR")
                .replace("{$env.WIDGET_CO_TOKEN}", "test")),
        ),
    ]);
    assert!(rules(&r).contains(&"legacy-placeholder".to_string()));
}

#[test]
fn wrapping_auth_expr_in_env_is_an_error() {
    let r = lint(vec![(
        "widget-co.graphql",
        s(&SDL.replace("Bearer {{AUTH_EXPR}}", "Bearer {$env.{{AUTH_EXPR}}}")),
    )]);
    assert!(rules(&r).contains(&"wrapped-auth-expr".to_string()));
}

#[test]
fn an_undeclared_placeholder_is_an_error() {
    let r = lint(vec![(
        "template.yaml",
        s(TEMPLATE.split("  - name: AUTH_EXPR").next().unwrap()),
    )]);
    assert!(rules(&r).contains(&"undeclared-placeholder".to_string()));
}

#[test]
fn an_unused_template_variable_is_an_error() {
    let r = lint(vec![(
        "template.yaml",
        s(&format!(
            "{}  - name: REGION\n    description: \"unused\"\n    test_default: \"x\"\n",
            TEMPLATE
        )),
    )]);
    assert!(rules(&r).contains(&"unused-template-variable".to_string()));
}

#[test]
fn a_template_variable_without_a_test_default_is_an_error() {
    let r = lint(vec![(
        "template.yaml",
        s(&TEMPLATE.replace("    test_default: \"https://api.widgets.test\"\n", "")),
    )]);
    assert!(rules(&r).contains(&"missing-test-default".to_string()));
}

#[test]
fn auth_expr_test_default_must_be_an_unquoted_local_expression() {
    let quoted = lint(vec![(
        "template.yaml",
        s(&TEMPLATE.replace("{$env.WIDGET_CO_TOKEN}", "{$env.'WIDGET_CO_TOKEN'}")),
    )]);
    assert!(rules(&quoted).contains(&"auth-test-default".to_string()));
    let bare = lint(vec![(
        "template.yaml",
        s(&TEMPLATE.replace("{$env.WIDGET_CO_TOKEN}", "test-token")),
    )]);
    assert!(rules(&bare).contains(&"auth-test-default".to_string()));
}

#[test]
fn a_type_without_the_workspace_prefix_is_an_error() {
    let r = lint(vec![(
        "widget-co.graphql",
        s(&SDL.replace("Widget_Co_Widget", "Widget")),
    )]);
    assert!(rules(&r).contains(&"type-prefix".to_string()));
}

/// ADR 0051 lever 4. `type_declarations` read `(type|enum|input|…) Name`
/// anywhere in the schema text, so an enum whose values include `enum`,
/// `type`, `input` or `scalar` followed by another value (Databricks
/// `SqlParameterType { datetime enum number query text }`) reported
/// `[type-prefix] enum number must be prefixed`. A declaration is now read
/// only outside every type body and argument list.
#[test]
fn an_enum_value_named_like_a_keyword_is_not_a_declaration() {
    let sdl = SDL.replace(
        "type Query {",
        "enum Widget_Co_ParameterType {\n  datetime\n  enum\n  number\n  type\n  query\n  input\n  text\n}\n\ntype Query {",
    );
    let r = lint(vec![("widget-co.graphql", s(&sdl))]);
    let prefix: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "type-prefix")
        .map(|f| f.message.clone())
        .collect();
    assert!(prefix.is_empty(), "{:?}", prefix);
    let decls: Vec<String> = graphos_factory_core::graphql::type_declarations(&sdl)
        .into_iter()
        .map(|d| format!("{} {}", d.kind, d.name))
        .collect();
    assert!(
        decls.contains(&"enum Widget_Co_ParameterType".to_string()),
        "{:?}",
        decls
    );
    assert!(
        !decls
            .iter()
            .any(|d| d == "enum number" || d == "type query" || d == "input text"),
        "{:?}",
        decls
    );
}

#[test]
fn a_root_field_without_the_field_prefix_is_an_error() {
    let r = lint(vec![(
        "widget-co.graphql",
        s(&SDL.replace("widget_co_listWidgets", "listWidgets")),
    )]);
    assert!(rules(&r).contains(&"field-prefix".to_string()));
}

#[test]
fn a_connect_spec_link_that_drifts_from_the_pin_is_an_error() {
    let r = lint(vec![(
        "widget-co.graphql",
        s(&SDL.replace("connect/v0.3", "connect/v0.4")),
    )]);
    assert!(rules(&r).contains(&"connect-spec-drift".to_string()));
}

#[test]
fn a_federation_link_that_drifts_from_the_pin_is_an_error_with_no_spec_version_set() {
    // Legacy workspace, no federation_spec_version: falls back to comparing
    // against federation_version itself, exactly as before that field
    // existed. Genuine schema-spec drift must still fail.
    let r = lint(vec![(
        "widget-co.graphql",
        s(&SDL.replace("federation/v2.12", "federation/v2.13")),
    )]);
    assert!(rules(&r).contains(&"federation-drift".to_string()));
}

#[test]
fn a_composition_plugin_ahead_of_the_linked_federation_spec_is_accepted() {
    // The F10-verified target: composition plugin 2.15.1 (federation_version,
    // matched against supergraph.yaml by render) linking federation/v2.14 in
    // the schema (federation_spec_version) is a normal, supported
    // combination, not drift.
    let r = lint(vec![
        (
            "widget-co.graphql",
            s(&SDL.replace("federation/v2.12", "federation/v2.14")),
        ),
        (
            ".factory/workspace.yaml",
            s(&WORKSPACE.replace(
                "federation_version: \"2.12.0\"",
                "federation_version: \"2.15.1\"\nfederation_spec_version: \"2.14\"",
            )),
        ),
    ]);
    assert!(
        !rules(&r).contains(&"federation-drift".to_string()),
        "{:?}",
        rules(&r)
    );
}

#[test]
fn a_federation_spec_version_that_drifts_from_the_link_is_an_error() {
    // With federation_spec_version explicitly set, genuine schema-spec
    // drift against *that* field must still fail, even though the plugin
    // pin (federation_version) is unrelated and unchanged.
    let r = lint(vec![
        (
            "widget-co.graphql",
            s(&SDL.replace("federation/v2.12", "federation/v2.13")),
        ),
        (
            ".factory/workspace.yaml",
            s(&WORKSPACE.replace(
                "federation_version: \"2.12.0\"",
                "federation_version: \"2.15.1\"\nfederation_spec_version: \"2.14\"",
            )),
        ),
    ]);
    assert!(rules(&r).contains(&"federation-drift".to_string()));
}

#[test]
fn an_undocumented_json_scalar_is_an_error_under_the_default_policy() {
    let r = lint(vec![(
        "widget-co.graphql",
        s(&SDL.replace(
            "  \"Free-form vendor metadata; the spec documents no properties.\"\n",
            "",
        )),
    )]);
    assert!(rules(&r).contains(&"undocumented-json-scalar".to_string()));
}

#[test]
fn allow_with_reason_downgrades_an_undocumented_json_scalar_to_a_warning() {
    let r = lint(vec![
        (
            "widget-co.graphql",
            s(&SDL.replace(
                "  \"Free-form vendor metadata; the spec documents no properties.\"\n",
                "",
            )),
        ),
        (
            ".factory/selection.yaml",
            s(&SELECTION.replace(
                "opaque_json_policy: forbid",
                "opaque_json_policy: allow_with_reason",
            )),
        ),
    ]);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "undocumented-json-scalar")
        .unwrap();
    assert_eq!(f.severity, "warn");
}

#[test]
fn selecting_an_operation_the_inventory_does_not_have_is_an_error() {
    let r = lint(vec![(
        ".factory/selection.yaml",
        s(&SELECTION.replace("get:/widgets", "get:/widgetz")),
    )]);
    assert!(rules(&r).contains(&"unknown-operation".to_string()));
}

#[test]
fn an_included_operation_with_no_root_is_an_error() {
    let r = lint(vec![(
        ".factory/selection.yaml",
        s(&SELECTION.replace("      root: query\n", "")),
    )]);
    assert!(rules(&r).contains(&"missing-root".to_string()));
}

#[test]
fn including_an_unsupported_operation_without_a_force_reason_is_an_error() {
    let mut inv = inventory();
    inv["operations"][0]["support"] = json!("unsupported");
    inv["operations"][0]["support_reason"] = json!("response is text/csv");
    let r = lint(vec![(
        ".factory/inventory.json",
        s(&graphos_factory_core::json::pretty(&inv)),
    )]);
    assert!(rules(&r).contains(&"forced-unsupported".to_string()));
}

#[test]
fn a_force_reason_satisfies_the_unsupported_gate() {
    let mut inv = inventory();
    inv["operations"][0]["support"] = json!("unsupported");
    inv["operations"][0]["support_reason"] = json!("response is text/csv");
    let r = lint(vec![
        (".factory/inventory.json", s(&graphos_factory_core::json::pretty(&inv))),
        (".factory/selection.yaml", s(&SELECTION.replace("    include: true", "    include: true\n    force_reason: \"vendor confirmed JSON is served when Accept is set\""))),
    ]);
    assert!(!rules(&r).contains(&"forced-unsupported".to_string()));
}

#[test]
fn two_operations_mapped_to_the_same_field_name_are_an_error() {
    let mut inv = inventory();
    let mut second = inv["operations"][0].clone();
    second["key"] = json!("get:/widgets/all");
    second["path"] = json!("/widgets/all");
    inv["operations"].as_array_mut().unwrap().push(second);
    let r = lint(vec![
        (".factory/inventory.json", s(&graphos_factory_core::json::pretty(&inv))),
        (".factory/selection.yaml", s(&format!("{}  \"get:/widgets/all\":\n    include: true\n    graphql:\n      root: query\n      name: listWidgets\n", SELECTION))),
    ]);
    assert!(rules(&r).contains(&"duplicate-name".to_string()));
}

#[test]
fn an_included_operation_missing_from_the_schema_is_an_error() {
    let cut = SDL.find("type Query {").unwrap();
    let r = lint(vec![("widget-co.graphql", s(&SDL[..cut]))]);
    assert!(rules(&r).contains(&"missing-field".to_string()));
}

#[test]
fn an_included_operation_with_no_test_case_is_an_error() {
    let r = lint(vec![
        ("tests/cases/list_widgets.graphql", None),
        ("tests/fixtures/mappings/list_widgets.json", None),
    ]);
    assert!(
        rules(&r).contains(&"missing-case".to_string()),
        "{:?}",
        r.findings
    );
}

#[test]
fn a_case_with_no_scoped_fixture_is_an_error() {
    let r = lint(vec![
        ("tests/fixtures/mappings/list_widgets.json", None),
        (
            "tests/fixtures/mappings/something_else.json",
            s(&json!({"metadata": {"x-cases": ["another_case"]}, "request": {"method": "GET", "urlPath": "/widgets"}, "response": {"status": 200, "jsonBody": {}}}).to_string()),
        ),
    ]);
    assert!(rules(&r).contains(&"missing-fixture".to_string()));
}

#[test]
fn an_x_cases_tag_scopes_a_fixture_to_a_case_whose_filename_differs() {
    let r = lint(vec![
        ("tests/fixtures/mappings/list_widgets.json", None),
        (
            "tests/fixtures/mappings/recorded-2026-09-08.json",
            s(&json!({"metadata": {"x-cases": ["list_widgets"]}, "request": {"method": "GET", "urlPath": "/widgets"}, "response": {"status": 200, "jsonBody": {}}}).to_string()),
        ),
    ]);
    assert!(!rules(&r).contains(&"missing-fixture".to_string()));
}

#[test]
fn two_stubs_with_an_identical_request_are_a_fixture_collision() {
    let r = lint(vec![(
        "tests/fixtures/mappings/list_widgets_conflict.json",
        s(&json!({"metadata": {"x-cases": ["list_widgets"]}, "request": {"method": "GET", "urlPath": "/widgets"}, "response": {"status": 409, "jsonBody": {}}}).to_string()),
    )]);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "fixture-collision")
        .unwrap_or_else(|| panic!("no fixture-collision finding: {:?}", r.findings));
    assert_eq!(f.severity, "error");
    assert!(f.message.contains("list_widgets.json"), "{}", f.message);
    assert!(
        f.message.contains("list_widgets_conflict.json"),
        "{}",
        f.message
    );
}

#[test]
fn stubs_that_differ_by_query_parameters_are_not_a_collision() {
    let r = lint(vec![(
        "tests/fixtures/mappings/list_widgets_filtered.json",
        s(&json!({"metadata": {"x-cases": ["list_widgets"]}, "request": {"method": "GET", "urlPath": "/widgets", "queryParameters": {"limit": {"equalTo": "2"}}}, "response": {"status": 200, "jsonBody": {}}}).to_string()),
    )]);
    assert!(
        !rules(&r).contains(&"fixture-collision".to_string()),
        "{:?}",
        r.findings
    );
}

/// ADR 0115: thecatapi's `generate_genealogy_minimal` stub named no query
/// parameter, so it also matched `generate_genealogy`'s `?lang=` request,
/// and which of the two answered was the load order's call.
#[test]
fn a_stub_constraining_a_subset_of_a_siblings_request_is_a_fixture_overlap() {
    let r = lint(vec![(
        "tests/fixtures/mappings/list_widgets_limited.json",
        s(&json!({"metadata": {"x-cases": ["list_widgets"]}, "request": {"method": "GET", "urlPath": "/widgets", "queryParameters": {"limit": {"equalTo": "2"}}}, "response": {"status": 200, "jsonBody": {}}}).to_string()),
    )]);
    let f: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "fixture-overlap")
        .collect();
    assert_eq!(f.len(), 1, "{:?}", r.findings);
    assert_eq!(f[0].severity, "error");
    assert!(
        f[0].message.starts_with(
            "list_widgets.json matches every request list_widgets_limited.json matches"
        ),
        "{}",
        f[0].message
    );
    assert!(
        f[0].message
            .contains("add {\"absent\": true} to list_widgets.json for queryParameters.limit"),
        "{}",
        f[0].message
    );
}

#[test]
fn an_absent_matcher_or_a_higher_priority_settles_an_overlap() {
    let specific = json!({"metadata": {"x-cases": ["list_widgets"]}, "request": {"method": "GET", "urlPath": "/widgets", "queryParameters": {"limit": {"equalTo": "2"}}}, "response": {"status": 200, "jsonBody": {}}});
    let absent = lint(vec![
        (
            "tests/fixtures/mappings/list_widgets.json",
            s(&json!({"request": {"method": "GET", "urlPath": "/widgets", "queryParameters": {"limit": {"absent": true}}}, "response": {"status": 200, "jsonBody": {"widgets": []}}}).to_string()),
        ),
        (
            "tests/fixtures/mappings/list_widgets_limited.json",
            s(&specific.to_string()),
        ),
    ]);
    assert!(
        !rules(&absent).contains(&"fixture-overlap".to_string()),
        "{:?}",
        absent.findings
    );
    let mut ranked = specific.clone();
    ranked["priority"] = json!(1);
    let priority = lint(vec![(
        "tests/fixtures/mappings/list_widgets_limited.json",
        s(&ranked.to_string()),
    )]);
    assert!(
        !rules(&priority).contains(&"fixture-overlap".to_string()),
        "{:?}",
        priority.findings
    );
}

/// `equalToJson` without `ignoreExtraElements` is exact: `{}` and a full
/// body never match the same request (measured on WireMock 3.13.2), so the
/// scaffold's write pair is not an overlap.
#[test]
fn two_distinct_exact_bodies_on_one_path_are_not_an_overlap() {
    let stub = |body: Value| {
        json!({"metadata": {"x-cases": ["list_widgets"]}, "request": {"method": "POST", "urlPath": "/widgets", "bodyPatterns": [{"equalToJson": body}]}, "response": {"status": 201, "jsonBody": {}}}).to_string()
    };
    let r = lint(vec![
        (
            "tests/fixtures/mappings/create_full.json",
            s(&stub(json!({"name": "a", "color": "RED"}))),
        ),
        (
            "tests/fixtures/mappings/create_min.json",
            s(&stub(json!({}))),
        ),
    ]);
    assert!(
        !rules(&r).contains(&"fixture-overlap".to_string()),
        "{:?}",
        r.findings
    );
}

#[test]
fn a_wiremock_scenario_pair_with_distinct_states_is_not_a_collision() {
    // A real RPC-style customFields.fetch: the request is empty by definition,
    // so it is byte-identical for a success and a negative-control case —
    // WireMock Scenarios is the only way to answer them differently.
    let r = lint(vec![
        (
            "tests/fixtures/mappings/list_widgets.json",
            s(&json!({"scenarioName": "fetch", "requiredScenarioState": "Started", "request": {"method": "GET", "urlPath": "/widgets"}, "response": {"status": 200, "jsonBody": {"widgets": []}}}).to_string()),
        ),
        (
            "tests/fixtures/mappings/list_widgets_error.json",
            s(&json!({"metadata": {"x-cases": ["list_widgets"]}, "scenarioName": "fetch", "requiredScenarioState": "Errored", "request": {"method": "GET", "urlPath": "/widgets"}, "response": {"status": 500, "jsonBody": {}}}).to_string()),
        ),
    ]);
    assert!(
        !rules(&r).contains(&"fixture-collision".to_string()),
        "{:?}",
        r.findings
    );
}

#[test]
fn a_wiremock_scenario_pair_sharing_the_same_state_is_still_a_collision() {
    // The whole point of a scenario pair is a distinct state each — the
    // same state on both is exactly as ambiguous as no scenario at all.
    let r = lint(vec![
        (
            "tests/fixtures/mappings/list_widgets.json",
            s(&json!({"scenarioName": "fetch", "requiredScenarioState": "Started", "request": {"method": "GET", "urlPath": "/widgets"}, "response": {"status": 200, "jsonBody": {"widgets": []}}}).to_string()),
        ),
        (
            "tests/fixtures/mappings/list_widgets_error.json",
            s(&json!({"metadata": {"x-cases": ["list_widgets"]}, "scenarioName": "fetch", "requiredScenarioState": "Started", "request": {"method": "GET", "urlPath": "/widgets"}, "response": {"status": 500, "jsonBody": {}}}).to_string()),
        ),
    ]);
    assert!(
        rules(&r).contains(&"fixture-collision".to_string()),
        "{:?}",
        r.findings
    );
}

#[test]
fn a_missing_connector_unit_entry_is_a_warning_not_an_error() {
    let r = lint(vec![(
        "tests/widget-co.connector.yaml",
        s("tests:\n  - name: \"other\"\n    target: \"Query.widget_co_other\"\n"),
    )]);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "missing-unit")
        .unwrap();
    assert_eq!(f.severity, "warn");
    assert_eq!(r.errors, 0);
}

#[test]
fn evidence_that_skips_a_layer_without_a_reason_is_an_error() {
    let mut ev = evidence();
    ev["layers"]["wiremock_e2e"]
        .as_object_mut()
        .unwrap()
        .remove("reason");
    let r = lint(vec![(
        ".factory/evidence/latest.json",
        s(&graphos_factory_core::json::pretty(&ev)),
    )]);
    assert!(rules(&r).contains(&"unexplained-status".to_string()));
}

#[test]
fn an_included_operation_absent_from_evidence_is_an_error() {
    let mut ev = evidence();
    ev["operations"] = json!({});
    let r = lint(vec![(
        ".factory/evidence/latest.json",
        s(&graphos_factory_core::json::pretty(&ev)),
    )]);
    assert!(rules(&r).contains(&"unproven-operation".to_string()));
}

#[test]
fn no_evidence_at_all_is_a_warning() {
    let r = lint(vec![(".factory/evidence/latest.json", None)]);
    let f = r.findings.iter().find(|f| f.rule == "no-evidence").unwrap();
    assert_eq!(f.severity, "warn");
}

#[test]
fn skip_evidence_leaves_the_evidence_file_unchecked() {
    let dir = make_workspace(
        [(".factory/evidence/latest.json", None)]
            .into_iter()
            .collect(),
    );
    let r = lint_workspace(
        dir.path(),
        &LintOptions {
            schemas_dir: None,
            skip_evidence: true,
            target: &graphos_factory_core::target::BARE,
        },
    );
    assert!(!rules(&r).contains(&"no-evidence".to_string()));
}

#[test]
fn a_workspace_that_violates_the_contract_schema_is_reported() {
    let r = lint(vec![(
        ".factory/workspace.yaml",
        s(&WORKSPACE.replace("service: widget_co", "service: widgetCo")),
    )]);
    assert!(rules(&r).contains(&"contract".to_string()));
}

#[test]
fn service_and_directory_must_be_the_same_name_in_two_cases() {
    let r = lint(vec![(
        ".factory/workspace.yaml",
        s(&WORKSPACE.replace("directory: widget-co", "directory: widgets-co")),
    )]);
    assert!(rules(&r).contains(&"name-mismatch".to_string()));
}

/// The widget-co workspace renamed to `file_system`: snake_case directory,
/// `file_system.graphql`, and the `File_System` prefix the directory derives.
fn file_system_workspace(directory: &str) -> tempfile::TempDir {
    let rename = |text: &str| {
        text.replace("Widget_Co", "File_System")
            .replace("widget_co", "file_system")
            .replace("WIDGET_CO", "FILE_SYSTEM")
            .replace("widget-co", directory)
    };
    let sdl = rename(SDL);
    let spans = graphos_factory_core::spans::spans(&sdl, Some(&inventory()), &Default::default());
    let lock = graphos_factory_core::yaml::stringify(
        &graphos_factory_core::spans::lock_document(&format!("{directory}.graphql"), &spans),
        0,
    );
    let unit = concat!(
        "tests:\n  - name: \"list\"\n    target: \"Query.widget_co_listWidgets\"\n    apiResponseBody: |\n      {\"widgets\": []}\n",
        "    expect:\n      connectorRequest:\n        method: GET\n        url: https://api.widgets.test/widgets\n        headers:\n          authorization: Bearer test-token\n      connectorResponse: |\n        {\"widgets\": []}\n",
    );
    let schema_file = format!("{directory}.graphql");
    let unit_file = format!("tests/{directory}.connector.yaml");
    make_workspace(
        [
            ("widget-co.graphql", None),
            ("tests/widget-co.connector.yaml", None),
            (schema_file.as_str(), Some(sdl)),
            (unit_file.as_str(), Some(rename(unit))),
            (".factory/applied.lock.yaml", Some(lock)),
            (".factory/workspace.yaml", Some(rename(WORKSPACE))),
            ("template.yaml", Some(rename(TEMPLATE))),
            (
                "tests/cases/list_widgets.graphql",
                Some(
                    "query { file_system_listWidgets(limit: 2) { id name metadata } }\n"
                        .to_string(),
                ),
            ),
        ]
        .into_iter()
        .collect(),
    )
}

fn lint_dir(dir: &tempfile::TempDir) -> LintResult {
    lint_workspace(
        dir.path(),
        &LintOptions {
            schemas_dir: None,
            skip_evidence: false,
            target: &graphos_factory_core::target::BARE,
        },
    )
}

#[test]
fn a_snake_case_directory_lints_clean() {
    // A directory matching [a-z][a-z0-9_-]* is valid: `file_system` derives
    // File_System_ exactly as `file-system` does (ADR 0108). Before, the workspace schema allowed
    // kebab-case only and this reported `contract`.
    let dir = file_system_workspace("file_system");
    let r = lint_dir(&dir);
    assert!(r.findings.is_empty(), "{:?}", r.findings);
}

#[test]
fn a_kebab_case_directory_still_lints_clean() {
    let dir = file_system_workspace("file-system");
    let r = lint_dir(&dir);
    assert!(r.findings.is_empty(), "{:?}", r.findings);
}

#[test]
fn a_directory_outside_kebab_or_snake_case_is_a_contract_error() {
    for directory in [
        "File_System",
        "file-system_",
        "file__system",
        "file_-system",
        "_file_system",
        "1file_system",
    ] {
        let r = lint(vec![(
            ".factory/workspace.yaml",
            s(&WORKSPACE.replace("directory: widget-co", &format!("directory: {directory}"))),
        )]);
        assert!(
            r.findings
                .iter()
                .any(|f| f.rule == "contract" && f.message.contains("directory")),
            "{directory}: {:?}",
            r.findings
        );
    }
}

#[test]
fn a_directory_that_is_not_a_workspace_fails_fast() {
    let dir = tempfile::tempdir().unwrap();
    let r = lint_workspace(
        Path::new(dir.path()),
        &LintOptions {
            schemas_dir: None,
            skip_evidence: false,
            target: &graphos_factory_core::target::BARE,
        },
    );
    assert_eq!(rules(&r), vec!["missing-workspace"]);
}

fn lock_for(sdl: &str) -> String {
    let spans = graphos_factory_core::spans::spans(sdl, Some(&inventory()), &Default::default());
    graphos_factory_core::yaml::stringify(
        &graphos_factory_core::spans::lock_document("widget-co.graphql", &spans),
        0,
    )
}

#[test]
fn a_hand_edit_since_the_applied_lock_is_an_error_naming_the_span() {
    let edited = SDL.replace(
        "widget_co_listWidgets(limit: Int)",
        "widget_co_listWidgets(limit: Int, offset: Int)",
    );
    let r = lint(vec![
        ("widget-co.graphql", s(&edited)),
        (".factory/applied.lock.yaml", s(&lock_for(SDL))),
    ]);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "unacknowledged-edit")
        .expect("unacknowledged-edit");
    assert!(
        f.message
            .starts_with("get:/widgets changed since applied.lock.yaml"),
        "{}",
        f.message
    );
    assert!(f
        .message
        .contains("graphos-factory-core codify --key \"get:/widgets\""));
    assert_eq!(f.line, Some(23));
    assert!(r.errors >= 1);
}

#[test]
fn a_missing_applied_lock_is_only_a_warning() {
    let r = lint(vec![(".factory/applied.lock.yaml", None)]);
    assert_eq!(rules(&r), vec!["no-applied-lock"]);
    assert_eq!(r.errors, 0);
}

#[test]
fn overrides_are_checked_against_the_schema_and_the_retired_list_is_an_error() {
    let selection = format!(
        "{}overrides:\n  - key: \"get:/widgets\"\n    reason: \"limit first\"\n    decision: D-0002\n    assert:\n      - arg: limit\n      - tag: internal\n  - key: \"type:Widget_Co_Widget\"\n    reason: \"frozen\"\n  - key: \"type:Nope\"\n    reason: \"typo\"\n    expires: \"2020-01-01\"\n",
        SELECTION
    );
    let r = lint(vec![(".factory/selection.yaml", s(&selection))]);
    let names = rules(&r);
    assert!(
        names.contains(&"override-assertion-failed".to_string()),
        "{:?}",
        r.findings
    );
    let failed = r
        .findings
        .iter()
        .find(|f| f.rule == "override-assertion-failed")
        .unwrap();
    assert!(failed.message.contains("`tag \"internal\"` does not hold"));
    assert!(failed.message.contains("D-0002"));
    assert!(names.contains(&"override-pinned".to_string()));
    // ADR 0113 retired override-undecided: `reason` carries the why, and an
    // override with no decision draws nothing.
    assert!(!names.contains(&"override-undecided".to_string()));
    assert!(names.contains(&"override-unknown-key".to_string()));
    assert!(names.contains(&"override-expired".to_string()));
    assert!(!names.contains(&"unacknowledged-edit".to_string()));

    let retired = format!("{}customized:\n  - \"get:/widgets\"\n", SELECTION);
    let r = lint(vec![(".factory/selection.yaml", s(&retired))]);
    assert!(rules(&r).contains(&"customized-retired".to_string()));
    assert!(
        rules(&r).contains(&"contract".to_string()),
        "the schema rejects it too"
    );
}

#[test]
fn waiver_rules_mirror_the_override_rules() {
    // Targets: both / neither → bad-target; an unknown operation; a bad
    // status; no decision; expired; and one that matches nothing, because
    // the list fixture conforms and so is never `unchecked`.
    let sel = format!(
        "{}waivers:\n  - where: \"tests/fixtures/mappings/list_widgets.json\"\n    operation: \"get:/widgets\"\n    status: unchecked\n    reason: both\n    decision: D-0001\n  - operation: \"get:/gadgets\"\n    status: unchecked\n    reason: unknown op\n    decision: D-0001\n  - operation: \"get:/widgets\"\n    status: pass\n    reason: bad status\n    decision: D-0001\n  - where: \"tests/fixtures/mappings/list_widgets.json\"\n    status: unchecked\n    reason: undecided and unused\n    expires: \"2020-01-01\"\n",
        SELECTION
    );
    let r = lint(vec![(".factory/selection.yaml", s(&sel))]);
    let names = rules(&r);
    for rule in [
        "waiver-bad-target",
        "waiver-unknown-key",
        "waiver-bad-status",
        "waiver-expired",
        "waiver-unused",
    ] {
        assert!(
            names.contains(&rule.to_string()),
            "missing {}: {:?}",
            rule,
            names
        );
    }
    let unused: Vec<&str> = r
        .findings
        .iter()
        .filter(|f| f.rule == "waiver-unused")
        .map(|f| f.message.as_str())
        .collect();
    assert!(
        unused
            .iter()
            .any(|m| m.contains("list_widgets.json (unchecked)")),
        "{:?}",
        unused
    );
    assert!(
        names.contains(&"contract".to_string()),
        "the schema rejects the malformed entries too"
    );

    // A waiver that matches a real gap is clean: a 404 fixture for a status
    // the spec does not document.
    let gap = json!({"request": {"method": "GET", "urlPath": "/widgets"}, "response": {"status": 404, "jsonBody": {"message": "gone"}}}).to_string();
    let sel = format!(
        "{}waivers:\n  - where: \"tests/fixtures/mappings/widget_missing.json\"\n    status: unchecked\n    reason: \"no 404 body is documented\"\n    decision: D-0001\n",
        SELECTION
    );
    let r = lint(vec![
        (".factory/selection.yaml", s(&sel)),
        ("tests/fixtures/mappings/widget_missing.json", s(&gap)),
        // the new fixture needs a case of its own to keep `x-cases` quiet
        (
            "tests/cases/widget_missing.graphql",
            s("query { widget_co_listWidgets { id } }\n"),
        ),
    ]);
    let names = rules(&r);
    assert!(
        !names.iter().any(|n| n.starts_with("waiver-")),
        "{:?}",
        names
    );
}

#[test]
fn root_field_args_and_passed_args_read_declarations_and_documents() {
    use graphos_factory_core::lint::{passed_args, root_field_args, Arg};
    let sdl = r#"
type Query {
  "doc"
  w_list(
    limit: Int
    "a list"
    ids: [ID!]
    tags: [String!]!
  ): [W] @connect(source: "w", http: { GET: "/w" }, selection: "$.w { id }")
  w_one(id: ID!): W @connect(source: "w", http: { GET: "/w/{$args.id}" }, selection: "id")
  w_none: [W] @connect(source: "w", http: { GET: "/all" }, selection: "id")
}
type Mutation {
  w_create(from: String!, title: String!, body: String, labels: [Int!]): W
}
"#;
    let q = root_field_args(sdl, "Query");
    assert_eq!(q.len(), 2, "w_none has no argument list: {:?}", q);
    let (name, args) = &q[0];
    assert_eq!(name, "w_list");
    assert_eq!(
        args,
        &vec![
            Arg {
                name: "limit".into(),
                type_: "Int".into()
            },
            Arg {
                name: "ids".into(),
                type_: "[ID!]".into()
            },
            Arg {
                name: "tags".into(),
                type_: "[String!]!".into()
            },
        ]
    );
    assert!(args[1].is_list() && !args[1].is_required());
    assert!(args[2].is_list() && args[2].is_required());
    let m = root_field_args(sdl, "Mutation");
    assert_eq!(m[0].1.iter().filter(|a| a.is_required()).count(), 2);

    let doc = "# two ids on purpose\nquery {\n  w_list(limit: 2, ids: [\"a, b\", \"c\"], tags: [x]) {\n    id\n  }\n}\n";
    let passed = passed_args(doc, "w_list").unwrap();
    assert_eq!(
        passed,
        vec![
            ("limit".to_string(), None),
            ("ids".to_string(), Some(2)),
            ("tags".to_string(), Some(1)),
        ],
        "the comma inside the string does not count"
    );
    let multi = "mutation {\n  w_create(\n    from: \"ana@example.com\"\n    title: \"Disk full\"\n    labels: []\n  ) { id }\n}\n";
    assert_eq!(
        passed_args(multi, "w_create").unwrap(),
        vec![
            ("from".to_string(), None),
            ("title".to_string(), None),
            ("labels".to_string(), Some(0))
        ]
    );
    assert!(
        passed_args(doc, "w_one").is_none(),
        "not called in this document"
    );
    assert_eq!(
        passed_args("query { w_none { id } }", "w_none"),
        None,
        "no argument list"
    );
}

#[test]
fn test_shape_rules_report_thin_unit_entries() {
    // A schema with a list argument on the query and a write with optional
    // arguments; the default workspace's case passes no list and there is no
    // write case at all.
    let sdl = SDL.replace(
        "  widget_co_listWidgets(limit: Int): [Widget_Co_Widget]",
        "  widget_co_listWidgets(limit: Int, ids: [ID!]): [Widget_Co_Widget]",
    ) + "\ntype Mutation {\n  widget_co_createWidget(name: String!, color: String, tags: [String!]): Widget_Co_Widget\n    @tag(name: \"beta\")\n    @connect(source: \"widget_co\", http: { POST: \"/widgets\", body: \"name: $args.name color: $args.color tags: $args.tags\" }, selection: \"$.widget { id name }\")\n}\n";
    let selection = format!(
        "{}  \"post:/widgets\":\n    include: true\n    graphql: {{ root: mutation, name: createWidget }}\n",
        SELECTION
    );
    let inventory = {
        let mut inv = inventory();
        inv["operations"].as_array_mut().unwrap().push(json!({"key": "post:/widgets", "operation_id": "createWidget", "method": "POST", "path": "/widgets", "semantics": "unknown", "provenance": "spec", "confidence": 1, "parameters": [], "request_body": null,
            "response": {"status": "201", "content_type": "application/json", "envelope": "widget", "shape_ref": "#/shapes/GetWidget", "list": false}, "errors": [], "support": "supported", "support_reason": null}));
        graphos_factory_core::json::pretty(&inv)
    };
    let loose = json!({"request": {"method": "POST", "urlPath": "/widgets", "bodyPatterns": [{"equalToJson": {"name": "a"}, "ignoreExtraElements": true}]}, "response": {"status": 201, "jsonBody": {"widget": {"id": "W9", "name": "a"}}}}).to_string();
    let unit = concat!(
        "tests:\n",
        "  - name: \"list\"\n    target: \"Query.widget_co_listWidgets\"\n    apiResponseBody: |\n      {\"widgets\": []}\n",
        "    expect:\n      connectorRequest:\n        method: GET\n        url: https://api.widgets.test/widgets\n",
        "  - name: \"create\"\n    target: \"Mutation.widget_co_createWidget\"\n    apiResponseBody: |\n      {\"widget\": {\"id\": \"W9\", \"name\": \"a\"}}\n",
        "    expect:\n      connectorRequest:\n        method: POST\n        url: https://api.widgets.test/widgets\n      connectorResponse: |\n        {\"id\": \"W9\", \"name\": \"a\"}\n",
    );
    let r = lint(vec![
        ("widget-co.graphql", s(&sdl)),
        (".factory/selection.yaml", s(&selection)),
        (".factory/inventory.json", s(&inventory)),
        ("tests/widget-co.connector.yaml", s(unit)),
        (
            "tests/cases/create_widget.graphql",
            s("mutation { widget_co_createWidget(name: \"a\", color: \"red\") { id } }\n"),
        ),
        ("tests/fixtures/mappings/create_widget.json", s(&loose)),
    ]);
    let msgs: Vec<String> = r
        .findings
        .iter()
        .map(|f| format!("{} {}", f.rule, f.message))
        .collect();
    let has = |rule: &str, frag: &str| msgs.iter().any(|m| m.starts_with(rule) && m.contains(frag));
    assert!(
        has(
            "unit-no-credential",
            "widget-co.connector.yaml › list asserts the request but not the authorization header"
        ),
        "{:?}",
        msgs
    );
    assert!(
        has(
            "unit-no-response",
            "1 unit entry targets widget_co_listWidgets and none asserts connectorResponse"
        ),
        "{:?}",
        msgs
    );
    assert!(
        !has("unit-no-response", "widget_co_createWidget"),
        "the create entry asserts its response: {:?}",
        msgs
    );

    // Everything proven: a two-element list behind a hasExactly stub, full
    // and minimal write cases with exact bodies, unit entries asserting the
    // credential, the body and the mapping, and a loose body explained.
    let listed = json!({"request": {"method": "GET", "urlPath": "/widgets", "queryParameters": {"ids[]": {"hasExactly": [{"equalTo": "1"}, {"equalTo": "2"}]}}}, "response": {"status": 200, "jsonBody": {"widgets": []}}}).to_string();
    let full = json!({"request": {"method": "POST", "urlPath": "/widgets", "bodyPatterns": [{"equalToJson": {"name": "a", "color": "red", "tags": ["x", "y"]}}]}, "response": {"status": 201, "jsonBody": {"widget": {"id": "W9", "name": "a"}}}}).to_string();
    let minimal = json!({"request": {"method": "POST", "urlPath": "/widgets", "bodyPatterns": [{"equalToJson": {"name": "a"}, "ignoreExtraElements": true}]}, "metadata": {"x-loose-body": "the API echoes server-set keys we do not send"}, "response": {"status": 201, "jsonBody": {"widget": {"id": "W9", "name": "a"}}}}).to_string();
    let unit_ok = concat!(
        "tests:\n",
        "  - name: \"list\"\n    target: \"Query.widget_co_listWidgets\"\n    apiResponseBody: |\n      {\"widgets\": []}\n",
        "    expect:\n      connectorRequest:\n        method: GET\n        url: https://api.widgets.test/widgets\n        headers:\n          authorization: Bearer test-token\n      connectorResponse: |\n        {\"widgets\": []}\n",
        "  - name: \"create\"\n    target: \"Mutation.widget_co_createWidget\"\n    apiResponseBody: |\n      {\"widget\": {\"id\": \"W9\", \"name\": \"a\"}}\n",
        "    expect:\n      connectorRequest:\n        method: POST\n        url: https://api.widgets.test/widgets\n        headers:\n          authorization: Bearer test-token\n        body: |\n          {\"name\": \"a\"}\n      connectorResponse: |\n        {\"id\": \"W9\", \"name\": \"a\"}\n",
    );
    let r = lint(vec![
        ("widget-co.graphql", s(&sdl)),
        (".factory/selection.yaml", s(&selection)),
        (".factory/inventory.json", s(&inventory)),
        ("tests/widget-co.connector.yaml", s(unit_ok)),
        ("tests/cases/list_widgets.graphql", s("query { widget_co_listWidgets(limit: 2, ids: [\"1\", \"2\"]) { id name metadata } }\n")),
        ("tests/fixtures/mappings/list_widgets.json", s(&listed)),
        ("tests/cases/create_widget.graphql", s("mutation { widget_co_createWidget(name: \"a\", color: \"red\", tags: [\"x\", \"y\"]) { id } }\n")),
        ("tests/fixtures/mappings/create_widget.json", s(&full)),
        ("tests/cases/create_widget_minimal.graphql", s("mutation { widget_co_createWidget(name: \"a\") { id } }\n")),
        ("tests/fixtures/mappings/create_widget_minimal.json", s(&minimal)),
    ]);
    let names = rules(&r);
    for rule in ["unit-no-credential", "unit-no-response"] {
        assert!(
            !names.contains(&rule.to_string()),
            "{} should be clean: {:?}",
            rule,
            r.findings
                .iter()
                .filter(|f| f.rule == rule)
                .map(|f| &f.message)
                .collect::<Vec<_>>()
        );
    }
}

// ── Casing and the wire vocabulary (ADR 0016) ─────────────────────────────

/// A schema with an enum argument, an enum response leaf under an envelope,
/// and a spec (inventory) that spells the wire values in lower case.
fn enum_sdl(arg_values: &str, leaf_values: &str, query_lines: &str) -> String {
    format!(
        r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {{
    baseURL: "{{{{BASE_URL}}}}"
    headers: [{{ name: "Authorization", value: "Bearer {{{{AUTH_EXPR}}}}" }}]
  }}
)

enum Widget_Co_Color {{
{arg}
}}

enum Widget_Co_Status {{
{leaf}
}}

type Widget_Co_Widget {{
  id: ID
  name: String
  state: Widget_Co_Status
  owner: Widget_Co_Owner
}}

type Widget_Co_Owner {{
  login: String
  role: Widget_Co_Status
}}

type Query {{
  widget_co_listWidgets(limit: Int, color: Widget_Co_Color): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: {{
        GET: "/widgets"
        queryParams: """
        limit: $args.limit
{query}
        """
      }}
      selection: """
      $.widgets {{
        id
        name
        state: status
        owner {{
          login
          role
        }}
      }}
      """
    )
}}
"#,
        arg = arg_values,
        leaf = leaf_values,
        query = query_lines
    )
}

fn enum_inventory() -> Value {
    json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [{"key": "get:/widgets", "operation_id": "listWidgets", "method": "GET", "path": "/widgets", "semantics": "read", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null,
            "parameters": [
                {"name": "limit", "in": "query", "required": false, "type": "integer"},
                {"name": "color", "in": "query", "required": false, "type": "string", "enum": ["red", "blue"]}
            ],
            "request_body": null,
            "response": {"status": "200", "content_type": "application/json", "envelope": "widgets", "shape_ref": "#/shapes/WidgetList", "list": true}}],
        "shapes": {
            "WidgetList": {"type": "object", "properties": {"widgets": {"type": ["array", "null"], "items": {"$ref": "#/shapes/Widget"}}}},
            "Widget": {"allOf": [{"$ref": "#/shapes/Base"}, {"type": "object", "properties": {"name": {"type": "string"}, "status": {"$ref": "#/shapes/Status"}, "owner": {"$ref": "#/shapes/Owner"}}}]},
            "Base": {"type": "object", "properties": {"id": {"type": "string"}}},
            "Owner": {"type": "object", "properties": {"login": {"type": "string"}, "role": {"type": "string", "enum": ["active", "retired", "pending"]}}},
            "Status": {"type": "string", "enum": ["active", "retired", "pending"]}
        },
        "unresolved": []
    })
}

fn drift_findings(sdl: String, inventory: Value) -> Vec<(String, Option<usize>)> {
    let result = lint(vec![
        ("widget-co.graphql", Some(sdl)),
        (".factory/inventory.json", Some(graphos_factory_core::json::pretty(&inventory))),
        ("tests/cases/list_widgets.graphql", s("query { widget_co_listWidgets(limit: 2, color: red) { id name state owner { login role } } }\n")),
    ]);
    result
        .findings
        .iter()
        .filter(|f| f.rule == "wire-enum-drift")
        .map(|f| (f.message.clone(), f.line))
        .collect()
}

#[test]
fn enums_spelled_as_the_wire_spells_them_are_clean() {
    let drift = drift_findings(
        enum_sdl(
            "  red\n  blue",
            "  active\n  retired\n  pending",
            "        color: $args.color",
        ),
        enum_inventory(),
    );
    assert!(drift.is_empty(), "{:?}", drift);
    // An enum value the API never returns is not drift on the response side:
    // a filter vocabulary reused as the field type is the common case.
    let drift = drift_findings(
        enum_sdl(
            "  red\n  blue",
            "  active\n  retired\n  pending\n  all",
            "        color: $args.color",
        ),
        enum_inventory(),
    );
    assert!(drift.is_empty(), "{:?}", drift);
}

#[test]
fn an_argument_enum_the_spec_does_not_spell_that_way_is_reported_at_the_enum_unless_its_slot_is_mapped(
) {
    let sdl = enum_sdl(
        "  RED\n  blue",
        "  active\n  retired\n  pending",
        "        color: $args.color",
    );
    let enum_line = sdl
        .lines()
        .position(|l| l.starts_with("enum Widget_Co_Color"))
        .unwrap()
        + 1;
    let drift = drift_findings(sdl, enum_inventory());
    assert_eq!(drift.len(), 1, "{:?}", drift);
    assert!(
        drift[0].0.starts_with("Widget_Co_Color declares `RED`")
            && drift[0].0.contains("query parameter `color`"),
        "{}",
        drift[0].0
    );
    assert_eq!(
        drift[0].1,
        Some(enum_line),
        "reported at the enum's declaration"
    );
    // The slot's own expression translates it: the mapping owns the spelling.
    let drift = drift_findings(
        enum_sdl(
            "  RED\n  blue",
            "  active\n  retired\n  pending",
            "        color: $args.color->match([\"RED\", \"red\"], [\"blue\", \"blue\"])",
        ),
        enum_inventory(),
    );
    assert!(drift.is_empty(), "{:?}", drift);
    // A second, mapped use of the argument does not excuse the plain one.
    let drift = drift_findings(
        enum_sdl(
            "  RED\n  blue",
            "  active\n  retired\n  pending",
            "        color: $args.color\n        alt: $args.color->first",
        ),
        enum_inventory(),
    );
    assert_eq!(drift.len(), 1, "{:?}", drift);
}

#[test]
fn a_response_enum_lacking_a_spec_value_is_reported_at_its_aliased_path() {
    // `ACTIVE` replaces `active`: the enum lacks `active` and `pending`.
    let drift = drift_findings(
        enum_sdl(
            "  red\n  blue",
            "  ACTIVE\n  retired",
            "        color: $args.color",
        ),
        enum_inventory(),
    );
    // One enum, one gap, two leaves (`widgets.status`, `widgets.owner.role`):
    // reported once, at the first leaf reached and at the enum's declaration.
    assert_eq!(drift.len(), 1, "{:?}", drift);
    let (message, line) = &drift[0];
    assert!(
        message.starts_with("Widget_Co_Status lacks `active`, `pending`")
            && message.contains("`widgets.status`"),
        "{}",
        message
    );
    assert!(line.is_some(), "reported at the enum's declaration");
}

/// The third exemption `walk_enums` carries: a leaf the connector maps is
/// skipped, because the mapping and not the source decides what reaches the
/// schema. The enum, the spec gap and both leaf types are exactly those of
/// `a_response_enum_lacking_a_spec_value_is_reported_at_its_aliased_path`
/// above — only the two leaf expressions change — so the skip is the only
/// thing that can keep this quiet. `opaque` is the whole test:
/// `reconcile::parse_item` sets it on every node carrying a method, which
/// `a_leaf_carrying_a_method_is_opaque` in `tests/reconcile.rs` pins. The
/// mapping is a real expression — `->match(["pending", "retired"], [@, @])`
/// composes on rover 0.41.0 / connect v0.3 and survives into the supergraph
/// (ADR 0030).
#[test]
fn wire_enum_drift_quiet_when_an_enum_leaf_carries_a_mapping() {
    let map = "->match([\"pending\", \"retired\"], [@, @])";
    let sdl = enum_sdl(
        "  red\n  blue",
        "  ACTIVE\n  retired",
        "        color: $args.color",
    )
    .replace("state: status\n", &format!("state: status{}\n", map))
    .replace(
        "          role\n",
        &format!("          role: role{}\n", map),
    );
    // A method on a leaf must be aliased (connectors-language.md), and both
    // `Widget_Co_Status` leaves must really carry the mapping: without this
    // the case could go quiet because a `replace` silently missed.
    assert_eq!(sdl.matches(map).count(), 2, "{}", sdl);
    let drift = drift_findings(sdl, enum_inventory());
    assert!(drift.is_empty(), "{:?}", drift);
}

#[test]
fn path_and_body_slots_are_compared_only_where_the_argument_is_used() {
    let sdl = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

enum Widget_Co_Color {
  RED
  blue
}

enum Widget_Co_Mode {
  FAST
  slow
}

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  widget_co_listWidgets(limit: Int): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id name")
}

type Mutation {
  widget_co_paint(id: ID!, mode: Widget_Co_Mode, color: Widget_Co_Color, style: Widget_Co_Mode): Widget_Co_Widget
    @connect(
      source: "widget_co"
      http: {
        POST: "/widgets/{$args.id}/paint/{$args.mode}"
        body: """
        color: $args.color
        """
      }
      selection: "id name"
    )
}
"#;
    let inventory = json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [
            {"key": "get:/widgets", "operation_id": "listWidgets", "method": "GET", "path": "/widgets", "semantics": "read", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null, "parameters": [], "request_body": null,
             "response": {"status": "200", "content_type": "application/json", "envelope": null, "shape_ref": "#/shapes/Widget", "list": true}},
            {"key": "post:/widgets/{id}/paint/{mode}", "operation_id": "paint", "method": "POST", "path": "/widgets/{id}/paint/{mode}", "semantics": "unknown", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null,
             "parameters": [
                {"name": "id", "in": "path", "required": true, "type": "string"},
                {"name": "mode", "in": "path", "required": true, "type": "string", "enum": ["fast", "slow"]},
                {"name": "style", "in": "path", "required": false, "type": "string", "enum": ["fast", "slow"]}
             ],
             "request_body": {"content_type": "application/json", "required": true, "shape_ref": "#/shapes/Paint"},
             "response": {"status": "200", "content_type": "application/json", "envelope": null, "shape_ref": "#/shapes/Widget", "list": false}}
        ],
        "shapes": {
            "Widget": {"type": "object", "properties": {"id": {"type": "string"}, "name": {"type": "string"}}},
            "Paint": {"type": "object", "properties": {"color": {"type": "string", "enum": ["red", "blue"]}}}
        },
        "unresolved": []
    });
    let selection = "contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\noperations:\n  \"get:/widgets\":\n    include: true\n    graphql:\n      root: query\n      name: listWidgets\n  \"post:/widgets/{id}/paint/{mode}\":\n    include: true\n    graphql:\n      root: mutation\n      name: paint\n";
    let result = lint(vec![
        ("widget-co.graphql", Some(sdl.to_string())),
        (".factory/inventory.json", Some(graphos_factory_core::json::pretty(&inventory))),
        (".factory/selection.yaml", Some(selection.to_string())),
        ("tests/cases/paint.graphql", s("mutation { widget_co_paint(id: \"1\", mode: FAST, color: RED, style: FAST) { id } }\n")),
        ("tests/fixtures/mappings/paint.json", Some(json!({"request": {"method": "POST", "urlPath": "/widgets/1/paint/FAST", "bodyPatterns": [{"equalToJson": {"color": "RED"}}]}, "response": {"status": 200, "jsonBody": {"id": "1"}}}).to_string())),
    ]);
    let drift: Vec<&str> = result
        .findings
        .iter()
        .filter(|f| f.rule == "wire-enum-drift")
        .map(|f| f.message.as_str())
        .collect();
    // `mode` feeds the path, `color` the body; `style` shares a type with
    // `mode` but feeds nothing the spec enumerates, so the path parameter of
    // the same name is not held against it.
    assert!(
        drift
            .iter()
            .any(|m| m.starts_with("Widget_Co_Mode declares `FAST`")
                && m.contains("path parameter `mode`")),
        "{:?}",
        drift
    );
    assert!(
        drift
            .iter()
            .any(|m| m.starts_with("Widget_Co_Color declares `RED`")
                && m.contains("body key `color`")),
        "{:?}",
        drift
    );
    assert!(!drift.iter().any(|m| m.contains("`style`")), "{:?}", drift);
    assert_eq!(drift.len(), 2, "one finding per enum and slot: {:?}", drift);
}

#[test]
fn a_cyclic_allof_and_a_numeric_enum_do_not_crash_or_report() {
    let mut inventory = enum_inventory();
    inventory["shapes"]["Widget"] = json!({"allOf": [{"$ref": "#/shapes/Loop"}]});
    inventory["shapes"]["Loop"] = json!({"allOf": [{"$ref": "#/shapes/Widget"}]});
    inventory["shapes"]["Owner"]["properties"]["role"] = json!({"type": "integer", "enum": [1, 2]});
    let drift = drift_findings(
        enum_sdl("  red\n  blue", "  ACTIVE", "        color: $args.color"),
        inventory,
    );
    assert!(drift.is_empty(), "{:?}", drift);
}

#[test]
fn snake_case_fields_are_reported_with_exact_lines_unless_a_doc_comment_keeps_the_wire_name() {
    let sdl = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

"A widget — the type's own description must not excuse its first field."
type Widget_Co_Widget @key(fields: "id") @connect(source: "widget_co", http: { GET: "/widgets/{$this.id}" }, selection: "id") {
  created_at: String
  "The API's own vocabulary; kept as the wire spells it."
  html_url: String
  """
  A block description works too.
  """
  avatar_url: String
  id: ID
  thing(
    max_size: Int
  ): String
  max_size: Int
}

extend type Widget_Co_Widget {
  updated_at: String
}

interface Widget_Co_Node {
  node_id: ID
}

input Widget_Co_Filter {
  min_size: Int
}

type Query {
  widget_co_list_widgets_raw(limit: Int): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id")
  widget_co_listWidgets(limit: Int): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id")
}
"#;
    let line_of = |needle: &str| {
        sdl.lines()
            .position(|l| l.trim_start().starts_with(needle))
            .map(|i| i + 1)
            .unwrap()
    };
    let result = lint(vec![("widget-co.graphql", Some(sdl.to_string()))]);
    let casing: Vec<(&str, Option<usize>)> = result
        .findings
        .iter()
        .filter(|f| f.rule == "field-casing")
        .map(|f| (f.message.as_str(), f.line))
        .collect();
    let names: Vec<&str> = casing
        .iter()
        .map(|(m, _)| m.split(' ').next().unwrap())
        .collect();
    assert_eq!(
        names,
        vec![
            "Widget_Co_Widget.created_at",
            "Widget_Co_Widget.max_size",
            "Widget_Co_Widget.updated_at",
            "Widget_Co_Node.node_id",
            "Widget_Co_Filter.min_size",
            "Query.widget_co_list_widgets_raw",
        ],
        "{:?}",
        casing
    );
    let at = |name: &str| casing.iter().find(|(m, _)| m.starts_with(name)).unwrap().1;
    assert_eq!(
        at("Widget_Co_Widget.created_at"),
        Some(line_of("created_at: String")),
        "behind a @connect on the type"
    );
    let field_line = sdl
        .lines()
        .collect::<Vec<_>>()
        .iter()
        .rposition(|l| l.trim_start().starts_with("max_size: Int"))
        .map(|i| i + 1)
        .unwrap();
    assert_eq!(
        at("Widget_Co_Widget.max_size"),
        Some(field_line),
        "the field, not the argument of the same name"
    );
    assert_eq!(
        at("Widget_Co_Widget.updated_at"),
        Some(line_of("updated_at: String")),
        "the extension's own body"
    );
    assert!(casing
        .iter()
        .find(|(m, _)| m.starts_with("Widget_Co_Widget.created_at"))
        .unwrap()
        .0
        .contains("`createdAt: created_at`"));
    let root = casing
        .iter()
        .find(|(m, _)| m.starts_with("Query."))
        .unwrap()
        .0;
    assert!(
        root.contains("selection.yaml's graphql.name (`listWidgetsRaw`)"),
        "{}",
        root
    );
    assert!(!root.contains("alias it"), "{}", root);
}

#[test]
fn a_doc_comment_lookup_survives_multibyte_text_and_column_zero_fields() {
    let sdl = SDL.replace(
        "type Widget_Co_Widget {\n  id: ID\n",
        "type Widget_Co_Widget {\n  id: ID\n  # note —\ncreated_at: String\n",
    );
    let result = lint(vec![("widget-co.graphql", Some(sdl))]);
    let casing: Vec<&str> = result
        .findings
        .iter()
        .filter(|f| f.rule == "field-casing")
        .map(|f| f.message.as_str())
        .collect();
    assert_eq!(casing.len(), 1, "{:?}", casing);
}

#[test]
fn a_drifted_enum_two_operations_share_is_reported_once() {
    let sdl = enum_sdl("  red\n  blue", "  ACTIVE\n  retired", "        color: $args.color").replace(
        "type Query {\n",
        "type Query {\n  widget_co_widget(id: ID!): Widget_Co_Widget\n    @connect(source: \"widget_co\", http: { GET: \"/widgets/{$args.id}\" }, selection: \"id name state: status owner { login role }\")\n",
    );
    let mut inventory = enum_inventory();
    inventory["operations"].as_array_mut().unwrap().push(json!({
        "key": "get:/widgets/{id}", "operation_id": "getWidget", "method": "GET", "path": "/widgets/{id}", "semantics": "read", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null,
        "parameters": [{"name": "id", "in": "path", "required": true, "type": "string"}], "request_body": null,
        "response": {"status": "200", "content_type": "application/json", "envelope": null, "shape_ref": "#/shapes/Widget", "list": false}}));
    let result = lint(vec![
        ("widget-co.graphql", Some(sdl)),
        (".factory/inventory.json", Some(graphos_factory_core::json::pretty(&inventory))),
        (".factory/selection.yaml", s("contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\noperations:\n  \"get:/widgets\":\n    include: true\n    graphql:\n      root: query\n      name: listWidgets\n  \"get:/widgets/{id}\":\n    include: true\n    graphql:\n      root: query\n      name: widget\n")),
        ("tests/cases/widget.graphql", s("query { widget_co_widget(id: \"1\") { id state } }\n")),
        ("tests/fixtures/mappings/widget.json", Some(json!({"request": {"method": "GET", "urlPath": "/widgets/1"}, "response": {"status": 200, "jsonBody": {"id": "1"}}}).to_string())),
    ]);
    let drift: Vec<&str> = result
        .findings
        .iter()
        .filter(|f| f.rule == "wire-enum-drift")
        .map(|f| f.message.as_str())
        .collect();
    // Two operations select `status` and `owner.role`, one enum: one finding per gap.
    assert_eq!(drift.len(), 1, "{:?}", drift);
    assert!(
        drift[0].starts_with("Widget_Co_Status lacks `active`, `pending`"),
        "{}",
        drift[0]
    );
}

#[test]
fn a_declaration_without_a_body_does_not_borrow_the_next_one() {
    let sdl = SDL.replace(
        "type Widget_Co_Widget {\n  id: ID\n",
        "type Widget_Co_Empty\n\ntype Widget_Co_Widget {\n  id: ID\n  created_at: String\n",
    );
    let result = lint(vec![("widget-co.graphql", Some(sdl))]);
    let casing: Vec<&str> = result
        .findings
        .iter()
        .filter(|f| f.rule == "field-casing")
        .map(|f| f.message.as_str())
        .collect();
    assert_eq!(casing.len(), 1, "{:?}", casing);
    assert!(
        casing[0].starts_with("Widget_Co_Widget.created_at"),
        "{}",
        casing[0]
    );
}

#[test]
fn customer_context_blocks_build_even_when_evidence_is_skipped() {
    let context = json!({
        "contract_version": 1, "intent": "Expose customer-defined metrics", "inputs": [],
        "requirements": [{"id":"model-fields", "phase":"build", "affects":["typed metrics"], "reason":"Fields are supplied by the tenant", "resolve_with":"Read the authorized model export", "status":"missing"}]
    });
    let ws = make_workspace(HashMap::from([
        (
            ".factory/workspace.yaml",
            Some(WORKSPACE.replace("context_mode: generic", "context_mode: specialized")),
        ),
        (".factory/context.yaml", Some(context.to_string())),
    ]));
    for skip_evidence in [false, true] {
        let result = lint_workspace(
            ws.path(),
            &LintOptions {
                schemas_dir: None,
                skip_evidence,
                target: &graphos_factory_core::target::BARE,
            },
        );
        assert!(result
            .findings
            .iter()
            .any(|f| f.rule == "context-unresolved"
                && f.severity == "error"
                && f.message.contains("typed metrics")));
    }
}

#[test]
fn customer_context_keeps_live_gaps_visible_without_blocking_offline_builds() {
    let context = json!({
        "contract_version": 1, "intent": "Wrap documented operations", "inputs": [],
        "requirements": [{"id":"live-identity", "phase":"live", "affects":["live queries"], "reason":"No authorized test credential", "resolve_with":"Use the approved credential environment variable", "status":"missing"}]
    });
    let result = lint(vec![(".factory/context.yaml", Some(context.to_string()))]);
    assert_eq!(result.errors, 0);
    assert!(result
        .findings
        .iter()
        .any(|f| f.rule == "context-unresolved" && f.severity == "warn"));
    let invalid = lint(vec![(
        ".factory/context.yaml",
        Some("mode: generic".into()),
    )]);
    assert!(rules(&invalid).contains(&"context-invalid".into()));
}

#[test]
fn an_unrecorded_mode_lints_clean_but_its_context_file_is_enforced() {
    // ADR 0081: no marker reads as generic — no finding on its own ...
    let bare = WORKSPACE.replace("context_mode: generic\n", "");
    assert_ne!(bare, WORKSPACE);
    let clean = lint(vec![(".factory/workspace.yaml", Some(bare.clone()))]);
    assert!(clean.findings.is_empty(), "{:?}", clean.findings);
    // ... but a companion file's build gap is still an error.
    let context = json!({
        "contract_version": 1, "intent": "Expose customer-defined metrics", "inputs": [],
        "requirements": [{"id":"model-fields", "phase":"build", "affects":["typed metrics"], "reason":"Fields are supplied by the tenant", "resolve_with":"Read the authorized model export", "status":"missing"}]
    });
    let blocked = lint(vec![
        (".factory/workspace.yaml", Some(bare)),
        (".factory/context.yaml", Some(context.to_string())),
    ]);
    assert!(blocked
        .findings
        .iter()
        .any(|f| f.rule == "context-unresolved" && f.severity == "error"));
}

// ─── Pagination and copy-state lint rules ────────────────────────────────────

/// Helper to create an inventory with pagination info.
fn paginated_inventory(default: Option<i64>, maximum: Option<i64>, param_type: &str) -> Value {
    let mut param = json!({
        "name": "limit",
        "in": "query",
        "required": false,
        "type": param_type
    });
    if let Some(d) = default {
        param["default"] = json!(d);
    }
    if let Some(m) = maximum {
        param["maximum"] = json!(m);
    }
    json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [{
            "key": "get:/widgets",
            "operation_id": "listWidgets",
            "method": "GET",
            "path": "/widgets",
            "semantics": "read",
            "provenance": "spec",
            "confidence": 1,
            "support": "supported",
            "support_reason": null,
            "parameters": [param],
            "pagination": {
                "style": "offset",
                "request": "offset",
                "size_param": "limit",
                "response": null
            }
        }],
        "shapes": {},
        "unresolved": []
    })
}

/// Helper for paginated selection.
fn paginated_selection() -> String {
    "contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\noperations:\n  \"get:/widgets\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: query\n      name: listWidgets\n    pagination: { expose: [limit], next_cursor: null }\n".to_string()
}

/// Schema with a paginated operation (limit arg, no doc comment on arg).
const PAGINATED_SDL_NO_DOC: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  "List widgets."
  widget_co_listWidgets(limit: Int): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: { GET: "/widgets", queryParams: "limit: $args.limit" }
      selection: "id name"
    )
}
"#;

/// Schema with documented pagination bounds.
const PAGINATED_SDL_WITH_DOC: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  "Returns one page of widgets; iterate with offset to complete."
  widget_co_listWidgets(
    "Page size. Default 25, maximum 100."
    limit: Int
  ): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: { GET: "/widgets", queryParams: "limit: $args.limit" }
      selection: "id name"
    )
}
"#;

/// Schema whose doc comment contains the maximum (100) only as a substring of
/// an unrelated number (1000).
const PAGINATED_SDL_SUBSTRING_BOUND: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  "Returns one page of widgets; iterate with offset to complete."
  widget_co_listWidgets(
    "Page size. Default 25, no more than 1000 items."
    limit: Int
  ): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: { GET: "/widgets", queryParams: "limit: $args.limit" }
      selection: "id name"
    )
}
"#;

/// Schema with "no documented maximum" stated.
const PAGINATED_SDL_UNKNOWN_BOUND: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  "Returns one page of widgets; iterate with offset to complete."
  widget_co_listWidgets(
    "Page size. No documented maximum in the source."
    limit: Int
  ): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: { GET: "/widgets", queryParams: "limit: $args.limit" }
      selection: "id name"
    )
}
"#;

/// Schema with String type for limit (wrong).
const PAGINATED_SDL_WRONG_TYPE: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  "Returns one page of widgets; iterate with offset to complete."
  widget_co_listWidgets(
    "Page size. Default 25, maximum 100."
    limit: String
  ): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: { GET: "/widgets", queryParams: "limit: $args.limit" }
      selection: "id name"
    )
}
"#;

// ─── pagination-bounds-undocumented ─────────────────────────────────────────

#[test]
fn pagination_bounds_undocumented_fires_when_maximum_declared_but_not_documented() {
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_NO_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        rules(&result).contains(&"pagination-bounds-undocumented".to_string()),
        "expected pagination-bounds-undocumented but got {:?}",
        rules(&result)
    );
}

#[test]
fn pagination_bounds_undocumented_passes_when_bounds_documented() {
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_WITH_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        !rules(&result).contains(&"pagination-bounds-undocumented".to_string()),
        "expected no pagination-bounds-undocumented but got {:?}",
        rules(&result)
    );
}

/// The schema renames the wire's `limit` to `pageSize`
/// (`queryParams: "limit: $args.pageSize"`): the doc comment is read on the
/// argument the connector maps, not looked up by the wire name (Granola's
/// `page_size` / `pageSize`, 6 false warnings).
fn renamed_size_arg(sdl: &str) -> String {
    let renamed = sdl
        .replace("    limit: Int\n", "    pageSize: Int\n")
        .replace("(limit: Int)", "(pageSize: Int)")
        .replace("\"limit: $args.limit\"", "\"limit: $args.pageSize\"");
    assert_ne!(renamed, sdl);
    assert!(!renamed.contains("$args.limit"), "{}", renamed);
    assert!(!renamed.contains("limit: Int"), "{}", renamed);
    renamed
}

#[test]
fn pagination_bounds_undocumented_reads_the_renamed_size_argument() {
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let lint_with = |sdl: String| {
        rules(&lint(vec![
            ("widget-co.graphql", Some(sdl)),
            (
                ".factory/inventory.json",
                s(&graphos_factory_core::json::pretty(&inv)),
            ),
            (".factory/selection.yaml", s(&paginated_selection())),
        ]))
    };
    let documented = lint_with(renamed_size_arg(PAGINATED_SDL_WITH_DOC));
    assert!(
        !documented.contains(&"pagination-bounds-undocumented".to_string()),
        "{:?}",
        documented
    );
    let undocumented = lint_with(renamed_size_arg(PAGINATED_SDL_NO_DOC));
    assert!(
        undocumented.contains(&"pagination-bounds-undocumented".to_string()),
        "{:?}",
        undocumented
    );
}

#[test]
fn pagination_bounds_undocumented_fires_when_bound_appears_only_inside_larger_number() {
    // Maximum is 100; the doc comment says "no more than 1000 items" — "100"
    // appears only as a substring of "1000", which must not count as stating
    // the bound.
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_SUBSTRING_BOUND)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        rules(&result).contains(&"pagination-bounds-undocumented".to_string()),
        "expected pagination-bounds-undocumented but got {:?}",
        rules(&result)
    );
}

#[test]
fn pagination_bounds_undocumented_passes_when_float_bound_documented_as_integer() {
    // Inventory declares default 25.0 (a JSON float); the doc comment states
    // "Default 25". No warning: normalize_numbers folds integral floats to
    // integers at parse, and the numeric comparison treats 25.0 == 25 either
    // way. Pins the guarantee that a float-typed bound never demands the
    // literal string "25.0" in the doc comment.
    let mut inv = paginated_inventory(None, Some(100), "number");
    inv["operations"][0]["parameters"][0]["default"] = json!(25.0);
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_WITH_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        !rules(&result).contains(&"pagination-bounds-undocumented".to_string()),
        "expected no pagination-bounds-undocumented but got {:?}",
        rules(&result)
    );
}

// ─── pagination bounds outside Int (ADR 0065) ──────────────────────────────

fn paginated_sdl_with_doc(doc: &str) -> String {
    PAGINATED_SDL_WITH_DOC.replace("Page size. Default 25, maximum 100.", doc)
}

fn pagination_undocumented(inv: &Value, sdl: &str) -> Vec<String> {
    let result = lint(vec![
        ("widget-co.graphql", s(sdl)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    result
        .findings
        .iter()
        .filter(|f| f.rule == "pagination-bounds-undocumented")
        .map(|f| f.message.clone())
        .collect()
}

/// AppWorld's page limits declare `maximum: 9223372036854775807`. No `Int`
/// argument reaches it, so "no practical maximum" states the bound honestly.
#[test]
fn pagination_bounds_undocumented_accepts_no_practical_maximum_for_an_int64_maximum() {
    let inv = paginated_inventory(Some(5), Some(9_223_372_036_854_775_807), "integer");
    let sdl = paginated_sdl_with_doc("Page size (provider default: 5; no practical maximum).");
    let found = pagination_undocumented(&inv, &sdl);
    assert!(found.is_empty(), "expected no finding, got {:?}", found);
}

#[test]
fn pagination_bounds_undocumented_names_the_gap_phrase_for_an_int64_maximum() {
    let inv = paginated_inventory(Some(5), Some(9_223_372_036_854_775_807), "integer");
    let sdl = paginated_sdl_with_doc("Page size (provider default: 5).");
    let found = pagination_undocumented(&inv, &sdl);
    assert!(
        found.iter().any(|m| m.contains("no practical maximum")),
        "expected the finding to offer the gap phrase, got {:?}",
        found
    );
}

/// Stating the number still satisfies the rule; the phrase is an alternative.
#[test]
fn pagination_bounds_undocumented_still_accepts_the_int64_number_itself() {
    let inv = paginated_inventory(Some(5), Some(9_223_372_036_854_775_807), "integer");
    let sdl =
        paginated_sdl_with_doc("Page size (provider default: 5, maximum: 9223372036854775807).");
    let found = pagination_undocumented(&inv, &sdl);
    assert!(found.is_empty(), "expected no finding, got {:?}", found);
}

/// The phrase is accepted only for a bound outside `Int`: a maximum of 100 is
/// a real limit the caller must be told.
#[test]
fn pagination_bounds_undocumented_rejects_no_practical_maximum_for_a_bound_inside_int() {
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let sdl = paginated_sdl_with_doc("Page size (provider default: 25; no practical maximum).");
    let found = pagination_undocumented(&inv, &sdl);
    assert!(
        found.iter().any(|m| m.contains("maximum: 100")),
        "expected the maximum to be demanded, got {:?}",
        found
    );
}

#[test]
fn pagination_bounds_undocumented_accepts_no_practical_default_for_an_int64_default() {
    let inv = paginated_inventory(Some(9_223_372_036_854_775_807), Some(100), "integer");
    let sdl = paginated_sdl_with_doc("Page size (no practical default, maximum: 100).");
    let found = pagination_undocumented(&inv, &sdl);
    assert!(found.is_empty(), "expected no finding, got {:?}", found);
}

// ─── pagination-bounds-unknown ──────────────────────────────────────────────

#[test]
fn pagination_bounds_unknown_fires_when_no_bounds_and_no_gap_stated() {
    // Inventory has neither default nor maximum
    let inv = paginated_inventory(None, None, "integer");
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_NO_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        rules(&result).contains(&"pagination-bounds-unknown".to_string()),
        "expected pagination-bounds-unknown but got {:?}",
        rules(&result)
    );
}

#[test]
fn pagination_bounds_unknown_passes_when_gap_stated() {
    let inv = paginated_inventory(None, None, "integer");
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_UNKNOWN_BOUND)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        !rules(&result).contains(&"pagination-bounds-unknown".to_string()),
        "expected no pagination-bounds-unknown but got {:?}",
        rules(&result)
    );
}

// ─── page-limit-not-int ─────────────────────────────────────────────────────

#[test]
fn page_limit_not_int_fires_when_schema_uses_string() {
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_WRONG_TYPE)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        rules(&result).contains(&"page-limit-not-int".to_string()),
        "expected page-limit-not-int but got {:?}",
        rules(&result)
    );
}

#[test]
fn page_limit_not_int_passes_when_schema_uses_int() {
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_WITH_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        !rules(&result).contains(&"page-limit-not-int".to_string()),
        "expected no page-limit-not-int but got {:?}",
        rules(&result)
    );
}

// ─── list-completion-missing ────────────────────────────────────────────────

#[test]
fn list_completion_missing_fires_when_no_pagination_keywords() {
    // "List widgets." has no page/iterate/collection/total/cursor/complete
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_NO_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        rules(&result).contains(&"list-completion-missing".to_string()),
        "expected list-completion-missing but got {:?}",
        rules(&result)
    );
}

#[test]
fn list_completion_missing_passes_when_pagination_keywords_present() {
    // "Returns one page of widgets; iterate with offset to complete."
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let result = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_WITH_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        !rules(&result).contains(&"list-completion-missing".to_string()),
        "expected no list-completion-missing but got {:?}",
        rules(&result)
    );
}

/// Paginated schema whose root field carries a `"""` BLOCK doc comment with
/// none of the completion keywords, preceded earlier in the file by another
/// block doc that does contain one ("page"). A multi-line operation
/// description is what the Returns-line rule produces
/// (schema-authoring.md § Descriptions), so the rule has to read the field's
/// own block and nothing before it.
const PAGINATED_SDL_BLOCK_DOC_NO_COMPLETION: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

"""
A widget. The vendor's own guide has a page about widget lifecycles.
"""
type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  """
  List widgets. Filters combine with AND.

  Returns a list of items with: id, name.
  """
  widget_co_listWidgets(
    "Page size. Default 25, maximum 100."
    limit: Int
  ): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id name")
}
"#;

/// The same shape, with the one-page/stop-condition sentence restored ahead of
/// the Returns line — what a compliant paginated operation looks like after
/// ADR 0032.
const PAGINATED_SDL_BLOCK_DOC_WITH_COMPLETION: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

"""
A widget. The vendor's own guide has a page about widget lifecycles.
"""
type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  """
  List widgets. One page, not the whole collection; iterate until a short or
  empty page.

  Returns a list of items with: id, name.
  """
  widget_co_listWidgets(
    "Page size. Default 25, maximum 100."
    limit: Int
  ): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id name")
}
"#;

#[test]
fn list_completion_missing_fires_when_block_doc_lacks_keywords() {
    // Regression (ADR 0032): `doc_comment_before` used to capture from the
    // FIRST `"""` in the file to the last one before the field, so any block
    // doc anywhere above — here the Widget type's, which says "page" — kept
    // the rule quiet for every block-doc root field.
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let result = lint(vec![
        (
            "widget-co.graphql",
            s(PAGINATED_SDL_BLOCK_DOC_NO_COMPLETION),
        ),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        rules(&result).contains(&"list-completion-missing".to_string()),
        "expected list-completion-missing but got {:?}",
        rules(&result)
    );
}

#[test]
fn list_completion_missing_passes_when_block_doc_has_keywords() {
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let result = lint(vec![
        (
            "widget-co.graphql",
            s(PAGINATED_SDL_BLOCK_DOC_WITH_COMPLETION),
        ),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        !rules(&result).contains(&"list-completion-missing".to_string()),
        "expected no list-completion-missing but got {:?}",
        rules(&result)
    );
}

// ─── copy-state-undocumented ────────────────────────────────────────────────

/// Mutation schema with copy operation (no preservation keywords).
const COPY_SDL_NO_DOC: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Note {
  id: ID
  content: String
}

type Mutation {
  "Copy a note to a new location."
  widget_co_copyNote(id: ID!, targetFolder: ID!): Widget_Co_Note
    @connect(source: "widget_co", http: { POST: "/notes/{$args.id}/copy" }, selection: "id content")
}

type Query {
  _empty: String
}
"#;

/// Mutation schema with copy operation and preservation keywords.
const COPY_SDL_WITH_DOC: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Note {
  id: ID
  content: String
}

type Mutation {
  "Copy a note to a new location. Tags and metadata carry over from the source."
  widget_co_copyNote(id: ID!, targetFolder: ID!): Widget_Co_Note
    @connect(source: "widget_co", http: { POST: "/notes/{$args.id}/copy" }, selection: "id content")
}

type Query {
  _empty: String
}
"#;

fn copy_inventory() -> Value {
    json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [{
            "key": "post:/notes/{id}/copy",
            "operation_id": "copyNote",
            "method": "POST",
            "path": "/notes/{id}/copy",
            "semantics": "write",
            "provenance": "spec",
            "confidence": 1,
            "support": "supported",
            "support_reason": null
        }],
        "shapes": {},
        "unresolved": []
    })
}

fn copy_selection() -> String {
    "contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\noperations:\n  \"post:/notes/{id}/copy\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: mutation\n      name: copyNote\n".to_string()
}

#[test]
fn copy_state_undocumented_fires_when_no_preservation_keywords() {
    let result = lint(vec![
        ("widget-co.graphql", s(COPY_SDL_NO_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&copy_inventory())),
        ),
        (".factory/selection.yaml", s(&copy_selection())),
    ]);
    assert!(
        rules(&result).contains(&"copy-state-undocumented".to_string()),
        "expected copy-state-undocumented but got {:?}",
        rules(&result)
    );
}

#[test]
fn copy_state_undocumented_passes_when_preservation_keywords_present() {
    let result = lint(vec![
        ("widget-co.graphql", s(COPY_SDL_WITH_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&copy_inventory())),
        ),
        (".factory/selection.yaml", s(&copy_selection())),
    ]);
    assert!(
        !rules(&result).contains(&"copy-state-undocumented".to_string()),
        "expected no copy-state-undocumented but got {:?}",
        rules(&result)
    );
}

// ─── int-overflow (ADR 0030) ────────────────────────────────────────────────

/// A one-operation schema. `@DOC@` is a whole line (empty, or a doc comment
/// on the `views` leaf); `@LEAF@` and `@ARG@` are GraphQL types; `@SEL@` is
/// what the connector selects for the leaf.
const INT64_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Widget {
  id: ID
@DOC@  views: @LEAF@
}

type Query {
  "List widgets."
  widget_co_listWidgets(@ARGDOC@index: @ARG@): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: { GET: "/widgets", queryParams: "index: $args.index" }
      selection: "id @SEL@"
    )
}
"#;

/// `leaf_doc` and `arg_doc` are inserted verbatim; pass `""` for none.
fn int64_sdl(leaf_doc: &str, leaf: &str, arg: &str, sel: &str, arg_doc: &str) -> String {
    INT64_SDL
        .replace("@DOC@", leaf_doc)
        .replace("@LEAF@", leaf)
        .replace("@ARGDOC@", arg_doc)
        .replace("@ARG@", arg)
        .replace("@SEL@", sel)
}

/// An inventory whose `Widget.views` property and whose `index` query
/// parameter both carry `format` and, when given, a `minimum`/`maximum` pair.
fn int64_inventory(format: Option<&str>, bounds: Option<(i64, i64)>) -> Value {
    let mut prop = json!({"type": "integer"});
    let mut param = json!({"name": "index", "in": "query", "required": false, "type": "integer"});
    if let Some(f) = format {
        prop["format"] = json!(f);
        param["format"] = json!(f);
    }
    if let Some((lo, hi)) = bounds {
        prop["minimum"] = json!(lo);
        prop["maximum"] = json!(hi);
        param["minimum"] = json!(lo);
        param["maximum"] = json!(hi);
    }
    json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [{
            "key": "get:/widgets", "operation_id": "listWidgets", "method": "GET", "path": "/widgets",
            "semantics": "read", "provenance": "spec", "confidence": 1,
            "support": "supported", "support_reason": null,
            "parameters": [param],
            "response": {"status": "200", "content_type": "application/json", "shape_ref": "#/shapes/Widget"}
        }],
        "shapes": {"Widget": {"type": "object", "properties": {
            "id": {"type": "integer", "format": "int64"},
            "views": prop
        }}},
        "unresolved": []
    })
}

fn int64_lint(inv: &Value, sdl: &str) -> LintResult {
    lint(vec![
        ("widget-co.graphql", s(sdl)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(inv)),
        ),
    ])
}

fn fired(result: &LintResult) -> bool {
    rules(result).contains(&"int-overflow".to_string())
}

/// A leaf reached through an expansion boundary (ADR 0047) is still a
/// response leaf the rule checks: the boundary keeps its `$ref`, so the
/// target's `format: int64` is where it always was.
#[test]
fn int_overflow_fires_beneath_an_expansion_boundary() {
    let mut inv = int64_inventory(Some("int64"), None);
    inv["shapes"]["Owner"] = json!({"type": "object", "properties": {
        "id": {"type": "string"},
        "views": {"type": "integer", "format": "int64"}
    }});
    inv["shapes"]["Widget"]["properties"]["owner"] = json!({
        "$ref": "#/shapes/Owner",
        "x-expansion": {"target": "Owner", "mechanism": "fields", "default": ["id"],
            "evidence": {"kind": "doc", "ref": "https://widgets.test/docs"}}
    });
    let sdl = int64_sdl("", "Int", "String", "owner { views }", "").replace(
        "  views: Int\n}",
        "  views: Int\n  owner: Widget_Co_Owner\n}\n\ntype Widget_Co_Owner {\n  views: Int\n}",
    );
    let r = int64_lint(&inv, &sdl);
    let hits: Vec<&String> = r
        .findings
        .iter()
        .filter(|f| f.rule == "int-overflow")
        .map(|f| &f.message)
        .collect();
    assert!(
        hits.iter().any(|m| m.contains("owner")),
        "expected int-overflow on owner.views, got {:?}",
        hits
    );
}

#[test]
fn int_overflow_fires_when_int64_response_field_is_int() {
    let inv = int64_inventory(Some("int64"), None);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "String", "views", ""));
    assert!(fired(&r), "expected int-overflow but got {:?}", rules(&r));
}

#[test]
fn int_overflow_fires_on_a_uint64_response_field() {
    let inv = int64_inventory(Some("uint64"), None);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "String", "views", ""));
    assert!(fired(&r), "expected int-overflow but got {:?}", rules(&r));
}

/// `uint32` is unsigned: 2147483648 .. 4294967295 is a valid `uint32` and
/// none of it fits `Int`, so the rule treats it like the 64-bit formats.
#[test]
fn int_overflow_fires_on_a_uint32_response_field() {
    let inv = int64_inventory(Some("uint32"), None);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "String", "views", ""));
    assert!(fired(&r), "expected int-overflow but got {:?}", rules(&r));
}

/// The other half of the same range claim: `int32` is exactly `Int`.
#[test]
fn int_overflow_quiet_on_an_int32_response_field() {
    let inv = int64_inventory(Some("int32"), None);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
}

#[test]
fn int_overflow_is_an_error_not_a_warning() {
    let inv = int64_inventory(Some("int64"), None);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "String", "views", ""));
    let severities: Vec<&str> = r
        .findings
        .iter()
        .filter(|f| f.rule == "int-overflow")
        .map(|f| f.severity.as_str())
        .collect();
    assert_eq!(severities, vec!["error"], "{:?}", r.findings);
}

#[test]
fn int_overflow_fires_on_int64_argument() {
    let inv = int64_inventory(Some("int64"), None);
    let r = int64_lint(&inv, &int64_sdl("", "ID", "Int", "views", ""));
    assert!(fired(&r), "expected int-overflow but got {:?}", rules(&r));
    let messages: Vec<&str> = r
        .findings
        .iter()
        .filter(|f| f.rule == "int-overflow")
        .map(|f| f.message.as_str())
        .collect();
    assert!(
        messages.iter().any(|m| m.contains("index")),
        "expected the finding to name the argument: {:?}",
        messages
    );
}

#[test]
fn int_overflow_exempts_id_typed_fields() {
    let inv = int64_inventory(Some("int64"), None);
    let r = int64_lint(&inv, &int64_sdl("", "ID", "String", "views", ""));
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
}

#[test]
fn int_overflow_quiet_when_declared_bounds_are_inside_i32() {
    let inv = int64_inventory(Some("int64"), Some((0, 1000)));
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
}

#[test]
fn int_overflow_fires_on_out_of_range_declared_maximum() {
    let inv = int64_inventory(None, Some((0, 9007199254740991)));
    let r = int64_lint(&inv, &int64_sdl("", "Int", "String", "views", ""));
    assert!(fired(&r), "expected int-overflow but got {:?}", rules(&r));
}

#[test]
fn int_overflow_quiet_on_a_plain_integer_with_no_format_and_no_bounds() {
    let inv = int64_inventory(None, None);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
}

/// The leaf stays `Int`, so only the mapped-leaf skip can keep this quiet:
/// replace the `if n.opaque` skip in `walk_int_overflow` with `if false` and
/// it goes red. `opaque` is the whole test — `reconcile::parse_selection`
/// sets it on every node carrying a method — so there is no separate
/// `methods` clause to remove. `views: views->match([0, 0], [@, 1])` is a
/// real expression: it composes on rover 0.41.0 / connect v0.3 and yields an
/// `Int`, so the mapping, not the int64 source property, decides what
/// reaches the schema.
#[test]
fn int_overflow_quiet_when_an_int_leaf_carries_a_mapping() {
    let inv = int64_inventory(Some("int64"), None);
    let r = int64_lint(
        &inv,
        &int64_sdl(
            "",
            "Int",
            "String",
            "views: views->match([0, 0], [@, 1])",
            "",
        ),
    );
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
}

/// The string-typed conversion the rule's own advice recommends is quiet
/// because `String` is not `Int` — asserted separately from the mapped-leaf
/// skip above, which this case does *not* exercise.
#[test]
fn int_overflow_quiet_when_the_leaf_is_converted_to_string() {
    let inv = int64_inventory(Some("int64"), None);
    let r = int64_lint(
        &inv,
        &int64_sdl(
            "",
            "String",
            "String",
            "views: views->match([null, null], [@, @->jsonStringify])",
            "",
        ),
    );
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
}

/// There is no doc-comment escape hatch (ADR 0030): a sentence beside the
/// field does not change what the router coerces.
#[test]
fn int_overflow_fires_even_when_the_doc_comment_claims_the_value_fits() {
    let inv = int64_inventory(Some("int64"), None);
    let doc = "  \"View count. int64 in the API; the count fits in Int.\"\n";
    let r = int64_lint(&inv, &int64_sdl(doc, "Int", "String", "views", ""));
    assert!(fired(&r), "expected int-overflow but got {:?}", rules(&r));
}

#[test]
fn int_overflow_fires_on_an_argument_whose_doc_claims_the_value_fits() {
    let inv = int64_inventory(Some("int64"), None);
    let arg_doc = "\"Row index. int64 in the API; the index fits in Int.\" ";
    let r = int64_lint(&inv, &int64_sdl("", "ID", "Int", "views", arg_doc));
    assert!(fired(&r), "expected int-overflow but got {:?}", rules(&r));
}

/// A `[Int]` leaf over an array of int64 items has the defect once per
/// element; the rule reads `items`, not the array shape's own `type`.
#[test]
fn int_overflow_fires_on_an_int_list_over_int64_items() {
    let mut inv = int64_inventory(None, None);
    inv["shapes"]["Widget"]["properties"]["views"] =
        json!({"type": "array", "items": {"type": "integer", "format": "int64"}});
    let r = int64_lint(&inv, &int64_sdl("", "[Int]", "String", "views", ""));
    assert!(fired(&r), "expected int-overflow but got {:?}", rules(&r));
}

/// The same list whose items carry no 64-bit format stays quiet.
#[test]
fn int_overflow_quiet_on_an_int_list_over_plain_integer_items() {
    let mut inv = int64_inventory(None, None);
    inv["shapes"]["Widget"]["properties"]["views"] =
        json!({"type": "array", "items": {"type": "integer"}});
    let r = int64_lint(&inv, &int64_sdl("", "[Int]", "String", "views", ""));
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
}

/// A type reached from two operations is one defect and one finding.
#[test]
fn int_overflow_reports_a_shared_leaf_once() {
    let mut inv = int64_inventory(Some("int64"), None);
    let response = inv["operations"][0]["response"].clone();
    inv["operations"].as_array_mut().unwrap().push(json!({
        "key": "get:/widgets/featured", "operation_id": "featuredWidgets",
        "method": "GET", "path": "/widgets/featured",
        "semantics": "read", "provenance": "spec", "confidence": 1,
        "support": "supported", "support_reason": null,
        "parameters": [],
        "response": response
    }));
    // A second root field over the same type, selecting the same leaf.
    let sdl = int64_sdl("", "Int", "String", "views", "").replace(
        "      selection: \"id views\"\n    )\n}\n",
        concat!(
            "      selection: \"id views\"\n    )\n\n",
            "  \"Featured widgets.\"\n",
            "  widget_co_featuredWidgets: [Widget_Co_Widget]\n",
            "    @connect(\n",
            "      source: \"widget_co\"\n",
            "      http: { GET: \"/widgets/featured\" }\n",
            "      selection: \"id views\"\n",
            "    )\n}\n"
        ),
    );
    let selection = concat!(
        "contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\n",
        "operations:\n  \"get:/widgets\":\n    include: true\n    response:\n      envelope: null\n",
        "    graphql:\n      root: query\n      name: listWidgets\n",
        "  \"get:/widgets/featured\":\n    include: true\n    response:\n      envelope: null\n",
        "    graphql:\n      root: query\n      name: featuredWidgets\n"
    );
    let r = lint(vec![
        ("widget-co.graphql", s(&sdl)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(selection)),
    ]);
    let n = rules(&r).iter().filter(|x| *x == "int-overflow").count();
    assert_eq!(n, 1, "expected one int-overflow, got {:?}", r.findings);
}

/// An inventory with a paginated `limit` the source declares as int64.
fn paginated_int64_inventory(bounds: Option<(i64, i64)>) -> Value {
    let mut inv = paginated_inventory(None, Some(100), "integer");
    let param = &mut inv["operations"][0]["parameters"][0];
    param["format"] = json!("int64");
    if let Some((lo, hi)) = bounds {
        param["minimum"] = json!(lo);
        param["maximum"] = json!(hi);
    }
    inv
}

#[test]
fn int_overflow_and_page_limit_not_int_never_fire_on_the_same_field() {
    // The size param is Int, as page-limit-not-int wants: neither rule fires.
    let inv = paginated_int64_inventory(Some((1, 100)));
    let r = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_WITH_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        !rules(&r).contains(&"int-overflow".to_string())
            && !rules(&r).contains(&"page-limit-not-int".to_string()),
        "expected neither rule but got {:?}",
        rules(&r)
    );

    // The size param is String: page-limit-not-int fires, int-overflow does not.
    let r = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_WRONG_TYPE)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        rules(&r).contains(&"page-limit-not-int".to_string())
            && !rules(&r).contains(&"int-overflow".to_string()),
        "expected page-limit-not-int alone but got {:?}",
        rules(&r)
    );
}

/// An unbounded int64 size param stays Int: page-limit-not-int owns it.
#[test]
fn int_overflow_skips_a_pagination_size_param() {
    let inv = paginated_int64_inventory(None);
    let r = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_WITH_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        !rules(&r).contains(&"int-overflow".to_string()),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
}

#[test]
fn int_overflow_degrades_gracefully() {
    // The default fixture has `shapes: {}` and no format anywhere.
    let r = lint(vec![]);
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
    // No selection file at all: the rule returns without reading anything.
    let r = lint(vec![(".factory/selection.yaml", None)]);
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
    // No inventory either.
    let r = lint(vec![
        (".factory/selection.yaml", None),
        (".factory/inventory.json", None),
    ]);
    assert!(
        !fired(&r),
        "expected no int-overflow but got {:?}",
        rules(&r)
    );
}

/// Copy mutation whose description is a `"""` BLOCK doc with none of the
/// preservation keywords, preceded earlier in the file by another block doc
/// that does contain one ("default"). This is the shape ADR 0032 makes
/// ordinary: a Returns line turns every one-line copy-mutation description
/// into a block, so `copy-state-undocumented` — which reads the text, not
/// just its presence — has to read the field's own block and nothing before
/// it.
const COPY_SDL_BLOCK_DOC_NO_PRESERVATION: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

"""
A note. The folder a note lives in is set by default from its source.
"""
type Widget_Co_Note {
  id: ID
  content: String
}

type Mutation {
  """
  Copy a note to a new location.

  Returns: id, content.
  """
  widget_co_copyNote(id: ID!, targetFolder: ID!): Widget_Co_Note
    @connect(source: "widget_co", http: { POST: "/notes/{$args.id}/copy" }, selection: "id content")
}

type Query {
  _empty: String
}
"#;

/// The same shape, with the preservation sentence ahead of the Returns line —
/// what a compliant copy mutation looks like after ADR 0032.
const COPY_SDL_BLOCK_DOC_WITH_PRESERVATION: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

"""
A note. The folder a note lives in is set by default from its source.
"""
type Widget_Co_Note {
  id: ID
  content: String
}

type Mutation {
  """
  Copy a note to a new location. Tags and metadata carry over from the
  source; the reminder schedule does not.

  Returns: id, content.
  """
  widget_co_copyNote(id: ID!, targetFolder: ID!): Widget_Co_Note
    @connect(source: "widget_co", http: { POST: "/notes/{$args.id}/copy" }, selection: "id content")
}

type Query {
  _empty: String
}
"#;

#[test]
fn copy_state_undocumented_fires_when_block_doc_lacks_keywords() {
    // Regression (ADR 0032): `doc_comment_before` used to capture from the
    // FIRST `"""` in the file, so the Note type's description — which says
    // "default" — kept this rule quiet for every block-doc copy mutation.
    let result = lint(vec![
        ("widget-co.graphql", s(COPY_SDL_BLOCK_DOC_NO_PRESERVATION)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&copy_inventory())),
        ),
        (".factory/selection.yaml", s(&copy_selection())),
    ]);
    assert!(
        rules(&result).contains(&"copy-state-undocumented".to_string()),
        "expected copy-state-undocumented but got {:?}",
        rules(&result)
    );
}

#[test]
fn copy_state_undocumented_passes_when_block_doc_has_keywords() {
    let result = lint(vec![
        ("widget-co.graphql", s(COPY_SDL_BLOCK_DOC_WITH_PRESERVATION)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&copy_inventory())),
        ),
        (".factory/selection.yaml", s(&copy_selection())),
    ]);
    assert!(
        !rules(&result).contains(&"copy-state-undocumented".to_string()),
        "expected no copy-state-undocumented but got {:?}",
        rules(&result)
    );
}

// ── int-overflow: defaults and exclusive bounds (ADR 0065) ─────────────────

/// int64's maximum, the literal AppWorld's specs use to mean "no bound".
const INT64_MAX: i64 = 9_223_372_036_854_775_807;

fn int_overflow_messages(r: &LintResult) -> Vec<String> {
    r.findings
        .iter()
        .filter(|f| f.rule == "int-overflow")
        .map(|f| f.message.clone())
        .collect()
}

/// The venmo `max_like_count` shape: a plain integer with no format and no
/// bounds whose default is int64's maximum. Before ADR 0065 only `format`,
/// `minimum` and `maximum` were read, so this was silent.
#[test]
fn int_overflow_fires_on_an_int64_default_on_an_argument() {
    let mut inv = int64_inventory(None, None);
    inv["operations"][0]["parameters"][0]["default"] = json!(INT64_MAX);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    let messages = int_overflow_messages(&r);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("argument `index`") && m.contains("default 9223372036854775807")),
        "expected an argument finding naming the default, got {:?}",
        messages
    );
}

#[test]
fn int_overflow_fires_on_an_int64_default_on_a_response_leaf() {
    let mut inv = int64_inventory(None, None);
    inv["shapes"]["Widget"]["properties"]["views"]["default"] = json!(INT64_MAX);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    let messages = int_overflow_messages(&r);
    assert!(
        messages
            .iter()
            .any(|m| m.contains("Widget_Co_Widget.views")
                && m.contains("default 9223372036854775807")),
        "expected a leaf finding naming the default, got {:?}",
        messages
    );
}

/// The one doc-comment spelling that clears a finding: the router never sends
/// an argument's default (`= literal` is banned), so the int64 reaches
/// coercion only if a caller copies it out of the prose, and the spelling
/// tells the caller to omit the argument instead.
#[test]
fn int_overflow_quiet_when_an_argument_spells_its_int64_default_unbounded() {
    let mut inv = int64_inventory(None, None);
    inv["operations"][0]["parameters"][0]["default"] = json!(INT64_MAX);
    let arg_doc = "\"Row index (default: unbounded: the source's 9223372036854775807 does not fit Int; omit the argument to accept it).\" ";
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", arg_doc));
    assert!(
        int_overflow_messages(&r).is_empty(),
        "expected no int-overflow but got {:?}",
        int_overflow_messages(&r)
    );
}

/// The spelling covers a default and nothing else: an argument whose source
/// declares a maximum outside `Int` can be sent that value, and an `Int`
/// argument cannot carry it.
#[test]
fn int_overflow_unbounded_spelling_does_not_clear_an_out_of_range_maximum() {
    let mut inv = int64_inventory(None, None);
    inv["operations"][0]["parameters"][0]["default"] = json!(INT64_MAX);
    inv["operations"][0]["parameters"][0]["maximum"] = json!(INT64_MAX);
    let arg_doc = "\"Row index (default: unbounded: the source's 9223372036854775807 does not fit Int; omit the argument to accept it).\" ";
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", arg_doc));
    assert!(
        int_overflow_messages(&r)
            .iter()
            .any(|m| m.contains("argument `index`") && m.contains("maximum")),
        "expected the maximum to be reported, got {:?}",
        int_overflow_messages(&r)
    );
}

/// A response leaf's default is a value the API may send, so no sentence
/// beside the field clears it.
#[test]
fn int_overflow_unbounded_spelling_does_not_clear_a_response_leaf() {
    let mut inv = int64_inventory(None, None);
    inv["shapes"]["Widget"]["properties"]["views"]["default"] = json!(INT64_MAX);
    let doc = "  \"Views (default: unbounded: the source's 9223372036854775807 does not fit Int; omit the argument to accept it).\"\n";
    let r = int64_lint(&inv, &int64_sdl(doc, "Int", "Int", "views", ""));
    assert!(
        int_overflow_messages(&r)
            .iter()
            .any(|m| m.contains("Widget_Co_Widget.views")),
        "expected the leaf to be reported, got {:?}",
        int_overflow_messages(&r)
    );
}

#[test]
fn int_overflow_quiet_on_a_default_inside_int() {
    let mut inv = int64_inventory(None, None);
    inv["operations"][0]["parameters"][0]["default"] = json!(5);
    inv["shapes"]["Widget"]["properties"]["views"]["default"] = json!(0);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    assert!(
        int_overflow_messages(&r).is_empty(),
        "expected no int-overflow but got {:?}",
        int_overflow_messages(&r)
    );
}

// ── int-overflow: the pre-ADR-0065 default spelling (ADR 0093) ──────────────

/// The spelling § Argument constraints prescribed before ADR 0065: the
/// source's default stated as the number. AppWorld workspaces built then
/// carry it on spotify's six `max_*_count`/`max_duration` arguments and
/// venmo's `max_like_count`.
const LEGACY_DEFAULT_DOC: &str = "\"(default 9223372036854775807, min 0)\" ";

/// The replacement, with the source's number in place of ADR 0065's `N`.
const UNBOUNDED_SPELLING: &str = "default: unbounded: the source's 9223372036854775807 does not fit Int; omit the argument to accept it";

/// A doc comment that states the int64 default as a number stays an error
/// (ADR 0093 declines a downgrade), and the finding names the clause to
/// replace and gives the replacement with the number filled in.
#[test]
fn int_overflow_names_the_legacy_default_clause_and_its_replacement() {
    let mut inv = int64_inventory(None, None);
    inv["operations"][0]["parameters"][0]["default"] = json!(INT64_MAX);
    let r = int64_lint(
        &inv,
        &int64_sdl("", "Int", "Int", "views", LEGACY_DEFAULT_DOC),
    );
    let hits: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "int-overflow")
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert_eq!(hits[0].severity, "error");
    let m = &hits[0].message;
    assert!(
        m.contains("replace `default 9223372036854775807`"),
        "expected the legacy clause quoted, got {}",
        m
    );
    assert!(
        m.contains(&format!("with `{}`", UNBOUNDED_SPELLING)),
        "expected the filled-in replacement, got {}",
        m
    );
}

/// With no doc comment there is no clause to replace: the finding offers the
/// spelling to add, with the number filled in.
#[test]
fn int_overflow_offers_the_filled_in_spelling_when_the_argument_has_no_doc() {
    let mut inv = int64_inventory(None, None);
    inv["operations"][0]["parameters"][0]["default"] = json!(INT64_MAX);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    let messages = int_overflow_messages(&r);
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(
        messages[0].contains(&format!("state it as `({})`", UNBOUNDED_SPELLING)),
        "expected the filled-in spelling, got {}",
        messages[0]
    );
    assert!(!messages[0].contains("replace `"), "{}", messages[0]);
}

/// The clause is matched as the source's number: a doc comment that states a
/// different default (a hand edit, or a stale one) is not quoted as the
/// clause to replace.
#[test]
fn int_overflow_does_not_quote_a_default_clause_with_another_number() {
    let mut inv = int64_inventory(None, None);
    inv["operations"][0]["parameters"][0]["default"] = json!(INT64_MAX);
    let r = int64_lint(
        &inv,
        &int64_sdl(
            "",
            "Int",
            "Int",
            "views",
            "\"(default 92233720368547758, min 0)\" ",
        ),
    );
    let messages = int_overflow_messages(&r);
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(!messages[0].contains("replace `"), "{}", messages[0]);
}

/// The `int-overflow` messages for an `Int` argument whose source default is
/// int64's maximum, under the doc comment `arg_doc`.
fn int64_default_messages(arg_doc: &str) -> Vec<String> {
    let mut inv = int64_inventory(None, None);
    inv["operations"][0]["parameters"][0]["default"] = json!(INT64_MAX);
    int_overflow_messages(&int64_lint(
        &inv,
        &int64_sdl("", "Int", "Int", "views", arg_doc),
    ))
}

/// A clause that ends a sentence is quoted: the period after the number is
/// punctuation, not a decimal part.
#[test]
fn int_overflow_quotes_a_default_clause_that_ends_a_sentence() {
    let messages = int64_default_messages("\"Maximum count. Default 9223372036854775807.\" ");
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(
        messages[0].contains("replace `Default 9223372036854775807` with `"),
        "expected the clause quoted, got {}",
        messages[0]
    );
}

/// The colon form `default: N` is quoted as written.
#[test]
fn int_overflow_quotes_the_colon_form_of_the_default_clause() {
    let messages = int64_default_messages("\"(default: 9223372036854775807, min 0)\" ");
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(
        messages[0].contains("replace `default: 9223372036854775807` with `"),
        "expected the colon form quoted, got {}",
        messages[0]
    );
}

/// The source's number followed by more digits, a decimal part or an
/// exponent is a longer number, and is not quoted as the source's clause.
#[test]
fn int_overflow_does_not_quote_a_longer_number_that_starts_with_the_source_number() {
    for doc in [
        "\"(default 92233720368547758070, min 0)\" ",
        "\"(default 9223372036854775807.5, min 0)\" ",
        "\"(default 9223372036854775807e2, min 0)\" ",
    ] {
        let messages = int64_default_messages(doc);
        assert_eq!(messages.len(), 1, "{}: {:?}", doc, messages);
        assert!(
            !messages[0].contains("replace `"),
            "{}: {}",
            doc,
            messages[0]
        );
    }
}

/// `default: unbounded` beside a clause that still states the number does
/// not clear the finding: the number is still there to copy. The finding
/// says to delete the clause rather than add a second unbounded one.
#[test]
fn int_overflow_keeps_the_finding_when_the_legacy_clause_sits_beside_the_unbounded_one() {
    let messages = int64_default_messages(&format!(
        "\"(default 9223372036854775807, min 0) ({})\" ",
        UNBOUNDED_SPELLING
    ));
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(
        messages[0].contains("delete `default 9223372036854775807`"),
        "expected a delete, got {}",
        messages[0]
    );
    assert!(!messages[0].contains("replace `"), "{}", messages[0]);
}

// ── int-overflow: every legacy default wording, named (ADR 0104) ──────────

/// The wording AppWorld workspaces built at crate 0.5.1 carry, which states
/// no number but is not the `default: unbounded` spelling either.
const NO_UPPER_BOUND_DOC: &str = "\"(default: no upper bound, min 0)\" ";

/// `default: no upper bound` is named as the clause to replace, with the
/// filled-in replacement: following the message literally leaves one clause.
#[test]
fn int_overflow_names_the_no_upper_bound_clause_to_replace() {
    let messages = int64_default_messages(NO_UPPER_BOUND_DOC);
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(
        messages[0].contains(&format!(
            "replace `default: no upper bound` with `{}`",
            UNBOUNDED_SPELLING
        )),
        "expected the no-upper-bound clause quoted, got {}",
        messages[0]
    );
    assert!(!messages[0].contains("state it as"), "{}", messages[0]);
}

/// The clause is quoted as written: without the colon, and in the case the
/// doc comment uses.
#[test]
fn int_overflow_quotes_the_no_upper_bound_clause_as_written() {
    let messages = int64_default_messages("\"Most plays. Default no upper bound.\" ");
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(
        messages[0].contains("replace `Default no upper bound` with `"),
        "expected the clause quoted as written, got {}",
        messages[0]
    );
}

/// Adding the unbounded spelling beside `default: no upper bound`, which is
/// what the pre-ADR-0104 message led to, does not clear the finding: it says
/// to delete the legacy clause.
#[test]
fn int_overflow_keeps_the_finding_when_no_upper_bound_sits_beside_the_unbounded_one() {
    let messages = int64_default_messages(&format!(
        "\"(default: no upper bound, min 0) ({})\" ",
        UNBOUNDED_SPELLING
    ));
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(
        messages[0]
            .contains("already says `default: unbounded`, so delete `default: no upper bound`"),
        "expected a delete, got {}",
        messages[0]
    );
    assert!(!messages[0].contains("replace `"), "{}", messages[0]);
}

/// Replacing the clause as the message says clears the finding.
#[test]
fn int_overflow_clears_once_no_upper_bound_is_replaced_as_the_message_says() {
    let messages = int64_default_messages(&format!("\"({}, min 0)\" ", UNBOUNDED_SPELLING));
    assert!(messages.is_empty(), "{:?}", messages);
}

/// A doc comment carrying both legacy wordings has every clause named: the
/// first is replaced and the other deleted, in the order they appear.
#[test]
fn int_overflow_names_every_legacy_clause_when_the_doc_has_both_wordings() {
    let messages = int64_default_messages(
        "\"(default: no upper bound, min 0). Default 9223372036854775807.\" ",
    );
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(
        messages[0].contains(&format!(
            "replace `default: no upper bound` with `{}` and delete `Default 9223372036854775807`",
            UNBOUNDED_SPELLING
        )),
        "expected both clauses named, got {}",
        messages[0]
    );
    let with_unbounded = int64_default_messages(&format!(
        "\"(default 9223372036854775807) (default: no upper bound) ({})\" ",
        UNBOUNDED_SPELLING
    ));
    assert_eq!(with_unbounded.len(), 1, "{:?}", with_unbounded);
    assert!(
        with_unbounded[0]
            .contains("so delete `default 9223372036854775807` and `default: no upper bound`"),
        "expected both clauses deleted, got {}",
        with_unbounded[0]
    );
}

/// `no upper bound` is read only after `default`: a maximum stated that way
/// is not the default clause.
#[test]
fn int_overflow_does_not_quote_no_upper_bound_outside_a_default_clause() {
    let messages = int64_default_messages("\"(min 0, no upper bound)\" ");
    assert_eq!(messages.len(), 1, "{:?}", messages);
    assert!(messages[0].contains("state it as `("), "{}", messages[0]);
    assert!(!messages[0].contains("replace `"), "{}", messages[0]);
}

/// OpenAPI 3.1 spells an exclusive bound as the number itself.
#[test]
fn int_overflow_fires_on_a_numeric_exclusive_maximum_outside_int_on_a_leaf() {
    let mut inv = int64_inventory(None, None);
    inv["shapes"]["Widget"]["properties"]["views"]["exclusiveMaximum"] = json!(INT64_MAX);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    assert!(
        int_overflow_messages(&r)
            .iter()
            .any(|m| m.contains("Widget_Co_Widget.views") && m.contains("exclusiveMaximum")),
        "expected an exclusiveMaximum finding, got {:?}",
        int_overflow_messages(&r)
    );
}

#[test]
fn int_overflow_fires_on_a_numeric_exclusive_minimum_outside_int_on_an_argument() {
    let mut inv = int64_inventory(None, None);
    inv["operations"][0]["parameters"][0]["exclusiveMinimum"] = json!(-INT64_MAX);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    assert!(
        int_overflow_messages(&r)
            .iter()
            .any(|m| m.contains("argument `index`") && m.contains("exclusiveMinimum")),
        "expected an exclusiveMinimum finding, got {:?}",
        int_overflow_messages(&r)
    );
}

/// `exclusiveMaximum: 2147483648` admits 2147483647 and nothing above it:
/// with `minimum: 0` the pair sits inside `Int` and clears `format: int64`,
/// exactly as an inclusive pair does.
#[test]
fn int_overflow_quiet_when_a_numeric_exclusive_pair_fits_int() {
    let mut inv = int64_inventory(Some("int64"), None);
    for slot in [
        "/shapes/Widget/properties/views",
        "/operations/0/parameters/0",
    ] {
        let slot = inv.pointer_mut(slot).unwrap();
        slot["exclusiveMinimum"] = json!(-1);
        slot["exclusiveMaximum"] = json!(2_147_483_648_i64);
    }
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    assert!(
        int_overflow_messages(&r).is_empty(),
        "expected no int-overflow but got {:?}",
        int_overflow_messages(&r)
    );
}

/// OpenAPI 3.0 and Swagger 2.0 spell it as a boolean that makes `maximum`
/// exclusive: `maximum: 2147483648, exclusiveMaximum: true` admits exactly
/// what `Int` holds.
#[test]
fn int_overflow_quiet_when_a_boolean_exclusive_maximum_brings_the_bound_inside_int() {
    let mut inv = int64_inventory(None, Some((0, 2_147_483_648)));
    inv["shapes"]["Widget"]["properties"]["views"]["exclusiveMaximum"] = json!(true);
    inv["operations"][0]["parameters"][0]["exclusiveMaximum"] = json!(true);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "Int", "views", ""));
    assert!(
        int_overflow_messages(&r).is_empty(),
        "expected no int-overflow but got {:?}",
        int_overflow_messages(&r)
    );
}

/// The guard on the boolean form: it tightens a bound by one, so int64's
/// maximum made exclusive is still far outside `Int`.
#[test]
fn int_overflow_fires_when_a_boolean_exclusive_maximum_is_still_outside_int() {
    let mut inv = int64_inventory(None, Some((0, INT64_MAX)));
    inv["shapes"]["Widget"]["properties"]["views"]["exclusiveMaximum"] = json!(true);
    let r = int64_lint(&inv, &int64_sdl("", "Int", "ID", "views", ""));
    assert!(
        int_overflow_messages(&r)
            .iter()
            .any(|m| m.contains("Widget_Co_Widget.views") && m.contains("maximum")),
        "expected the leaf to be reported, got {:?}",
        int_overflow_messages(&r)
    );
}

// ── closed-enum-as-string (ADR 0041) ─────────────────────────────────────

/// One query argument (`color`), one flat body key (`color` on the paint
/// mutation) and two response leaves — `state`, aliased from `status` under
/// the `widgets` envelope, and `owner.role` — whose source vocabularies are
/// all closed and GraphQL-spellable. `@ARG@`, `@BODY@`, `@STATE@` and `@ROLE@`
/// are the four slot types; `@ARGEXPR@` and `@STATEEXPR@` the two slot
/// expressions the mapped-slot case rewrites. `Widget_Co_Color` and
/// `Widget_Co_Status` are declared so a case can type a slot as an enum.
const CLOSED_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

enum Widget_Co_Color {
  red
  blue
}

enum Widget_Co_Status {
  active
  retired
  pending
}

type Widget_Co_Widget {
  id: ID
  name: String
  state: @STATE@
  owner: Widget_Co_Owner
}

type Widget_Co_Owner {
  login: String
  role: @ROLE@
}

type Query {
  widget_co_listWidgets(limit: Int, color: @ARG@): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: {
        GET: "/widgets"
        queryParams: """
        limit: $args.limit
        @ARGKEY@: @ARGEXPR@
        """
      }
      selection: """
      $.widgets {
        id
        name
        state: @STATEEXPR@
        owner {
          login
          role
        }
      }
      """
    )
}

type Mutation {
  widget_co_paint(id: ID!, color: @BODY@): Widget_Co_Widget
    @connect(
      source: "widget_co"
      http: {
        POST: "/widgets/{$args.id}/paint"
        body: """
        color: $args.color
        """
      }
      selection: "id name"
    )
}
"#;

const CLOSED_SELECTION: &str = "contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\noperations:\n  \"get:/widgets\":\n    include: true\n    response:\n      envelope: widgets\n    graphql:\n      root: query\n      name: listWidgets\n  \"post:/widgets/{id}/paint\":\n    include: true\n    graphql:\n      root: mutation\n      name: paint\n";

fn closed_sdl(
    arg: &str,
    body: &str,
    state: &str,
    role: &str,
    arg_expr: &str,
    state_expr: &str,
) -> String {
    closed_sdl_keyed("color", arg, body, state, role, arg_expr, state_expr)
}

/// `key` is the `queryParams` key the `color` argument is wired to — `color`
/// everywhere but the repeated-parameter case, which wires `color[]`.
fn closed_sdl_keyed(
    key: &str,
    arg: &str,
    body: &str,
    state: &str,
    role: &str,
    arg_expr: &str,
    state_expr: &str,
) -> String {
    CLOSED_SDL
        .replace("@ARGKEY@", key)
        .replace("@ARG@", arg)
        .replace("@BODY@", body)
        .replace("@STATE@", state)
        .replace("@ROLE@", role)
        .replace("@ARGEXPR@", arg_expr)
        .replace("@STATEEXPR@", state_expr)
}

/// Every slot `String`, no mapping: the four findings the rule reports.
fn closed_sdl_all_string() -> String {
    closed_sdl(
        "String",
        "String",
        "String",
        "String",
        "$args.color",
        "status",
    )
}

fn closed_inventory() -> Value {
    json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [
            {"key": "get:/widgets", "operation_id": "listWidgets", "method": "GET", "path": "/widgets", "semantics": "read", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null,
             "parameters": [
                {"name": "limit", "in": "query", "required": false, "type": "integer"},
                {"name": "color", "in": "query", "required": false, "type": "string", "enum": ["red", "blue"]}
             ],
             "request_body": null,
             "response": {"status": "200", "content_type": "application/json", "envelope": "widgets", "shape_ref": "#/shapes/WidgetList", "list": true}},
            {"key": "post:/widgets/{id}/paint", "operation_id": "paint", "method": "POST", "path": "/widgets/{id}/paint", "semantics": "unknown", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null,
             "parameters": [{"name": "id", "in": "path", "required": true, "type": "string"}],
             "request_body": {"content_type": "application/json", "required": true, "shape_ref": "#/shapes/Paint"},
             "response": {"status": "200", "content_type": "application/json", "envelope": null, "shape_ref": "#/shapes/Widget", "list": false}}
        ],
        "shapes": {
            "WidgetList": {"type": "object", "properties": {"widgets": {"type": ["array", "null"], "items": {"$ref": "#/shapes/Widget"}}}},
            "Widget": {"allOf": [{"$ref": "#/shapes/Base"}, {"type": "object", "properties": {"name": {"type": "string"}, "status": {"$ref": "#/shapes/Status"}, "owner": {"$ref": "#/shapes/Owner"}}}]},
            "Base": {"type": "object", "properties": {"id": {"type": "string"}}},
            "Owner": {"type": "object", "properties": {"login": {"type": "string"}, "role": {"type": "string", "enum": ["admin", "member"]}}},
            "Status": {"type": "string", "enum": ["active", "retired", "pending"]},
            "Paint": {"type": "object", "properties": {"color": {"type": "string", "enum": ["red", "blue"]}}}
        },
        "unresolved": []
    })
}

/// A decisions.json holding one record whose `affects` is `affects`;
/// `resolved` decides whether it carries a resolution (and so the status).
fn decisions_json(affects: &[&str], resolved: bool) -> String {
    let mut record = json!({
        "id": "D-0001",
        "title": "The vocabulary stays String",
        "status": if resolved { "resolved" } else { "open" },
        "date": "2026-09-23",
        "affects": affects,
    });
    if resolved {
        record["resolution"] = json!({"decision": "kept as String: the vocabulary is a discriminator the consumer never filters on"});
    }
    graphos_factory_core::json::pretty(&json!({"contract_version": 1, "decisions": [record]}))
}

/// Lints the closed-enum fixture and returns only this rule's findings.
/// `extra` overrides or adds files (a decisions.json, a different inventory).
fn closed_findings(
    sdl: &str,
    extra: Vec<(&str, Option<String>)>,
) -> Vec<graphos_factory_core::lint::Finding> {
    let mut files: Vec<(&str, Option<String>)> = vec![
        ("widget-co.graphql", s(sdl)),
        (".factory/inventory.json", s(&graphos_factory_core::json::pretty(&closed_inventory()))),
        (".factory/selection.yaml", s(CLOSED_SELECTION)),
        ("tests/cases/list_widgets.graphql", s("query { widget_co_listWidgets(limit: 2, color: \"red\") { id name state owner { login role } } }\n")),
        ("tests/cases/paint.graphql", s("mutation { widget_co_paint(id: \"1\", color: \"red\") { id } }\n")),
        ("tests/fixtures/mappings/paint.json", Some(json!({"request": {"method": "POST", "urlPath": "/widgets/1/paint", "bodyPatterns": [{"equalToJson": {"color": "red"}}]}, "response": {"status": 200, "jsonBody": {"id": "1"}}}).to_string())),
    ];
    files.extend(extra);
    lint(files)
        .findings
        .into_iter()
        .filter(|f| f.rule == "closed-enum-as-string")
        .collect()
}

fn line_starting_with(sdl: &str, prefix: &str) -> usize {
    sdl.lines()
        .position(|l| l.trim_start().starts_with(prefix))
        .map(|i| i + 1)
        .unwrap_or_else(|| panic!("no line starting with {:?}", prefix))
}

#[test]
fn closed_enum_as_string_fires_on_a_query_parameter_argument() {
    let sdl = closed_sdl_all_string();
    let found = closed_findings(&sdl, vec![]);
    let f = found
        .iter()
        .find(|f| {
            f.message
                .starts_with("get:/widgets: argument `color` of `widget_co_listWidgets` is String")
        })
        .unwrap_or_else(|| panic!("no argument finding in {:?}", found));
    assert_eq!(f.severity, "warn", "{:?}", f);
    assert!(
        f.message.contains("query parameter `color`"),
        "{}",
        f.message
    );
    assert!(
        f.message.contains("closed enum of `red`, `blue`"),
        "{}",
        f.message
    );
    // Both remedies, in the words the pilots use.
    assert!(
        f.message
            .contains("declare `enum Widget_Co_Color { red blue }` in wire casing"),
        "{}",
        f.message
    );
    assert!(
        f.message
            .contains("--affects \"widget_co_listWidgets(color)\""),
        "{}",
        f.message
    );
    assert_eq!(
        f.line,
        Some(line_starting_with(&sdl, "widget_co_listWidgets(")),
        "the root field's line"
    );
    assert_eq!(found.len(), 4, "one per slot: {:?}", found);
}

#[test]
fn closed_enum_as_string_fires_on_a_flat_body_key() {
    let sdl = closed_sdl_all_string();
    let found = closed_findings(&sdl, vec![]);
    let f = found
        .iter()
        .find(|f| {
            f.message.starts_with(
                "post:/widgets/{id}/paint: argument `color` of `widget_co_paint` is String",
            )
        })
        .unwrap_or_else(|| panic!("no body-key finding in {:?}", found));
    assert!(f.message.contains("body key `color`"), "{}", f.message);
    assert!(
        f.message.contains("--affects \"widget_co_paint(color)\""),
        "{}",
        f.message
    );
    assert_eq!(f.line, Some(line_starting_with(&sdl, "widget_co_paint(")));
}

#[test]
fn closed_enum_as_string_fires_on_a_response_leaf_under_an_envelope_and_at_an_aliased_path() {
    let sdl = closed_sdl_all_string();
    let found = closed_findings(&sdl, vec![]);
    let state = found
        .iter()
        .find(|f| f.message.contains("`Widget_Co_Widget.state` is String"))
        .unwrap_or_else(|| panic!("no `state` finding in {:?}", found));
    // The path is the wire path through the envelope; the slot is the
    // exposed (aliased) name on the GraphQL type.
    assert!(
        state.message.starts_with("get:/widgets:"),
        "{}",
        state.message
    );
    assert!(
        state
            .message
            .contains("source property at `widgets.status`"),
        "{}",
        state.message
    );
    assert!(
        state.message.contains("`active`, `retired`, `pending`"),
        "{}",
        state.message
    );
    assert!(
        state
            .message
            .contains("--affects \"Widget_Co_Widget.state\""),
        "{}",
        state.message
    );
    assert_eq!(
        state.line,
        Some(line_starting_with(&sdl, "state: String")),
        "the field's own line"
    );
    let role = found
        .iter()
        .find(|f| f.message.contains("`Widget_Co_Owner.role` is String"))
        .unwrap_or_else(|| panic!("no `role` finding in {:?}", found));
    assert!(
        role.message.contains("`widgets.owner.role`"),
        "{}",
        role.message
    );
    assert_eq!(role.line, Some(line_starting_with(&sdl, "role: String")));
}

#[test]
fn closed_enum_as_string_quiet_when_the_slot_is_a_graphql_enum() {
    // The leaf and the argument typed as enums: their two findings go, the
    // other two stay (so the silence is the type's, not the fixture's).
    let sdl = closed_sdl(
        "Widget_Co_Color",
        "String",
        "Widget_Co_Status",
        "String",
        "$args.color",
        "status",
    );
    let found = closed_findings(&sdl, vec![]);
    assert!(
        !found
            .iter()
            .any(|f| f.message.contains("`Widget_Co_Widget.state`")
                || f.message.contains("`widget_co_listWidgets`")),
        "{:?}",
        found
    );
    assert_eq!(found.len(), 2, "{:?}", found);
}

/// schema-authoring.md § Enums: a vocabulary with a value that is not a valid
/// GraphQL name is mapped to `String`, so the rule has nothing to say.
#[test]
fn closed_enum_as_string_quiet_when_any_value_is_not_a_valid_graphql_name() {
    let mut inv = closed_inventory();
    // A leading digit, a hyphen, and a reserved word, one per slot.
    inv["operations"][0]["parameters"][1]["enum"] = json!(["10", "2"]);
    inv["shapes"]["Paint"]["properties"]["color"]["enum"] = json!(["in-progress", "done"]);
    inv["shapes"]["Owner"]["properties"]["role"]["enum"] = json!(["true", "false"]);
    let found = closed_findings(
        &closed_sdl_all_string(),
        vec![(
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        )],
    );
    // Only `state` (still `active retired pending`) is left.
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(
        found[0].message.contains("`Widget_Co_Widget.state`"),
        "{}",
        found[0].message
    );
}

/// A slot the connector maps is the mapping's to spell (as `wire-enum-drift`
/// treats it): both expressions are real ones that compose on rover 0.41.0.
#[test]
fn closed_enum_as_string_quiet_when_the_slot_is_mapped_with_match() {
    let sdl = closed_sdl(
        "String",
        "String",
        "String",
        "String",
        "$args.color->match([\"RED\", \"red\"], [\"BLUE\", \"blue\"])",
        "status->match([\"pending\", \"retired\"], [@, @])",
    );
    let found = closed_findings(&sdl, vec![]);
    assert!(
        !found
            .iter()
            .any(|f| f.message.contains("`Widget_Co_Widget.state`")
                || f.message.contains("`widget_co_listWidgets`")),
        "{:?}",
        found
    );
    assert_eq!(
        found.len(),
        2,
        "the body key and `role` still fire: {:?}",
        found
    );
}

/// Each accepted `affects` spelling, one at a time, silences exactly the slot
/// it names and nothing else. The spellings are the ones the pilots already
/// write (gitea D-0003 `Gitea_Issue.state`, `gitea_listIssues(state, type)`).
#[test]
fn closed_enum_as_string_quiet_when_a_resolved_decision_names_the_slot() {
    let sdl = closed_sdl_all_string();
    let cases: Vec<(&str, &str)> = vec![
        // (affects entry, the text that identifies the slot it must silence)
        ("Widget_Co_Widget.state", "`Widget_Co_Widget.state`"),
        ("Widget_Co_Owner.role", "`Widget_Co_Owner.role`"),
        ("widget_co_listWidgets(color)", "`widget_co_listWidgets`"),
        (
            "Query.widget_co_listWidgets(color)",
            "`widget_co_listWidgets`",
        ),
        (
            "widget_co_listWidgets(limit, color)",
            "`widget_co_listWidgets`",
        ),
        ("widget_co_listWidgets(color:)", "`widget_co_listWidgets`"),
        ("widget_co_paint(color)", "`widget_co_paint`"),
        ("Mutation.widget_co_paint(color)", "`widget_co_paint`"),
    ];
    for (affects, slot) in cases {
        let found = closed_findings(
            &sdl,
            vec![(
                ".factory/decisions.json",
                Some(decisions_json(&[affects], true)),
            )],
        );
        assert!(
            !found.iter().any(|f| f.message.contains(slot)),
            "{:?} did not silence {}: {:?}",
            affects,
            slot,
            found
        );
        assert_eq!(
            found.len(),
            3,
            "{:?} silenced more than its slot: {:?}",
            affects,
            found
        );
    }
    // Two controls: a root-type prefix that is not the field's, and a list
    // that does not contain the argument, name nothing.
    for wrong in [
        "Mutation.widget_co_listWidgets(color)",
        "widget_co_listWidgets(limit)",
    ] {
        let found = closed_findings(
            &sdl,
            vec![(
                ".factory/decisions.json",
                Some(decisions_json(&[wrong], true)),
            )],
        );
        assert_eq!(
            found.len(),
            4,
            "{:?} must not silence anything: {:?}",
            wrong,
            found
        );
    }
}

/// A pending question is not a recorded reason.
#[test]
fn closed_enum_as_string_is_not_silenced_by_an_open_decision() {
    let found = closed_findings(
        &closed_sdl_all_string(),
        vec![(
            ".factory/decisions.json",
            Some(decisions_json(
                &["Widget_Co_Widget.state", "widget_co_listWidgets(color)"],
                false,
            )),
        )],
    );
    assert_eq!(found.len(), 4, "{:?}", found);
}

/// No decisions.json is an empty log: the rule runs and says nothing about the
/// file. An unreadable one is treated the same way — no suppression, and the
/// finding about the file (if any) is another rule's.
#[test]
fn closed_enum_as_string_runs_without_a_decisions_file() {
    let sdl = closed_sdl_all_string();
    let found = closed_findings(&sdl, vec![]);
    assert_eq!(found.len(), 4, "{:?}", found);
    assert!(
        !found.iter().any(|f| f.message.contains("decisions.json")),
        "{:?}",
        found
    );
    let found = closed_findings(&sdl, vec![(".factory/decisions.json", s("{ not json"))]);
    assert_eq!(found.len(), 4, "{:?}", found);
    assert!(
        !found.iter().any(|f| f.message.contains("decisions.json")),
        "{:?}",
        found
    );
}

#[test]
fn closed_enum_as_string_degrades_gracefully() {
    // The default fixture: `shapes: {}`, no enum anywhere.
    let r = lint(vec![]);
    assert!(
        !rules(&r).contains(&"closed-enum-as-string".to_string()),
        "{:?}",
        rules(&r)
    );
    // The closed fixture with no selection, then with no inventory either.
    let found = closed_findings(
        &closed_sdl_all_string(),
        vec![(".factory/selection.yaml", None)],
    );
    assert!(found.is_empty(), "{:?}", found);
    let found = closed_findings(
        &closed_sdl_all_string(),
        vec![
            (".factory/selection.yaml", None),
            (".factory/inventory.json", None),
        ],
    );
    assert!(found.is_empty(), "{:?}", found);
    // An inventory with no `shapes` object at all.
    let mut inv = closed_inventory();
    inv.as_object_mut().unwrap().remove("shapes");
    let found = closed_findings(
        &closed_sdl_all_string(),
        vec![(
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        )],
    );
    assert!(found.is_empty(), "{:?}", found);
}

/// How the second operation of `shared_type_findings_with`,
/// `get:/widgets/featured`, documents its response.
enum Featured {
    /// The first operation's `WidgetList` shape: every property is the same
    /// one, reached twice.
    Shared,
    /// A `FeaturedList` / `Featured` shape of its own — `status` as given,
    /// so the two `state` slots can declare different things, and `owner`
    /// as `#/shapes/Owner` when `owner` is true. When it is false the shape
    /// has no `owner` property while the selection still reads
    /// `owner { login role }`, so `Widget_Co_Owner.role` is reached through
    /// an object the spec does not document.
    Own { status: Value, owner: bool },
    /// No response `shape_ref` at all: the selection still fills
    /// `Widget_Co_Widget` and `Widget_Co_Owner`, from nothing the spec says.
    Undocumented,
}

/// The closed fixture plus a second operation, `get:/widgets/featured`,
/// whose root field selects the same `Widget_Co_Widget` leaves (`state:
/// status`, `owner { login role }`) under a `featured` envelope, so the
/// shared type is reached from two operations. `status` is the spec property
/// the second operation reaches `Widget_Co_Widget.state` through: `None`
/// reuses the first operation's shape (the same property, reached twice);
/// `Some(prop)` gives the second operation a shape of its own with that
/// property, so the two slots can declare different things. `Owner` is
/// shared by both shapes either way, so `Widget_Co_Owner.role` is always
/// reached through the same property twice.
fn shared_type_findings(status: Option<Value>) -> Vec<graphos_factory_core::lint::Finding> {
    shared_type_findings_with(match status {
        None => Featured::Shared,
        Some(status) => Featured::Own {
            status,
            owner: true,
        },
    })
}

/// `shared_type_findings` with the second operation's response documented
/// as `featured` says.
fn shared_type_findings_with(featured: Featured) -> Vec<graphos_factory_core::lint::Finding> {
    let mut inv = closed_inventory();
    let (shape_ref, envelope) = match featured {
        Featured::Shared => (json!("#/shapes/WidgetList"), "widgets"),
        Featured::Own { status, owner } => {
            inv["shapes"]["FeaturedList"] = json!({"type": "object", "properties": {"featured": {"type": ["array", "null"], "items": {"$ref": "#/shapes/Featured"}}}});
            let mut properties = json!({"name": {"type": "string"}, "status": status});
            if owner {
                properties["owner"] = json!({"$ref": "#/shapes/Owner"});
            }
            inv["shapes"]["Featured"] = json!({"allOf": [{"$ref": "#/shapes/Base"}, {"type": "object", "properties": properties}]});
            (json!("#/shapes/FeaturedList"), "featured")
        }
        Featured::Undocumented => (Value::Null, "featured"),
    };
    inv["operations"].as_array_mut().unwrap().push(json!({
        "key": "get:/widgets/featured", "operation_id": "featuredWidgets",
        "method": "GET", "path": "/widgets/featured",
        "semantics": "read", "provenance": "spec", "confidence": 1,
        "support": "supported", "support_reason": null,
        "parameters": [], "request_body": null,
        "response": {"status": "200", "content_type": "application/json", "envelope": envelope, "shape_ref": shape_ref, "list": true}
    }));
    // A second root field over the same type, selecting the same leaves.
    let sdl = closed_sdl_all_string().replace(
        "type Query {\n",
        &format!(
            concat!(
                "type Query {{\n",
                "  widget_co_featuredWidgets: [Widget_Co_Widget]\n",
                "    @connect(\n",
                "      source: \"widget_co\"\n",
                "      http: {{ GET: \"/widgets/featured\" }}\n",
                "      selection: \"$.{} {{ id name state: status owner {{ login role }} }}\"\n",
                "    )\n"
            ),
            envelope
        ),
    );
    let selection = format!(
        "{}  \"get:/widgets/featured\":\n    include: true\n    response:\n      envelope: {}\n    graphql:\n      root: query\n      name: featuredWidgets\n",
        CLOSED_SELECTION, envelope
    );
    closed_findings(
        &sdl,
        vec![
            (
                ".factory/inventory.json",
                s(&graphos_factory_core::json::pretty(&inv)),
            ),
            (".factory/selection.yaml", Some(selection)),
            (
                "tests/cases/featured.graphql",
                s("query { widget_co_featuredWidgets { id name state owner { login role } } }\n"),
            ),
        ],
    )
}

/// A type reached from two operations, each through a spec property
/// declaring the same enum, is one leaf and one finding — for `state`
/// (the same `Status` property reached twice) and for `role` alike.
#[test]
fn closed_enum_as_string_reports_a_shared_type_once_when_both_slots_declare_the_same_enum() {
    let found = shared_type_findings(None);
    let state = found
        .iter()
        .filter(|f| f.message.contains("`Widget_Co_Widget.state`"))
        .count();
    let role = found
        .iter()
        .filter(|f| f.message.contains("`Widget_Co_Owner.role`"))
        .count();
    assert_eq!((state, role), (1, 1), "{:?}", found);
    assert_eq!(found.len(), 4, "{:?}", found);
}

/// A shared leaf fires only when **every** reaching spec property declares
/// a closed enum: an enum built from the declaring slot would fail coercion
/// on the other. Here `Widget_Co_Widget.state` is reached through `Status`
/// (`active | retired | pending`) by one operation and through a plain
/// string — or a vocabulary the grammar cannot spell — by the other, and is
/// quiet; `Widget_Co_Owner.role`, reached through the same enum twice, still
/// fires, so the silence is the leaf's and not the fixture's.
#[test]
fn closed_enum_as_string_quiet_on_a_shared_type_when_one_reaching_slot_declares_no_enum() {
    for other in [
        json!({"type": "string"}),
        json!({"type": "string", "enum": ["in-progress", "done"]}),
    ] {
        let found = shared_type_findings(Some(other.clone()));
        assert!(
            !found
                .iter()
                .any(|f| f.message.contains("`Widget_Co_Widget.state`")),
            "{}: {:?}",
            other,
            found
        );
        assert_eq!(
            found
                .iter()
                .filter(|f| f.message.contains("`Widget_Co_Owner.role`"))
                .count(),
            1,
            "{}: {:?}",
            other,
            found
        );
        assert_eq!(found.len(), 3, "{}: {:?}", other, found);
    }
}

/// A reach the spec does not document declares nothing, and blocks the leaf
/// as a plain string would: the enum a finding proposed would type a field
/// another operation fills from an object the spec never describes. First
/// the second operation's `Featured` shape has no `owner` property while its
/// selection still reads `owner { login role }` — `Widget_Co_Owner.role` is
/// quiet, and `Widget_Co_Widget.state`, whose two properties both declare,
/// still fires, so the silence is the leaf's and not the fixture's. Then the
/// second operation has no response `shape_ref` at all, and both leaves it
/// reaches are quiet. Before this test, `walk_closed_enums` returned early
/// on a missing shape and an operation without one was skipped, so neither
/// reach was recorded and `role` fired on its documented slot alone.
#[test]
fn closed_enum_as_string_quiet_on_a_shared_type_when_a_reach_is_undocumented() {
    // The parent object absent from the operation's shape.
    let found = shared_type_findings_with(Featured::Own {
        status: json!({"$ref": "#/shapes/Status"}),
        owner: false,
    });
    assert!(
        !found
            .iter()
            .any(|f| f.message.contains("`Widget_Co_Owner.role`")),
        "{:?}",
        found
    );
    assert_eq!(
        found
            .iter()
            .filter(|f| f.message.contains("`Widget_Co_Widget.state`"))
            .count(),
        1,
        "{:?}",
        found
    );
    assert_eq!(found.len(), 3, "{:?}", found);
    // The operation with no response shape at all.
    let found = shared_type_findings_with(Featured::Undocumented);
    assert!(
        !found.iter().any(|f| {
            f.message.contains("`Widget_Co_Owner.role`")
                || f.message.contains("`Widget_Co_Widget.state`")
        }),
        "{:?}",
        found
    );
    assert_eq!(found.len(), 2, "{:?}", found);
}

/// When every reaching slot declares a closed enum but the sets differ, the
/// one finding lists their union in first-seen order, names each reaching
/// property with its operation, and the proposed declaration carries the
/// union — it is the one enum the shared field would be typed with.
#[test]
fn closed_enum_as_string_lists_the_union_when_the_reaching_slots_declare_different_enums() {
    let found = shared_type_findings(Some(
        json!({"type": "string", "enum": ["archived", "active"]}),
    ));
    let state: Vec<_> = found
        .iter()
        .filter(|f| f.message.contains("`Widget_Co_Widget.state`"))
        .collect();
    assert_eq!(state.len(), 1, "{:?}", found);
    let m = &state[0].message;
    assert!(m.starts_with("get:/widgets: "), "{}", m);
    assert!(
        m.contains(
            "every source property reaching it (`widgets.status` via get:/widgets, `featured.status` via get:/widgets/featured) is a closed enum"
        ),
        "{}",
        m
    );
    // The union, first-seen and deduplicated: `active` is not repeated.
    assert!(
        m.contains("together they enumerate `active`, `retired`, `pending`, `archived`;"),
        "{}",
        m
    );
    assert!(
        m.contains(
            "declare `enum Widget_Co_Status { active retired pending archived }` in wire casing"
        ),
        "{}",
        m
    );
    assert_eq!(found.len(), 4, "{:?}", found);
}

/// A one-member `enum` is a constant discriminator, not a vocabulary: a
/// one-member GraphQL enum documents nothing the field does not already say.
/// The floor is `closed_enum_values`', so it holds for an argument …
#[test]
fn closed_enum_as_string_quiet_when_an_argument_enum_has_one_value() {
    let mut inv = closed_inventory();
    inv["operations"][0]["parameters"][1]["enum"] = json!(["red"]);
    let found = closed_findings(
        &closed_sdl_all_string(),
        vec![(
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        )],
    );
    assert!(
        !found
            .iter()
            .any(|f| f.message.contains("`widget_co_listWidgets`")),
        "{:?}",
        found
    );
    assert_eq!(found.len(), 3, "{:?}", found);
}

/// … and for a response leaf alike.
#[test]
fn closed_enum_as_string_quiet_when_a_leaf_enum_has_one_value() {
    let mut inv = closed_inventory();
    inv["shapes"]["Owner"]["properties"]["role"]["enum"] = json!(["admin"]);
    let found = closed_findings(
        &closed_sdl_all_string(),
        vec![(
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        )],
    );
    assert!(
        !found
            .iter()
            .any(|f| f.message.contains("`Widget_Co_Owner.role`")),
        "{:?}",
        found
    );
    assert_eq!(found.len(), 3, "{:?}", found);
}

/// A `[String]` leaf over an array of enumerated items, and a `[String]`
/// argument over an enumerated repeated query parameter, are both the same
/// flattened vocabulary once per element.
#[test]
fn closed_enum_as_string_fires_on_a_string_list_over_enum_items() {
    let mut inv = closed_inventory();
    inv["shapes"]["Owner"]["properties"]["role"] =
        json!({"type": "array", "items": {"type": "string", "enum": ["admin", "member"]}});
    inv["operations"][0]["parameters"][1] = json!({
        "name": "color[]", "in": "query", "required": false, "type": "array",
        "enum": ["red", "blue"]
    });
    let sdl = closed_sdl_keyed(
        "color[]",
        "[String]",
        "String",
        "String",
        "[String]",
        "$args.color",
        "status",
    );
    let found = closed_findings(
        &sdl,
        vec![(
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        )],
    );
    assert!(
        found
            .iter()
            .any(|f| f.message.contains("`Widget_Co_Owner.role` is String")),
        "{:?}",
        found
    );
    assert!(
        found.iter().any(|f| f
            .message
            .contains("argument `color` of `widget_co_listWidgets` is String")
            && f.message.contains("query parameter `color`")),
        "{:?}",
        found
    );
    assert_eq!(found.len(), 4, "{:?}", found);
}

/// A vocabulary longer than eight values is capped in the message.
#[test]
fn closed_enum_as_string_caps_a_long_vocabulary_at_eight() {
    let mut inv = closed_inventory();
    inv["shapes"]["Owner"]["properties"]["role"]["enum"] =
        json!(["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
    let found = closed_findings(
        &closed_sdl_all_string(),
        vec![(
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        )],
    );
    let role = found
        .iter()
        .find(|f| f.message.contains("`Widget_Co_Owner.role`"))
        .unwrap_or_else(|| panic!("{:?}", found));
    assert!(role.message.contains("`h` (+2 more)"), "{}", role.message);
    assert!(!role.message.contains("`i`"), "{}", role.message);
    // The proposed declaration is the paste target, so it carries every
    // value; only the summary list is capped.
    assert!(
        role.message
            .contains("declare `enum Widget_Co_Role { a b c d e f g h i j }` in wire casing"),
        "{}",
        role.message
    );
}

// ── One-line connector blocks (ADR 0042) ───────────────────────────────────
//
// The AppWorld LLM arm writes every `queryParams` and `body` block on one
// line with several `key: $args.x` pairs. Until ADR 0042 the reading shared
// by `wire-enum-drift`, `int-overflow` and `closed-enum-as-string` was
// line-oriented: such a `queryParams` yielded its first pair only and such a
// `body` was `NotFlat`, so the defective slot in every test below — always
// the second or third pair — was invisible. Each firing case here was run
// red against the old readers.

/// `@LIST_ARGS@` / `@QUERY@` are the query root field's argument list and its
/// `queryParams` attribute; `@PAINT_ARGS@` / `@BODY@` the mutation's argument
/// list and its `body` attribute.
const ONE_LINE_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

enum Widget_Co_Color {
  RED
  blue
}

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  widget_co_listWidgets(@LIST_ARGS@): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: { GET: "/widgets", @QUERY@ }
      selection: "$.widgets { id name }"
    )
}

type Mutation {
  widget_co_paint(@PAINT_ARGS@): Widget_Co_Widget
    @connect(
      source: "widget_co"
      http: { POST: "/widgets/{$args.id}/paint", @BODY@ }
      selection: "id name"
    )
}
"#;

fn one_line_sdl(list_args: &str, query: &str, paint_args: &str, body: &str) -> String {
    ONE_LINE_SDL
        .replace("@LIST_ARGS@", list_args)
        .replace("@QUERY@", query)
        .replace("@PAINT_ARGS@", paint_args)
        .replace("@BODY@", body)
}

/// Query parameters `name` (no enum), `color` and `mode` (closed enums),
/// `size` (int64) and `ids` (a list); body keys `name`, `color` (enum),
/// `size` (int64) and `tags` (a list).
fn one_line_inventory() -> Value {
    json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [
            {"key": "get:/widgets", "operation_id": "listWidgets", "method": "GET", "path": "/widgets", "semantics": "read", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null,
             "parameters": [
                {"name": "limit", "in": "query", "required": false, "type": "integer"},
                {"name": "name", "in": "query", "required": false, "type": "string"},
                {"name": "color", "in": "query", "required": false, "type": "string", "enum": ["red", "blue"]},
                {"name": "mode", "in": "query", "required": false, "type": "string", "enum": ["fast", "slow"]},
                {"name": "size", "in": "query", "required": false, "type": "integer", "format": "int64"},
                {"name": "ids", "in": "query", "required": false, "type": "array", "items": {"type": "string"}}
             ],
             "request_body": null,
             "response": {"status": "200", "content_type": "application/json", "envelope": "widgets", "shape_ref": "#/shapes/WidgetList", "list": true}},
            {"key": "post:/widgets/{id}/paint", "operation_id": "paint", "method": "POST", "path": "/widgets/{id}/paint", "semantics": "unknown", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null,
             "parameters": [{"name": "id", "in": "path", "required": true, "type": "string"}],
             "request_body": {"content_type": "application/json", "required": true, "shape_ref": "#/shapes/Paint"},
             "response": {"status": "200", "content_type": "application/json", "envelope": null, "shape_ref": "#/shapes/Widget", "list": false}}
        ],
        "shapes": {
            "WidgetList": {"type": "object", "properties": {"widgets": {"type": ["array", "null"], "items": {"$ref": "#/shapes/Widget"}}}},
            "Widget": {"type": "object", "properties": {"id": {"type": "string"}, "name": {"type": "string"}}},
            "Paint": {"type": "object", "properties": {
                "name": {"type": "string"},
                "color": {"type": "string", "enum": ["red", "blue"]},
                "size": {"type": "integer", "format": "int64"},
                "tags": {"type": "array", "items": {"type": "string"}}
            }}
        },
        "unresolved": []
    })
}

/// Lints the one-line fixture and returns `rule`'s messages. `extra` adds or
/// overrides files (cases, stubs, a unit suite).
fn one_line_messages(sdl: &str, rule: &str, extra: Vec<(&str, Option<String>)>) -> Vec<String> {
    let mut files: Vec<(&str, Option<String>)> = vec![
        ("widget-co.graphql", s(sdl)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&one_line_inventory())),
        ),
        (".factory/selection.yaml", s(CLOSED_SELECTION)),
        (
            "tests/cases/paint.graphql",
            s("mutation { widget_co_paint(id: \"1\", name: \"a\") { id } }\n"),
        ),
    ];
    files.extend(extra);
    lint(files)
        .findings
        .into_iter()
        .filter(|f| f.rule == rule)
        .map(|f| f.message)
        .collect()
}

fn flat_pairs(mapping: &BodyMapping) -> Option<Vec<(&str, &str)>> {
    match mapping {
        BodyMapping::Flat(pairs) => Some(
            pairs
                .iter()
                .map(|(a, k)| (a.as_str(), k.as_str()))
                .collect(),
        ),
        _ => None,
    }
}

#[test]
fn closed_enum_as_string_reads_the_third_pair_of_a_one_line_query_params() {
    let sdl = one_line_sdl(
        "limit: Int, name: String, color: String",
        r#"queryParams: "limit: $args.limit name: $args.name color: $args.color""#,
        "id: ID!",
        "",
    );
    let found = one_line_messages(&sdl, "closed-enum-as-string", vec![]);
    assert!(
        found.iter().any(|m| m
            .starts_with("get:/widgets: argument `color` of `widget_co_listWidgets` is String")
            && m.contains("query parameter `color`")
            && m.contains("closed enum of `red`, `blue`")),
        "{:?}",
        found
    );
    // `name` sits between them and carries no enum: read, and quiet.
    assert!(!found.iter().any(|m| m.contains("`name`")), "{:?}", found);
    assert_eq!(found.len(), 1, "{:?}", found);
}

/// amazon's `duration` is the second pair of its one-line `body`; this is
/// that shape.
#[test]
fn closed_enum_as_string_reads_the_second_pair_of_a_one_line_body() {
    let sdl = one_line_sdl(
        "limit: Int",
        r#"queryParams: "limit: $args.limit""#,
        "id: ID!, name: String, color: String",
        r#"body: "name: $args.name color: $args.color""#,
    );
    let found = one_line_messages(&sdl, "closed-enum-as-string", vec![]);
    assert!(
        found.iter().any(|m| m.starts_with(
            "post:/widgets/{id}/paint: argument `color` of `widget_co_paint` is String"
        ) && m.contains("body key `color`")),
        "{:?}",
        found
    );
    assert_eq!(found.len(), 1, "{:?}", found);
}

#[test]
fn wire_enum_drift_reads_the_second_pair_of_a_one_line_query_params() {
    let sdl = one_line_sdl(
        "limit: Int, color: Widget_Co_Color, name: String",
        r#"queryParams: "limit: $args.limit color: $args.color name: $args.name""#,
        "id: ID!",
        "",
    );
    let found = one_line_messages(
        &sdl,
        "wire-enum-drift",
        vec![(
            "tests/cases/list_widgets.graphql",
            s("query { widget_co_listWidgets(limit: 2, color: RED) { id name } }\n"),
        )],
    );
    assert!(
        found
            .iter()
            .any(|m| m.starts_with("Widget_Co_Color declares `RED`")
                && m.contains("query parameter `color`")),
        "{:?}",
        found
    );
    assert_eq!(found.len(), 1, "{:?}", found);
}

#[test]
fn wire_enum_drift_reads_the_third_pair_of_a_one_line_body() {
    let sdl = one_line_sdl(
        "limit: Int",
        r#"queryParams: "limit: $args.limit""#,
        "id: ID!, name: String, size: String, color: Widget_Co_Color",
        r#"body: "name: $args.name size: $args.size color: $args.color""#,
    );
    let found = one_line_messages(&sdl, "wire-enum-drift", vec![]);
    assert!(
        found
            .iter()
            .any(|m| m.starts_with("Widget_Co_Color declares `RED`")
                && m.contains("body key `color`")),
        "{:?}",
        found
    );
    assert_eq!(found.len(), 1, "{:?}", found);
}

#[test]
fn int_overflow_reads_the_third_pair_of_a_one_line_query_params() {
    let sdl = one_line_sdl(
        "limit: Int, name: String, size: Int",
        r#"queryParams: "limit: $args.limit name: $args.name size: $args.size""#,
        "id: ID!",
        "",
    );
    let found = one_line_messages(&sdl, "int-overflow", vec![]);
    assert!(
        found
            .iter()
            .any(|m| m.starts_with("get:/widgets: argument `size` is Int")
                && m.contains("query parameter `size`")),
        "{:?}",
        found
    );
    // `limit` is the first pair and a plain integer: read, and quiet.
    assert_eq!(found.len(), 1, "{:?}", found);
}

#[test]
fn int_overflow_reads_the_second_pair_of_a_one_line_body() {
    let sdl = one_line_sdl(
        "limit: Int",
        r#"queryParams: "limit: $args.limit""#,
        "id: ID!, name: String, size: Int",
        r#"body: "name: $args.name size: $args.size""#,
    );
    let found = one_line_messages(&sdl, "int-overflow", vec![]);
    assert!(
        found.iter().any(
            |m| m.starts_with("post:/widgets/{id}/paint: argument `size` is Int")
                && m.contains("body key `size`")
        ),
        "{:?}",
        found
    );
    assert_eq!(found.len(), 1, "{:?}", found);
}

/// ADR 0016: a `->match` marks *that slot* as the mapping's to spell, not
/// every slot on its line. The old `slot_expression` returned the rest of
/// the line, so a mapped second pair silenced the first pair too and the
/// third was never read.
#[test]
fn a_mapped_pair_on_a_one_line_block_is_skipped_alone_and_its_neighbours_are_still_read() {
    let sdl = one_line_sdl(
        "limit: Int, mode: String, color: String, size: Int",
        r#"queryParams: """limit: $args.limit mode: $args.mode color: $args.color->match(["red", "RED"], ["blue", "BLUE"]) size: $args.size""""#,
        "id: ID!",
        "",
    );
    let closed = one_line_messages(&sdl, "closed-enum-as-string", vec![]);
    assert!(
        closed.iter().any(
            |m| m.contains("argument `mode` of `widget_co_listWidgets` is String")
                && m.contains("query parameter `mode`")
        ),
        "the pair before the mapped one is read: {:?}",
        closed
    );
    assert!(
        !closed.iter().any(|m| m.contains("`color`")),
        "the mapped pair itself is skipped: {:?}",
        closed
    );
    let overflow = one_line_messages(&sdl, "int-overflow", vec![]);
    assert!(
        overflow
            .iter()
            .any(|m| m.contains("argument `size` is Int") && m.contains("query parameter `size`")),
        "the pair after the mapped one is read: {:?}",
        overflow
    );
}

#[test]
fn body_mapping_reads_a_one_line_multi_pair_body_as_flat_in_order() {
    let quoted = r#"widget_co_paint(id: ID!): Widget_Co_Widget @connect(source: "widget_co", http: { POST: "/widgets", body: "name: $args.name color: $args.color size: $args.size" }, selection: "id")"#;
    assert_eq!(
        flat_pairs(&body_mapping(quoted)),
        Some(vec![("name", "name"), ("color", "color"), ("size", "size")])
    );
    let triple = r#"widget_co_paint(id: ID!): Widget_Co_Widget @connect(source: "widget_co", http: { POST: "/widgets", body: """name: $args.name color: $args.color""" }, selection: "id")"#;
    assert_eq!(
        flat_pairs(&body_mapping(triple)),
        Some(vec![("name", "name"), ("color", "color")])
    );
    let wire = wiring(quoted);
    assert!(wire.sends_body);
    assert_eq!(wire.body_args, vec!["name", "color", "size"]);
}

#[test]
fn body_mapping_keeps_not_flat_for_a_one_line_body_that_is_not_a_pure_pair_list() {
    let field = |body: &str| {
        format!(
            r#"widget_co_paint(id: ID!): Widget_Co_Widget @connect(source: "widget_co", http: {{ POST: "/widgets", body: {} }}, selection: "id")"#,
            body
        )
    };
    for body in [
        // a nested object on the second pair
        r#""name: $args.name meta: { k: $args.k }""#,
        // a literal
        r#""""name: $args.name kind: "widget" """"#,
        // a method
        r#""name: $args.name size: $args.size->jsonStringify""#,
        // an expression block
        r#""name: $args.name all: $($args.a ?? $args.b)""#,
        // a path into the argument
        r#""name: $args.name owner: $args.owner.login""#,
        // a bare token after a pair
        r#""name: $args.name flag""#,
        // no key at all
        r#""$args.input""#,
        // commas are not the mapping language's separator
        r#""name: $args.name, color: $args.color""#,
    ] {
        assert!(
            matches!(body_mapping(&field(body)), BodyMapping::NotFlat),
            "{} should not be flat",
            body
        );
    }
}

#[test]
fn quoted_keys_are_read_on_one_line() {
    let text = r#"widget_co_listWidgets(x: String, y: String): [Widget_Co_Widget] @connect(source: "widget_co", http: { GET: "/widgets", queryParams: """ "x-key": $args.x "y-key": $args.y """ }, selection: "id")"#;
    assert_eq!(
        wiring(text).query_keys,
        vec![
            ("x".to_string(), "x-key".to_string()),
            ("y".to_string(), "y-key".to_string())
        ]
    );
    let body = r#"widget_co_paint(x: String, y: String): Widget_Co_Widget @connect(source: "widget_co", http: { POST: "/widgets", body: """ "x-key": $args.x "y-key": $args.y """ }, selection: "id")"#;
    assert_eq!(
        flat_pairs(&body_mapping(body)),
        Some(vec![("x", "x-key"), ("y", "y-key")])
    );
}

/// The guard for the widening: a block written one pair per line — with a
/// blank line, a comment line, a quoted key, methods whose arguments carry
/// spaces, a literal and a `??` — reads exactly as the line-oriented readers
/// read it.
#[test]
fn a_multi_line_block_reads_exactly_as_before() {
    let text = r#"widget_co_listWidgets(limit: Int): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: {
        GET: "/widgets"
        queryParams: """
        limit: $args.limit

        # a comment line
        "x-key": $args.x
        cards: $args.cards->joinNotNull(",")
        size: $args.size->match([null, 1], [@, @])
        sort: "created"
        page: $args.page ?? 1
        """
      }
      selection: "id"
    )"#;
    let pairs: Vec<(&str, &str)> = [
        ("limit", "limit"),
        ("x", "x-key"),
        ("cards", "cards"),
        ("size", "size"),
        ("page", "page"),
    ]
    .to_vec();
    assert_eq!(
        wiring(text)
            .query_keys
            .iter()
            .map(|(a, k)| (a.as_str(), k.as_str()))
            .collect::<Vec<_>>(),
        pairs
    );
    // Exactly `$args.<path>` only: a method or a `??` is not a plain key.
    assert_eq!(
        wiring(text).plain_query_keys,
        vec![
            (vec!["limit".to_string()], "limit".to_string()),
            (vec!["x".to_string()], "x-key".to_string())
        ]
    );
    let nested = r#"widget_co_nested(name: String!, kind: String): Widget_Co_Widget
    @connect(
      source: "widget_co"
      http: {
        POST: "/nested"
        body: """
        thing: {
          name: $args.name
        }
        kind: $args.kind
        """
      }
      selection: "id"
    )"#;
    assert!(matches!(body_mapping(nested), BodyMapping::NotFlat));
    let flat = r#"widget_co_paint(id: ID!, color: String, name: String): Widget_Co_Widget
    @connect(
      source: "widget_co"
      http: {
        POST: "/widgets/{$args.id}/paint"
        body: """
        color: $args.color
        name: $args.name
        """
      }
      selection: "id"
    )"#;
    assert_eq!(
        flat_pairs(&body_mapping(flat)),
        Some(vec![("color", "color"), ("name", "name")])
    );
}

// ─── error-path-unresolved (ADR 0043) ───────────────────────────────────────

/// Two operations on one source. `@SOURCE_ERRORS@` and `@CONNECT_ERRORS@`
/// are whole lines: an `errors: { … }` argument, or empty.
const ERRPATH_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
@SOURCE_ERRORS@
)

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  widget_co_listWidgets: [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id name")
  widget_co_getWidget(id: ID!): Widget_Co_Widget
    @connect(
      source: "widget_co"
      http: { GET: "/widgets/{$args.id}" }
      selection: "id name"
@CONNECT_ERRORS@
    )
}
"#;

const ERRPATH_SELECTION: &str = "contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\noperations:\n  \"get:/widgets\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: query\n      name: listWidgets\n  \"get:/widgets/{id}\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: query\n      name: getWidget\n";

/// An `errors: { … }` argument with this `message` string and these
/// `extensions` lines (a triple-quoted block, omitted when empty).
fn errors_arg(indent: &str, message: &str, extensions: &[&str]) -> String {
    let mut out = format!("{i}errors: {{\n{i}  message: \"{}\"\n", message, i = indent);
    if !extensions.is_empty() {
        out.push_str(&format!("{i}  extensions: \"\"\"\n", i = indent));
        for e in extensions {
            out.push_str(&format!("{i}  {}\n", e, i = indent));
        }
        out.push_str(&format!("{i}  \"\"\"\n", i = indent));
    }
    out.push_str(&format!("{i}}}", i = indent));
    out
}

fn errpath_sdl(source_errors: &str, connect_errors: &str) -> String {
    ERRPATH_SDL
        .replace("@SOURCE_ERRORS@", source_errors)
        .replace("@CONNECT_ERRORS@", connect_errors)
}

/// `list_errors` / `get_errors` are each operation's `errors` array;
/// `shapes` the inventory's shapes.
fn errpath_inventory(list_errors: Value, get_errors: Value, shapes: Value) -> Value {
    json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [
            {"key": "get:/widgets", "operation_id": "listWidgets", "method": "GET", "path": "/widgets", "semantics": "read", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null,
             "errors": list_errors},
            {"key": "get:/widgets/{id}", "operation_id": "getWidget", "method": "GET", "path": "/widgets/{id}", "semantics": "read", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null,
             "parameters": [{"name": "id", "in": "path", "required": true, "type": "string"}],
             "errors": get_errors}
        ],
        "shapes": shapes,
        "unresolved": []
    })
}

fn err_ref(status: &str, shape: &str) -> Value {
    json!({"status": status, "description": "an error", "shape_ref": format!("#/shapes/{}", shape)})
}

/// The AppWorld case: every documented error body is an inline
/// `{ message }` (ADR 0043 names it `{Op}Error{Status}`).
fn message_bodies() -> Value {
    errpath_inventory(
        json!([
            err_ref("422", "ListWidgetsError422"),
            err_ref("401", "ListWidgetsError401")
        ]),
        json!([err_ref("404", "GetWidgetError404")]),
        json!({
            "ListWidgetsError422": {"type": "object", "properties": {"message": {"type": "string"}}},
            "ListWidgetsError401": {"type": "object", "properties": {"message": {"type": "string"}}},
            "GetWidgetError404": {"type": "object", "properties": {"message": {"type": "string"}}}
        }),
    )
}

/// (message, line, severity) of every `error-path-unresolved` finding.
fn errpath_findings(sdl: &str, inv: &Value) -> Vec<(String, Option<usize>, String)> {
    lint(vec![
        ("widget-co.graphql", s(sdl)),
        (".factory/selection.yaml", s(ERRPATH_SELECTION)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(inv)),
        ),
    ])
    .findings
    .into_iter()
    .filter(|f| f.rule == "error-path-unresolved")
    .map(|f| (f.message, f.line, f.severity))
    .collect()
}

/// The 1-based line of the first line of `sdl` containing `needle`.
fn line_containing(sdl: &str, needle: &str) -> usize {
    sdl.lines().position(|l| l.contains(needle)).unwrap() + 1
}

#[test]
fn an_error_path_the_documented_bodies_lack_is_a_warning_at_its_line() {
    let sdl = errpath_sdl(
        &errors_arg(
            "  ",
            "$($.detail ?? 'Widget Co request failed')",
            &["httpStatus: $status"],
        ),
        "",
    );
    let found = errpath_findings(&sdl, &message_bodies());
    assert_eq!(found.len(), 1, "{:?}", found);
    let (message, line, severity) = &found[0];
    assert_eq!(severity, "warn");
    assert_eq!(*line, Some(line_containing(&sdl, "$.detail")));
    for part in [
        "`$.detail`",
        "the @source(name: \"widget_co\") errors block",
        "1 distinct body checked across the 2 operations it covers",
        "documented error bodies carry: message",
        "errors[].shape_ref in .factory/inventory.json",
    ] {
        assert!(message.contains(part), "{:?} lacks {:?}", message, part);
    }
}

#[test]
fn an_error_path_a_documented_body_carries_is_clean() {
    // A `$.`-looking text inside a string literal is not a path.
    let sdl = errpath_sdl(
        &errors_arg(
            "  ",
            "$($.message ?? 'no $.detail here')",
            &[
                "httpStatus: $status",
                "note: \"$.detail\"",
                "text: $.message",
            ],
        ),
        "",
    );
    assert!(errpath_findings(&sdl, &message_bodies()).is_empty());
}

#[test]
fn a_nested_error_path_is_walked_segment_by_segment() {
    let inv = errpath_inventory(
        json!([err_ref("400", "ListWidgetsError400")]),
        json!([]),
        json!({"ListWidgetsError400": {"type": "object", "properties": {
            "error": {"type": "object", "properties": {"message": {"type": "string"}, "code": {"type": "integer"}}}
        }}}),
    );
    let sdl = errpath_sdl(
        &errors_arg(
            "  ",
            "$($.error.message ?? 'Widget Co request failed')",
            &[
                "httpStatus: $status",
                "code: $.error.code",
                "detail: $.error.detail",
            ],
        ),
        "",
    );
    let found = errpath_findings(&sdl, &inv);
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(found[0].0.starts_with("`$.error.detail`"), "{:?}", found);
    assert!(found[0].0.contains("carry: error"), "{:?}", found);
    assert_eq!(found[0].1, Some(line_containing(&sdl, "$.error.detail")));
}

#[test]
fn first_last_and_get_step_into_an_array_of_errors() {
    let inv = errpath_inventory(
        json!([err_ref("422", "ListWidgetsError422")]),
        json!([]),
        json!({"ListWidgetsError422": {"type": "object", "properties": {
            "errors": {"type": "array", "items": {"type": "object", "properties": {"message": {"type": "string"}}}}
        }}}),
    );
    let sdl = errpath_sdl(
        &errors_arg(
            "  ",
            "$($.errors->first.message ?? 'Widget Co request failed')",
            &[
                "optional: $.errors?->first?.message",
                "last: $.errors->last.message",
                "nth: $.errors->get(0).message",
                "all: $.errors.message",
                "wrong: $.errors->first.detail",
                "wrong_last: $.errors?->last?.detail",
                "wrong_nth: $.errors->get(0).detail",
            ],
        ),
        "",
    );
    // Each wrong path is read through its array step to `detail`; were a
    // step unreadable, the path would end at `$.errors` and resolve.
    let found: Vec<String> = errpath_findings(&sdl, &inv)
        .into_iter()
        .map(|(m, _, _)| m.split('`').nth(1).unwrap_or("").to_string())
        .collect();
    assert_eq!(
        found,
        vec![
            "$.errors->first.detail",
            "$.errors?->last?.detail",
            "$.errors->get(0).detail"
        ],
    );
}

#[test]
fn each_alternative_of_a_fallback_chain_is_judged_on_its_own() {
    let sdl = errpath_sdl(
        &errors_arg(
            "  ",
            "$($.message ?? $.detail ?? 'Widget Co request failed')",
            &[],
        ),
        "",
    );
    let found = errpath_findings(&sdl, &message_bodies());
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(found[0].0.starts_with("`$.detail`"), "{:?}", found);
}

#[test]
fn a_path_repeated_in_one_block_is_reported_once() {
    let sdl = errpath_sdl(
        &errors_arg(
            "  ",
            "$($.detail ?? 'Widget Co request failed')",
            &["detail: $.detail"],
        ),
        "",
    );
    let found = errpath_findings(&sdl, &message_bodies());
    assert_eq!(found.len(), 1, "{:?}", found);
    assert_eq!(
        found[0].1,
        Some(line_containing(&sdl, "message: \"$($.detail"))
    );
}

#[test]
fn no_documented_error_body_means_nothing_to_check() {
    let sdl = errpath_sdl(
        &errors_arg("  ", "$($.detail ?? 'Widget Co request failed')", &[]),
        "",
    );
    let bare = errpath_inventory(
        json!([{"status": "422", "description": "Validation failed", "shape_ref": null}]),
        json!([]),
        json!({}),
    );
    assert!(errpath_findings(&sdl, &bare).is_empty());
    // The same block against a documented `{ message }` body fires.
    assert_eq!(errpath_findings(&sdl, &message_bodies()).len(), 1);
}

#[test]
fn a_connect_errors_block_is_judged_against_its_own_operation_only() {
    // listWidgets documents `{ message }`, getWidget `{ error }`.
    let inv = errpath_inventory(
        json!([err_ref("422", "ListWidgetsError422")]),
        json!([err_ref("404", "GetWidgetError404")]),
        json!({
            "ListWidgetsError422": {"type": "object", "properties": {"message": {"type": "string"}}},
            "GetWidgetError404": {"type": "object", "properties": {"error": {"type": "string"}}}
        }),
    );
    // The @source block covers both operations, so each path resolves in
    // one of the two bodies; the @connect block sees getWidget's alone.
    let sdl = errpath_sdl(
        &errors_arg(
            "  ",
            "$($.message ?? $.error ?? 'Widget Co request failed')",
            &[],
        ),
        &errors_arg(
            "      ",
            "$($.message ?? $.error ?? 'Widget not found')",
            &[],
        ),
    );
    let found = errpath_findings(&sdl, &inv);
    assert_eq!(found.len(), 1, "{:?}", found);
    let (message, line, _) = &found[0];
    assert!(message.starts_with("`$.message`"), "{:?}", message);
    assert!(
        message.contains("the `Query.widget_co_getWidget` @connect errors block"),
        "{:?}",
        message
    );
    assert!(
        message.contains("across the 1 operation it covers"),
        "{:?}",
        message
    );
    assert!(message.contains("carry: error"), "{:?}", message);
    assert_eq!(*line, Some(line_containing(&sdl, "Widget not found")));
}

#[test]
fn a_body_that_cannot_say_what_it_carries_counts_as_resolving() {
    let sdl = errpath_sdl(
        &errors_arg("  ", "$($.detail ?? 'Widget Co request failed')", &[]),
        "",
    );
    for body in [
        json!({"type": "object"}),
        json!({"type": "object", "additionalProperties": true}),
        json!({"type": "object", "properties": {"message": {"type": "string"}}, "additionalProperties": {"type": "string"}}),
        json!({"oneOf": [{"type": "object", "properties": {"message": {"type": "string"}}}]}),
        json!({"type": "object", "properties": {"message": {"type": "string"}}, "anyOf": [{"properties": {"detail": {"type": "string"}}}]}),
    ] {
        let inv = errpath_inventory(
            json!([err_ref("422", "ListWidgetsError422")]),
            json!([]),
            json!({"ListWidgetsError422": body}),
        );
        assert!(errpath_findings(&sdl, &inv).is_empty(), "{}", body);
    }
    // A closed `{ message }` body does fire on the same block.
    let closed = errpath_inventory(
        json!([err_ref("422", "ListWidgetsError422")]),
        json!([]),
        json!({"ListWidgetsError422": {"type": "object", "properties": {"message": {"type": "string"}}}}),
    );
    assert_eq!(errpath_findings(&sdl, &closed).len(), 1);
}

#[test]
fn paths_after_an_unreadable_step_or_inside_a_method_are_not_judged() {
    // `->map(…)` ends the path at `$.message`, and the `$.detail` inside its
    // argument is not the response body; `$.message->slice(0, 10)` reads
    // `$.message` alone.
    let sdl = errpath_sdl(
        &errors_arg(
            "  ",
            "$($.message->slice(0, 10) ?? 'Widget Co request failed')",
            &["tags: $.message->map($.detail)"],
        ),
        "",
    );
    assert!(errpath_findings(&sdl, &message_bodies()).is_empty());
}

// ─── undocumented-root-field, argument-constraints-undocumented (ADR 0062) ──

/// A one-operation schema. `@FIELDDOC@` is a whole line (empty, or the root
/// field's doc comment); `@ARGS@` is the argument list's interior; `@HTTP@`
/// the connector's `http` block interior.
const DOC_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
@FIELDDOC@  widget_co_listWidgets(@ARGS@): [Widget_Co_Widget]
    @connect(
      source: "widget_co"
      http: { @HTTP@ }
      selection: "id name"
    )
}
"#;

fn doc_sdl(field_doc: &str, args: &str, http: &str) -> String {
    DOC_SDL
        .replace("@FIELDDOC@", field_doc)
        .replace("@ARGS@", args)
        .replace("@HTTP@", http)
}

/// `get:/widgets/{widget-id}/parts` with the given summary, description and
/// parameters; `sort` is an enum query parameter unless `params` says
/// otherwise.
fn doc_inventory(summary: Option<&str>, description: Option<&str>, params: Value) -> Value {
    json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [{
            "key": "get:/widgets", "operation_id": "listWidgets", "method": "GET",
            "path": "/widgets/{widget-id}/parts",
            "summary": summary, "description": description,
            "semantics": "read", "provenance": "spec", "confidence": 1,
            "support": "supported", "support_reason": null,
            "parameters": params
        }],
        "shapes": {},
        "unresolved": []
    })
}

fn sort_param() -> Value {
    json!({"name": "sort", "in": "query", "required": false, "type": "string",
        "description": "Sort order", "enum": ["asc", "desc"], "default": "asc"})
}

fn doc_selection(description_override: Option<&str>) -> String {
    let mut sel = SELECTION.to_string();
    if let Some(d) = description_override {
        sel.push_str(&format!("      description: \"{}\"\n", d));
    }
    sel
}

fn doc_lint(sdl: &str, inv: &Value, selection: &str) -> LintResult {
    lint(vec![
        ("widget-co.graphql", s(sdl)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(inv)),
        ),
        (".factory/selection.yaml", s(selection)),
    ])
}

fn findings_of<'a>(
    result: &'a LintResult,
    rule: &str,
) -> Vec<&'a graphos_factory_core::lint::Finding> {
    result.findings.iter().filter(|f| f.rule == rule).collect()
}

const PLAIN_HTTP: &str = r#"GET: "/widgets""#;

#[test]
fn undocumented_root_field_is_an_error_carrying_the_summary() {
    let inv = doc_inventory(Some("List widgets"), Some("Every widget."), json!([]));
    let r = doc_lint(
        &doc_sdl("", "", PLAIN_HTTP).replace("()", ""),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "undocumented-root-field");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert_eq!(hits[0].severity, "error");
    assert!(
        hits[0].message.contains("\"\"\"List widgets\"\"\"")
            && hits[0].message.contains("Query.widget_co_listWidgets"),
        "{}",
        hits[0].message
    );
    assert_eq!(hits[0].line, Some(19), "{:?}", hits[0]);
}

/// Google discovery-derived specs carry no summaries: the description is
/// the text there is.
#[test]
fn undocumented_root_field_falls_back_to_the_description() {
    let inv = doc_inventory(None, Some("Lists the widgets."), json!([]));
    let r = doc_lint(
        &doc_sdl("", "", PLAIN_HTTP).replace("()", ""),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "undocumented-root-field");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("\"\"\"Lists the widgets.\"\"\""),
        "{}",
        hits[0].message
    );
}

#[test]
fn undocumented_root_field_prefers_the_selection_override() {
    let inv = doc_inventory(Some("List widgets"), None, json!([]));
    let r = doc_lint(
        &doc_sdl("", "", PLAIN_HTTP).replace("()", ""),
        &inv,
        &doc_selection(Some("Every widget, one page at a time.")),
    );
    let hits = findings_of(&r, "undocumented-root-field");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0]
            .message
            .contains("Every widget, one page at a time.")
            && !hits[0].message.contains("List widgets"),
        "{}",
        hits[0].message
    );
}

#[test]
fn undocumented_root_field_quiet_when_the_field_has_a_doc_comment() {
    let inv = doc_inventory(Some("List widgets"), None, json!([]));
    for doc in [
        "  \"List widgets.\"\n",
        "  \"\"\"\n  List widgets.\n  \"\"\"\n",
    ] {
        let r = doc_lint(
            &doc_sdl(doc, "", PLAIN_HTTP).replace("()", ""),
            &inv,
            &doc_selection(None),
        );
        assert!(
            findings_of(&r, "undocumented-root-field").is_empty(),
            "{:?}",
            r.findings
        );
    }
}

#[test]
fn undocumented_root_field_quiet_when_the_source_has_no_text() {
    for (summary, description) in [(None, None), (Some("  "), Some(""))] {
        let inv = doc_inventory(summary, description, json!([]));
        let r = doc_lint(
            &doc_sdl("", "", PLAIN_HTTP).replace("()", ""),
            &inv,
            &doc_selection(None),
        );
        assert!(
            findings_of(&r, "undocumented-root-field").is_empty(),
            "{:?}",
            r.findings
        );
    }
}

/// An undocumented paginated root field is one finding, not two: the
/// content rules defer when there is no doc comment at all and there is
/// vendor text to give it. With no vendor text they still fire.
#[test]
fn list_completion_missing_defers_to_undocumented_root_field() {
    let mut inv = paginated_inventory(Some(25), Some(100), "integer");
    let sdl = PAGINATED_SDL_NO_DOC.replace("  \"List widgets.\"\n", "");
    assert_ne!(sdl, PAGINATED_SDL_NO_DOC, "the fixture's doc line moved");
    let run = |inv: &Value| {
        lint(vec![
            ("widget-co.graphql", s(&sdl)),
            (
                ".factory/inventory.json",
                s(&graphos_factory_core::json::pretty(inv)),
            ),
            (".factory/selection.yaml", s(&paginated_selection())),
        ])
    };
    let r = run(&inv);
    assert!(
        rules(&r).contains(&"list-completion-missing".to_string()),
        "{:?}",
        rules(&r)
    );
    assert!(
        !rules(&r).contains(&"undocumented-root-field".to_string()),
        "{:?}",
        rules(&r)
    );
    inv["operations"][0]["summary"] = json!("List widgets");
    let r = run(&inv);
    assert!(
        rules(&r).contains(&"undocumented-root-field".to_string()),
        "{:?}",
        rules(&r)
    );
    assert!(
        !rules(&r).contains(&"list-completion-missing".to_string()),
        "{:?}",
        rules(&r)
    );
}

#[test]
fn argument_constraints_undocumented_is_a_warning_carrying_the_clause() {
    let inv = doc_inventory(Some("List widgets"), None, json!([sort_param()]));
    let r = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "sort: String",
            r#"GET: "/widgets", queryParams: "sort: $args.sort""#,
        ),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert_eq!(hits[0].severity, "warn");
    assert!(
        hits[0].message.contains("(default asc, one of asc|desc)")
            && hits[0].message.contains("argument `sort`")
            && !hits[0].message.contains("Sort order"),
        "{}",
        hits[0].message
    );
}

#[test]
fn argument_constraints_quiet_when_the_argument_has_a_doc_comment() {
    let inv = doc_inventory(Some("List widgets"), None, json!([sort_param()]));
    let http = r#"GET: "/widgets", queryParams: "sort: $args.sort""#;
    for args in [
        "\"Sort order (default asc, one of asc|desc).\" sort: String",
        "\n    \"\"\"Sort order (default asc, one of asc|desc).\"\"\"\n    sort: String\n  ",
    ] {
        let r = doc_lint(
            &doc_sdl("  \"List widgets.\"\n", args, http),
            &inv,
            &doc_selection(None),
        );
        assert!(
            findings_of(&r, "argument-constraints-undocumented").is_empty(),
            "{:?}",
            r.findings
        );
    }
}

/// The source sentence is opt-in (ADR 0040): a described parameter with no
/// constraint gets no finding.
#[test]
fn argument_constraints_quiet_on_an_unconstrained_parameter() {
    let param = json!({"name": "q", "in": "query", "required": false, "type": "string",
        "description": "Free-text search"});
    let inv = doc_inventory(Some("List widgets"), None, json!([param]));
    let r = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "q: String",
            r#"GET: "/widgets", queryParams: "q: $args.q""#,
        ),
        &inv,
        &doc_selection(None),
    );
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
}

/// `{$args.widgetId}` aligns with the inventory path's `{widget-id}`, so a
/// renamed path argument still finds its parameter.
#[test]
fn argument_constraints_reads_a_renamed_path_parameter() {
    let param = json!({"name": "widget-id", "in": "path", "required": true, "type": "integer",
        "minimum": 1, "maximum": 99});
    let inv = doc_inventory(Some("List widgets"), None, json!([param]));
    let r = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "widgetId: Int!",
            r#"GET: "/widgets/{$args.widgetId}/parts""#,
        ),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("path parameter `widget-id`")
            && hits[0].message.contains("(min 1, max 99)"),
        "{}",
        hits[0].message
    );
}

/// The page-size parameter is `pagination-bounds-*`'s: one rule per slot.
#[test]
fn argument_constraints_skips_the_page_size_parameter() {
    let inv = paginated_inventory(Some(25), Some(100), "integer");
    let r = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_NO_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        rules(&r).contains(&"pagination-bounds-undocumented".to_string()),
        "the fixture no longer exercises the page-size argument: {:?}",
        rules(&r)
    );
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
}

#[test]
fn argument_constraints_reads_a_flat_body_key() {
    // ADR 0062: an argument is traced through a flat `body` key to the
    // request shape's property, as well as through queryParams and paths.
    let inv = json!({
        "contract_version": 1,
        "api": {"title": "Widget Co", "base_urls": ["https://api.widgets.test"]},
        "operations": [{
            "key": "post:/widgets", "operation_id": "listWidgets", "method": "POST",
            "path": "/widgets", "summary": "List widgets",
            "semantics": "read", "provenance": "spec", "confidence": 1,
            "support": "supported", "support_reason": null,
            "parameters": [],
            "request_body": {"content_type": "application/json", "required": true, "shape_ref": "#/shapes/WidgetQuery"}
        }],
        "shapes": {
            "WidgetQuery": {"type": "object", "properties": {
                "color": {"type": "string", "enum": ["red", "blue"], "default": "red"}
            }}
        },
        "unresolved": []
    });
    let r = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "color: String",
            r#"POST: "/widgets", body: "color: $args.color""#,
        ),
        &inv,
        &SELECTION.replace("\"get:/widgets\"", "\"post:/widgets\""),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("body key `color`")
            && hits[0].message.contains("(default red, one of red|blue)"),
        "{}",
        hits[0].message
    );
}

#[test]
fn copy_state_undocumented_defers_to_undocumented_root_field() {
    // A copy-named mutation with no field doc but a vendor summary: the
    // missing comment is one finding, undocumented-root-field, not two.
    let sdl = COPY_SDL_NO_DOC.replace("  \"Copy a note to a new location.\"\n", "");
    assert_ne!(sdl, COPY_SDL_NO_DOC);
    let mut inv = copy_inventory();
    inv["operations"][0]["summary"] = json!("Copy a note");
    let result = lint(vec![
        ("widget-co.graphql", s(&sdl)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&copy_selection())),
    ]);
    let rules = rules(&result);
    assert!(
        rules.contains(&"undocumented-root-field".to_string()),
        "{:?}",
        rules
    );
    assert!(
        !rules.contains(&"copy-state-undocumented".to_string()),
        "{:?}",
        rules
    );
}

// ADR 0069 — links: in selection.yaml. Lint checks every reference a link
// makes against the inventory and warns while the entry is a draft; it
// never reads the schema for this — the judgement is checked where it is
// written.

fn link_inventory() -> Value {
    let mut inv = inventory();
    // The listing returns Widgets, so a schema that applies it gives a
    // `Widget > …` link its host type (`reconcile::link_hosts`).
    inv["operations"][0]["response"] = json!({"status": "200", "content_type": "application/json", "shape_ref": "#/shapes/WidgetList"});
    inv["operations"].as_array_mut().unwrap().push(json!({
        "key": "get:/owners/{ownerId}", "operation_id": "getOwner", "method": "GET", "path": "/owners/{ownerId}",
        "semantics": "read", "provenance": "spec", "confidence": 1,
        "parameters": [{"name": "ownerId", "in": "path", "required": true, "type": "string"}],
        "response": {"status": "200", "content_type": "application/json", "shape_ref": "#/shapes/Owner"},
        "support": "supported", "support_reason": null
    }));
    inv["shapes"] = json!({
        "WidgetList": {"type": "array", "items": {"$ref": "#/shapes/Widget"}},
        "Widget": {"type": "object", "properties": {
            "id": {"type": "string"}, "name": {"type": "string"}, "owner_id": {"type": "string",
                "candidate_entity_link": {"operation": "get:/owners/{ownerId}", "parameter": "ownerId", "list_context": false}}}},
        "Owner": {"type": "object", "properties": {"id": {"type": "string"}, "name": {"type": "string"}}}
    });
    inv
}

/// The default schema with the listing's Widget carrying its foreign key and
/// a nested `maker`: every `Widget > …` link below has a host type, as it
/// does once the operation returning the shape is applied.
const LINK_HOST_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Maker {
  id: ID
  ownerId: ID
}

type Widget_Co_Widget {
  id: ID
  name: String
  ownerId: ID
  maker: Widget_Co_Maker
}

type Query {
  widget_co_listWidgets(limit: Int): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id name ownerId: owner_id maker { id ownerId: owner_id }")
  widget_co_owner(ownerId: ID!): Widget_Co_Owner
    @connect(source: "widget_co", http: { GET: "/owners/{$args.ownerId}" }, selection: "id name")
}

type Widget_Co_Owner {
  id: ID
  name: String
}
"#;

const LINK_SELECTION: &str = concat!(
    "contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\n",
    "operations:\n  \"get:/widgets\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: query\n      name: listWidgets\n",
    "  \"get:/owners/{ownerId}\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: query\n      name: owner\n",
    "links:\n  - shape: Widget\n    path: owner_id\n    operation: \"get:/owners/{ownerId}\"\n    parameter: ownerId\n    include: true\n    confirmed: false\n",
);

fn lint_links(selection: &str) -> Vec<String> {
    rules(&lint(vec![
        ("widget-co.graphql", s(LINK_HOST_SDL)),
        (".factory/selection.yaml", s(selection)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&link_inventory())),
        ),
    ]))
}

fn link_rules(selection: &str) -> Vec<String> {
    lint_links(selection)
        .into_iter()
        .filter(|r| r.starts_with("link-"))
        .collect()
}

#[test]
fn a_drafted_link_warns_until_it_is_confirmed() {
    assert_eq!(link_rules(LINK_SELECTION), vec!["link-unconfirmed"]);
    let confirmed = LINK_SELECTION.replace("    confirmed: false\n", "    confirmed: true\n");
    assert_eq!(link_rules(&confirmed), Vec::<String>::new());
    let silent = LINK_SELECTION.replace("    confirmed: false\n", "");
    assert_eq!(
        link_rules(&silent),
        Vec::<String>::new(),
        "absent means confirmed — a hand-written entry is the user's word"
    );
    // A declined draft is an answer, not a proposal: nothing to agree.
    let declined = LINK_SELECTION.replace(
        "    parameter: ownerId\n    include: true\n",
        "    parameter: ownerId\n    include: false\n",
    );
    assert_ne!(declined, LINK_SELECTION);
    assert_eq!(link_rules(&declined), Vec::<String>::new());
    // The message names the link, the field it would add and the operation.
    let r = lint(vec![
        (".factory/selection.yaml", s(LINK_SELECTION)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&link_inventory())),
        ),
    ]);
    let finding = r
        .findings
        .iter()
        .find(|f| f.rule == "link-unconfirmed")
        .unwrap();
    assert_eq!(finding.severity, "warn");
    assert_eq!(
        finding.message,
        "link Widget > owner_id is still the tool's draft (field owner via get:/owners/{ownerId}); agree it with the user, then set confirmed: true or drop the entry"
    );
    assert_eq!(finding.file.as_deref(), Some("selection.yaml"));
}

#[test]
fn a_link_naming_an_unknown_shape_operation_or_path_is_an_error() {
    let confirmed = LINK_SELECTION.replace("    confirmed: false\n", "");
    let cases = [
        (
            "  - shape: Widget\n",
            "  - shape: Nope\n",
            "link-unknown-shape",
        ),
        (
            "    operation: \"get:/owners/{ownerId}\"\n",
            "    operation: \"get:/nowhere/{id}\"\n",
            "link-unknown-operation",
        ),
        (
            "    path: owner_id\n",
            "    path: creator_id\n",
            "link-unknown-path",
        ),
    ];
    for (from, to, rule) in cases {
        let text = confirmed.replace(from, to);
        assert_ne!(text, confirmed);
        let r = lint(vec![
            (".factory/selection.yaml", s(&text)),
            (
                ".factory/inventory.json",
                s(&graphos_factory_core::json::pretty(&link_inventory())),
            ),
        ]);
        let finding = r
            .findings
            .iter()
            .find(|f| f.rule == rule)
            .unwrap_or_else(|| panic!("no {} in {:?}", rule, rules(&r)));
        assert_eq!(finding.severity, "error", "{}", rule);
        assert!(finding.message.starts_with("link "), "{}", finding.message);
    }
    // An unknown shape does not also report an unknown path: there is
    // nothing to resolve the path in.
    let nope = confirmed.replace("  - shape: Widget\n", "  - shape: Nope\n");
    assert_eq!(link_rules(&nope), vec!["link-unknown-shape"]);
}

#[test]
fn a_link_through_an_excluded_operation_is_an_error_unless_the_link_is_declined() {
    let confirmed = LINK_SELECTION.replace("    confirmed: false\n", "");
    let excluded = confirmed.replace(
        "  \"get:/owners/{ownerId}\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: query\n      name: owner\n",
        "  \"get:/owners/{ownerId}\":\n    include: false\n    reason: \"not yet\"\n",
    );
    assert_ne!(excluded, confirmed);
    assert_eq!(link_rules(&excluded), vec!["link-operation-excluded"]);
    // Declining the link too is consistent: nothing fires. (The op entry's
    // `include: true` line is gone in `excluded`, so this replace touches
    // only the link entry.)
    let declined = excluded.replace(
        "    parameter: ownerId\n    include: true\n",
        "    parameter: ownerId\n    include: false\n",
    );
    assert_ne!(declined, excluded);
    assert_eq!(link_rules(&declined), Vec::<String>::new());
}

#[test]
fn a_confirmed_link_no_root_field_reaches_warns_that_it_has_no_host() {
    // The by-id operation is included and applied, but the only operation
    // returning Widget is excluded and so never applied: no root field
    // returns a type that reaches the shape, and `links apply` would refuse
    // the entry `no-host` (R42) while reconcile stays clean. Gitea's drafted
    // `Organization > username` is this case (reached only through
    // `Repository.repo_transfer`, which the selection excludes).
    let owner_only = LINK_HOST_SDL.replace(
        "  widget_co_listWidgets(limit: Int): [Widget_Co_Widget]\n    @connect(source: \"widget_co\", http: { GET: \"/widgets\" }, selection: \"id name ownerId: owner_id maker { id ownerId: owner_id }\")\n",
        "",
    );
    assert_ne!(owner_only, LINK_HOST_SDL, "fixture drifted");
    let confirmed = LINK_SELECTION.replace("    confirmed: false\n", "    confirmed: true\n");
    let excluded = confirmed.replace(
        "  \"get:/widgets\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: query\n      name: listWidgets\n",
        "  \"get:/widgets\":\n    include: false\n    reason: \"not yet\"\n",
    );
    assert_ne!(excluded, confirmed, "fixture drifted");
    let lint_with = |sdl: &str, selection: &str| {
        lint(vec![
            ("widget-co.graphql", s(sdl)),
            (".factory/selection.yaml", s(selection)),
            (
                ".factory/inventory.json",
                s(&graphos_factory_core::json::pretty(&link_inventory())),
            ),
        ])
    };
    let link_only = |r: &LintResult| -> Vec<String> {
        rules(r)
            .into_iter()
            .filter(|r| r.starts_with("link-"))
            .collect()
    };
    let r = lint_with(&owner_only, &excluded);
    assert_eq!(link_only(&r), vec!["link-no-host"]);
    let finding = r
        .findings
        .iter()
        .find(|f| f.rule == "link-no-host")
        .unwrap();
    assert_eq!(finding.severity, "warn");
    assert_eq!(
        finding.message,
        "link Widget > owner_id: no root field returns a type that reaches this shape, so the link has no host; apply an operation returning the shape or decline the link (include: false)"
    );
    assert_eq!(finding.file.as_deref(), Some("selection.yaml"));

    // Applying the listing gives the link its host: silent.
    assert_eq!(
        link_only(&lint_with(LINK_HOST_SDL, &confirmed)),
        Vec::<String>::new()
    );
    // A draft is `link-unconfirmed` alone; a declined entry is silent.
    let draft = excluded.replace("    confirmed: true\n", "    confirmed: false\n");
    assert_ne!(draft, excluded);
    assert_eq!(
        link_only(&lint_with(&owner_only, &draft)),
        vec!["link-unconfirmed"]
    );
    let declined = excluded.replace(
        "    parameter: ownerId\n    include: true\n",
        "    parameter: ownerId\n    include: false\n",
    );
    assert_ne!(declined, excluded);
    assert_eq!(
        link_only(&lint_with(&owner_only, &declined)),
        Vec::<String>::new()
    );
    // A reference problem is the error and derives no host to miss.
    let by_id_excluded = excluded.replace(
        "  \"get:/owners/{ownerId}\":\n    include: true\n    response:\n      envelope: null\n    graphql:\n      root: query\n      name: owner\n",
        "  \"get:/owners/{ownerId}\":\n    include: false\n    reason: \"not yet\"\n",
    );
    assert_ne!(by_id_excluded, excluded);
    assert_eq!(
        link_only(&lint_with(&owner_only, &by_id_excluded)),
        vec!["link-operation-excluded"]
    );
}

#[test]
fn two_links_resolving_to_the_same_field_on_a_shape_are_an_error() {
    let confirmed = LINK_SELECTION.replace("    confirmed: false\n", "");
    let twice = format!(
        "{}  - shape: Widget\n    path: id\n    operation: \"get:/owners/{{ownerId}}\"\n    include: true\n",
        confirmed
    );
    let r = lint(vec![
        (".factory/selection.yaml", s(&twice)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&link_inventory())),
        ),
    ]);
    let finding = r
        .findings
        .iter()
        .find(|f| f.rule == "link-duplicate-field")
        .unwrap_or_else(|| panic!("{:?}", rules(&r)));
    assert_eq!(
        finding.message,
        "links Widget > owner_id and Widget > id both resolve to field owner on shape Widget"
    );
    // Naming the second field differently resolves it.
    let named = twice.replace(
        "    path: id\n    operation: \"get:/owners/{ownerId}\"\n    include: true\n",
        "    path: id\n    operation: \"get:/owners/{ownerId}\"\n    field: creator\n    include: true\n",
    );
    assert!(!link_rules(&named).contains(&"link-duplicate-field".to_string()));
    // Declared or derived, the name is what collides: an explicit `field:`
    // equal to another link's derived name is the same field.
    let declared = format!(
        "{}  - shape: Widget\n    path: name\n    operation: \"get:/widgets\"\n    field: owner\n    include: true\n",
        confirmed
    );
    let r = lint(vec![
        (".factory/selection.yaml", s(&declared)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&link_inventory())),
        ),
    ]);
    let finding = r
        .findings
        .iter()
        .find(|f| f.rule == "link-duplicate-field")
        .unwrap_or_else(|| panic!("{:?}", rules(&r)));
    assert_eq!(
        finding.message,
        "links Widget > owner_id and Widget > name both resolve to field owner on shape Widget"
    );
    // The host is the object that carries the fk: the same name on the
    // shape's root and on a nested object is two fields on two types, while
    // two fks on the one nested object collide there.
    let mut nested_inventory = link_inventory();
    nested_inventory["shapes"]["Widget"]["properties"]["maker"] = json!({"type": "object", "properties": {
        "id": {"type": "string"}, "owner_id": {"type": "string",
            "candidate_entity_link": {"operation": "get:/owners/{ownerId}", "parameter": "ownerId", "list_context": false}}}});
    let lint_nested = |text: &str| {
        lint(vec![
            ("widget-co.graphql", s(LINK_HOST_SDL)),
            (".factory/selection.yaml", s(text)),
            (
                ".factory/inventory.json",
                s(&graphos_factory_core::json::pretty(&nested_inventory)),
            ),
        ])
    };
    let nested = format!(
        "{}  - shape: Widget\n    path: maker>owner_id\n    operation: \"get:/owners/{{ownerId}}\"\n    include: true\n",
        confirmed
    );
    let link_findings: Vec<String> = rules(&lint_nested(&nested))
        .into_iter()
        .filter(|r| r.starts_with("link-"))
        .collect();
    assert_eq!(link_findings, Vec::<String>::new());
    let both = format!(
        "{}  - shape: Widget\n    path: maker>id\n    operation: \"get:/owners/{{ownerId}}\"\n    include: true\n",
        nested
    );
    let r = lint_nested(&both);
    let finding = r
        .findings
        .iter()
        .find(|f| f.rule == "link-duplicate-field")
        .unwrap_or_else(|| panic!("{:?}", rules(&r)));
    assert_eq!(
        finding.message,
        "links Widget > maker>owner_id and Widget > maker>id both resolve to field owner on shape Widget at maker"
    );
}

// ─── ADR 0069 — a relationship field mirrors its by-id root's credential ─────

/// Case 1 (source-level auth): the @source carries `{{AUTH_EXPR}}` and no
/// root field reaching `/owners/{…}` carries a per-call credential, so the
/// field's own header and argument are the error.
const LINK_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

type Widget_Co_Owner {
  id: ID
}

type Widget_Co_Widget {
  id: ID
  name: String
  "The wire name, kept: it is the foreign key `owner` reads."
  owner_id: ID
  owner(access_token: String!): Widget_Co_Owner
    @connect(source: "widget_co", http: { GET: "/owners/{$this.owner_id}", headers: [{ name: "Authorization", value: "Bearer {$args.access_token}" }] }, selection: "id")
}

type Query {
  widget_co_listWidgets(limit: Int): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id name owner_id")
}
"#;

#[test]
fn a_relationship_field_carrying_its_own_credential_is_an_error() {
    let result = lint(vec![("widget-co.graphql", s(LINK_SDL))]);
    let finding = result
        .findings
        .iter()
        .find(|f| f.rule == "link-credential")
        .unwrap_or_else(|| panic!("{:?}", rules(&result)));
    // No root field in LINK_SDL reaches /owners/{…}, so the message makes no
    // claim about the @source (R44); with one, it names {{AUTH_EXPR}}
    // (`the_source_level_message_names_the_credential_path_the_field_opens`).
    assert_eq!(
        finding.message,
        "Widget_Co_Widget.owner carries its own credential (an Authorization header, {$args.access_token} in a header); no root field reaches GET /owners/{$this.owner_id} for it to mirror, so it has no per-call credential to carry — drop the header and its argument"
    );
    // FieldSpan.line is the line of the field name (`owner(access_token…`,
    // line 22), not of its @connect on line 23.
    assert_eq!(finding.line, Some(22), "{:?}", finding);

    // The token moved out of the header and into the query string is still
    // a credential the field carries.
    let query = LINK_SDL
        .replace(
            ", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }]",
            "",
        )
        .replace(
            "/owners/{$this.owner_id}\"",
            "/owners/{$this.owner_id}?access_token={$args.access_token}\"",
        );
    assert!(rules(&lint(vec![("widget-co.graphql", s(&query))]))
        .contains(&"link-credential".to_string()));

    // The same field with the credential left to @source: no finding.
    let clean = LINK_SDL
        .replace(
            ", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }]",
            "",
        )
        .replace(
            "owner(access_token: String!): Widget_Co_Owner",
            "owner: Widget_Co_Owner",
        );
    let result = lint(vec![("widget-co.graphql", s(&clean))]);
    assert!(
        !rules(&result).contains(&"link-credential".to_string()),
        "{:?}",
        rules(&result)
    );
}

#[test]
fn a_target_resolved_by_both_a_relationship_field_and_an_entity_connector_is_a_warning() {
    let both = LINK_SDL
        .replace(", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }]", "")
        .replace("owner(access_token: String!): Widget_Co_Owner", "owner: Widget_Co_Owner")
        .replace(
            "type Widget_Co_Owner {\n",
            "type Widget_Co_Owner @key(fields: \"id\") @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.id}\" }, selection: \"id\") {\n",
        );
    let result = lint(vec![("widget-co.graphql", s(&both))]);
    let finding = result
        .findings
        .iter()
        .find(|f| f.rule == "link-key-drift")
        .unwrap_or_else(|| panic!("{:?}", rules(&result)));
    assert_eq!(finding.severity, "warn");
    // TypeConnector.line is decl.line — the 1-based line of the `type`
    // keyword; the replaced `type Widget_Co_Owner …{` sits on line 13.
    assert!(
        finding.message.starts_with(
            "Widget_Co_Widget.owner resolves GET /owners/{$this.owner_id} through a field-level connector while type Widget_Co_Owner resolves the same operation through @key and a type-level @connect (line 13)"
        ),
        "{}",
        finding.message
    );
    assert!(!rules(&result).contains(&"link-credential".to_string()));
    // The field alone draws no such warning.
    let field_only = LINK_SDL
        .replace(
            ", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }]",
            "",
        )
        .replace(
            "owner(access_token: String!): Widget_Co_Owner",
            "owner: Widget_Co_Owner",
        );
    let result = lint(vec![("widget-co.graphql", s(&field_only))]);
    assert!(
        !rules(&result).contains(&"link-key-drift".to_string()),
        "{:?}",
        rules(&result)
    );
}

/// Case 2 (per-call auth, every AppWorld service): no credential on the
/// @source; every root field takes `access_token: String!` and sends it as
/// `Authorization: Bearer {$args.access_token}`, and the relationship field
/// mirrors the by-id root field `widget_co_owner` exactly.
const PER_CALL_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(name: "widget_co", http: { baseURL: "{{BASE_URL}}" })

type Widget_Co_Owner {
  id: ID
}

type Widget_Co_Widget {
  id: ID
  name: String
  "The wire name, kept: it is the foreign key `owner` reads."
  owner_id: ID
  owner(access_token: String!): Widget_Co_Owner
    @connect(source: "widget_co", http: { GET: "/owners/{$this.owner_id}", headers: [{ name: "Authorization", value: "Bearer {$args.access_token}" }] }, selection: "id")
}

type Query {
  widget_co_listWidgets(access_token: String!): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets", headers: [{ name: "Authorization", value: "Bearer {$args.access_token}" }] }, selection: "id name owner_id")
  widget_co_owner(ownerId: ID!, access_token: String!): Widget_Co_Owner
    @connect(source: "widget_co", http: { GET: "/owners/{$args.ownerId}", headers: [{ name: "Authorization", value: "Bearer {$args.access_token}" }] }, selection: "id")
}
"#;

/// The link field's declaration and `http` block in PER_CALL_SDL, which
/// each variant below edits (the root fields' identical header stays put).
const PER_CALL_FIELD: &str = "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }";

#[test]
fn a_relationship_field_mirrors_its_by_id_root_fields_per_call_credential() {
    assert!(PER_CALL_SDL.contains(PER_CALL_FIELD), "fixture drifted");
    let variant = |field: &str| {
        let sdl = PER_CALL_SDL.replace(PER_CALL_FIELD, field);
        assert_ne!(sdl, PER_CALL_SDL, "the variant must edit the field");
        lint(vec![("widget-co.graphql", s(&sdl))])
    };

    // Mirrored — the harness's form: same argument, same header. No finding.
    let result = lint(vec![("widget-co.graphql", s(PER_CALL_SDL))]);
    assert!(
        !rules(&result).contains(&"link-credential".to_string()),
        "{:?}",
        rules(&result)
    );

    // Missing: the field drops the argument and its header, so the router
    // would call /owners/{id} unauthenticated. The error names both sides.
    let result = variant("  owner: Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\" }");
    let finding = result
        .findings
        .iter()
        .find(|f| f.rule == "link-credential")
        .unwrap_or_else(|| panic!("{:?}", rules(&result)));
    assert_eq!(
        finding.message,
        "Widget_Co_Widget.owner must carry exactly the credential its by-id root field Query.widget_co_owner carries (access_token: String! as header Authorization: Bearer {$args.access_token}); it carries none — declare the same argument and send it the same way"
    );
    // `owner…` is line 16 of PER_CALL_SDL, the field name's line.
    assert_eq!(finding.line, Some(16), "{:?}", finding);

    // Renamed, retyped (nullable), and sent through another slot: each differs.
    for field in [
        "  owner(token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.token}\" }] }",
        "  owner(access_token: String): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }",
        "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?access_token={$args.access_token}\" }",
    ] {
        let result = variant(field);
        assert!(
            rules(&result).contains(&"link-credential".to_string()),
            "{}: {:?}",
            field,
            rules(&result)
        );
    }

    // The same mirrored field under a by-id root field that authenticates
    // through the @source alone is Case 1: its header is now the error, and
    // so is the argument on its own once the header is gone.
    let source_level = PER_CALL_SDL.replace(
        "  widget_co_owner(ownerId: ID!, access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$args.ownerId}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }, selection: \"id\")",
        "  widget_co_owner(ownerId: ID!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$args.ownerId}\" }, selection: \"id\")",
    );
    assert_ne!(
        source_level, PER_CALL_SDL,
        "the by-id root field must lose its credential"
    );
    let result = lint(vec![("widget-co.graphql", s(&source_level))]);
    let finding = result
        .findings
        .iter()
        .find(|f| f.rule == "link-credential")
        .unwrap_or_else(|| panic!("{:?}", rules(&result)));
    assert!(
        finding.message.starts_with("Widget_Co_Widget.owner carries its own credential (an Authorization header, {$args.access_token} in a header)"),
        "{}",
        finding.message
    );
    let bare_argument = source_level.replace(
        PER_CALL_FIELD,
        "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\" }",
    );
    assert_ne!(bare_argument, source_level);
    let result = lint(vec![("widget-co.graphql", s(&bare_argument))]);
    let finding = result
        .findings
        .iter()
        .find(|f| f.rule == "link-credential")
        .unwrap_or_else(|| panic!("{:?}", rules(&result)));
    assert!(
        finding.message.starts_with(
            "Widget_Co_Widget.owner carries its own credential (argument access_token: String!)"
        ),
        "{}",
        finding.message
    );
}

/// PER_CALL_SDL with the credential optional and sent through `value`,
/// an expression over the argument rather than a bare `{$args.<a>}`.
fn per_call_expression_sdl(value: &str) -> String {
    let sdl = PER_CALL_SDL
        .replace("access_token: String!", "access_token: String")
        .replace("\"Bearer {$args.access_token}\"", &format!("\"{}\"", value));
    assert!(
        !sdl.contains("Bearer {$args.access_token}"),
        "fixture drifted"
    );
    sdl
}

#[test]
fn a_credential_sent_through_an_expression_is_mirrored_like_a_bare_one() {
    use graphos_factory_core::lint::{mirrored_credential, CallCredential, CredentialSlot};
    for value in [
        // AppWorld Spotify's null-preserving form: no header when absent.
        "{$args.access_token->match([null, null], [@, $(['Bearer', @])->joinNotNull(' ')])}",
        // The argument inside the expression rather than at its head.
        "{$(['Bearer', $args.access_token])->joinNotNull(' ')}",
    ] {
        let sdl = per_call_expression_sdl(value);
        assert_eq!(
            mirrored_credential(&sdl, Some("GET"), Some("/owners/{$this.owner_id}")),
            Some((
                "widget_co_owner".to_string(),
                vec![CallCredential {
                    arg: "access_token".to_string(),
                    arg_type: "String".to_string(),
                    slot: CredentialSlot::Header {
                        name: "Authorization".to_string(),
                        value: value.to_string(),
                    },
                }]
            )),
            "{}",
            value
        );

        // Mirrored exactly: no finding.
        assert!(
            link_credential(&sdl).is_none(),
            "{}: {:?}",
            value,
            link_credential(&sdl)
        );

        // Missing: the field drops the argument and its header. Case 2 still
        // applies, so the error names what the root carries.
        let field = format!(
            "  owner(access_token: String): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: {{ GET: \"/owners/{{$this.owner_id}}\", headers: [{{ name: \"Authorization\", value: \"{}\" }}] }}",
            value
        );
        assert!(sdl.contains(&field), "fixture drifted: {}", value);
        let missing = sdl.replace(
            &field,
            "  owner: Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\" }",
        );
        let finding = link_credential(&missing).unwrap_or_else(|| panic!("{}", value));
        assert_eq!(
            finding.message,
            format!(
                "Widget_Co_Widget.owner must carry exactly the credential its by-id root field Query.widget_co_owner carries (access_token: String as header Authorization: {}); it carries none — declare the same argument and send it the same way",
                value
            )
        );

        // A bare `Bearer {$args.access_token}` on the field is a different
        // value template, so it does not mirror the expression.
        let bare = sdl.replace(
            &field,
            "  owner(access_token: String): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }",
        );
        assert!(link_credential(&bare).is_some(), "{}", value);
    }
}

/// LINK_SDL (Case 1: the @source is the credential) with the relationship
/// field's declaration and `http` block replaced; `http` ends with the
/// block's closing `}`.
fn case_1_field(decl: &str, http: &str) -> String {
    let field = "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }";
    assert!(LINK_SDL.contains(field), "fixture drifted");
    LINK_SDL.replace(
        field,
        &format!(
            "  {}: Widget_Co_Owner\n    @connect(source: \"widget_co\", http: {{ {}",
            decl, http
        ),
    )
}

fn credential_message(sdl: &str) -> String {
    link_credential(sdl)
        .unwrap_or_else(|| panic!("no link-credential for:\n{}", sdl))
        .message
}

#[test]
fn every_link_credential_reader_sees_an_argument_inside_an_expression() {
    use graphos_factory_core::lint::{mirrored_credential, CallCredential, CredentialSlot};

    // call_credentials' URI query loop: the root sends the credential as a
    // query value that is an expression.
    let root = "GET: \"/owners/{$args.ownerId}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }, selection: \"id\")";
    let field = "GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }";
    let query = "?access_token={$args.access_token->match([null, null], [@, @])}";
    let sdl = PER_CALL_SDL
        .replace(
            root,
            &format!(
                "GET: \"/owners/{{$args.ownerId}}{}\" }}, selection: \"id\")",
                query
            ),
        )
        .replace(
            field,
            &format!("GET: \"/owners/{{$this.owner_id}}{}\" }}", query),
        );
    assert_eq!(sdl.matches(query).count(), 2, "fixture drifted");
    assert_eq!(
        mirrored_credential(&sdl, Some("GET"), Some("/owners/{$this.owner_id}")),
        Some((
            "widget_co_owner".to_string(),
            vec![CallCredential {
                arg: "access_token".to_string(),
                arg_type: "String!".to_string(),
                slot: CredentialSlot::Query {
                    param: "access_token".to_string()
                },
            }]
        ))
    );
    assert!(
        link_credential(&sdl).is_none(),
        "{:?}",
        link_credential(&sdl)
    );
    let dropped = sdl.replace(
        &format!("  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: {{ GET: \"/owners/{{$this.owner_id}}{}\" }}", query),
        "  owner: Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\" }",
    );
    assert_ne!(dropped, sdl, "fixture drifted");
    assert!(credential_message(&dropped).contains("it carries none"));

    // Case 1's scan of what the field sends: an expression in a header is
    // the field's own credential, whatever the argument is called.
    let sdl = case_1_field(
        "owner(trace: String)",
        "GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"X-Trace\", value: \"{$args.trace->match([null, null], [@, @])}\" }] }",
    );
    assert!(
        credential_message(&sdl).starts_with(
            "Widget_Co_Widget.owner carries its own credential ({$args.trace} in a header); "
        ),
        "{}",
        credential_message(&sdl)
    );

    // Case 1's "declared and never sent" check: a credential-named argument
    // the field sends through an expression is not also "declared".
    let sdl = case_1_field(
        "owner(api_token: String)",
        "GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"X-Key\", value: \"{$args.api_token->match([null, null], [@, @])}\" }] }",
    );
    assert!(
        credential_message(&sdl).starts_with(
            "Widget_Co_Widget.owner carries its own credential ({$args.api_token} in a header); "
        ),
        "{}",
        credential_message(&sdl)
    );

    // Case 2's extra-argument scan and its "sent nowhere" check: an
    // argument the field sends through an expression is in the request.
    let sdl = PER_CALL_SDL.replace(
        PER_CALL_FIELD,
        "  owner(access_token: String!, verbose: Boolean): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?verbose={$args.verbose->match([null, 'n'], [@, @])}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }",
    );
    assert_ne!(sdl, PER_CALL_SDL, "fixture drifted");
    let message = credential_message(&sdl);
    assert!(
        message.contains("{$args.verbose} in the request"),
        "{}",
        message
    );
    assert!(!message.contains("sent nowhere"), "{}", message);
}

#[test]
fn the_expression_reader_honours_quotes_escapes_and_repeats() {
    use graphos_factory_core::lint::mirrored_credential;

    // A `}` inside a '…' literal closes nothing: the argument after it is
    // still inside the interpolation.
    let sdl = case_1_field(
        "owner(trace: String)",
        "GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"X-Trace\", value: \"{$(['}', $args.trace])->joinNotNull(' ')}\" }] }",
    );
    assert!(
        credential_message(&sdl).starts_with(
            "Widget_Co_Widget.owner carries its own credential ({$args.trace} in a header); "
        ),
        "{}",
        credential_message(&sdl)
    );

    // An apostrophe inside an escaped \"…\" literal opens no '…' literal, so
    // the interpolation closes at its own `}` and the header after it is
    // still read as a header.
    let sdl = case_1_field(
        "owner(tenant: String!)",
        "GET: \"/owners/{$this.owner_id}?v={$this.owner_id->match([\\\"it's\\\", 'y'], [@, @])}\", headers: [{ name: \"X-Tenant\", value: \"{$args.tenant}\" }] }",
    );
    assert!(
        credential_message(&sdl).starts_with(
            "Widget_Co_Widget.owner carries its own credential ({$args.tenant} in a header); "
        ),
        "{}",
        credential_message(&sdl)
    );

    // A `}` inside an escaped \"…\" literal closes nothing either.
    let sdl = case_1_field(
        "owner(trace: String)",
        "GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"X-Trace\", value: \"{$([\\\"}\\\", $args.trace])->joinNotNull(' ')}\" }] }",
    );
    assert!(
        credential_message(&sdl).starts_with(
            "Widget_Co_Widget.owner carries its own credential ({$args.trace} in a header); "
        ),
        "{}",
        credential_message(&sdl)
    );

    // One argument read twice in one interpolation is one credential.
    let value = "{$args.access_token->match([null, $args.access_token], [@, @])}";
    let sdl = per_call_expression_sdl(value);
    let (_, credential) =
        mirrored_credential(&sdl, Some("GET"), Some("/owners/{$this.owner_id}")).unwrap();
    assert_eq!(credential.len(), 1, "{:?}", credential);
    assert!(
        link_credential(&sdl).is_none(),
        "{:?}",
        link_credential(&sdl)
    );
}

// ─── ADR 0069 fix round 1: credential names, header order, sub-resources ────

fn link_credential(sdl: &str) -> Option<graphos_factory_core::lint::Finding> {
    lint(vec![("widget-co.graphql", s(sdl))])
        .findings
        .into_iter()
        .find(|f| f.rule == "link-credential")
}

/// LINK_SDL with the relationship field left to the @source (no argument,
/// no header), and a by-id root field on `/owners/{…}` whose query string
/// sends `?<param>={$args.<param>}`.
fn by_id_root_with_query(param: &str) -> String {
    LINK_SDL
        .replace(
            ", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }]",
            "",
        )
        .replace(
            "owner(access_token: String!): Widget_Co_Owner",
            "owner: Widget_Co_Owner",
        )
        .replace(
            "type Query {\n",
            &format!(
                "type Query {{\n  widget_co_owner(ownerId: ID!, {p}: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: {{ GET: \"/owners/{{$args.ownerId}}?{p}={{$args.{p}}}\" }}, selection: \"id\")\n",
                p = param
            ),
        )
}

#[test]
fn names_a_credential_reads_whole_words_not_substrings() {
    use graphos_factory_core::lint::names_a_credential;
    for name in [
        "token",
        "access_token",
        "accessToken",
        "AccessToken",
        "auth_token",
        "api_token",
        "id_token",
        "bearer",
        "Authorization",
        "api_key",
        "apiKey",
        "APIKey",
        "apikey",
        "key",
        "secret",
        "password",
        "auth",
        "client_secret",
        "refresh_token",
        "session-token",
        "appKey",
        "api_secret_key",
        // Vendor spellings (R45).
        "private_token",
        "personal_access_token",
        "oauth_token",
        "x_api_key",
        "x-api-key",
        "subscription-key",
        "consumer_key",
        "user_token",
        "token_v2",
        "access_token_v2",
        "accessToken2",
        "accesstoken",
    ] {
        assert!(names_a_credential(name), "{} names a credential", name);
    }
    for name in [
        "pageToken",
        "nextToken",
        "next_page_token",
        "prevToken",
        "continuation_token",
        "sync_token",
        "max_tokens",
        "min_key",
        "cursor_token",
        "include_token_metadata",
        "token_type",
        "keyword",
        "monkey",
        "author",
        "secretary",
        "passwords_enabled",
        "limit",
        "ownerId",
        // A key that names a record, not a credential; a CSRF token (R45).
        "sort_key",
        "partition_key",
        "idempotency_key",
        "object_key",
        "csrf_token",
    ] {
        assert!(!names_a_credential(name), "{} names no credential", name);
    }
}

#[test]
fn a_paging_token_on_the_by_id_root_field_is_no_credential_to_mirror() {
    // The root field pages with a token-named parameter; the link field
    // carries no argument, as a field under source-level auth should.
    for param in [
        "pageToken",
        "nextToken",
        "continuation_token",
        "max_tokens",
        "include_token_metadata",
    ] {
        let sdl = by_id_root_with_query(param);
        assert!(
            sdl.contains(&format!("?{}={{$args.{}}}", param, param)),
            "{}",
            sdl
        );
        assert!(
            link_credential(&sdl).is_none(),
            "{}: {:?}",
            param,
            link_credential(&sdl)
        );
    }
    // A real per-call credential on the same root field: the link field
    // that lacks it is the error, naming the query slot.
    let finding = link_credential(&by_id_root_with_query("access_token"))
        .expect("the root sends access_token per call");
    assert_eq!(
        finding.message,
        "Widget_Co_Widget.owner must carry exactly the credential its by-id root field Query.widget_co_owner carries (access_token: String! as query parameter access_token); it carries none — declare the same argument and send it the same way"
    );
}

/// PER_CALL_SDL with both the by-id root field and the relationship field
/// sending the token as `?access_token={$args.access_token}` instead of a
/// header.
fn query_slot_sdl() -> String {
    let sdl = PER_CALL_SDL
        .replace(
            "http: { GET: \"/owners/{$args.ownerId}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }",
            "http: { GET: \"/owners/{$args.ownerId}?access_token={$args.access_token}\" }",
        )
        .replace(PER_CALL_FIELD, QUERY_SLOT_FIELD);
    assert!(
        sdl.contains(QUERY_SLOT_FIELD) && !sdl.contains(PER_CALL_FIELD),
        "fixture drifted"
    );
    sdl
}

const QUERY_SLOT_FIELD: &str = "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?access_token={$args.access_token}\" }";

#[test]
fn a_query_slot_credential_is_mirrored_exactly() {
    let sdl = query_slot_sdl();
    assert!(
        link_credential(&sdl).is_none(),
        "{:?}",
        link_credential(&sdl)
    );
    // Missing on the field: the error names the root field's query slot.
    let missing = sdl.replace(
        QUERY_SLOT_FIELD,
        "  owner: Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\" }",
    );
    assert!(link_credential(&missing)
        .expect("missing")
        .message
        .contains("(access_token: String! as query parameter access_token); it carries none"));
    // The same token moved to a header is another slot.
    let header = sdl.replace(
        QUERY_SLOT_FIELD,
        "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }",
    );
    assert!(link_credential(&header).is_some());
}

#[test]
fn a_mirrored_field_carries_nothing_beside_the_credential() {
    let sdl = query_slot_sdl();
    let with = |field: &str| {
        let variant = sdl.replace(QUERY_SLOT_FIELD, field);
        assert_ne!(variant, sdl, "the variant must edit the field");
        link_credential(&variant).map(|f| f.message)
    };
    // An Authorization header that reads no argument rides along.
    let message = with("  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?access_token={$args.access_token}\", headers: [{ name: \"Authorization\", value: \"Bearer static-token\" }] }")
        .expect("a static Authorization header is a second credential");
    assert!(
        message.contains("it carries access_token: String! as query parameter access_token, an Authorization header that reads no argument —"),
        "{}",
        message
    );
    // An argument the field never declares is interpolated into the request.
    let message = with("  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?access_token={$args.access_token}&region={$args.region}\" }")
        .expect("an undeclared {$args.region} is not the root's credential");
    assert!(
        message.contains("it carries access_token: String! as query parameter access_token, {$args.region} in the request —"),
        "{}",
        message
    );
    // A declared argument the connector sends nowhere.
    let message = with("  owner(access_token: String!, verbose: Boolean): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?access_token={$args.access_token}\" }")
        .expect("an unsent argument is more than the credential");
    assert!(
        message.contains("it carries access_token: String! as query parameter access_token, argument verbose: Boolean, sent nowhere —"),
        "{}",
        message
    );
    // A declared argument the request does interpolate is carried along, not
    // sent nowhere.
    let message = with("  owner(access_token: String!, region: String): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?access_token={$args.access_token}&region={$args.region}\" }")
        .expect("a declared, sent {$args.region} is still more than the credential");
    assert!(
        message.contains("it carries access_token: String! as query parameter access_token, {$args.region} in the request —"),
        "{}",
        message
    );
    assert!(!message.contains("sent nowhere"), "{}", message);
}

#[test]
fn a_header_entry_reads_its_name_and_value_in_either_order() {
    // The field writes `value` before `name`: the same entry, mirrored.
    let reversed_field = PER_CALL_SDL.replace(
        PER_CALL_FIELD,
        "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\", headers: [{ value: \"Bearer {$args.access_token}\", name: \"Authorization\" }] }",
    );
    assert_ne!(reversed_field, PER_CALL_SDL);
    assert!(
        link_credential(&reversed_field).is_none(),
        "{:?}",
        link_credential(&reversed_field)
    );
    // The root field writes it reversed and the field does not.
    let reversed_root = PER_CALL_SDL.replace(
        "http: { GET: \"/owners/{$args.ownerId}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }",
        "http: { GET: \"/owners/{$args.ownerId}\", headers: [{ value: \"Bearer {$args.access_token}\", name: \"Authorization\" }] }",
    );
    assert_ne!(reversed_root, PER_CALL_SDL);
    assert!(
        link_credential(&reversed_root).is_none(),
        "{:?}",
        link_credential(&reversed_root)
    );
    // Reversed and wrong is still wrong: another header name.
    let other_name = PER_CALL_SDL.replace(
        PER_CALL_FIELD,
        "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\", headers: [{ value: \"Bearer {$args.access_token}\", name: \"X-Token\" }] }",
    );
    assert!(link_credential(&other_name).is_some());
}

#[test]
fn the_source_level_message_names_the_credential_path_the_field_opens() {
    let own = "Widget_Co_Widget.owner carries its own credential (an Authorization header, {$args.access_token} in a header); ";
    // A by-id root field reaches the operation and the @source carries
    // {{AUTH_EXPR}}: the @source is the one credential.
    let with_root = LINK_SDL.replace(
        "type Query {\n",
        "type Query {\n  widget_co_owner(ownerId: ID!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$args.ownerId}\" }, selection: \"id\")\n",
    );
    assert_eq!(
        link_credential(&with_root).expect("with root").message,
        format!("{}the one @source supplies {{{{AUTH_EXPR}}}} to every request — drop the header and its argument", own)
    );
    // No root field reaches it: nothing to mirror, and no claim about the @source.
    assert_eq!(
        link_credential(LINK_SDL).expect("no root").message,
        format!("{}no root field reaches GET /owners/{{$this.owner_id}} for it to mirror, so it has no per-call credential to carry — drop the header and its argument", own)
    );
    // The by-id root field sends nothing and the @source carries nothing.
    let source_level = PER_CALL_SDL.replace(
        "  widget_co_owner(ownerId: ID!, access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$args.ownerId}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }, selection: \"id\")",
        "  widget_co_owner(ownerId: ID!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$args.ownerId}\" }, selection: \"id\")",
    );
    assert_ne!(source_level, PER_CALL_SDL);
    assert_eq!(
        link_credential(&source_level).expect("uncredentialed").message,
        format!("{}its by-id root field Query.widget_co_owner sends no credential and the @source carries none — drop the header and its argument", own)
    );
}

#[test]
fn a_source_level_field_sending_an_ordinary_argument_is_told_it_is_an_argument() {
    // Case 1: `{$args.expand}` is no credential, so the message must not
    // call it one (the fix is to drop an argument, not a credential path).
    let with_root = LINK_SDL.replace(
        "type Query {\n",
        "type Query {\n  widget_co_owner(ownerId: ID!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$args.ownerId}\" }, selection: \"id\")\n",
    );
    let field = "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }";
    assert!(with_root.contains(field), "fixture drifted");
    let expand = with_root.replace(
        field,
        "  owner(expand: String): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?expand={$args.expand}\" }",
    );
    assert_eq!(
        link_credential(&expand).expect("an argument").message,
        "Widget_Co_Widget.owner carries an argument its by-id root field does not ({$args.expand} in the request); a relationship field declares only the mirrored credential — drop the argument"
    );
    // The by-id root field taking the same argument does not make it the
    // field's, and the message does not claim the root lacks it.
    let root_takes = expand.replace(
        "  widget_co_owner(ownerId: ID!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$args.ownerId}\" }",
        "  widget_co_owner(ownerId: ID!, expand: String): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$args.ownerId}?expand={$args.expand}\" }",
    );
    assert_ne!(root_takes, expand);
    assert_eq!(
        link_credential(&root_takes).expect("still an argument").message,
        "Widget_Co_Widget.owner carries an argument that is no credential ({$args.expand} in the request); a relationship field declares only the mirrored credential — drop the argument"
    );
    // A credential-named query argument keeps the credential wording.
    let token = with_root.replace(
        field,
        "  owner(access_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?access_token={$args.access_token}\" }",
    );
    assert!(
        link_credential(&token)
            .expect("a credential")
            .message
            .starts_with("Widget_Co_Widget.owner carries its own credential ({$args.access_token} in the request); "),
        "{:?}",
        link_credential(&token)
    );
}

#[test]
fn mirrored_credential_names_the_first_by_id_root_field_that_carries_one() {
    use graphos_factory_core::lint::{mirrored_credential, CallCredential, CredentialSlot};
    // Two root fields reach /owners/{…}; the first sends nothing.
    let sdl = PER_CALL_SDL.replace(
        "  widget_co_owner(ownerId: ID!, access_token: String!)",
        "  widget_co_publicOwner(ownerId: ID!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$args.ownerId}\" }, selection: \"id\")\n  widget_co_owner(ownerId: ID!, access_token: String!)",
    );
    assert_ne!(sdl, PER_CALL_SDL);
    let want = CallCredential {
        arg: "access_token".to_string(),
        arg_type: "String!".to_string(),
        slot: CredentialSlot::Header {
            name: "Authorization".to_string(),
            value: "Bearer {$args.access_token}".to_string(),
        },
    };
    assert_eq!(
        mirrored_credential(&sdl, Some("GET"), Some("/owners/{$this.owner_id}")),
        Some(("widget_co_owner".to_string(), vec![want.clone()]))
    );
    assert_eq!(
        want.describe(),
        "access_token: String! as header Authorization: Bearer {$args.access_token}"
    );
    // Another verb, another path, or no per-call credential anywhere: Case 1.
    assert_eq!(
        mirrored_credential(&sdl, Some("POST"), Some("/owners/{$this.owner_id}")),
        None
    );
    assert_eq!(
        mirrored_credential(&sdl, Some("GET"), Some("/widgets/{$this.id}")),
        None
    );
    assert_eq!(
        mirrored_credential(LINK_SDL, Some("GET"), Some("/owners/{$this.owner_id}")),
        None
    );
}

#[test]
fn a_sub_resource_connector_is_no_link_and_draws_no_link_credential() {
    // `Repo.issues` reads two parent fields and a page size: a sub-resource
    // GET, never a link target (ADR 0069, R43). Its `{$args.limit}` is no
    // credential the field opens.
    let sdl = LINK_SDL
        .replace(
            ", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }]",
            "",
        )
        .replace(
            "owner(access_token: String!): Widget_Co_Owner",
            "owner: Widget_Co_Owner",
        )
        .replace(
            "type Query {\n",
            "type Widget_Co_Repo {\n  owner: String\n  name: String\n  issues(limit: Int): [Widget_Co_Owner]\n    @connect(source: \"widget_co\", http: { GET: \"/repos/{$this.owner}/{$this.name}/issues?limit={$args.limit}\" }, selection: \"id\")\n}\n\ntype Query {\n",
        );
    assert!(sdl.contains("type Widget_Co_Repo"), "fixture drifted");
    assert!(
        link_credential(&sdl).is_none(),
        "{:?}",
        link_credential(&sdl)
    );
}

// ─── ADR 0069 fix round 2: vendor spellings, a trailing slash ───────────────

#[test]
fn a_vendor_spelled_credential_on_the_by_id_root_field_is_mirrored_not_dropped() {
    // GitLab's `?private_token=`: the root sends it per call, so the link
    // field mirrors it (Case 2) rather than dropping it (Case 1).
    let missing = by_id_root_with_query("private_token");
    let bare_field = "  owner: Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}\" }";
    assert!(missing.contains(bare_field), "fixture drifted");
    let mirrored = missing.replace(
        bare_field,
        "  owner(private_token: String!): Widget_Co_Owner\n    @connect(source: \"widget_co\", http: { GET: \"/owners/{$this.owner_id}?private_token={$args.private_token}\" }",
    );
    assert!(
        link_credential(&mirrored).is_none(),
        "{:?}",
        link_credential(&mirrored)
    );
    assert_eq!(
        link_credential(&missing).expect("the root sends private_token per call").message,
        "Widget_Co_Widget.owner must carry exactly the credential its by-id root field Query.widget_co_owner carries (private_token: String! as query parameter private_token); it carries none — declare the same argument and send it the same way"
    );
}

#[test]
fn a_link_field_on_a_trailing_slash_or_extension_path_is_still_linted() {
    for suffix in ["/", ".json"] {
        // Case 1: its own Authorization header is still an error.
        let own = LINK_SDL.replace(
            "\"/owners/{$this.owner_id}\"",
            &format!("\"/owners/{{$this.owner_id}}{}\"", suffix),
        );
        assert_ne!(own, LINK_SDL);
        assert!(link_credential(&own).is_some(), "{}", suffix);
        // Case 2: the root and the field on the same suffixed path mirror.
        let mirrored = PER_CALL_SDL
            .replace(
                "\"/owners/{$this.owner_id}\"",
                &format!("\"/owners/{{$this.owner_id}}{}\"", suffix),
            )
            .replace(
                "\"/owners/{$args.ownerId}\"",
                &format!("\"/owners/{{$args.ownerId}}{}\"", suffix),
            );
        assert!(
            link_credential(&mirrored).is_none(),
            "{}: {:?}",
            suffix,
            link_credential(&mirrored)
        );
        // …and a field that drops the header (the first match: the field
        // precedes the root) is the Case 2 error.
        let dropped = mirrored.replacen(
            ", headers: [{ name: \"Authorization\", value: \"Bearer {$args.access_token}\" }] }, selection: \"id\")\n}",
            " }, selection: \"id\")\n}",
            1,
        );
        assert_ne!(
            dropped, mirrored,
            "the variant must drop the field's header"
        );
        assert!(
            link_credential(&dropped)
                .expect("dropped")
                .message
                .contains("must carry exactly the credential"),
            "{}",
            suffix
        );
    }
}

// ---- returns-line-nesting (ADR 0067) ----

/// A two-level fixture: `Widget_Co_Share` carries an object field whose type
/// has 2 leaf fields (`debtor`), one with exactly 6 leaves counting an enum
/// and an opaque-JSON scalar (`six`), one with 7 (`wide`) and one with an
/// object field of its own (`deep`). `doc` is the root field's doc comment
/// body and `returns` its declared return type.
fn nesting_sdl(doc: &str, returns: &str) -> String {
    format!(
        r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {{
    baseURL: "{{{{BASE_URL}}}}"
    headers: [{{ name: "Authorization", value: "Bearer {{{{AUTH_EXPR}}}}" }}]
  }}
)

"Free-form vendor metadata; the spec documents no properties."
scalar Widget_Co_JSON

enum Widget_Co_Kind {{
  RED
  BLUE
}}

type Widget_Co_Owner {{
  name: String
  email: String
}}

type Widget_Co_Six {{
  b1: String
  b2: String
  b3: String
  b4: Int
  kind: Widget_Co_Kind
  extra: Widget_Co_JSON
}}

type Widget_Co_Wide {{
  a1: String
  a2: String
  a3: String
  a4: String
  a5: String
  a6: String
  a7: String
}}

type Widget_Co_Deep {{
  id: ID
  owner: Widget_Co_Owner
}}

type Widget_Co_Share {{
  debtor: Widget_Co_Owner
  six: Widget_Co_Six
  wide: [Widget_Co_Wide]
  deep: Widget_Co_Deep
  amount: Int
}}

type Widget_Co_Widget {{
  id: ID
  share: Widget_Co_Share
  metadata: Widget_Co_JSON
}}

type Widget_Co_Page {{
  widgets: [Widget_Co_Widget]
  total: Int
}}

type Query {{
  """
{doc}
  """
  widget_co_listWidgets(limit: Int): {returns}
    @connect(source: "widget_co", http: {{ GET: "/widgets" }}, selection: "id")
  """
  Not selected, so never judged.

  Returns: nothing, here.
  """
  widget_co_other: Widget_Co_Widget
    @connect(source: "widget_co", http: {{ GET: "/other" }}, selection: "id")
}}
"#
    )
}

const COMPLIANT_SHARE: &str =
    "share { debtor { name, email },\n  six { b1, b2, b3, b4, kind, extra }, wide, deep, amount }";

fn nesting_findings(sdl: &str) -> Vec<(String, Option<usize>)> {
    lint(vec![("widget-co.graphql", s(sdl))])
        .findings
        .into_iter()
        .filter(|f| f.rule == "returns-line-nesting")
        .inspect(|f| {
            assert_eq!(f.severity, "warn", "{:?}", f);
            assert_eq!(f.file.as_deref(), Some("widget-co.graphql"), "{:?}", f);
        })
        .map(|f| (f.message, f.line))
        .collect()
}

#[test]
fn a_returns_line_braced_two_levels_where_the_rule_says_is_clean() {
    for (doc, returns) in [
        (
            format!("  List widgets.\n\n  Returns a list of items with: id,\n  {COMPLIANT_SHARE},\n  metadata."),
            "[Widget_Co_Widget]",
        ),
        (
            format!("  Returns: id, {COMPLIANT_SHARE}, metadata."),
            "Widget_Co_Widget!",
        ),
        (
            format!("  Returns: widgets, total.\n  Each item in `widgets` has: id,\n  {COMPLIANT_SHARE}, metadata."),
            "Widget_Co_Page",
        ),
        // The nested cap: an entry past it is simply not listed.
        (
            "  Returns: id, share { debtor { name, email }, six { b1, b2, b3, b4, kind, extra } (+3 more) }.".to_string(),
            "Widget_Co_Widget",
        ),
        // No Returns line at all: another rule's business, not this one's.
        ("  List widgets. One page per call.".to_string(), "[Widget_Co_Widget]"),
    ] {
        let sdl = nesting_sdl(&doc, returns);
        assert_eq!(nesting_findings(&sdl), vec![], "{}", doc);
    }
}

#[test]
fn a_bare_object_entry_at_the_first_level_is_reported_at_its_line() {
    let sdl = nesting_sdl(
        "  Returns a list of items with: id,\n  share, metadata.",
        "[Widget_Co_Widget]",
    );
    let found = nesting_findings(&sdl);
    assert_eq!(found.len(), 1, "{:?}", found);
    let (message, line) = &found[0];
    assert!(message.starts_with("get:/widgets: "), "{}", message);
    assert!(message.contains("`share`"), "{}", message);
    assert!(message.contains("Widget_Co_Share"), "{}", message);
    assert!(
        message.contains("share { debtor, six, wide, deep, amount }"),
        "{}",
        message
    );
    assert_eq!(*line, Some(line_containing(&sdl, "  share, metadata.")));
}

#[test]
fn a_bare_leaf_only_object_at_the_second_level_is_reported_with_its_braces() {
    // `debtor` (2 leaves) and `six` (6 leaves: an enum and an opaque-JSON
    // scalar count as leaves) are owed braces; `wide` (7) and `deep` (an
    // object field) are right to stay bare.
    let sdl = nesting_sdl(
        "  Returns a list of items with: id,\n  share { debtor, six, wide, deep, amount }, metadata.",
        "[Widget_Co_Widget]",
    );
    let found = nesting_findings(&sdl);
    assert_eq!(found.len(), 2, "{:?}", found);
    assert!(found[0].0.contains("debtor { name, email }"), "{:?}", found);
    assert!(
        found[1].0.contains("six { b1, b2, b3, b4, kind, extra }"),
        "{:?}",
        found
    );
    let at = line_containing(&sdl, "share { debtor, six");
    assert_eq!(found[0].1, Some(at));
    assert_eq!(found[1].1, Some(at));
}

#[test]
fn a_braced_second_level_entry_whose_type_is_too_wide_or_nests_is_reported() {
    let sdl = nesting_sdl(
        "  Returns: id, share { debtor { name, email },\n  wide { a1, a2, a3, a4, a5, a6 },\n  deep { id, owner } }.",
        "Widget_Co_Widget",
    );
    let found = nesting_findings(&sdl);
    assert_eq!(found.len(), 2, "{:?}", found);
    assert!(found[0].0.contains("`wide`"), "{:?}", found);
    assert!(found[0].0.contains("7 fields"), "{:?}", found);
    assert_eq!(found[0].1, Some(line_containing(&sdl, "wide { a1")));
    assert!(found[1].0.contains("`deep`"), "{:?}", found);
    assert!(found[1].0.contains("owner"), "{:?}", found);
    assert_eq!(
        found[1].1,
        Some(line_containing(&sdl, "deep { id, owner }"))
    );
}

#[test]
fn a_name_the_type_does_not_declare_is_reported_at_any_level() {
    let sdl = nesting_sdl(
        "  Returns: id, share { debtor { name, mail }, amount },\n  colour.",
        "Widget_Co_Widget",
    );
    let found = nesting_findings(&sdl);
    assert_eq!(found.len(), 2, "{:?}", found);
    assert!(found[0].0.contains("`mail`"), "{:?}", found);
    assert!(found[0].0.contains("Widget_Co_Owner"), "{:?}", found);
    assert_eq!(found[0].1, Some(line_containing(&sdl, "mail }")));
    assert!(found[1].0.contains("`colour`"), "{:?}", found);
    assert!(found[1].0.contains("Widget_Co_Widget"), "{:?}", found);
    assert_eq!(found[1].1, Some(line_containing(&sdl, "  colour.")));
}

#[test]
fn the_envelope_item_line_is_judged_against_the_item_type_and_its_payload_stays_bare() {
    // `widgets` stays a bare name in the first line — the second line is its
    // expansion — while `debtor` in the item line is owed its braces.
    let sdl = nesting_sdl(
        "  Returns: widgets, total.\n  Each item in `widgets` has: id,\n  share { debtor, six { b1, b2, b3, b4, kind, extra }, wide, deep, amount }, metadata.",
        "Widget_Co_Page!",
    );
    let found = nesting_findings(&sdl);
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(found[0].0.contains("debtor { name, email }"), "{:?}", found);
    assert_eq!(
        found[0].1,
        Some(line_containing(&sdl, "share { debtor, six"))
    );
    // An item line naming a field the envelope does not have is itself a
    // name the type does not declare, and then nothing expands `widgets`.
    let sdl = nesting_sdl(
        "  Returns: widgets, total.\n  Each item in `items` has: id.",
        "Widget_Co_Page",
    );
    let found = nesting_findings(&sdl);
    assert_eq!(found.len(), 2, "{:?}", found);
    assert!(found[0].0.contains("names `items`"), "{:?}", found);
    assert!(found[1].0.contains("`widgets` is a"), "{:?}", found);
}

#[test]
fn a_brace_inside_a_leaf_only_second_level_group_is_a_non_object_not_a_second_level_one() {
    // The only way to reach the third level is through a second-level group
    // that was owed its braces, so every third-level name is a leaf: braces
    // there are reported as braces on a non-object, never as a misplaced
    // second-level group.
    let sdl = nesting_sdl(
        "  Returns: id, share { debtor { name { first },\n  email }, amount }.",
        "Widget_Co_Widget",
    );
    let found = nesting_findings(&sdl);
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(
        found[0].0.contains("`name` is a `String`, not an object"),
        "{:?}",
        found
    );
    assert!(!found[0].0.contains("second level"), "{:?}", found);
    assert_eq!(found[0].1, Some(line_containing(&sdl, "name { first }")));
}

#[test]
fn a_pathologically_deep_brace_nest_is_read_to_its_end_without_recursing_per_level() {
    // Past the third level the reader skips a brace group rather than
    // descending into it, so nesting depth costs no stack, and the list
    // resumes after the group closes.
    let depth = 200_000;
    let doc = format!(
        "  Returns: id, share {{ debtor {{ name {}{} }} }},\n  colour.",
        "{ x ".repeat(depth),
        "} ".repeat(depth)
    );
    let sdl = nesting_sdl(&doc, "Widget_Co_Widget");
    let found = nesting_findings(&sdl);
    assert_eq!(found.len(), 2, "{:?}", found);
    assert!(
        found[0].0.contains("`name` is a `String`, not an object"),
        "{:?}",
        found
    );
    assert!(found[1].0.contains("`colour`"), "{:?}", found);
    assert_eq!(found[1].1, Some(line_containing(&sdl, "  colour.")));
}

// ─── credential-source-undocumented (ADR 0064) ──────────────────────────────

/// An AppWorld-shaped service that mints its own credential: a login mutation
/// returns a type declaring `access_token`, and two other selected root fields
/// take `access_token: String!`. `{LOGIN_DOC}` is the login root's doc comment,
/// `{TOKEN_FIELD}` the minting type's credential field.
const CREDENTIAL_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(name: "widget_co", http: { baseURL: "{{BASE_URL}}" })

"A widget."
type Widget_Co_Widget {
  id: ID
  name: String
}

"The login result."
type Widget_Co_AccessToken {
  {TOKEN_FIELD}: String
  token_type: String
}

type Query {
  "List widgets. Returns a list of items with: id, name."
  widget_co_listWidgets(access_token: String!, limit: Int): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets?access_token={$args.access_token}" }, selection: "id name")
}

type Mutation {
  {LOGIN_DOC}
  widget_co_login(username: String!, password: String!): Widget_Co_AccessToken!
    @connect(source: "widget_co", http: { POST: "/auth/token", body: "username: $args.username password: $args.password" }, selection: "{TOKEN_FIELD} token_type")
  "Create a widget. Returns: id, name."
  widget_co_createWidget(access_token: String!, name: String!): Widget_Co_Widget
    @connect(source: "widget_co", http: { POST: "/widgets?access_token={$args.access_token}", body: "name: $args.name" }, selection: "id name")
}
"#;

const CREDENTIAL_SELECTION: &str = "contract_version: 1\ndefaults:\n  fields: all\n  max_depth: 6\n  opaque_json_policy: forbid\noperations:\n  \"get:/widgets\":\n    include: true\n    graphql:\n      root: query\n      name: listWidgets\n  \"post:/auth/token\":\n    include: true\n    graphql:\n      root: mutation\n      name: login\n  \"post:/widgets\":\n    include: true\n    graphql:\n      root: mutation\n      name: createWidget\n";

fn credential_sdl(login_doc: &str, token_field: &str) -> String {
    CREDENTIAL_SDL
        .replace("{LOGIN_DOC}", login_doc)
        .replace("{TOKEN_FIELD}", token_field)
}

/// The firing fixture, so a guard that stays silent on its variant also
/// proves the rule fires on the unmodified one.
fn assert_fixture_fires() {
    let sdl = credential_sdl(
        "\"Log in with a form-urlencoded body. Returns: access_token, token_type.\"",
        "access_token",
    );
    assert_eq!(credential_findings(&sdl, CREDENTIAL_SELECTION).len(), 1);
}

fn credential_findings(sdl: &str, selection: &str) -> Vec<(String, Option<usize>)> {
    lint(vec![
        ("widget-co.graphql", s(sdl)),
        (".factory/selection.yaml", s(selection)),
    ])
    .findings
    .into_iter()
    .filter(|f| f.rule == "credential-source-undocumented")
    .map(|f| {
        assert_eq!(f.severity, "warn", "{:?}", f);
        (f.message, f.line)
    })
    .collect()
}

#[test]
fn a_login_root_that_does_not_name_the_credential_it_mints_is_a_warning() {
    // The 25 Sep AppWorld bundle's login doc comment: the transport and the
    // Returns line. The Returns line names access_token as a field of the
    // result, which says nothing about where the credential goes, so the
    // rule reads the doc comment before it.
    let sdl = credential_sdl(
        "\"Log in with a form-urlencoded body. Returns: access_token, token_type.\"",
        "access_token",
    );
    let found = credential_findings(&sdl, CREDENTIAL_SELECTION);
    assert_eq!(found.len(), 1, "{:?}", found);
    let (message, line) = &found[0];
    assert!(message.contains("Mutation.widget_co_login"), "{}", message);
    assert!(message.contains("`access_token`"), "{}", message);
    assert!(
        message.contains("Query.widget_co_listWidgets")
            && message.contains("Mutation.widget_co_createWidget"),
        "{}",
        message
    );
    assert!(message.contains("schema-authoring.md"), "{}", message);
    assert_eq!(*line, Some(line_containing(&sdl, "widget_co_login(")));
}

#[test]
fn a_login_root_whose_doc_comment_names_the_credential_is_clean() {
    assert_fixture_fires();
    let sdl = credential_sdl(
        "\"\"\"\n  Log in. Every other operation takes the access_token this returns as its access_token argument; the username is the account's email.\n  Returns: access_token, token_type.\n  \"\"\"",
        "access_token",
    );
    assert_eq!(credential_findings(&sdl, CREDENTIAL_SELECTION), vec![]);
}

#[test]
fn a_camel_case_token_field_still_marks_the_minting_root() {
    let sdl = credential_sdl("\"Log in with a form-urlencoded body.\"", "accessToken");
    let found = credential_findings(&sdl, CREDENTIAL_SELECTION);
    assert_eq!(found.len(), 1, "{:?}", found);
    assert!(
        found[0].0.contains("Mutation.widget_co_login"),
        "{:?}",
        found
    );
}

#[test]
fn a_credential_on_the_source_with_no_minting_root_is_clean() {
    assert_fixture_fires();
    // Two roots take access_token, but no root returns a field of that
    // name: the credential lives on @source and in the README.
    let sdl = credential_sdl("\"Log in with a form-urlencoded body.\"", "session");
    assert_eq!(credential_findings(&sdl, CREDENTIAL_SELECTION), vec![]);
}

#[test]
fn one_root_taking_the_credential_or_a_non_credential_name_is_clean() {
    assert_fixture_fires();
    let doc = "\"Log in with a form-urlencoded body.\"";
    // Only one root takes it: createWidget's argument renamed.
    let one = credential_sdl(doc, "access_token").replace(
        "widget_co_createWidget(access_token: String!",
        "widget_co_createWidget(owner: String!",
    );
    assert_eq!(credential_findings(&one, CREDENTIAL_SELECTION), vec![]);
    // A shared name that is not on the credential list (`cursor`) is not a
    // credential, even though the login type declares it.
    let not_credential =
        credential_sdl(doc, "cursor").replace("(access_token: String!", "(cursor: String!");
    assert_eq!(
        credential_findings(&not_credential, CREDENTIAL_SELECTION),
        vec![]
    );
}

#[test]
fn an_unselected_root_does_not_count_toward_the_credential_rule() {
    assert_fixture_fires();
    // createWidget excluded: only listWidgets takes access_token.
    let selection = CREDENTIAL_SELECTION.replace(
        "  \"post:/widgets\":\n    include: true",
        "  \"post:/widgets\":\n    include: false",
    );
    let sdl = credential_sdl("\"Log in with a form-urlencoded body.\"", "access_token");
    assert_eq!(credential_findings(&sdl, &selection), vec![]);
}

#[test]
fn the_pilot_raises_no_credential_source_finding() {
    let pilots = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pilots/graphos");
    for pilot in ["gitea"] {
        let result = lint_workspace(
            &pilots.join(pilot),
            &LintOptions {
                schemas_dir: None,
                skip_evidence: true,
                target: &graphos_factory_core::target::BARE,
            },
        );
        let hits: Vec<_> = result
            .findings
            .iter()
            .filter(|f| f.rule == "credential-source-undocumented")
            .collect();
        assert!(hits.is_empty(), "{}: {:?}", pilot, hits);
    }
}

// ─── argument-constraints-undocumented: the omission sentence (ADR 0066) ────

/// AppWorld's Venmo `payment_card_id`, the case that motivated the rule.
const VENMO_CARD: &str =
    "ID of the payment card to use for the transaction. If not passed, Venmo balance will be used.";

#[test]
fn omission_sentence_is_the_sentence_that_carries_a_cue() {
    use graphos_factory_core::lint::omission_sentence;
    assert_eq!(
        omission_sentence(VENMO_CARD, "payment_card_id", &["amount", "receiver_email"]),
        Some("If not passed, Venmo balance will be used.".to_string())
    );
    assert_eq!(
        omission_sentence("The ID of the card.", "card_id", &[]),
        None
    );
    for (text, expected) in [
        (
            "Page cursor. If omitted, the first page.",
            "If omitted, the first page.",
        ),
        (
            "Owner. If not provided, the caller.",
            "If not provided, the caller.",
        ),
        (
            "Region. If not specified, us-east-1.",
            "If not specified, us-east-1.",
        ),
        ("Tag. Ignored when absent.", "Ignored when absent."),
        ("Tag. Every tag when omitted.", "Every tag when omitted."),
        ("Status to mark as. DEFAULTS TO read.", "DEFAULTS TO read."),
        (
            "Archived repositories are hidden by default.",
            "Archived repositories are hidden by default.",
        ),
        // one sentence, no final period (Gitea's own spelling)
        (
            "include private repositories (defaults to true)",
            "include private repositories (defaults to true)",
        ),
    ] {
        assert_eq!(
            omission_sentence(text, "x", &[]).as_deref(),
            Some(expected),
            "{}",
            text
        );
    }
}

/// Paragraphs and list items are sentence boundaries, wrapped lines are
/// joined, and an abbreviation's period is not one.
#[test]
fn omission_sentence_splits_paragraphs_and_keeps_abbreviations() {
    use graphos_factory_core::lint::omission_sentence;
    let pagerduty_total = "By default the `total` field in pagination responses is set to `null` to provide the fastest possible response times. Set `total` to `true` for this field to be populated.\n\nSee our [Pagination Docs](https://developer.pagerduty.com/docs/rest-api-v2/pagination/) for more information.\n";
    assert_eq!(
        omission_sentence(pagerduty_total, "total", &["limit", "offset"]).as_deref(),
        Some("By default the `total` field in pagination responses is set to `null` to provide the fastest possible response times.")
    );
    let wrapped = "For explicitly setting the user creation timestamp. Useful when users are\nmigrated from other systems. When omitted, the user's creation timestamp\nwill be set to \"now\".";
    assert_eq!(
        omission_sentence(wrapped, "created_at", &["email"]).as_deref(),
        Some("When omitted, the user's creation timestamp will be set to \"now\".")
    );
    assert_eq!(
        omission_sentence("A zone, e.g. UTC. Defaults to UTC.", "tz", &[]).as_deref(),
        Some("Defaults to UTC.")
    );
    assert_eq!(
        omission_sentence("A zone, e.g. defaults to nothing here", "tz", &[]).as_deref(),
        Some("A zone, e.g. defaults to nothing here")
    );
    let listed =
        "Truncated at the bounds.\n- If not passed, entries are truncated.\n- Otherwise not.";
    assert_eq!(
        omission_sentence(listed, "overflow", &[]).as_deref(),
        Some("If not passed, entries are truncated.")
    );
}

/// A sentence that names a different parameter describes an interaction of
/// two arguments, not this argument's omission: it does not count. Since
/// ADR 0083, Gitea's own `order` sentence carries a cue ("if sort is not
/// specified"), so only the sibling rule keeps it out: the same text is owed
/// when the operation has no `sort`.
#[test]
fn omission_sentence_that_names_another_parameter_does_not_count() {
    use graphos_factory_core::lint::omission_sentence;
    assert_eq!(
        omission_sentence("Ignored if sort is not specified.", "order", &["sort"]),
        None
    );
    assert_eq!(
        omission_sentence("Ignored if sort is not specified.", "order", &[]).as_deref(),
        Some("Ignored if sort is not specified.")
    );
    let with_cue = "Sort order. Defaults to asc; ignored if \"sort\" is not specified.";
    assert_eq!(
        omission_sentence(with_cue, "order", &["sort", "page"]),
        None
    );
    assert_eq!(
        omission_sentence(with_cue, "order", &["page"]).as_deref(),
        Some("Defaults to asc; ignored if \"sort\" is not specified.")
    );
    // PagerDuty's `since` on get:/schedules: the default is stated in terms
    // of `until`.
    let since = "The start of the date range over which you want to show schedule entries. Defaults to 2 weeks before until if an until is given. Optional parameter.";
    assert_eq!(
        omission_sentence(since, "since", &["until", "time_zone"]),
        None
    );
    // The argument's own name, a longer name containing the sibling's, and
    // a different case are not the sibling.
    assert_eq!(
        omission_sentence(
            "Defaults to all urgencies.",
            "urgencies[]",
            &["urgencies[]", "urgency"]
        )
        .as_deref(),
        Some("Defaults to all urgencies.")
    );
    assert_eq!(
        omission_sentence(
            "Sort direction defaults to ascending.",
            "sort_by",
            &["sort"]
        )
        .as_deref(),
        Some("Sort direction defaults to ascending.")
    );
    assert_eq!(
        omission_sentence("Defaults to the sort-order key.", "order", &["sort"]).as_deref(),
        Some("Defaults to the sort-order key.")
    );
    // A second cue sentence that names no sibling still counts.
    assert_eq!(
        omission_sentence(
            "Defaults to sort's order. If omitted, ascending.",
            "order",
            &["sort"]
        )
        .as_deref(),
        Some("If omitted, ascending.")
    );
}

fn venmo_param(required: bool) -> Value {
    json!({"name": "payment_card_id", "in": "query", "required": required, "type": "integer",
        "description": VENMO_CARD})
}

const VENMO_HTTP: &str = r#"GET: "/widgets", queryParams: "payment_card_id: $args.paymentCardId""#;

#[test]
fn omission_sentence_missing_from_an_undocumented_argument_is_a_warning_naming_it() {
    let inv = doc_inventory(Some("List widgets"), None, json!([venmo_param(false)]));
    let r = doc_lint(
        &doc_sdl("  \"List widgets.\"\n", "paymentCardId: Int", VENMO_HTTP),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert_eq!(hits[0].severity, "warn");
    assert!(
        hits[0]
            .message
            .contains("\"If not passed, Venmo balance will be used.\"")
            && hits[0].message.contains("argument `paymentCardId`")
            && hits[0]
                .message
                .contains("query parameter `payment_card_id`")
            && !hits[0].message.contains("ID of the payment card"),
        "{}",
        hits[0].message
    );
}

/// The existing doc comment is kept (it is curation), but it still owes the
/// sentence: this half reads the comment's content.
#[test]
fn omission_sentence_missing_from_a_documented_argument_is_a_warning() {
    let inv = doc_inventory(Some("List widgets"), None, json!([venmo_param(false)]));
    for args in [
        "\"The card to pay with.\" paymentCardId: Int",
        "\"ID of the payment card to use for the transaction.\" paymentCardId: Int",
    ] {
        let r = doc_lint(
            &doc_sdl("  \"List widgets.\"\n", args, VENMO_HTTP),
            &inv,
            &doc_selection(None),
        );
        let hits = findings_of(&r, "argument-constraints-undocumented");
        assert_eq!(hits.len(), 1, "{}: {:?}", args, r.findings);
        assert!(
            hits[0]
                .message
                .contains("\"If not passed, Venmo balance will be used.\""),
            "{}",
            hits[0].message
        );
    }
}

/// Carried means a whitespace-normalised, case-insensitive substring of the
/// sentence without its final period: a wrapped `"""` block, other casing
/// and a following clause all carry it.
#[test]
fn omission_sentence_quiet_when_the_doc_comment_carries_it() {
    let inv = doc_inventory(Some("List widgets"), None, json!([venmo_param(false)]));
    for args in [
        "\"If not passed, Venmo balance will be used.\" paymentCardId: Int",
        "\"ID of the payment card to use for the transaction. If not passed, Venmo balance will be used.\" paymentCardId: Int",
        "\n    \"\"\"\n    The card to pay with. If not passed,\n    Venmo balance will be used.\n    \"\"\"\n    paymentCardId: Int\n  ",
        "\"if NOT passed, venmo balance will be used (see the wallet).\" paymentCardId: Int",
    ] {
        let r = doc_lint(
            &doc_sdl("  \"List widgets.\"\n", args, VENMO_HTTP),
            &inv,
            &doc_selection(None),
        );
        assert!(
            findings_of(&r, "argument-constraints-undocumented").is_empty(),
            "{}: {:?}",
            args,
            r.findings
        );
    }
}

/// A required argument cannot be omitted, so its omission sentence is moot.
#[test]
fn omission_sentence_quiet_on_a_required_argument() {
    let inv = doc_inventory(Some("List widgets"), None, json!([venmo_param(true)]));
    let r = doc_lint(
        &doc_sdl("  \"List widgets.\"\n", "paymentCardId: Int!", VENMO_HTTP),
        &inv,
        &doc_selection(None),
    );
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
}

/// Gitea's `order` next to `sort`: the cue sentence names `sort`, so it is
/// not this argument's omission sentence and nothing is owed. With no
/// `sort` parameter in the operation the same text is owed.
#[test]
fn omission_sentence_quiet_when_it_names_a_sibling_parameter() {
    let order = json!({"name": "order", "in": "query", "required": false, "type": "string",
        "description": "Sort order. Defaults to asc; ignored if \"sort\" is not specified."});
    let sort = json!({"name": "sort", "in": "query", "required": false, "type": "string",
        "description": "Sort field"});
    let http = r#"GET: "/widgets", queryParams: "order: $args.order""#;
    let sdl = doc_sdl("  \"List widgets.\"\n", "order: String", http);
    let r = doc_lint(
        &sdl,
        &doc_inventory(Some("List widgets"), None, json!([order.clone(), sort])),
        &doc_selection(None),
    );
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
    let r = doc_lint(
        &sdl,
        &doc_inventory(Some("List widgets"), None, json!([order])),
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0]
            .message
            .contains("\"Defaults to asc; ignored if \"sort\" is not specified.\""),
        "{}",
        hits[0].message
    );
}

/// A constrained argument with an omission sentence and no doc comment is
/// one finding, whose text to write is the clause and then the sentence.
#[test]
fn omission_sentence_and_clause_are_one_finding_in_order() {
    let mut sort = sort_param();
    sort["description"] = json!("Sort order. Ascending when omitted.");
    let inv = doc_inventory(Some("List widgets"), None, json!([sort]));
    let r = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "sort: String",
            r#"GET: "/widgets", queryParams: "sort: $args.sort""#,
        ),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0]
            .message
            .contains("\"(default asc, one of asc|desc). Ascending when omitted.\""),
        "{}",
        hits[0].message
    );
}

/// The opt-in source sentence that is itself the omission sentence
/// (Gitea's `private`) carries it once; the clause after it is fine.
#[test]
fn omission_sentence_quiet_when_it_is_the_source_sentence() {
    let private = json!({"name": "private", "in": "query", "required": false, "type": "boolean",
        "description": "include private repositories this user has access to (defaults to true)"});
    let inv = doc_inventory(Some("List widgets"), None, json!([private]));
    let r = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "\"include private repositories this user has access to (defaults to true)\" private: Boolean",
            r#"GET: "/widgets", queryParams: "private: $args.private""#,
        ),
        &inv,
        &doc_selection(None),
    );
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
}

/// A request-body property's description is read the same way.
#[test]
fn omission_sentence_on_a_body_property_is_read() {
    let mut inv = doc_inventory(Some("Pay"), None, json!([]));
    inv["operations"][0]["request_body"] = json!({"content_type": "application/json", "required": true, "shape_ref": "#/shapes/Payment"});
    inv["shapes"] = json!({"Payment": {"type": "object", "properties": {
        "amount": {"type": "number", "description": "Amount to pay."},
        "payment_card_id": {"type": "integer", "description": VENMO_CARD}
    }}});
    let r = doc_lint(
        &doc_sdl(
            "  \"Pay.\"\n",
            "amount: Float!, paymentCardId: Int",
            r#"POST: "/widgets", body: "amount: $args.amount payment_card_id: $args.paymentCardId""#,
        ),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("body key `payment_card_id`")
            && hits[0]
                .message
                .contains("\"If not passed, Venmo balance will be used.\""),
        "{}",
        hits[0].message
    );
}

/// The page-size argument keeps § Pagination's wording and nothing else.
#[test]
fn omission_sentence_skips_the_page_size_parameter() {
    let mut inv = paginated_inventory(Some(25), Some(100), "integer");
    inv["operations"][0]["parameters"][0]["description"] = json!("Page size. Defaults to 25.");
    let r = lint(vec![
        ("widget-co.graphql", s(PAGINATED_SDL_NO_DOC)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&paginated_selection())),
    ]);
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
}

// ─── ADR 0083: the omission sentence's reach ───────────────────────────────

/// ADR 0066's phrases missed the ways the pilots' own sources say it
/// ("default is 1 month", "This will default to the account time zone.",
/// "Default is \"alpha\"") and ordinary rewordings of the Venmo sentence.
#[test]
fn omission_cue_reads_the_phrasings_sources_use() {
    use graphos_factory_core::lint::omission_sentence;
    for text in [
        "Maximum range is 6 months and default is 1 month.",
        "This will default to the account time zone.",
        "Default to the repository's default branch.",
        "Default value is false",
        "If this parameter is not provided, the Venmo balance is used.",
        "If no card is given, the Venmo balance is used.",
        "When unset, the Venmo balance is used.",
        "Omit to pay from the Venmo balance.",
        "Uses the Venmo balance unless otherwise specified.",
        "If not passed, Venmo balance will be used.",
    ] {
        assert_eq!(
            omission_sentence(text, "card", &[]),
            Some(text.to_string()),
            "{}",
            text
        );
    }
    for text in [
        "If set, only cards on file are listed.",
        "Fails if the user is missing a role.",
        "The ID of the card.",
    ] {
        assert_eq!(omission_sentence(text, "card", &[]), None, "{}", text);
    }
}

/// A period inside parentheses does not end the sentence: the finding once
/// asked for the fragment "Defaults to 10 (max.".
#[test]
fn omission_sentence_keeps_a_parenthesised_period() {
    use graphos_factory_core::lint::omission_sentence;
    assert_eq!(
        omission_sentence(
            "Page size. Defaults to 10 (max. 100 per page). Slow.",
            "per",
            &[]
        ),
        Some("Defaults to 10 (max. 100 per page).".to_string())
    );
}

fn venmo_payment_inventory() -> Value {
    let mut inv = doc_inventory(Some("Pay"), None, json!([]));
    inv["operations"][0]["request_body"] = json!({"content_type": "application/json", "required": true, "shape_ref": "#/shapes/Payment"});
    inv["shapes"] = json!({
        "Payment": {"type": "object", "properties": {
            "payment": {"$ref": "#/shapes/Card"}
        }},
        "Card": {"type": "object", "properties": {
            "amount": {"type": "number", "description": "Amount to pay."},
            "card_id": {"type": "integer", "description": VENMO_CARD},
            "note": {"type": "string", "description": "If not passed, the amount is noted as a gift."}
        }}
    });
    inv
}

/// A write whose body nests the argument in an object literal, the form
/// every pagerduty mutation uses, was never traced: the Venmo case in a
/// `$({ … })` body went unread.
#[test]
fn omission_sentence_is_read_through_a_nested_body() {
    let r = doc_lint(
        &doc_sdl(
            "  \"Pay.\"\n",
            "amount: Float!, paymentCardId: Int",
            "POST: \"/widgets\", body: \"\"\"\n$({ payment: { amount: $args.amount, card_id: $args.paymentCardId } })\n\"\"\"",
        ),
        &venmo_payment_inventory(),
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("body key `payment.card_id`")
            && hits[0]
                .message
                .contains("\"If not passed, Venmo balance will be used.\""),
        "{}",
        hits[0].message
    );
}

/// An argument sent through methods (`->map(…)->first`) reaches the key it
/// sits under.
#[test]
fn omission_sentence_is_read_through_a_method_chain() {
    let r = doc_lint(
        &doc_sdl(
            "  \"Pay.\"\n",
            "amount: Float!, paymentCardId: Int",
            "POST: \"/widgets\", body: \"\"\"\n$({ payment: { amount: $args.amount, card_id: $args.paymentCardId->map(@)->first } })\n\"\"\"",
        ),
        &venmo_payment_inventory(),
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("body key `payment.card_id`"),
        "{}",
        hits[0].message
    );
}

/// A nested key's siblings are the keys beside it: "the amount" names
/// `amount`, so `note`'s sentence is about the two together.
#[test]
fn omission_sentence_naming_a_nested_peer_does_not_count() {
    let r = doc_lint(
        &doc_sdl(
            "  \"Pay.\"\n",
            "amount: Float!, note: String",
            "POST: \"/widgets\", body: \"\"\"\n$({ payment: { amount: $args.amount, note: $args.note } })\n\"\"\"",
        ),
        &venmo_payment_inventory(),
        &doc_selection(None),
    );
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
}

/// A header whose value is the argument is traced to the header parameter.
#[test]
fn omission_sentence_is_read_on_a_header_argument() {
    let inv = doc_inventory(
        Some("List widgets"),
        None,
        json!([{"name": "X-Card", "in": "header", "required": false, "type": "integer",
            "description": VENMO_CARD}]),
    );
    let r = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "paymentCardId: Int",
            r#"GET: "/widgets", headers: [{ name: "X-Card", value: "{$args.paymentCardId}" }]"#,
        ),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("header `X-Card`"),
        "{}",
        hits[0].message
    );
}

/// An argument sub-selection sends the input type's fields, not the
/// argument: its fields' doc comments are not read (ADR 0083's stated
/// limit), and the argument itself owes nothing.
#[test]
fn omission_sentence_is_not_demanded_through_an_argument_sub_selection() {
    let r = doc_lint(
        &doc_sdl(
            "  \"Pay.\"\n",
            "payment: WidgetCo_PaymentInput",
            "POST: \"/widgets\", body: \"\"\"\n$({ payment: $args.payment { card_id: cardId } })\n\"\"\"",
        ),
        &venmo_payment_inventory(),
        &doc_selection(None),
    );
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
}

/// A one-line doc comment with an escaped quote is read whole: the reader
/// used to start at the escaped quote, so `"… Default is \"alpha\"."` read
/// as `alpha\".` and never carried the sentence (Gitea's `sort`).
#[test]
fn omission_sentence_is_carried_by_a_doc_comment_with_escaped_quotes() {
    let inv = doc_inventory(
        Some("List widgets"),
        None,
        json!([{"name": "sort", "in": "query", "required": false, "type": "string",
            "description": "Sort by attribute. Default is \"alpha\""}]),
    );
    let quiet = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "\"Attribute to sort by. Default is \\\"alpha\\\".\"\n    sort: String",
            r#"GET: "/widgets", queryParams: "sort: $args.sort""#,
        ),
        &inv,
        &doc_selection(None),
    );
    assert!(
        findings_of(&quiet, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        quiet.findings
    );
}

/// A page-size key in the request body (an RPC-style API's POST list endpoints) is
/// skipped the same way: the skip used to compare only a parameter's
/// `name`, which a body property does not have.
#[test]
fn omission_sentence_skips_a_page_size_body_key() {
    let mut inv = paginated_inventory(Some(25), Some(100), "integer");
    let op = &mut inv["operations"][0];
    op["key"] = json!("post:/widgets");
    op["method"] = json!("POST");
    op["parameters"] = json!([]);
    op["request_body"] = json!({"content_type": "application/json", "required": false, "shape_ref": "#/shapes/ListWidgets"});
    inv["shapes"] = json!({"ListWidgets": {"type": "object", "properties": {
        "limit": {"type": "integer", "default": 25, "maximum": 100,
            "description": "Page size. The maximum and default value is 100."}
    }}});
    let sdl = PAGINATED_SDL_NO_DOC.replace(
        r#"http: { GET: "/widgets", queryParams: "limit: $args.limit" }"#,
        r#"http: { POST: "/widgets", body: "limit: $args.limit" }"#,
    );
    let r = lint(vec![
        ("widget-co.graphql", s(&sdl)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (
            ".factory/selection.yaml",
            s(&paginated_selection().replace("get:/widgets", "post:/widgets")),
        ),
    ]);
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
}

// ─── unknown-tag and failure-case-missing (ADR 0072) ────────────────────────

fn count(result: &LintResult, rule: &str) -> usize {
    result.findings.iter().filter(|f| f.rule == rule).count()
}

/// The widget workspace with `get:/widgets` documenting `statuses` as
/// errors, plus `stubs` as extra mapping files (name, status).
fn with_errors(statuses: &[&str], stubs: &[(&str, u64)]) -> LintResult {
    let mut inv = inventory();
    inv["operations"][0]["errors"] = Value::Array(
        statuses
            .iter()
            .map(|s| json!({ "status": s, "description": "failure", "shape_ref": null }))
            .collect(),
    );
    let mut overrides: Vec<(&str, Option<String>)> = vec![(
        ".factory/inventory.json",
        Some(graphos_factory_core::json::pretty(&inv)),
    )];
    let files: Vec<(String, String)> = stubs
        .iter()
        .map(|(name, status)| {
            (
                format!("tests/fixtures/mappings/{}.json", name),
                json!({
                    "request": { "method": "GET", "urlPath": "/widgets", "queryParameters": { "limit": { "equalTo": format!("{}", status) } } },
                    "response": { "status": status, "jsonBody": { "error": "x" } },
                    "metadata": { "x-cases": ["list_widgets"] }
                })
                .to_string(),
            )
        })
        .collect();
    for (rel, text) in &files {
        overrides.push((rel.as_str(), Some(text.clone())));
    }
    lint(overrides)
}

#[test]
fn failure_case_missing_warns_once_per_operation_listing_its_statuses() {
    let r = with_errors(&["404", "429"], &[]);
    let found: Vec<&graphos_factory_core::lint::Finding> = r
        .findings
        .iter()
        .filter(|f| f.rule == "failure-case-missing")
        .collect();
    assert_eq!(found.len(), 1, "one per operation: {:?}", r.findings);
    assert!(
        found[0]
            .message
            .starts_with("get:/widgets: no e2e case answers 404, 429"),
        "{}",
        found[0].message
    );
    assert!(found[0].message.contains("widget_co_listWidgets"));
    let detail = found[0].detail.as_ref().unwrap().as_array().unwrap();
    let statuses: Vec<&str> = detail
        .iter()
        .map(|d| d["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["404", "429"]);
}

#[test]
fn failure_case_missing_is_met_by_a_stub_serving_the_case_with_that_status() {
    // The case's own stub answers 404 through x-cases; the operation stays
    // open for 429 alone.
    let r = with_errors(&["404", "429"], &[("list_widgets_not_found", 404)]);
    let found: Vec<&graphos_factory_core::lint::Finding> = r
        .findings
        .iter()
        .filter(|f| f.rule == "failure-case-missing")
        .collect();
    assert_eq!(found.len(), 1, "{:?}", r.findings);
    assert!(
        found[0]
            .message
            .starts_with("get:/widgets: no e2e case answers 429 "),
        "{}",
        found[0].message
    );
    assert_eq!(
        found[0].detail.as_ref().unwrap().as_array().unwrap().len(),
        1
    );
}

/// `get:/widgets` documenting `statuses`, and each of `stubs` as a mapping the
/// case `list_widgets` is served by (`x-cases`). The stub carries its own
/// request; only its response status and request vary between tests.
fn with_stubs(statuses: &[&str], stubs: &[(Value, Value)]) -> LintResult {
    let mut inv = inventory();
    inv["operations"][0]["errors"] = Value::Array(
        statuses
            .iter()
            .map(|s| json!({ "status": s, "description": "failure", "shape_ref": null }))
            .collect(),
    );
    let mut files: Vec<(String, String)> = vec![(
        ".factory/inventory.json".to_string(),
        graphos_factory_core::json::pretty(&inv),
    )];
    for (i, (request, status)) in stubs.iter().enumerate() {
        files.push((
            format!("tests/fixtures/mappings/stub_{}.json", i),
            json!({
                "request": request,
                "response": { "status": status },
                "metadata": { "x-cases": ["list_widgets"] }
            })
            .to_string(),
        ));
    }
    lint(
        files
            .iter()
            .map(|(n, c)| (n.as_str(), Some(c.clone())))
            .collect(),
    )
}

#[test]
fn failure_case_missing_credits_only_the_stub_answering_the_operations_own_request() {
    // The case is served by a stub for a different request (a nested
    // lookup answering 404): the case saw a 404, the listing did not.
    let nested = json!({ "method": "GET", "urlPath": "/owners/o1" });
    let r = with_stubs(&["404"], &[(nested, json!(404))]);
    assert_eq!(count(&r, "failure-case-missing"), 1, "{:?}", r.findings);
    // Same path, another method: not the operation's own request.
    let other_method = json!({ "method": "POST", "urlPath": "/widgets" });
    let r = with_stubs(&["404"], &[(other_method, json!(404))]);
    assert_eq!(count(&r, "failure-case-missing"), 1, "{:?}", r.findings);
    // A longer path that only starts like the template is another request.
    let deeper = json!({ "method": "GET", "urlPath": "/widgets/w1" });
    let r = with_stubs(&["404"], &[(deeper, json!(404))]);
    assert_eq!(count(&r, "failure-case-missing"), 1, "{:?}", r.findings);
    // The nested stub beside the operation's own stub credits only what the
    // own stub answers.
    let own = json!({ "method": "GET", "urlPath": "/widgets" });
    let nested = json!({ "method": "GET", "urlPath": "/owners/o1" });
    let r = with_stubs(&["404", "429"], &[(own, json!(429)), (nested, json!(404))]);
    let found: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "failure-case-missing")
        .collect();
    assert_eq!(found.len(), 1, "{:?}", r.findings);
    assert!(
        found[0]
            .message
            .starts_with("get:/widgets: no e2e case answers 404 "),
        "{}",
        found[0].message
    );
}

#[test]
fn failure_case_missing_reads_every_way_a_stub_names_the_operations_request() {
    for request in [
        json!({ "method": "GET", "urlPath": "/widgets" }),
        json!({ "method": "get", "urlPath": "/widgets" }),
        json!({ "urlPath": "/widgets" }),
        json!({ "method": "ANY", "urlPath": "/widgets" }),
        json!({ "method": "GET", "url": "/widgets?limit=1" }),
        json!({ "method": "GET", "urlPathTemplate": "/widgets" }),
        json!({ "method": "GET", "urlPathPattern": "/wid.*" }),
        json!({ "method": "GET", "urlPath": "/api/v1/widgets" }),
    ] {
        let r = with_stubs(&["404"], &[(request.clone(), json!(404))]);
        assert_eq!(count(&r, "failure-case-missing"), 0, "{request}");
    }
    for request in [
        json!({ "method": "GET", "urlPattern": "/widgets" }),
        json!({ "method": "GET", "urlPathPattern": "/wid(" }),
        json!({ "method": "GET" }),
    ] {
        let r = with_stubs(&["404"], &[(request.clone(), json!(404))]);
        assert_eq!(count(&r, "failure-case-missing"), 1, "{request}");
    }
}

#[test]
fn failure_case_missing_reads_a_status_only_as_a_string_or_unsigned_integer() {
    let own = json!({ "method": "GET", "urlPath": "/widgets" });
    for status in [json!("404"), json!(404)] {
        let r = with_stubs(&["404"], &[(own.clone(), status.clone())]);
        assert_eq!(count(&r, "failure-case-missing"), 0, "{status}");
    }
    // `default` is any non-2xx answer, so a value that is not a status must
    // not stand in for one.
    for status in [
        json!(-1),
        json!("-1"),
        json!(true),
        json!(4.5),
        json!("abc"),
    ] {
        let r = with_stubs(&["default"], &[(own.clone(), status.clone())]);
        assert_eq!(count(&r, "failure-case-missing"), 1, "{status}");
    }
}

/// Remove a pilot copy's e2e cases (graphql, expected and stub) by stem. The
/// pilots now carry the error cases ADR 0077's scaffold wrote, so a test that
/// needs an uncovered status drops the case first.
fn drop_cases(dir: &Path, stems: &[&str]) {
    for stem in stems {
        for rel in [
            format!("tests/cases/{stem}.graphql"),
            format!("tests/cases/{stem}.expected.json"),
            format!("tests/fixtures/mappings/{stem}.json"),
        ] {
            std::fs::remove_file(dir.join(&rel)).unwrap_or_else(|e| panic!("{rel}: {e}"));
        }
    }
}

#[test]
fn a_gitea_side_request_answering_404_does_not_cover_the_listing() {
    // list_issues_repository_owner_null: the listing answers 200 and only
    // its nested /users/ lookup answers 404. The pilot now has the listing's
    // own 404 case (`list_issues_404`), so the copy drops it first.
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pilots/graphos/gitea");
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("cp")
        .arg("-R")
        .arg(format!("{}/.", from.display()))
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
    drop_cases(dir.path(), &["list_issues_404"]);
    let r = lint_workspace(
        dir.path(),
        &LintOptions {
            schemas_dir: None,
            skip_evidence: true,
            target: &graphos_factory_core::target::BARE,
        },
    );
    let listing = r
        .findings
        .iter()
        .find(|f| {
            f.rule == "failure-case-missing"
                && f.message.starts_with("get:/repos/{owner}/{repo}/issues: ")
        })
        .expect("the listing's 404 has no case of its own");
    assert!(
        listing.message.contains("answers 404 "),
        "{}",
        listing.message
    );
}

#[test]
fn failure_case_missing_lists_every_open_status_in_ascending_order() {
    // Exact codes numerically, then ranges, then default; each once.
    let r = with_errors(&["429", "401", "default", "5XX", "404", "500", "401"], &[]);
    let f = r
        .findings
        .iter()
        .find(|f| f.rule == "failure-case-missing")
        .unwrap();
    assert!(
        f.message
            .starts_with("get:/widgets: no e2e case answers 401, 404, 429, 500, 5XX, default "),
        "{}",
        f.message
    );
    let statuses: Vec<&str> = f
        .detail
        .as_ref()
        .unwrap()
        .as_array()
        .unwrap()
        .iter()
        .map(|d| d["status"].as_str().unwrap())
        .collect();
    assert_eq!(statuses, ["401", "404", "429", "500", "5XX", "default"]);
}

#[test]
fn failure_case_missing_reads_ranges_and_default() {
    assert_eq!(
        count(
            &with_errors(&["4XX"], &[("e", 404)]),
            "failure-case-missing"
        ),
        0
    );
    assert_eq!(
        count(
            &with_errors(&["5XX"], &[("e", 404)]),
            "failure-case-missing"
        ),
        1
    );
    assert_eq!(
        count(
            &with_errors(&["default"], &[("e", 500)]),
            "failure-case-missing"
        ),
        0
    );
    // A 2xx stub never covers an error status, `default` included.
    assert_eq!(
        count(
            &with_errors(&["default"], &[("e", 201)]),
            "failure-case-missing"
        ),
        1
    );
    // A documented 2xx in `errors` is not a failure status.
    assert_eq!(
        count(&with_errors(&["204"], &[]), "failure-case-missing"),
        0
    );
}

// ── secret-field-exposed (ADR 0078) ─────────────────────────────────────────

/// Base SDL plus a credential-named response field (`token`) mapped by the
/// same connector, and an argument of the same shape (`password`) that is
/// never mapped anywhere -- the argument exercises "silent on an input
/// argument" from the same fixture the field fires from.
const SDL_SECRET_FIELD: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
)

scalar Widget_Co_JSON

type Widget_Co_Widget {
  id: ID
  name: String
  token: String
  "Free-form vendor metadata; the spec documents no properties."
  metadata: Widget_Co_JSON
}

type Query {
  widget_co_listWidgets(limit: Int, password: String): [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "id name metadata token")
}
"#;

#[test]
fn secret_field_exposed_fires_on_a_credential_named_response_field() {
    let r = lint(vec![("widget-co.graphql", s(SDL_SECRET_FIELD))]);
    let hits: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "secret-field-exposed")
        .collect();
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("Widget_Co_Widget.token"),
        "{}",
        hits[0].message
    );
    // The `password` argument on the same field never fires: an argument is
    // never a response field, and Query's own "fields" (the argument's
    // owner) are never walked by this rule in the first place.
    assert!(!hits[0].message.contains("password"), "{:?}", r.findings);
}

/// The field name read as whole words: a run of capitals is one word, so an
/// acronym-led or all-caps spelling of a credential fires like its camelCase
/// twin, and a name that only contains a credential word as a substring does
/// not.
#[test]
fn secret_field_exposed_reads_a_run_of_capitals_as_one_word() {
    let fires = |field: &str| {
        let sdl = SDL_SECRET_FIELD.replace("token", field);
        let r = lint(vec![("widget-co.graphql", s(&sdl))]);
        r.findings.iter().any(|f| f.rule == "secret-field-exposed")
    };
    for name in [
        "APIKey",
        "PIN",
        "apiKey",
        "API_KEY",
        "XMLHttpApiKey",
        "SECRET",
    ] {
        assert!(fires(name), "{name} is a credential name and must fire");
    }
    for name in [
        "pinned",
        "spinCount",
        "APIKeys",
        "XMLHttpRequest",
        "primaryKey",
    ] {
        assert!(
            !fires(name),
            "{name} is not a credential and must stay silent"
        );
    }
}

#[test]
fn secret_field_exposed_is_silent_on_an_input_argument() {
    // Same argument, no matching response field anywhere: the base SDL's
    // Query field gains a `password` argument but Widget_Co_Widget never
    // gains a field of that shape.
    let sdl = SDL.replace(
        "widget_co_listWidgets(limit: Int):",
        "widget_co_listWidgets(limit: Int, password: String):",
    );
    assert_ne!(sdl, SDL);
    let r = lint(vec![("widget-co.graphql", s(&sdl))]);
    assert!(
        !rules(&r).contains(&"secret-field-exposed".to_string()),
        "{:?}",
        r.findings
    );
}

#[test]
fn secret_field_exposed_is_silent_once_the_field_is_excluded_from_the_schema() {
    // What `fields.exclude: [token]` plus a regenerate produces: `token` is
    // simply not in the rendered type any more, so there is nothing to scan.
    let r = lint(vec![("widget-co.graphql", s(SDL))]);
    assert!(
        !rules(&r).contains(&"secret-field-exposed".to_string()),
        "{:?}",
        r.findings
    );
}

#[test]
fn secret_field_exposed_is_load_bearing_and_silenced_by_a_resolved_decision() {
    // Red: the field is exposed and no decision has reviewed it.
    let red = lint(vec![("widget-co.graphql", s(SDL_SECRET_FIELD))]);
    assert!(
        rules(&red).contains(&"secret-field-exposed".to_string()),
        "{:?}",
        red.findings
    );

    // Green: a resolved secret_fields record naming the exact Type.field
    // covers it, disposition expose.
    let decisions = concat!(
        r#"{"contract_version":1,"decisions":[{"id":"D-0001","#,
        r#""title":"Expose Widget_Co_Widget.token on purpose","status":"resolved","date":"2026-09-29","#,
        r#""resolution":{"decision":"Kept for parity with the vendor's own docs.","by":"agent","at":"2026-09-29"},"#,
        r#""secret_fields":[{"type":"Widget_Co_Widget","field":"token","disposition":"expose","reason":"Kept for parity with the vendor's own docs."}]}]}"#,
    );
    let green = lint(vec![
        ("widget-co.graphql", s(SDL_SECRET_FIELD)),
        (".factory/decisions.json", s(decisions)),
    ]);
    assert!(
        !rules(&green).contains(&"secret-field-exposed".to_string()),
        "{:?}",
        green.findings
    );
}

// ── ADR 0079 Step 2: the write-body-proof layer must be strictly stronger
// than the four shallow lints it replaces before they are retired -- a
// dropped argument whose value happens to recur elsewhere in the SAME
// demanded body. `loose-write-body` only checks whether the matcher is
// exact at all (it is here); `mutation-cases` only checks whether some case
// file passes every GraphQL argument (it does here) -- neither reads
// per-argument placement, so both stay silent on a connector that never
// wires `b` anywhere at all.

#[test]
fn the_new_layer_catches_a_dropped_argument_whose_value_recurs_elsewhere_the_old_lints_miss() {
    let sdl = format!(
        "{}\ntype Mutation {{\n  widget_co_updateThing(id: ID!, a: String, b: String): Widget_Co_Widget\n    @connect(\n      source: \"widget_co\"\n      http: {{\n        PATCH: \"/things/{{$args.id}}\"\n        body: \"a: $args.a\\necho: $args.a\"\n      }}\n      selection: \"id\"\n    )\n}}\n",
        SDL
    );
    let selection = format!(
        "{}  \"patch:/things/{{id}}\":\n    include: true\n    graphql:\n      root: mutation\n      name: updateThing\n",
        SELECTION
    );
    let mut inv = inventory();
    inv["operations"].as_array_mut().unwrap().push(json!({
        "key": "patch:/things/{id}", "operation_id": "updateThing", "method": "PATCH", "path": "/things/{id}",
        "semantics": "write", "provenance": "spec", "confidence": 1, "support": "supported", "support_reason": null
    }));

    let overrides: Vec<(&str, Option<String>)> = vec![
        ("widget-co.graphql", Some(sdl)),
        (".factory/selection.yaml", Some(selection)),
        (
            ".factory/inventory.json",
            Some(graphos_factory_core::json::pretty(&inv)),
        ),
        (
            "tests/cases/update_thing_full.graphql",
            Some(
                "mutation { widget_co_updateThing(id: \"1\", a: \"X\", b: \"X\") { id } }\n"
                    .to_string(),
            ),
        ),
        (
            "tests/cases/update_thing_required.graphql",
            Some("mutation { widget_co_updateThing(id: \"1\") { id } }\n".to_string()),
        ),
        (
            "tests/fixtures/mappings/update_thing_full.json",
            Some(
                json!({
                    "request": {"method": "PATCH", "urlPath": "/things/1",
                        "bodyPatterns": [{"equalToJson": {"a": "X", "echo": "X"}}]},
                    "response": {"status": 200, "jsonBody": {"id": "1"}}
                })
                .to_string(),
            ),
        ),
        (
            "tests/fixtures/mappings/update_thing_required.json",
            Some(
                json!({
                    "request": {"method": "PATCH", "urlPath": "/things/1",
                        "bodyPatterns": [{"equalToJson": {}}]},
                    "response": {"status": 200, "jsonBody": {"id": "1"}}
                })
                .to_string(),
            ),
        ),
    ];

    let r = lint(overrides.clone());
    let msgs: Vec<String> = r
        .findings
        .iter()
        .map(|f| format!("{} {}", f.rule, f.message))
        .collect();
    assert!(
        !msgs
            .iter()
            .any(|m| m.starts_with("loose-write-body") && m.contains("update_thing")),
        "the old rule only checks matcher looseness, not per-argument placement: {:?}",
        msgs
    );
    assert!(
        !msgs
            .iter()
            .any(|m| m.starts_with("mutation-cases") && m.contains("updateThing")),
        "the old rule only checks case-file argument coverage, not stub placement: {:?}",
        msgs
    );

    // Same workspace, the new instrument: `b` is never wired anywhere, and
    // is reported even though its value happens to appear (as `echo`) in
    // the exact body the old rules found unobjectionable.
    let dir = make_workspace(overrides.into_iter().collect());
    let report = graphos_factory_core::request_serialization::report(dir.path());
    let write = report
        .writes
        .iter()
        .find(|w| w.operation == "patch:/things/{id}")
        .expect("updateThing write reported");
    assert!(!write.body_proven, "{:#?}", write);
    assert!(
        write.gaps.iter().any(|g| g.message.contains("(b)")),
        "{:#?}",
        write.gaps
    );
}

// ─── ADR 0095: a behaviour waiver ───────────────────────────────────────────

fn lint_with_waiver(status: &str, path: &str, arg: &str, param: Value) -> LintResult {
    let inv = doc_inventory(Some("List widgets"), None, json!([param]));
    let mut record = json!({
        "id": "D-0001", "title": "t", "status": status, "date": "2026-09-29",
        "omits": [{"operation": "get:/widgets", "direction": "behaviour",
                   "path": path, "reason": "not-applicable"}]
    });
    if status == "resolved" {
        record["resolution"] = json!({"decision": "d"});
    }
    let decisions = json!({"contract_version": 1, "decisions": [record]});
    lint(vec![
        (
            "widget-co.graphql",
            s(&doc_sdl("  \"List widgets.\"\n", arg, VENMO_HTTP)),
        ),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
        (".factory/selection.yaml", s(&doc_selection(None))),
        (".factory/decisions.json", s(&decisions.to_string())),
    ])
}

/// A resolved decision's waiver, naming the operation and `query:x`, is the
/// recorded way out of the omission warning.
#[test]
fn omission_warning_is_quiet_when_a_resolved_decision_waives_it() {
    let r = lint_with_waiver(
        "resolved",
        "query:payment_card_id",
        "paymentCardId: Int",
        venmo_param(false),
    );
    assert!(
        findings_of(&r, "argument-constraints-undocumented").is_empty(),
        "{:?}",
        r.findings
    );
}

/// Only a resolved decision counts, and only for the source it names.
#[test]
fn omission_warning_stays_for_an_open_waiver_or_another_source() {
    for (status, path) in [
        ("open", "query:payment_card_id"),
        ("resolved", "query:amount"),
    ] {
        let r = lint_with_waiver(status, path, "paymentCardId: Int", venmo_param(false));
        assert_eq!(
            findings_of(&r, "argument-constraints-undocumented").len(),
            1,
            "{} {}: {:?}",
            status,
            path,
            r.findings
        );
    }
}

/// The waiver is for the sentence only: the constraint clause is still owed.
#[test]
fn a_waiver_does_not_excuse_the_constraint_clause() {
    let mut param = venmo_param(false);
    param["default"] = json!(7);
    let r = lint_with_waiver(
        "resolved",
        "query:payment_card_id",
        "paymentCardId: Int",
        param,
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("default 7")
            && !hits[0].message.contains("Venmo balance will be used"),
        "{}",
        hits[0].message
    );
}

/// The warning names the way out.
#[test]
fn omission_warning_on_a_documented_argument_names_the_waiver() {
    let inv = doc_inventory(Some("List widgets"), None, json!([venmo_param(false)]));
    let r = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "\"ID of the card.\"\n    paymentCardId: Int",
            VENMO_HTTP,
        ),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-constraints-undocumented");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0]
            .message
            .contains("--omit 'get:/widgets|behaviour|query:payment_card_id|not-applicable'"),
        "{}",
        hits[0].message
    );
}

// ─── the `!` loophole ───────────────────────────────────────────────────────

fn required_lint(arg: &str, param: Value) -> LintResult {
    let inv = doc_inventory(Some("List widgets"), None, json!([param]));
    doc_lint(
        &doc_sdl("  \"List widgets.\"\n", arg, VENMO_HTTP),
        &inv,
        &doc_selection(None),
    )
}

/// `!` on an argument whose source parameter is optional hides what omitting
/// it does: the caller can no longer leave it out. Making the Venmo card
/// required would clear every omission warning without fixing anything.
#[test]
fn a_required_argument_over_an_optional_source_parameter_is_an_error() {
    let r = required_lint("paymentCardId: Int!", venmo_param(false));
    let hits = findings_of(&r, "argument-required-optional-in-source");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert_eq!(hits[0].severity, "error");
    assert!(
        hits[0].message.contains("argument `paymentCardId`")
            && hits[0]
                .message
                .contains("query parameter `payment_card_id`")
            && hits[0]
                .message
                .contains("--omit 'get:/widgets|behaviour|query:payment_card_id|editorial'"),
        "{}",
        hits[0].message
    );
}

/// The three quiet cases: the source requires it too, the argument is
/// optional, and the inventory does not say (no `required` key).
#[test]
fn requiredness_agreeing_or_unknown_is_quiet() {
    for (arg, param) in [
        ("paymentCardId: Int!", venmo_param(true)),
        ("paymentCardId: Int", venmo_param(false)),
        ("paymentCardId: Int", venmo_param(true)),
        (
            "paymentCardId: Int!",
            json!({"name": "payment_card_id", "in": "query", "type": "integer"}),
        ),
    ] {
        let r = required_lint(arg, param.clone());
        assert!(
            findings_of(&r, "argument-required-optional-in-source").is_empty(),
            "{} {}: {:?}",
            arg,
            param,
            r.findings
        );
    }
}

/// A body key's requiredness is its enclosing object's `required` list.
#[test]
fn a_required_argument_over_an_optional_body_key_is_an_error() {
    let mut inv = doc_inventory(Some("Pay"), None, json!([]));
    inv["operations"][0]["request_body"] = json!({"content_type": "application/json", "required": true, "shape_ref": "#/shapes/Payment"});
    inv["shapes"] = json!({"Payment": {"type": "object", "required": ["amount"], "properties": {
        "amount": {"type": "number"},
        "payment_card_id": {"type": "integer"}
    }}});
    let lint_it = |args: &str| {
        doc_lint(
            &doc_sdl(
                "  \"Pay.\"\n",
                args,
                r#"POST: "/widgets", body: "amount: $args.amount payment_card_id: $args.paymentCardId""#,
            ),
            &inv,
            &doc_selection(None),
        )
    };
    let r = lint_it("amount: Float!, paymentCardId: Int!");
    let hits = findings_of(&r, "argument-required-optional-in-source");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("argument `paymentCardId`"),
        "{}",
        hits[0].message
    );
    let r = lint_it("amount: Float!, paymentCardId: Int");
    assert!(findings_of(&r, "argument-required-optional-in-source").is_empty());
}

/// The same resolved `behaviour` waiver that clears the sentence clears a
/// deliberate `!`: it names the operation and the source. An open one, or
/// one for another source, does not.
#[test]
fn a_resolved_behaviour_waiver_clears_a_deliberately_required_argument() {
    for (status, path, expected) in [
        ("resolved", "query:payment_card_id", 0),
        ("open", "query:payment_card_id", 1),
        ("resolved", "query:amount", 1),
    ] {
        let r = lint_with_waiver(status, path, "paymentCardId: Int!", venmo_param(false));
        assert_eq!(
            findings_of(&r, "argument-required-optional-in-source").len(),
            expected,
            "{} {}: {:?}",
            status,
            path,
            r.findings
        );
    }
}

// ─── the mirror: an optional argument over a required source ────────────────

/// A source the vendor requires, exposed as an optional argument: a caller
/// who leaves it out gets a call without it, and the vendor rejects it.
/// A recruiting API's five required body members exposed nullable were this.
#[test]
fn an_optional_argument_over_a_required_source_parameter_is_a_warning() {
    let r = required_lint("paymentCardId: Int", venmo_param(true));
    let hits = findings_of(&r, "argument-optional-required-in-source");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert_eq!(hits[0].severity, "warn");
    assert!(
        hits[0].message.contains("argument `paymentCardId`")
            && hits[0]
                .message
                .contains("query parameter `payment_card_id` is required")
            && hits[0]
                .message
                .contains("--omit 'get:/widgets|behaviour|query:payment_card_id|editorial'"),
        "{}",
        hits[0].message
    );
}

/// Quiet when the value is filled anyway (a GraphQL default, a connector
/// fallback), when the argument is required, when the source leaves it
/// optional, and when the inventory does not say.
#[test]
fn an_optional_argument_that_is_filled_or_agrees_is_quiet() {
    let inv = doc_inventory(Some("List widgets"), None, json!([venmo_param(true)]));
    for (args, http) in [
        ("paymentCardId: Int = 3", VENMO_HTTP),
        (
            "paymentCardId: Int",
            r#"GET: "/widgets", queryParams: "payment_card_id: $args.paymentCardId ?? 3""#,
        ),
        ("paymentCardId: Int!", VENMO_HTTP),
    ] {
        let r = doc_lint(
            &doc_sdl("  \"List widgets.\"\n", args, http),
            &inv,
            &doc_selection(None),
        );
        assert!(
            findings_of(&r, "argument-optional-required-in-source").is_empty(),
            "{} / {}: {:?}",
            args,
            http,
            r.findings
        );
    }
    for param in [
        venmo_param(false),
        json!({"name": "payment_card_id", "in": "query", "type": "integer"}),
    ] {
        let r = required_lint("paymentCardId: Int", param.clone());
        assert!(
            findings_of(&r, "argument-optional-required-in-source").is_empty(),
            "{}: {:?}",
            param,
            r.findings
        );
    }
}

/// A body key's `required` is its enclosing object's list: a top-level key
/// is judged, a key nested in an object that is itself optional is not.
#[test]
fn a_required_body_key_is_judged_at_the_top_level_only() {
    let mut inv = doc_inventory(Some("Pay"), None, json!([]));
    inv["operations"][0]["request_body"] = json!({"content_type": "application/json", "required": true, "shape_ref": "#/shapes/Payment"});
    inv["shapes"] = json!({
        "Payment": {"type": "object", "required": ["amount"], "properties": {
            "amount": {"type": "number"},
            "card": {"$ref": "#/shapes/Card"}
        }},
        "Card": {"type": "object", "required": ["id"], "properties": {"id": {"type": "integer"}}}
    });
    let r = doc_lint(
        &doc_sdl(
            "  \"Pay.\"\n",
            "amount: Float, cardId: Int",
            r#"POST: "/widgets", body: """
              amount: $args.amount
              card: { id: $args.cardId }
              """"#,
        ),
        &inv,
        &doc_selection(None),
    );
    let hits = findings_of(&r, "argument-optional-required-in-source");
    assert_eq!(hits.len(), 1, "{:?}", r.findings);
    assert!(
        hits[0].message.contains("argument `amount`")
            && hits[0].message.contains("body key `amount`"),
        "{}",
        hits[0].message
    );
}

/// The resolved `behaviour` waiver that clears a deliberate `!` clears a
/// deliberately optional argument too.
#[test]
fn a_resolved_behaviour_waiver_clears_a_deliberately_optional_argument() {
    for (status, expected) in [("resolved", 0), ("open", 1)] {
        let r = lint_with_waiver(
            status,
            "query:payment_card_id",
            "paymentCardId: Int",
            venmo_param(true),
        );
        assert_eq!(
            findings_of(&r, "argument-optional-required-in-source").len(),
            expected,
            "{}: {:?}",
            status,
            r.findings
        );
    }
}

// ─── ADR 0113: findings count where resolved decisions count ────────────────

/// A findings.json holding one finding: `status` current or superseded.
fn findings_json(status: &str, affects: &[&str], omits: Value) -> String {
    let mut record = json!({
        "id": "F-0001",
        "title": "The vocabulary is a discriminator",
        "date": "2026-10-01",
        "status": status,
        "body": "kept as String: references/naming.md settles it",
        "cites": "references/naming.md § Enums",
        "source": "agent",
    });
    if !affects.is_empty() {
        record["affects"] = json!(affects);
    }
    if omits.as_array().is_some_and(|o| !o.is_empty()) {
        record["omits"] = omits;
    }
    graphos_factory_core::json::pretty(&json!({"contract_version": 1, "findings": [record]}))
}

/// `closed-enum-as-string` reads the union of resolved decisions and current
/// findings (ADR 0113 §2): a current finding naming the slot silences it,
/// exactly as a resolved decision does; a superseded one does not.
#[test]
fn closed_enum_as_string_quiet_when_a_current_finding_names_the_slot() {
    let sdl = closed_sdl_all_string();
    let found = closed_findings(
        &sdl,
        vec![(
            ".factory/findings.json",
            Some(findings_json(
                "current",
                &["Widget_Co_Widget.state"],
                json!([]),
            )),
        )],
    );
    assert_eq!(found.len(), 3, "{:?}", found);
    assert!(
        !found
            .iter()
            .any(|f| f.message.contains("`Widget_Co_Widget.state`")),
        "{:?}",
        found
    );
    let found = closed_findings(
        &sdl,
        vec![(
            ".factory/findings.json",
            Some(findings_json(
                "superseded",
                &["Widget_Co_Widget.state"],
                json!([]),
            )),
        )],
    );
    assert_eq!(
        found.len(),
        4,
        "a superseded finding counts for nothing: {:?}",
        found
    );
}

/// Lint's description waivers read the union too: a current finding's
/// `behaviour` omit (`not-applicable`) is the recorded way out of the
/// omission warning, as a resolved decision's is.
#[test]
fn omission_warning_is_quiet_when_a_current_finding_waives_it() {
    let inv = doc_inventory(Some("List widgets"), None, json!([venmo_param(false)]));
    let waiver = json!([{"operation": "get:/widgets", "direction": "behaviour",
                         "path": "query:payment_card_id", "reason": "not-applicable"}]);
    for (status, expected) in [("current", 0), ("superseded", 1)] {
        let r = lint(vec![
            (
                "widget-co.graphql",
                s(&doc_sdl(
                    "  \"List widgets.\"\n",
                    "paymentCardId: Int",
                    VENMO_HTTP,
                )),
            ),
            (
                ".factory/inventory.json",
                s(&graphos_factory_core::json::pretty(&inv)),
            ),
            (".factory/selection.yaml", s(&doc_selection(None))),
            (
                ".factory/findings.json",
                s(&findings_json(status, &[], waiver.clone())),
            ),
        ]);
        assert_eq!(
            findings_of(&r, "argument-constraints-undocumented").len(),
            expected,
            "{}: {:?}",
            status,
            r.findings
        );
    }
}

/// `decision-without-alternative` (ADR 0113): a record with no question and
/// fewer than two choices is asked to gain them; one with either is not,
/// and a superseded record draws nothing.
#[test]
fn a_decision_without_an_alternative_is_a_warning() {
    let record = |id: &str, status: &str, extra: Value| {
        let mut r = json!({"id": id, "title": format!("record {}", id), "status": status, "date": "2026-10-01"});
        if status != "open" {
            r["resolution"] = json!({"decision": "d"});
        }
        for (k, v) in extra.as_object().unwrap() {
            r[k] = v.clone();
        }
        r
    };
    let log = json!({"contract_version": 1, "decisions": [
        record("D-0001", "resolved", json!({})),
        record("D-0002", "resolved", json!({"question": "Which?"})),
        record("D-0003", "open", json!({"choices": [{"id": "a", "label": "A"}, {"id": "b", "label": "B"}]})),
        record("D-0004", "open", json!({"choices": [{"id": "a", "label": "A"}]})),
        record("D-0005", "superseded", json!({})),
    ]});
    let r = lint(vec![(".factory/decisions.json", s(&log.to_string()))]);
    let found = findings_of(&r, "decision-without-alternative");
    let ids: Vec<&str> = found
        .iter()
        .map(|f| f.message.split(' ').next().unwrap())
        .collect();
    assert_eq!(ids, vec!["D-0001", "D-0004"], "{:?}", found);
    assert!(found.iter().all(|f| f.severity == "warn"));
    assert_eq!(found[0].file.as_deref(), Some(".factory/decisions.json"));
    assert!(
        found[0]
            .message
            .contains("decisions migrate --split --sorted FILE"),
        "{}",
        found[0].message
    );
}

/// ADR 0113 retired `waiver-undecided` with `override-undecided`: a waiver
/// that names no decision draws nothing; its `reason` carries the why.
#[test]
fn a_waiver_with_no_decision_draws_nothing() {
    let sel = format!(
        "{}waivers:\n  - where: \"tests/fixtures/mappings/list_widgets.json\"\n    status: unchecked\n    reason: no decision\n",
        SELECTION
    );
    let r = lint(vec![(".factory/selection.yaml", s(&sel))]);
    assert!(
        !rules(&r).contains(&"waiver-undecided".to_string()),
        "{:?}",
        rules(&r)
    );
}

/// A findings.json that does not load is lint's to report, and blanks
/// nothing: the union falls back to the decisions alone, so a resolved
/// decision's `affects` still silences its slot (ADR 0113 §2, review).
#[test]
fn an_invalid_findings_file_is_reported_and_decisions_keep_counting() {
    let sdl = closed_sdl_all_string();
    let files = |findings: &str| -> Vec<(&'static str, Option<String>)> {
        vec![
            (
                ".factory/decisions.json",
                Some(decisions_json(&["Widget_Co_Widget.state"], true)),
            ),
            (".factory/findings.json", Some(findings.to_string())),
        ]
    };
    for broken in [
        "{ not json",
        "{\"contract_version\": 1, \"findings\": [{\"id\": \"D-0001\"}]}",
    ] {
        let found = closed_findings(&sdl, files(broken));
        assert_eq!(found.len(), 3, "{}: {:?}", broken, found);
        assert!(
            !found
                .iter()
                .any(|f| f.message.contains("`Widget_Co_Widget.state`")),
            "{}: the decision still counts: {:?}",
            broken,
            found
        );
        let mut all = vec![
            ("widget-co.graphql", s(&sdl)),
            (
                ".factory/inventory.json",
                s(&graphos_factory_core::json::pretty(&closed_inventory())),
            ),
            (".factory/selection.yaml", s(CLOSED_SELECTION)),
        ];
        all.extend(files(broken));
        let r = lint(all);
        let unreadable: Vec<_> = r
            .findings
            .iter()
            .filter(|f| f.rule == "unreadable-file")
            .collect();
        assert_eq!(unreadable.len(), 1, "{}: {:?}", broken, r.findings);
        assert_eq!(unreadable[0].severity, "error");
        assert_eq!(
            unreadable[0].file.as_deref(),
            Some(".factory/findings.json")
        );
        assert!(
            unreadable[0].message.starts_with(".factory/findings.json"),
            "{}",
            unreadable[0].message
        );
    }
}

/// A copy of the public pilot with one decision added since ADR 0118, and
/// that decision's random id.
fn gitea_with_a_random_decision() -> (tempfile::TempDir, String) {
    let from = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../pilots/graphos/gitea");
    let dir = tempfile::tempdir().unwrap();
    assert!(std::process::Command::new("cp")
        .arg("-R")
        .arg(format!("{}/.", from.display()))
        .arg(dir.path())
        .status()
        .unwrap()
        .success());
    let argv: Vec<String> = [
        "add",
        &dir.path().to_string_lossy(),
        "--title",
        "Waive the undocumented 404 body",
        "--question",
        "Waive it?",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    assert_eq!(graphos_factory_core::cmd::decisions::main(&argv), 0);
    let log = graphos_factory_core::decisions::load(dir.path(), None).unwrap();
    let id = graphos_factory_core::json::get_arr(&log, "decisions")
        .into_iter()
        .flatten()
        .filter_map(|r| r["id"].as_str())
        .find(|id| graphos_factory_core::record_log::is_random(id))
        .unwrap()
        .to_string();
    (dir, id)
}

fn bare_lint(dir: &Path) -> LintResult {
    lint_workspace(
        dir,
        &LintOptions {
            schemas_dir: None,
            skip_evidence: true,
            target: &graphos_factory_core::target::BARE,
        },
    )
}

/// A waiver's hand-written `decision:` may cite a random id (ADR 0118): the
/// selection schema takes it, so lint has no contract finding, reconcile
/// is clean and `selection review` reads the file, as with `D-0010`.
#[test]
fn a_waiver_cites_a_random_decision_id_and_the_selection_still_validates() {
    let (dir, id) = gitea_with_a_random_decision();
    let d = dir.path();
    let rel = ".factory/selection.yaml";
    let text = std::fs::read_to_string(d.join(rel)).unwrap();
    assert!(text.contains("    decision: D-0010\n"));
    std::fs::write(
        d.join(rel),
        text.replacen(
            "    decision: D-0010\n",
            &format!("    decision: {}\n", id),
            1,
        ),
    )
    .unwrap();
    let contract: Vec<_> = bare_lint(d)
        .findings
        .into_iter()
        .filter(|f| f.rule == "contract")
        .map(|f| f.message)
        .collect();
    assert!(contract.is_empty(), "{:?}", contract);
    assert_eq!(
        graphos_factory_core::cmd::reconcile::main(&[d.to_string_lossy().to_string()]),
        0
    );
    assert!(graphos_factory_core::cmd::selection_review::review(d, None, None, None).is_ok());
}

/// A stray file in `.factory/decisions/` (a merge tool's `.orig`) leaves the
/// whole log unread; lint says so as an error naming the file, rather than
/// reading an empty log and reporting only what the missing decisions
/// leave behind. The findings log is pinned the same way.
#[test]
fn a_stray_file_in_the_decision_log_is_an_error_naming_it() {
    let (dir, id) = gitea_with_a_random_decision();
    let d = dir.path();
    assert!(!bare_lint(d)
        .findings
        .iter()
        .any(|f| f.rule == "unreadable-file"));
    let stray = format!(".factory/decisions/{}-x.json.orig", id);
    std::fs::write(d.join(&stray), "{}").unwrap();
    let r = bare_lint(d);
    let unreadable: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "unreadable-file")
        .collect();
    assert_eq!(unreadable.len(), 1, "{:?}", r.findings);
    assert_eq!(unreadable[0].severity, "error");
    assert_eq!(unreadable[0].file.as_deref(), Some(stray.as_str()));
    assert!(
        unreadable[0].message.starts_with(&stray)
            && unreadable[0].message.contains("the whole log is unread"),
        "{}",
        unreadable[0].message
    );
    std::fs::remove_file(d.join(&stray)).unwrap();
    std::fs::create_dir_all(d.join(".factory/findings")).unwrap();
    std::fs::write(d.join(".factory/findings/F-abc123-y.json.rej"), "").unwrap();
    let r = bare_lint(d);
    let files: Vec<_> = r
        .findings
        .iter()
        .filter(|f| f.rule == "unreadable-file")
        .map(|f| f.file.clone())
        .collect();
    assert_eq!(
        files,
        [Some(".factory/findings/F-abc123-y.json.rej".to_string())]
    );
}

// ─── success-path-unresolved ────────────────────────────────────────────────

/// Two operations on one source; `@SOURCE_SUCCESS@` and `@CONNECT_SUCCESS@`
/// are whole lines: an `isSuccess: "…"` argument, or empty.
const SUCCESS_SDL: &str = r#"extend schema
  @link(url: "https://specs.apollo.dev/federation/v2.12", import: ["@key", "@tag"])
  @link(url: "https://specs.apollo.dev/connect/v0.3", import: ["@source", "@connect"])

@source(
  name: "widget_co"
  http: {
    baseURL: "{{BASE_URL}}"
    headers: [{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }]
  }
@SOURCE_SUCCESS@
)

type Widget_Co_Widget {
  id: ID
  name: String
}

type Query {
  widget_co_listWidgets: [Widget_Co_Widget]
    @connect(source: "widget_co", http: { GET: "/widgets" }, selection: "$.widgets { id name }")
  widget_co_getWidget(id: ID!): Widget_Co_Widget
    @connect(
      source: "widget_co"
      http: { GET: "/widgets/{$args.id}" }
      selection: "id name"
@CONNECT_SUCCESS@
    )
}
"#;

/// The list body carries `ok`; the by-id body is the widget alone, unless
/// `get_shape` replaces it.
fn success_inventory(get_shape: Value) -> Value {
    let mut inv = errpath_inventory(json!([]), json!([]), json!({}));
    inv["operations"][0]["response"] = json!({"status": "200", "content_type": "application/json", "shape_ref": "#/shapes/WidgetList"});
    inv["operations"][1]["response"] = json!({"status": "200", "content_type": "application/json", "shape_ref": "#/shapes/Widget"});
    inv["shapes"] = json!({
        "WidgetList": {"type": "object", "properties": {"ok": {"type": "boolean"}, "widgets": {"type": "array", "items": {"$ref": "#/shapes/Widget"}}}},
        "Widget": get_shape
    });
    inv
}

fn widget_shape() -> Value {
    json!({"type": "object", "properties": {"id": {"type": "string"}, "name": {"type": "string"}}})
}

fn success_findings(
    source: &str,
    connect: &str,
    inv: &Value,
) -> Vec<graphos_factory_core::lint::Finding> {
    let sdl = SUCCESS_SDL
        .replace("@SOURCE_SUCCESS@", source)
        .replace("@CONNECT_SUCCESS@", connect);
    lint(vec![
        ("widget-co.graphql", s(&sdl)),
        (".factory/selection.yaml", s(ERRPATH_SELECTION)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(inv)),
        ),
    ])
    .findings
    .into_iter()
    .filter(|f| f.rule == "success-path-unresolved")
    .collect()
}

/// A source-wide `isSuccess` reading a key one operation's success body
/// lacks: that operation's every success would become a connector error.
#[test]
fn a_source_is_success_path_one_body_lacks_is_a_warning_naming_it() {
    let hits = success_findings(
        "  isSuccess: \"$.ok\"",
        "",
        &success_inventory(widget_shape()),
    );
    assert_eq!(hits.len(), 1, "{:?}", hits);
    assert_eq!(hits[0].severity, "warn");
    assert!(
        hits[0]
            .message
            .contains("`$.ok` in the @source(name: \"widget_co\") isSuccess")
            && hits[0]
                .message
                .contains("1 of the 2 operations it decides (get:/widgets/{id})"),
        "{}",
        hits[0].message
    );
    let sdl = SUCCESS_SDL.replace("@SOURCE_SUCCESS@", "  isSuccess: \"$.ok\"");
    assert_eq!(hits[0].line, Some(line_containing(&sdl, "isSuccess")));
}

/// The quiet cases: a `??` fallback, a body that cannot say what it
/// carries, every body carrying the key, and an operation that decides for
/// itself (its own isSuccess takes it out of the source's set).
#[test]
fn a_guarded_or_unknowable_or_overridden_success_path_is_quiet() {
    let open = json!({"type": "object", "additionalProperties": true});
    let with_ok = json!({"type": "object", "properties": {"ok": {"type": "boolean"}, "id": {"type": "string"}}});
    for (source, connect, shape) in [
        ("  isSuccess: \"$.ok ?? true\"", "", widget_shape()),
        ("  isSuccess: \"$.ok\"", "", open),
        ("  isSuccess: \"$.ok\"", "", with_ok.clone()),
        (
            "  isSuccess: \"$.ok\"",
            "      isSuccess: \"$.id\"",
            widget_shape(),
        ),
    ] {
        let hits = success_findings(source, connect, &success_inventory(shape.clone()));
        assert!(
            hits.is_empty(),
            "{} / {} / {}: {:?}",
            source,
            connect,
            shape,
            hits
        );
    }
}

/// A `@connect`'s own isSuccess is judged against that operation's body.
#[test]
fn a_connect_is_success_path_its_body_lacks_is_a_warning() {
    let hits = success_findings(
        "",
        "      isSuccess: \"$.success\"",
        &success_inventory(widget_shape()),
    );
    assert_eq!(hits.len(), 1, "{:?}", hits);
    assert!(
        hits[0].message.starts_with(
            "get:/widgets/{id}: `$.success` in the `Query.widget_co_getWidget` @connect isSuccess"
        ) && hits[0].message.contains("it carries: id, name"),
        "{}",
        hits[0].message
    );
}

// ─── wire-key-undocumented ──────────────────────────────────────────────────

fn wire_key_findings(http: &str, params: Value) -> Vec<graphos_factory_core::lint::Finding> {
    let inv = doc_inventory(Some("List widgets"), None, params);
    doc_lint(
        &doc_sdl("  \"List widgets.\"\n", "id: ID, after: String", http),
        &inv,
        &doc_selection(None),
    )
    .findings
    .into_iter()
    .filter(|f| f.rule == "wire-key-undocumented")
    .collect()
}

fn query_param(name: &str) -> Value {
    json!({"name": name, "in": "query", "required": false, "type": "string"})
}

/// incidentio's schedule entries: the connector sent `after` where the spec
/// has `entry_window_start`, and the scaffolded stub demanded `after` too.
#[test]
fn a_query_key_the_source_does_not_document_is_a_warning() {
    let params = json!([
        query_param("schedule_id"),
        query_param("entry_window_start")
    ]);
    for http in [
        r#"GET: "/widgets", queryParams: "schedule_id: $args.id after: $args.after""#,
        r#"GET: "/widgets?schedule_id={$args.id}&after={$args.after}""#,
    ] {
        let hits = wire_key_findings(http, params.clone());
        assert_eq!(hits.len(), 1, "{}: {:?}", http, hits);
        assert_eq!(hits[0].severity, "warn");
        assert!(
            hits[0].message.contains("sends query key `after`")
                && hits[0]
                    .message
                    .contains("(documented: entry_window_start, schedule_id)"),
            "{}",
            hits[0].message
        );
    }
}

/// Documented keys, a `[]` list suffix the source leaves off, a key the
/// source documents with its suffix (pagerduty's `statuses[]`), and a key
/// built from an expression are quiet.
#[test]
fn documented_list_suffixed_and_dynamic_query_keys_are_quiet() {
    let params = json!([
        query_param("schedule_id"),
        query_param("after"),
        query_param("statuses[]")
    ]);
    for http in [
        r#"GET: "/widgets", queryParams: "schedule_id: $args.id after: $args.after""#,
        r#"GET: "/widgets", queryParams: "\"after[]\": $args.after""#,
        r#"GET: "/widgets", queryParams: "\"statuses[]\": $args.after""#,
        r#"GET: "/widgets?schedule_id={$args.id}&{$args.after}""#,
    ] {
        let hits = wire_key_findings(http, params.clone());
        assert!(hits.is_empty(), "{}: {:?}", http, hits);
    }
}

/// A single-quoted key, a member of a documented object parameter (dotted
/// or deepObject), and a pair the operation's own path carries are the
/// source's keys too.
#[test]
fn quoted_object_member_and_path_carried_query_keys_are_quiet() {
    let params = json!([query_param("filter_by"), query_param("ids[]")]);
    for http in [
        r#"GET: "/widgets", queryParams: "'ids[]': $args.after""#,
        r#"GET: "/widgets", queryParams: "\"filter_by.state\": $args.after""#,
        r#"GET: "/widgets", queryParams: "\"filter_by[state]\": $args.after""#,
    ] {
        let hits = wire_key_findings(http, params.clone());
        assert!(hits.is_empty(), "{}: {:?}", http, hits);
    }
    let mut inv = doc_inventory(Some("List widgets"), None, json!([]));
    inv["operations"][0]["path"] = json!("/?operation=user");
    let hits: Vec<_> = doc_lint(
        &doc_sdl(
            "  \"List widgets.\"\n",
            "id: ID",
            r#"GET: "/?operation=user""#,
        ),
        &inv,
        &doc_selection(None),
    )
    .findings
    .into_iter()
    .filter(|f| f.rule == "wire-key-undocumented")
    .collect();
    assert!(hits.is_empty(), "{:?}", hits);
}

/// The source requires the suffix and the connector drops it: on the wire
/// `statuses` is not `statuses[]`.
#[test]
fn a_list_suffix_the_connector_drops_is_reported() {
    let params = json!([query_param("statuses[]")]);
    let hits = wire_key_findings(
        r#"GET: "/widgets", queryParams: "statuses: $args.after""#,
        params,
    );
    assert_eq!(hits.len(), 1, "{:?}", hits);
    assert!(
        hits[0].message.contains("sends query key `statuses`"),
        "{}",
        hits[0].message
    );
}

// ─── auth-scheme-drift ──────────────────────────────────────────────────────

fn scheme(kind: &str, header: Option<&str>, prefix: Option<&str>) -> Value {
    json!({"kind": kind, "header": header, "prefix": prefix, "scheme_name": kind, "source": "spec"})
}

/// The core fixture sends `Authorization: Bearer {{AUTH_EXPR}}` unless
/// `header_line` replaces that header entry.
fn auth_findings(
    schemes: Value,
    header_line: Option<&str>,
) -> Vec<graphos_factory_core::lint::Finding> {
    let mut inv = inventory();
    inv["api"]["auth"] = schemes;
    let sdl = match header_line {
        Some(h) => SDL.replace(
            r#"{ name: "Authorization", value: "Bearer {{AUTH_EXPR}}" }"#,
            h,
        ),
        None => SDL.to_string(),
    };
    lint(vec![
        ("widget-co.graphql", s(&sdl)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&inv)),
        ),
    ])
    .findings
    .into_iter()
    .filter(|f| f.rule == "auth-scheme-drift")
    .collect()
}

/// A prefix none of the documented schemes for that header gives, and a
/// credential in a header no scheme documents, are warnings.
#[test]
fn a_credential_header_the_documented_schemes_contradict_is_a_warning() {
    let hits = auth_findings(
        json!([scheme("basic", Some("Authorization"), Some("Basic "))]),
        None,
    );
    assert_eq!(hits.len(), 1, "{:?}", hits);
    assert_eq!(hits[0].severity, "warn");
    assert!(
        hits[0].message.contains("sent as `Bearer {{AUTH_EXPR}}`")
            && hits[0].message.contains("(`Basic `)"),
        "{}",
        hits[0].message
    );
    let hits = auth_findings(
        json!([scheme("bearer", Some("Authorization"), Some("Bearer "))]),
        Some(r#"{ name: "X-Api-Key", value: "{{AUTH_EXPR}}" }"#),
    );
    assert_eq!(hits.len(), 1, "{:?}", hits);
    assert!(
        hits[0].message.contains("in the `x-api-key` header")
            && hits[0]
                .message
                .contains("documented credential headers: authorization"),
        "{}",
        hits[0].message
    );
}

/// Quiet: a matching prefix, any one of several documented schemes
/// matching, a scheme that records no prefix, and a source that documents
/// no header-borne credential.
#[test]
fn a_credential_header_some_documented_scheme_allows_is_quiet() {
    for (schemes, header) in [
        (
            json!([scheme("bearer", Some("Authorization"), Some("Bearer "))]),
            None,
        ),
        (
            json!([
                scheme("basic", Some("Authorization"), Some("Basic ")),
                scheme("oauth2", Some("Authorization"), Some("Bearer "))
            ]),
            Some(r#"{ name: "Authorization", value: "Basic {{AUTH_EXPR}}" }"#),
        ),
        (
            json!([scheme("api_key", Some("Authorization"), None)]),
            Some(r#"{ name: "Authorization", value: "Token token={{AUTH_EXPR}}" }"#),
        ),
        (json!([scheme("api_key", None, None)]), None),
        (json!([]), None),
    ] {
        let hits = auth_findings(schemes.clone(), header);
        assert!(hits.is_empty(), "{} / {:?}: {:?}", schemes, header, hits);
    }
}

// ─── connector-path-undocumented ────────────────────────────────────────────

fn connector_path_findings(
    get_shape: Value,
    workspace: &str,
) -> Vec<graphos_factory_core::lint::Finding> {
    let sdl = SUCCESS_SDL
        .replace("@SOURCE_SUCCESS@", "")
        .replace("@CONNECT_SUCCESS@", "");
    lint(vec![
        ("widget-co.graphql", s(&sdl)),
        (".factory/workspace.yaml", s(workspace)),
        (".factory/selection.yaml", s(ERRPATH_SELECTION)),
        (
            ".factory/inventory.json",
            s(&graphos_factory_core::json::pretty(&success_inventory(
                get_shape,
            ))),
        ),
    ])
    .findings
    .into_iter()
    .filter(|f| f.rule == "connector-path-undocumented")
    .collect()
}

/// Both connectors select `name`, which the shared `Widget` body does not
/// carry: a scaffolded stub would carry it anyway, and the vendor would
/// not. One warning per operation, with the path as each one spells it.
#[test]
fn a_selected_path_the_documented_body_lacks_is_a_warning() {
    let hits = connector_path_findings(
        json!({"type": "object", "properties": {"id": {"type": "string"}}}),
        WORKSPACE,
    );
    let messages: Vec<&str> = hits.iter().map(|f| f.message.as_str()).collect();
    assert_eq!(hits.len(), 2, "{:?}", messages);
    assert!(hits.iter().all(|f| f.severity == "warn"));
    assert!(
        messages[0]
            .starts_with("get:/widgets: the Query.widget_co_listWidgets connector selects 1 path")
            && messages[0].contains("(WidgetList) does not have: `widgets[]>name`")
            && messages[1].starts_with(
                "get:/widgets/{id}: the Query.widget_co_getWidget connector selects 1 path"
            )
            && messages[1].contains("(Widget) does not have: `name`"),
        "{:?}",
        messages
    );
}

/// Quiet when every path is documented, when the body is open, and when the
/// shapes were inferred from samples (intake: discovered).
#[test]
fn documented_open_or_inferred_paths_are_quiet() {
    let id_only = json!({"type": "object", "properties": {"id": {"type": "string"}}});
    for (shape, workspace) in [
        (widget_shape(), WORKSPACE.to_string()),
        (
            json!({"type": "object", "additionalProperties": true}),
            WORKSPACE.to_string(),
        ),
        (
            id_only,
            WORKSPACE.replace("intake: spec", "intake: discovered"),
        ),
    ] {
        let hits = connector_path_findings(shape.clone(), &workspace);
        assert!(hits.is_empty(), "{}: {:?}", shape, hits);
    }
}

/// A recruiting API's custom-fields read: the 200 body is a `oneOf` whose error
/// branch carries `success` and whose success branch does not, so the
/// unresolved union is reported. When both branches carry it, or the
/// inventory resolved the success branch and it carries it, it is quiet.
#[test]
fn a_success_or_error_union_is_judged_branch_by_branch() {
    let union = |success_branch: Value| {
        json!({"oneOf": [
            success_branch,
            {"type": "object", "properties": {"success": {"type": "boolean"}, "errorInfo": {"type": "object"}}}
        ]})
    };
    let fields_only = json!({"type": "object", "properties": {"fields": {"type": "array", "items": {"type": "string"}}}});
    let hits = success_findings(
        "",
        "      isSuccess: \"$.success\"",
        &success_inventory(union(fields_only.clone())),
    );
    assert_eq!(hits.len(), 1, "{:?}", hits);
    assert!(
        hits[0].message.contains("(it carries: fields)"),
        "{}",
        hits[0].message
    );

    let flagged = json!({"type": "object", "properties": {"success": {"type": "boolean"}, "fields": {"type": "array", "items": {"type": "string"}}}});
    assert!(success_findings(
        "",
        "      isSuccess: \"$.success\"",
        &success_inventory(union(flagged.clone()))
    )
    .is_empty());

    let mut resolved = success_inventory(union(fields_only));
    resolved["shapes"]["Resolved"] = flagged;
    resolved["operations"][1]["response"]["referenced_shape"] = json!("#/shapes/Resolved");
    assert!(success_findings("", "      isSuccess: \"$.success\"", &resolved).is_empty());
}
