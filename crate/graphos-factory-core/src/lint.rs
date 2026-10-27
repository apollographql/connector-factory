//! Workspace lint: the mechanical invariants an agent cannot be trusted to
//! keep consistent across dozens of operations by memory alone.
//!
//! Every rule exists because breaking it breaks something outside the tests:
//! rendering (placeholders, one @source), supergraph composition
//! (prefixes/snake_case), the schema's contract (@tag names, opaque JSON),
//! or the claim that an operation was validated at all (coverage). A
//! target's rules run after these, and a target may adjust one of these
//! (ADR 0114).

use crate::graphql::{
    directives, doc_comment_before, line_of, placeholders, root_fields, type_declarations,
};
use crate::json::{get, get_arr, get_obj, get_str, truthy, Object};
use regex::Regex;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, serde::Serialize)]
pub struct Finding {
    pub severity: String,
    pub rule: String,
    pub message: String,
    pub file: Option<String>,
    pub line: Option<usize>,
    /// Structured detail for a finding that aggregates several facts
    /// (`failure-case-missing`'s per-status list); absent for the rest, so
    /// their JSON is unchanged.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<Value>,
    /// Whose contract the finding belongs to: `core`, or the name of the
    /// target whose rule produced it.
    pub origin: String,
}

#[derive(Default)]
pub struct Findings {
    pub items: Vec<Finding>,
    /// The `origin` the next findings carry; `None` is the core.
    pub origin: Option<&'static str>,
}

impl Findings {
    pub fn add(
        &mut self,
        severity: &str,
        rule: &str,
        message: impl Into<String>,
        file: Option<&str>,
        line: Option<usize>,
    ) {
        self.items.push(Finding {
            severity: severity.into(),
            rule: rule.into(),
            message: message.into(),
            file: file.map(str::to_string),
            line,
            detail: None,
            origin: self.origin.unwrap_or("core").to_string(),
        });
    }
    pub fn error(
        &mut self,
        rule: &str,
        message: impl Into<String>,
        file: Option<&str>,
        line: Option<usize>,
    ) {
        self.add("error", rule, message, file, line);
    }
    pub fn warn(
        &mut self,
        rule: &str,
        message: impl Into<String>,
        file: Option<&str>,
        line: Option<usize>,
    ) {
        self.add("warn", rule, message, file, line);
    }
}

pub struct LintResult {
    pub findings: Vec<Finding>,
    pub errors: usize,
    pub warnings: usize,
}

pub fn read(file: &Path) -> String {
    std::fs::read_to_string(file).unwrap_or_default()
}

/// One workspace file, for a lint rule to read. `.factory/*` goes through
/// custody (ADR 0025), so a symlinked input is a finding rather than a file
/// silently read from wherever it points; everything else is the user's own
/// authored file.
fn read_workspace_file(dir: &Path, name: &str, findings: &mut Findings) -> Option<String> {
    if Path::new(name).starts_with(crate::factory_io::STATE_DIR) {
        return match crate::factory_io::read_to_string_optional(dir, name) {
            Ok(text) => text,
            Err(e) => {
                findings.error("unreadable-file", e.to_string(), Some(name), None);
                None
            }
        };
    }
    let file = dir.join(name);
    file.exists().then(|| read(&file))
}

pub fn load_yaml_file(dir: &Path, name: &str, findings: &mut Findings) -> Option<Value> {
    let text = read_workspace_file(dir, name, findings)?;
    match crate::yaml::parse(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            findings.error(
                "unreadable-file",
                format!("{} is not valid YAML: {}", name, e),
                Some(name),
                None,
            );
            None
        }
    }
}

fn load_json_file(dir: &Path, name: &str, findings: &mut Findings) -> Option<Value> {
    let text = read_workspace_file(dir, name, findings)?;
    match crate::json::parse(&text) {
        Ok(v) => Some(v),
        Err(e) => {
            findings.error(
                "unreadable-file",
                format!("{} is not valid JSON: {}", name, e),
                Some(name),
                None,
            );
            None
        }
    }
}

fn contract_check(
    value: &Value,
    schema_file: &str,
    name: &str,
    findings: &mut Findings,
    schemas_dir: Option<&Path>,
) {
    if let Some(schema) = crate::schemas::load(schema_file, schemas_dir) {
        for error in crate::jsonschema::validate(value, &schema) {
            findings.error("contract", format!("{}{}", name, error), Some(name), None);
        }
    }
}

/// Whether the schema applies `directive` (`@key`): the name, not inside a
/// quoted string such as an `@link` import list and not on a `#` comment
/// line, followed by anything that cannot continue a name.
fn applies_directive(sdl: &str, directive: &str) -> bool {
    sdl.lines()
        .filter(|l| !l.trim_start().starts_with('#'))
        .any(|line| {
            line.match_indices(directive).any(|(at, _)| {
                let before = line[..at].chars().next_back();
                let after = line[at + directive.len()..].chars().next();
                before != Some('"')
                    && !after.is_some_and(|c| c.is_ascii_alphanumeric() || c == '_' || c == '"')
            })
        })
}

fn lint_schema(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    target: &crate::target::Target,
    foreign: &std::collections::BTreeSet<String>,
    findings: &mut Findings,
) {
    let sources = directives(sdl, "source");
    if sources.is_empty() {
        findings.error(
            "missing-source",
            "the schema declares no @source",
            Some(schema_file),
            None,
        );
    }
    if sources.len() > 1 {
        findings.error(
            "multiple-sources",
            format!(
                "the schema declares {} @source directives; this target renders exactly one",
                sources.len()
            ),
            Some(schema_file),
            Some(sources[1].line),
        );
    }
    let raw_sources = Regex::new(r"@source\s*\(").unwrap().find_iter(sdl).count();
    if raw_sources > sources.len() {
        findings.error(
            "commented-source",
            format!(
                "the file contains {} occurrences of \"@source(\" but only {} real directive(s); this target counts the raw text, so a commented-out one is a second source",
                raw_sources,
                sources.len()
            ),
            Some(schema_file),
            None,
        );
    }
    let name_re = Regex::new(r#"name\s*:\s*"([^"]*)""#).unwrap();
    let service = get_str(workspace, "service");
    // The first source's name keys the e2e router config's
    // `connectors.sources.<subgraph>.<source>` entry, so a wrong one sends
    // e2e to the real host: an error for every target. A later source is a
    // separate rule a target that allows more than one can relax.
    for (i, source) in sources.iter().enumerate() {
        let name = name_re.captures(&source.args).map(|m| m[1].to_string());
        if name.as_deref() != service {
            findings.error(
                if i == 0 {
                    "source-name"
                } else {
                    "source-name-secondary"
                },
                format!(
                    "@source(name: \"{}\") must equal workspace.service \"{}\" — rover derives join__Graph from it",
                    name.unwrap_or_else(|| "?".to_string()),
                    service.unwrap_or("undefined")
                ),
                Some(schema_file),
                Some(source.line),
            );
        }
    }

    for p in placeholders(sdl) {
        if !target.placeholders.contains(&p.name.as_str()) {
            findings.error(
                "unknown-placeholder",
                format!(
                    "{{{{{}}}}} is not one of this target's placeholders ({}); an unknown placeholder cannot be rendered",
                    p.name,
                    target.placeholders.join(", ")
                ),
                Some(schema_file),
                Some(p.line),
            );
        }
        if p.name == "TOKEN_VAR" {
            findings.error(
                "legacy-placeholder",
                "{{TOKEN_VAR}} is legacy flat auth; new templates use {{AUTH_EXPR}}",
                Some(schema_file),
                Some(p.line),
            );
        }
    }
    for _ in Regex::new(r"\{\s*\$env[^}]*\{\{\s*AUTH_EXPR\s*\}\}[^}]*\}")
        .unwrap()
        .find_iter(sdl)
    {
        findings.error("wrapped-auth-expr", "{{AUTH_EXPR}} is already a complete Connectors expression; do not wrap it in {$env...}", Some(schema_file), None);
    }
    for _ in Regex::new(r#""\{\{\s*AUTH_EXPR\s*\}\}""#)
        .unwrap()
        .find_iter(sdl)
    {
        findings.warn(
            "quoted-auth-expr",
            "a bare \"{{AUTH_EXPR}}\" header value is only correct when the API takes the credential with no scheme prefix",
            Some(schema_file),
            None,
        );
    }

    let link = Regex::new(r#"@link\(\s*url:\s*"https://specs\.apollo\.dev/connect/(v[\d.]+)""#)
        .unwrap()
        .captures(sdl)
        .map(|m| m[1].to_string());
    if let (Some(pin), Some(l)) = (get_str(workspace, "connect_spec"), link.as_deref()) {
        if l != pin {
            findings.error(
                "connect-spec-drift",
                format!(
                    "the schema links connect/{} but workspace.yaml pins {}",
                    l, pin
                ),
                Some(schema_file),
                None,
            );
        }
    }
    let fed = Regex::new(r#"@link\(\s*url:\s*"https://specs\.apollo\.dev/federation/v([\d.]+)""#)
        .unwrap()
        .captures(sdl)
        .map(|m| m[1].to_string());
    // federation_version is the composition PLUGIN pin (matched exactly
    // against supergraph.yaml by `render`) -- a different axis from the
    // schema's own linked federation SPEC version, and the two are only
    // guaranteed to coincide when a workspace has not opted into a newer
    // plugin than its linked spec (composition-version-vs-fed-spec).
    // federation_spec_version records the spec version explicitly for
    // exactly that case; fall back to comparing against federation_version
    // itself for a legacy workspace that has not set it, unchanged from
    // this rule's original behavior.
    // A target that fixes the spec version (ComposeConfig) holds the link
    // to it; otherwise the workspace's own pin does.
    if let (Some(pin), Some(f)) = (target.compose.federation_spec_version, fed.as_deref()) {
        if !pin.starts_with(f) {
            findings.error(
                "federation-drift",
                format!(
                    "the schema links federation/v{} but this target composes federation {}",
                    f, pin
                ),
                Some(schema_file),
                None,
            );
        }
    }
    let spec_pin = get(workspace, "federation_spec_version")
        .map(value_str)
        .or_else(|| get(workspace, "federation_version").map(value_str));
    if let (Some(pin), Some(f)) = (spec_pin, fed.as_deref()) {
        if !pin.starts_with(f) {
            findings.error(
                "federation-drift",
                format!(
                    "the schema links federation/v{} but workspace.yaml pins {} ({})",
                    f,
                    pin,
                    if get(workspace, "federation_spec_version").is_some() {
                        "federation_spec_version"
                    } else {
                        "federation_version, no federation_spec_version set"
                    }
                ),
                Some(schema_file),
                None,
            );
        }
    }
    if fed.is_some() && !target.compose.link_imports.is_empty() {
        let imports: Vec<String> = Regex::new(
            r#"@link\(\s*url:\s*"https://specs\.apollo\.dev/federation/v[\d.]+"\s*,?\s*import:\s*\[([^\]]*)\]"#,
        )
        .unwrap()
        .captures(sdl)
        .map(|m| {
            Regex::new(r#""([^"]+)""#)
                .unwrap()
                .captures_iter(&m[1])
                .map(|c| c[1].to_string())
                .collect()
        })
        .unwrap_or_default();
        // ComposeConfig::link_imports is the set a schema may import: a
        // directive of it the schema applies without importing is drift; an
        // import the schema never applies is not.
        let missing: Vec<&str> = target
            .compose
            .link_imports
            .iter()
            .copied()
            .filter(|d| !imports.iter().any(|i| i == d) && applies_directive(sdl, d))
            .collect();
        if !missing.is_empty() {
            findings.error(
                "federation-drift",
                format!(
                    "the schema applies {} but the federation @link does not import {}",
                    missing.join(", "),
                    if missing.len() == 1 { "it" } else { "them" }
                ),
                Some(schema_file),
                None,
            );
        }
    }

    let type_prefix = get_str(workspace, "type_prefix");
    let field_prefix = get_str(workspace, "field_prefix");
    if let Some(tp) = type_prefix {
        let roots = ["Query", "Mutation", "Subscription"];
        for decl in type_declarations(sdl) {
            if roots.contains(&decl.name.as_str()) || decl.name.starts_with(&format!("{}_", tp)) {
                continue;
            }
            // An object type a resolved decision declares another
            // subgraph's keeps its owner's name (empty unless the target
            // honours them). Only an object type can be an entity: a
            // declared enum, input, interface, union or scalar keeps the
            // rule, and the target says why (`foreign_type_findings`).
            if decl.kind == "type" && foreign.contains(&decl.name) {
                continue;
            }
            findings.error(
                "type-prefix",
                format!(
                    "{} {} must be prefixed {}_ so it cannot collide in the supergraph",
                    decl.kind, decl.name, tp
                ),
                Some(schema_file),
                Some(decl.line),
            );
        }
    }
    if let Some(fp) = field_prefix {
        for root in ["Query", "Mutation"] {
            for field in root_fields(sdl, root) {
                if !field.starts_with(&format!("{}_", fp)) {
                    findings.error(
                        "field-prefix",
                        format!("{}.{} must be prefixed {}_", root, field, fp),
                        Some(schema_file),
                        None,
                    );
                }
            }
        }
    }

    if let Some(tp) = type_prefix {
        let json_scalar = format!("{}_JSON", tp);
        if sdl.contains(&format!("scalar {}", json_scalar)) {
            let policy = selection
                .and_then(|s| get(s, "defaults"))
                .and_then(|d| get_str(d, "opaque_json_policy"))
                .unwrap_or("forbid");
            let re = Regex::new(&format!(
                r"(?m)^\s*[A-Za-z_][A-Za-z0-9_]*\s*(\([^)]*\))?\s*:\s*\[?{}",
                regex::escape(&json_scalar)
            ))
            .unwrap();
            for m in re.find_iter(sdl) {
                let text = m.as_str();
                let leading = text.len() - text.trim_start().len();
                let reason = doc_comment_before(sdl, m.start() + leading);
                if reason.is_none() {
                    findings.add(
                        if policy == "forbid" { "error" } else { "warn" },
                        "undocumented-json-scalar",
                        format!(
                            "a {} field has no doc comment saying why the shape cannot be typed",
                            json_scalar
                        ),
                        Some(schema_file),
                        Some(sdl[..m.start()].matches('\n').count() + 1),
                    );
                }
            }
        }
    }
}

fn value_str(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        other => crate::json::compact(other),
    }
}

/// True when the doc comment states the given inventory bound. A numeric bound
/// must appear as a standalone number in the text and is compared numerically:
/// "1000" in the doc does not satisfy a maximum of 100 (no substring credit),
/// and "20" satisfies a default of 20.0. Non-numeric values fall back to a
/// substring match.
fn doc_states_bound(doc: &str, bound: &Value) -> bool {
    match bound.as_f64() {
        Some(want) => Regex::new(r"\d+(?:\.\d+)?")
            .unwrap()
            .find_iter(doc)
            .any(|m| m.as_str().parse::<f64>() == Ok(want)),
        None => doc.contains(&value_str(bound)),
    }
}

fn lint_template(dir: &Path, sdl: &str, target: &crate::target::Target, findings: &mut Findings) {
    let template = match load_yaml_file(dir, "template.yaml", findings) {
        Some(t) => t,
        None => {
            if target.variables_file_required {
                findings.error(
                    "missing-template",
                    "template.yaml is missing; this target renders the schema from it",
                    Some("template.yaml"),
                    None,
                );
            }
            return;
        }
    };
    let mut declared: Vec<(String, Value)> = Vec::new();
    for variable in get_arr(&template, "variables").into_iter().flatten() {
        let name = match get_str(variable, "name") {
            Some(n) => n.to_string(),
            None => continue,
        };
        if declared.iter().any(|(n, _)| *n == name) {
            findings.error(
                "duplicate-template-variable",
                format!("template.yaml declares {} more than once", name),
                Some("template.yaml"),
                None,
            );
        }
        declared.retain(|(n, _)| *n != name);
        declared.push((name, variable.clone()));
    }
    let used: Vec<String> = {
        let mut seen = Vec::new();
        for p in placeholders(sdl) {
            if !seen.contains(&p.name) {
                seen.push(p.name);
            }
        }
        seen
    };
    for name in &used {
        if !declared.iter().any(|(n, _)| n == name) {
            findings.error(
                "undeclared-placeholder",
                format!(
                    "the schema uses {{{{{}}}}} but template.yaml does not declare it",
                    name
                ),
                Some("template.yaml"),
                None,
            );
        }
    }
    for (name, variable) in &declared {
        if !used.contains(name) {
            findings.error(
                "unused-template-variable",
                format!(
                    "template.yaml declares {} but the schema does not use it",
                    name
                ),
                Some("template.yaml"),
                None,
            );
        }
        let td = crate::json::field(variable, "test_default");
        if td.is_none() || td == Some(&Value::Null) || td.and_then(Value::as_str) == Some("") {
            findings.error(
                "missing-test-default",
                format!("template.yaml must give {} a test_default", name),
                Some("template.yaml"),
                None,
            );
        }
    }
    if let Some(auth_default) = declared
        .iter()
        .find(|(n, _)| n == "AUTH_EXPR")
        .and_then(|(_, v)| get_str(v, "test_default"))
    {
        if !Regex::new(r"^\{\s*\$env\.[A-Za-z_][A-Za-z0-9_]*\s*\}$")
            .unwrap()
            .is_match(auth_default)
        {
            findings.error(
                "auth-test-default",
                format!(
                    "AUTH_EXPR.test_default must be a complete unquoted local expression like {{$env.EXAMPLE_TOKEN}}, got {}",
                    serde_json::to_string(auth_default).unwrap_or_default()
                ),
                Some("template.yaml"),
                None,
            );
        }
    }
}

/// A per-call credential a connector sends (ADR 0069 R1): the field
/// argument it reads, that argument's declared type as the field declares it
/// (empty when the field does not declare it), and the request slot it
/// travels in. A relationship field mirrors its by-id root field's list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallCredential {
    pub arg: String,
    pub arg_type: String,
    pub slot: CredentialSlot,
}

/// Where a per-call credential travels on the request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialSlot {
    /// A `headers:` entry: its name as written and its value template
    /// verbatim (`Authorization`, `Bearer {$args.access_token}`).
    Header { name: String, value: String },
    /// A query parameter: `?<param>={$args.<a>}` in the URI, or a
    /// `queryParams` entry keyed `<param>` — the same slot on the wire.
    Query { param: String },
}

impl CallCredential {
    /// `access_token: String! as header Authorization: Bearer {$args.access_token}`
    /// or `access_token: String! as query parameter access_token`.
    pub fn describe(&self) -> String {
        let arg = if self.arg_type.is_empty() {
            format!("{} (not declared)", self.arg)
        } else {
            format!("{}: {}", self.arg, self.arg_type)
        };
        match &self.slot {
            CredentialSlot::Header { name, value } => {
                format!("{} as header {}: {}", arg, name, value)
            }
            CredentialSlot::Query { param } => format!("{} as query parameter {}", arg, param),
        }
    }

    /// Same argument, same declared type (nullability included), same slot.
    /// A header name compares case-insensitively, as HTTP does; its value
    /// template compares verbatim.
    pub fn same_as(&self, other: &CallCredential) -> bool {
        self.arg == other.arg
            && self.arg_type == other.arg_type
            && match (&self.slot, &other.slot) {
                (
                    CredentialSlot::Header { name: a, value: v },
                    CredentialSlot::Header { name: b, value: w },
                ) => a.eq_ignore_ascii_case(b) && v == w,
                (CredentialSlot::Query { param: a }, CredentialSlot::Query { param: b }) => a == b,
                _ => false,
            }
    }
}

/// The lower-cased words of an identifier: split on `_`, `-`, `.` and
/// camel-case boundaries, an acronym kept whole (`APIKey` → `api`, `key`;
/// `access_token` → `access`, `token`; `pageToken` → `page`, `token`).
fn name_words(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut words: Vec<String> = Vec::new();
    let mut current = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c == '_' || c == '-' || c == '.' {
            if !current.is_empty() {
                words.push(std::mem::take(&mut current));
            }
            continue;
        }
        let boundary = c.is_ascii_uppercase()
            && i > 0
            && (chars[i - 1].is_ascii_lowercase()
                || chars[i - 1].is_ascii_digit()
                || (chars[i - 1].is_ascii_uppercase()
                    && chars.get(i + 1).is_some_and(|n| n.is_ascii_lowercase())));
        if boundary && !current.is_empty() {
            words.push(std::mem::take(&mut current));
        }
        current.push(c.to_ascii_lowercase());
    }
    if !current.is_empty() {
        words.push(current);
    }
    words
}

/// Does a query-parameter or argument name name a credential (ADR 0069 R1,
/// R45)? Read as whole words (`name_words`), never as a substring, once a
/// trailing version is dropped: a last word of digits or `v<digits>`, and
/// digits closing the last word (`token_v2`, `access_token_v2` and
/// `accessToken2` all end in `token`). Then a name is a credential when
/// - its last word is `token`, `secret`, `password`, `apikey`,
///   `authorization` or `bearer`, and its first word does not page, limit or
///   guard a form (`page`, `next`, `prev`, `continuation`, `sync`, `max`,
///   `min`, `cursor`, `csrf`, `include`): `private_token` (GitLab),
///   `personal_access_token`, `client_secret` — never `pageToken`,
///   `csrf_token`, `include_token_metadata`;
/// - its last word is `key` or `auth` and its first word says whose
///   (`api`, `x`, `access`, `secret`, `client`, `app`, `license`,
///   `subscription`, `consumer`, `private`, `personal`, `oauth`, `user`,
///   `session`, `refresh`, `id`, `bearer`): `x-api-key`, `subscription-key`,
///   `consumer_key` — never `sort_key`, `partition_key`, `idempotency_key`;
/// - it is one word: `token`, `key`, `secret`, `password`, `auth`,
///   `apikey`, `authorization`, `bearer` or `accesstoken`.
pub fn names_a_credential(name: &str) -> bool {
    const LAST: [&str; 6] = [
        "token",
        "secret",
        "password",
        "apikey",
        "authorization",
        "bearer",
    ];
    const NEVER: [&str; 10] = [
        "page",
        "next",
        "prev",
        "continuation",
        "sync",
        "max",
        "min",
        "cursor",
        "csrf",
        "include",
    ];
    const KEYED: [&str; 2] = ["key", "auth"];
    const WHOSE: [&str; 17] = [
        "api",
        "x",
        "access",
        "secret",
        "client",
        "app",
        "license",
        "subscription",
        "consumer",
        "private",
        "personal",
        "oauth",
        "user",
        "session",
        "refresh",
        "id",
        "bearer",
    ];
    const ONE: [&str; 9] = [
        "token",
        "key",
        "secret",
        "password",
        "auth",
        "apikey",
        "authorization",
        "bearer",
        "accesstoken",
    ];
    let version = |w: &str| {
        let digits = w.strip_prefix('v').unwrap_or(w);
        !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit())
    };
    let mut words = name_words(name);
    while words.last().is_some_and(|w| version(w)) {
        words.pop();
    }
    if let Some(last) = words.last_mut() {
        let kept = last.trim_end_matches(|c: char| c.is_ascii_digit()).len();
        last.truncate(kept);
    }
    let (Some(first), Some(last)) = (words.first(), words.last()) else {
        return false;
    };
    let (first, last) = (first.as_str(), last.as_str());
    (LAST.contains(&last) && !NEVER.contains(&first))
        || (KEYED.contains(&last) && WHOSE.contains(&first))
        || (words.len() == 1 && ONE.contains(&last))
}

static HEADERS_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
static ARGS_REF_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
static HTTP_URI_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

/// An argument a connector's request interpolates: its name and the byte
/// span of the `{…}` that reads it.
struct Interpolated {
    start: usize,
    end: usize,
    arg: String,
}

/// Every `$args.<a>` a connector's request interpolates, in order. An
/// interpolation is a `{` whose expression opens with `$`, up to its
/// matching `}`, and `$args.<a>` anywhere inside it counts:
/// `{$args.token}`, `{$args.token->match([null, null], [@, $(['Bearer',
/// @])->joinNotNull(' ')])}` and `{$(['Bearer', $args.token])->joinNotNull('
/// ')}` all read `token`. A selection's `{ id name }` opens with a field
/// name and is no interpolation. A `'…'` literal, or a `\"…\"` one written
/// inside a GraphQL string, closes nothing; a backslash escapes the byte
/// after it. An interpolation never runs past an unescaped `"`, which ends
/// the GraphQL string it sits in, so an unbalanced expression cannot swallow
/// the rest of the directive.
fn interpolated_args(text: &str) -> Vec<Interpolated> {
    let refs = re(&ARGS_REF_RE, r"\$args\.([A-Za-z_][A-Za-z0-9_]*)");
    let b = text.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let opens = b[i] == b'{'
            && text[i + 1..]
                .trim_start()
                .as_bytes()
                .first()
                .is_some_and(|&c| c == b'$');
        if !opens {
            i += 1;
            continue;
        }
        // `single`: inside a `'…'` literal; `double`: inside a `\"…\"` one.
        let (mut depth, mut j, mut single, mut double) = (0usize, i, false, false);
        let mut end = b.len();
        while j < b.len() {
            match b[j] {
                b'\\' => {
                    if b.get(j + 1) == Some(&b'"') && !single {
                        double = !double;
                    }
                    j += 2;
                    continue;
                }
                b'"' => {
                    end = j;
                    break;
                }
                b'\'' if !double => single = !single,
                b'{' if !single && !double => depth += 1,
                b'}' if !single && !double => {
                    depth -= 1;
                    if depth == 0 {
                        end = j + 1;
                        break;
                    }
                }
                _ => {}
            }
            j += 1;
        }
        for m in refs.captures_iter(&text[i..end]) {
            if !out
                .iter()
                .any(|x: &Interpolated| x.start == i && x.arg == m[1])
            {
                out.push(Interpolated {
                    start: i,
                    end,
                    arg: m[1].to_string(),
                });
            }
        }
        i = end;
    }
    out
}

/// The names `interpolated_args` finds in `text`, in order, each once.
/// `links apply` reads a root URI's query pairs with it, so a pair whose
/// value is an expression over an argument is not taken for a static one.
pub fn interpolated_arg_names(text: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for a in interpolated_args(text) {
        if !out.contains(&a.arg) {
            out.push(a.arg);
        }
    }
    out
}

/// The byte span of a connector's `headers: [ … ]` list inside its raw
/// `@connect` arguments, found on the blanked text so a bracket inside a
/// string ends nothing.
fn headers_span(args: &str) -> Option<(usize, usize)> {
    let code = crate::graphql::blank(args);
    let open = re(&HEADERS_RE, r"\bheaders\s*:\s*\[").find(&code)?.end() - 1;
    let close = matching_close(code.as_bytes(), open)?;
    Some((open + 1, close))
}

/// Every `headers:` entry of a connector's raw `@connect` arguments, as
/// written between its `{ … }`, in order. `links apply` copies a static one
/// onto a relationship field verbatim (ADR 0069, R51).
pub fn header_entry_texts(args: &str) -> Vec<String> {
    let Some((start, end)) = headers_span(args) else {
        return vec![];
    };
    let code = crate::graphql::blank(args);
    let cb = code.as_bytes();
    let mut out = Vec::new();
    let mut i = start;
    while i < end {
        if cb[i] != b'{' {
            i += 1;
            continue;
        }
        let close = matching_close(cb, i).unwrap_or(end).min(end);
        out.push(args[i + 1..close].to_string());
        i = close + 1;
    }
    out
}

/// Every `headers:` entry of a connector's raw `@connect` arguments as
/// `(name, value)`, each key read wherever it sits in its `{ … }` (R44):
/// `{ name: "A", value: "B" }` and `{ value: "B", name: "A" }` are one entry.
/// An entry that propagates a header (`from:`) has no value.
fn header_entries(args: &str) -> Vec<(String, Option<String>)> {
    header_entry_texts(args)
        .iter()
        .filter_map(|entry| {
            crate::reconcile::string_arg(entry, "name")
                .map(|name| (name, crate::reconcile::string_arg(entry, "value")))
        })
        .collect()
}

/// The per-call credentials `type_name.field`'s `@connect` sends (ADR 0069
/// R1): every `headers:` entry whose value reads `{$args.<a>}`, then every
/// query parameter — `?<param>={$args.<a>}` in the URI or a `queryParams`
/// entry — whose argument or key names a credential. A path segment is never
/// one: that is the by-id key. Generic over the type name, so it reads a root
/// field and a relationship field alike; empty when the connector
/// authenticates through the `@source` alone.
pub fn call_credentials(sdl: &str, type_name: &str, field: &str) -> Vec<CallCredential> {
    let Some(span) = crate::reconcile::field_spans(sdl, type_name)
        .into_iter()
        .find(|s| s.name == field)
    else {
        return vec![];
    };
    let Some(d) = directives(&span.decl, "connect").into_iter().next() else {
        return vec![];
    };
    let declared: Vec<Arg> = root_field_args(sdl, type_name)
        .into_iter()
        .find(|(f, _)| f == field)
        .map(|(_, a)| a)
        .unwrap_or_default();
    let type_of = |arg: &str| {
        declared
            .iter()
            .find(|a| a.name == arg)
            .map(|a| a.type_.clone())
            .unwrap_or_default()
    };
    let uri_re = re(
        &HTTP_URI_RE,
        r#"\b(?:GET|POST|PUT|PATCH|DELETE|HEAD)\s*:\s*"([^"]*)""#,
    );
    let mut out: Vec<CallCredential> = Vec::new();
    for (name, value) in header_entries(&d.args) {
        let Some(value) = value else { continue };
        for a in interpolated_args(&value) {
            out.push(CallCredential {
                arg_type: type_of(&a.arg),
                arg: a.arg,
                slot: CredentialSlot::Header {
                    name: name.clone(),
                    value: value.clone(),
                },
            });
        }
    }
    // (argument, key): the URI's own query string first, then `queryParams`.
    let mut query: Vec<(String, String)> = Vec::new();
    if let Some(uri) = uri_re.captures(&d.args) {
        if let Some((_, qs)) = uri[1].split_once('?') {
            for pair in qs.split('&') {
                if let Some((k, v)) = pair.split_once('=') {
                    for a in interpolated_args(v) {
                        query.push((a.arg, k.to_string()));
                    }
                }
            }
        }
    }
    query.extend(wiring(&span.decl).query_keys);
    for (arg, key) in query {
        if names_a_credential(&arg) || names_a_credential(&key) {
            out.push(CallCredential {
                arg_type: type_of(&arg),
                arg,
                slot: CredentialSlot::Query { param: key },
            });
        }
    }
    out
}

/// The root `Query` fields that reach the same operation as a relationship
/// connector's `method path` (ADR 0069 R1): same method, same path once every
/// `{…}` is erased and any query string dropped (`/owners/{$this.owner_id}`
/// and `/owners/{$args.ownerId}` are both `/owners/{}`), in declaration order.
/// A Graph-style `/{id}` API yields several.
pub fn by_id_root_fields(sdl: &str, method: Option<&str>, path: Option<&str>) -> Vec<String> {
    let (Some(method), Some(path)) = (method, path) else {
        return vec![];
    };
    let want = crate::reconcile::normalize_path(path.split('?').next().unwrap_or(path));
    crate::reconcile::field_spans(sdl, "Query")
        .into_iter()
        .filter(|s| {
            s.connect.as_ref().is_some_and(|c| {
                c.method
                    .as_deref()
                    .is_some_and(|m| m.eq_ignore_ascii_case(method))
                    && c.path.as_deref().is_some_and(|p| {
                        crate::reconcile::normalize_path(p.split('?').next().unwrap_or(p)) == want
                    })
            })
        })
        .map(|s| s.name)
        .collect()
}

/// The credential a relationship field reaching `method path` mirrors
/// (ADR 0069 R1): the first by-id root field whose connector carries a
/// per-call credential, with that credential. `None` is Case 1 — the root
/// connector authenticates through the one `@source` alone, or no root field
/// reaches the operation — and the field then carries no credential of its
/// own. Derived from the SDL at read time; `selection.yaml` stores nothing.
pub fn mirrored_credential(
    sdl: &str,
    method: Option<&str>,
    path: Option<&str>,
) -> Option<(String, Vec<CallCredential>)> {
    by_id_root_fields(sdl, method, path)
        .into_iter()
        .map(|root| {
            let credential = call_credentials(sdl, "Query", &root);
            (root, credential)
        })
        .find(|(_, credential)| !credential.is_empty())
}

/// Which half of the null guard (ADR 0084) a relationship connector keyed by
/// `{$this.<fk>}` lacks: an `isSuccess` that holds when `$this.<fk>` is null,
/// and a selection headed by the guard's `->match` (either form `links apply`
/// prints). Whitespace is not significant.
pub fn null_guard_missing(connect_args: &str, selection: &str, fk: &str) -> Vec<&'static str> {
    use crate::cmd::links::{null_guard_head, null_guard_is_success_head};
    let squeeze = |t: &str| t.split_whitespace().collect::<String>();
    let mut missing = Vec::new();
    let is_success = squeeze(&format!("isSuccess: \"{}", null_guard_is_success_head(fk)));
    if !squeeze(connect_args).contains(&is_success) {
        missing.push("isSuccess null guard");
    }
    if !squeeze(selection).starts_with(&squeeze(&null_guard_head(fk))) {
        missing.push("selection null guard");
    }
    missing
}

/// Field-level relationship connectors (ADR 0069). A relationship field
/// mirrors the credential of its by-id operation's own root connector (R1).
/// Case 1 — no root field reaching the operation carries a per-call
/// credential, so the one `@source` (`{{AUTH_EXPR}}`) authenticates every
/// request: a field that brings its own — an `Authorization` header,
/// `{$args.<a>}` anywhere in the request (header, path or query), or a
/// declared credential-named argument — opens a second credential path
/// (ADR 0019), so it is an error. Case 2 — the root connector carries one (an
/// `access_token: String!` argument sent in a header or a query parameter, as
/// every AppWorld service does): the field must declare the same argument
/// (name, type, nullability), send it through the same slot, and carry
/// nothing else; the error fires when the two differ. Several root fields on
/// one path (a Graph-style `/{id}`) are all candidates, and mirroring any one
/// of them is enough. A target resolved by both forms at once — a
/// field-level connector on the host and `@key` + a type-level `@connect` on
/// the target for the same operation — is a warning: one form is enough, and
/// the entity needs a recorded decision.
fn lint_link_connectors(sdl: &str, schema_file: &str, findings: &mut Findings) {
    let type_level = crate::reconcile::type_connectors(sdl);
    // Whether the one @source supplies the workspace's credential: only then
    // may Case 1 say so (R44).
    let source_credential = directives(sdl, "source")
        .iter()
        .any(|d| d.args.contains("{{AUTH_EXPR}}"));
    let mut index = crate::sdl_index::SdlIndex::new(sdl);
    for lc in crate::reconcile::link_connectors(sdl) {
        let Some(c) = &lc.span.connect else {
            continue;
        };
        let args = directives(&lc.span.decl, "connect")
            .into_iter()
            .next()
            .map(|d| d.args)
            .unwrap_or_default();
        let header_span = headers_span(&args);
        let headers = header_entries(&args);
        let declared: Vec<Arg> = root_field_args(sdl, &lc.type_name)
            .into_iter()
            .find(|(f, _)| *f == lc.field)
            .map(|(_, a)| a)
            .unwrap_or_default();
        // Every root field reaching the same operation, with what its
        // connector sends per call.
        let roots: Vec<(String, Vec<CallCredential>)> =
            by_id_root_fields(sdl, c.method.as_deref(), c.path.as_deref())
                .into_iter()
                .map(|r| {
                    let credential = call_credentials(sdl, "Query", &r);
                    (r, credential)
                })
                .collect();
        match roots.iter().find(|(_, credential)| !credential.is_empty()) {
            // Case 1: the one @source is the credential.
            None => {
                let mut credentials: Vec<String> = Vec::new();
                // `{$args.<a>}` the field sends that reads as no credential:
                // not in a header, not credential-named, not a credential
                // query key (`call_credentials`, R45). It is an argument
                // the field does not mirror, and saying "credential" of it
                // would send the reader after the wrong fix.
                let mut arguments: Vec<(String, String)> = Vec::new();
                let carried = call_credentials(sdl, &lc.type_name, &lc.field);
                if headers
                    .iter()
                    .any(|(name, _)| name.eq_ignore_ascii_case("authorization"))
                {
                    credentials.push("an Authorization header".to_string());
                }
                for m in interpolated_args(&args) {
                    let inside_headers =
                        header_span.is_some_and(|(s, e)| m.start >= s && m.end <= e);
                    let item = format!(
                        "{{$args.{}}} {}",
                        m.arg,
                        if inside_headers {
                            "in a header"
                        } else {
                            "in the request"
                        }
                    );
                    if inside_headers
                        || names_a_credential(&m.arg)
                        || carried.iter().any(|k| k.arg == m.arg)
                    {
                        credentials.push(item);
                    } else if !arguments.iter().any(|(_, i)| i == &item) {
                        arguments.push((m.arg, item));
                    }
                }
                if !arguments.is_empty() {
                    // A by-id root field that takes the argument too does not
                    // make it the field's to carry (R1): say only what is true.
                    let root_args: Vec<String> = root_field_args(sdl, "Query")
                        .into_iter()
                        .filter(|(f, _)| roots.iter().any(|(r, _)| r == f))
                        .flat_map(|(_, a)| a.into_iter().map(|a| a.name))
                        .collect();
                    let one = arguments.len() == 1;
                    let root_takes = arguments.iter().any(|(a, _)| root_args.contains(a));
                    findings.error(
                        "link-credential",
                        format!(
                            "{}.{} carries {} ({}); a relationship field declares only the mirrored credential — drop the argument{}",
                            lc.type_name,
                            lc.field,
                            match (one, root_takes) {
                                (true, false) => "an argument its by-id root field does not",
                                (false, false) => "arguments its by-id root field does not",
                                (true, true) => "an argument that is no credential",
                                (false, true) => "arguments that are no credential",
                            },
                            arguments
                                .iter()
                                .map(|(_, i)| i.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                            if one { "" } else { "s" }
                        ),
                        Some(schema_file),
                        Some(lc.span.line),
                    );
                }
                for a in &declared {
                    if names_a_credential(&a.name)
                        && !interpolated_args(&args).iter().any(|m| m.arg == a.name)
                    {
                        credentials.push(format!("argument {}: {}", a.name, a.type_));
                    }
                }
                if !credentials.is_empty() {
                    // What the field diverges from, as the schema states it.
                    let from = match roots.first() {
                        None => format!(
                            "no root field reaches {} {} for it to mirror, so it has no per-call credential to carry — drop the header and its argument",
                            c.method.as_deref().unwrap_or("?"),
                            c.path.as_deref().unwrap_or("?")
                        ),
                        Some((root, _)) if !source_credential => format!(
                            "its by-id root field Query.{} sends no credential and the @source carries none — drop the header and its argument",
                            root
                        ),
                        Some(_) => "the one @source supplies {{AUTH_EXPR}} to every request — drop the header and its argument".to_string(),
                    };
                    findings.error(
                        "link-credential",
                        format!(
                            "{}.{} carries its own credential ({}); {}",
                            lc.type_name,
                            lc.field,
                            credentials.join(", "),
                            from
                        ),
                        Some(schema_file),
                        Some(lc.span.line),
                    );
                }
            }
            // Case 2: the root connector authenticates per call; the field
            // carries exactly that credential and nothing else.
            Some((root, want)) => {
                let carried = call_credentials(sdl, &lc.type_name, &lc.field);
                let mut extra: Vec<String> = Vec::new();
                for (name, value) in &headers {
                    if name.eq_ignore_ascii_case("authorization")
                        && !value
                            .as_deref()
                            .is_some_and(|v| !interpolated_args(v).is_empty())
                    {
                        extra.push("an Authorization header that reads no argument".to_string());
                    }
                }
                for m in interpolated_args(&args) {
                    if !carried.iter().any(|k| k.arg == m.arg) {
                        extra.push(format!("{{$args.{}}} in the request", m.arg));
                    }
                }
                // A declared argument the request does interpolate is the
                // `{$args.…} in the request` above; only one it never reads
                // is sent nowhere.
                for a in &declared {
                    if !carried.iter().any(|k| k.arg == a.name)
                        && !interpolated_args(&args).iter().any(|m| m.arg == a.name)
                    {
                        extra.push(format!("argument {}: {}, sent nowhere", a.name, a.type_));
                    }
                }
                let mirrors = |credential: &[CallCredential]| {
                    extra.is_empty()
                        && credential.len() == carried.len()
                        && credential
                            .iter()
                            .all(|x| carried.iter().any(|k| k.same_as(x)))
                };
                let mirrored = roots
                    .iter()
                    .any(|(_, credential)| !credential.is_empty() && mirrors(credential));
                if !mirrored {
                    let mut has: Vec<String> =
                        carried.iter().map(CallCredential::describe).collect();
                    has.extend(extra);
                    findings.error(
                        "link-credential",
                        format!(
                            "{}.{} must carry exactly the credential its by-id root field Query.{} carries ({}); it carries {} — declare the same argument and send it the same way",
                            lc.type_name,
                            lc.field,
                            root,
                            want.iter()
                                .map(CallCredential::describe)
                                .collect::<Vec<_>>()
                                .join(", "),
                            if has.is_empty() {
                                "none".to_string()
                            } else {
                                has.join(", ")
                            }
                        ),
                        Some(schema_file),
                        Some(lc.span.line),
                    );
                }
            }
        }
        // The null guard (ADR 0084): a nullable fk with no guard sends the
        // empty-segment GET for every parent whose fk is null, and the field
        // fails there (or maps whatever that GET answers).
        for fk in &lc.this_vars {
            let Some(fk_type) = index.field_type(&lc.type_name, fk) else {
                continue;
            };
            if fk_type.trim_end().ends_with('!') {
                continue;
            }
            let missing = null_guard_missing(&args, c.selection.as_deref().unwrap_or(""), fk);
            if missing.is_empty() {
                continue;
            }
            let empty_get = c
                .path
                .as_deref()
                .unwrap_or("?")
                .replace(&format!("{{$this.{}}}", fk), "");
            findings.warn(
                "link-null-guard",
                format!(
                    "{}.{} reads {{$this.{}}} and {} is nullable ({}), but the connector carries no {}; for a parent whose {} is null the router still sends GET {} and the field fails with CONNECTOR_FETCH (a 307 or 404 from the empty segment) or maps whatever that GET answers — print the field again with `graphos-factory-core links apply --dry-run` and paste its isSuccess and selection, and write an e2e case with a null-{} parent (connectors-language.md § Relationship fields)",
                    lc.type_name,
                    lc.field,
                    fk,
                    fk,
                    fk_type,
                    missing.join(" and no "),
                    fk,
                    empty_get,
                    fk
                ),
                Some(schema_file),
                Some(lc.span.line),
            );
        }
        let Some(target) = index
            .field_type(&lc.type_name, &lc.field)
            .map(|t| crate::sdl_index::base_type(&t))
        else {
            continue;
        };
        let same = type_level.iter().find(|t| {
            t.type_name == target
                && t.connect.method == c.method
                && t.connect
                    .path
                    .as_deref()
                    .map(crate::reconcile::normalize_path)
                    == c.path.as_deref().map(crate::reconcile::normalize_path)
        });
        if let Some(t) = same {
            findings.warn(
                "link-key-drift",
                format!(
                    "{}.{} resolves {} {} through a field-level connector while type {} resolves the same operation through @key and a type-level @connect (line {}); keep one form — the relationship field within this subgraph, the entity only when a decision records why",
                    lc.type_name,
                    lc.field,
                    c.method.as_deref().unwrap_or("?"),
                    c.path.as_deref().unwrap_or("?"),
                    target,
                    t.line
                ),
                Some(schema_file),
                Some(lc.span.line),
            );
        }
    }
}

/// `batchable-entity-unbatched` (ADR 0068): a keyed type that a
/// bulk-by-keys operation could resolve has no type-level `$batch`
/// connector, so every reference costs its own request. `graphql.batch:
/// false` on the entity operation is the recorded decline that silences it.
fn lint_batching(
    sdl: &str,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let inventory = match inventory {
        Some(i) => i,
        None => return,
    };
    // A schema that does not parse is compose's finding, not this rule's.
    let reports = match crate::batch::find(inventory, selection, sdl, false) {
        Ok(r) => r,
        Err(_) => return,
    };
    for r in crate::batch::missing_batch(&reports) {
        let via: Vec<&str> = r
            .candidates
            .iter()
            .filter(|c| c.key_param.is_some())
            .map(|c| c.operation.as_str())
            .collect();
        // With `batch: true` already recorded, the step left is the paste,
        // or, when `batch find` could not draft one, the connector by hand.
        let todo = match (r.batch, &r.draft, &r.draft_note) {
            (Some(true), Some(_), _) => "graphql.batch is true; paste `graphos-factory-core batch find`'s draft over the type's header and audit it, or change the judgement to graphql.batch: false with the reason in decisions.json".to_string(),
            (Some(true), None, note) => format!(
                "graphql.batch is true but `graphos-factory-core batch find` has no draft ({}); write the type-level $batch connector by hand, or change the judgement to graphql.batch: false with the reason in decisions.json",
                note.as_deref().unwrap_or("no reason recorded")
            ),
            _ => "set graphql.batch: true and paste `graphos-factory-core batch find`'s draft, or record graphql.batch: false with the reason in decisions.json".to_string(),
        };
        findings.error(
            "batchable-entity-unbatched",
            format!(
                "{} (keyed by {}) can be resolved in bulk by {} but has no type-level $batch connector; {}",
                r.type_name,
                r.keyed_by.as_deref().unwrap_or("?"),
                via.join(", "),
                todo
            ),
            Some(".factory/selection.yaml"),
            None,
        );
    }
}

fn lint_selection(
    selection: Option<&Value>,
    inventory: Option<&Value>,
    sdl: &str,
    workspace: &Value,
    decisions: Option<&Value>,
    findings: &mut Findings,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };
    let ops = get_arr(inventory, "operations")
        .cloned()
        .unwrap_or_default();
    let by_key = |key: &str| ops.iter().find(|o| get_str(o, "key") == Some(key));
    let mut names: Vec<(String, String)> = Vec::new();
    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        let op = match by_key(key) {
            Some(o) => o,
            None => {
                findings.error(
                    "unknown-operation",
                    format!(
                        "selection.yaml names {}, which is not in inventory.json",
                        key
                    ),
                    Some("selection.yaml"),
                    None,
                );
                continue;
            }
        };
        if !truthy(get(entry, "include")) {
            continue;
        }
        if get_str(op, "support") == Some("unsupported") && !truthy(get(entry, "force_reason")) {
            findings.error(
                "forced-unsupported",
                format!(
                    "{} is unsupported ({}); including it needs a force_reason",
                    key,
                    get_str(op, "support_reason").unwrap_or("null")
                ),
                Some("selection.yaml"),
                None,
            );
        }
        // The envelope is a judgement (ADR 0018): a draft that nobody
        // confirmed must never quietly become the schema's shape.
        match get(entry, "response") {
            None => findings.warn(
                "no-response-envelope",
                format!(
                    "{} declares no response.envelope; run `graphos-factory-core selection draft` and confirm the envelope with the user, so how the payload is read is written down rather than inferred",
                    key
                ),
                Some("selection.yaml"),
                None,
            ),
            Some(r) => {
                if get(r, "confirmed") == Some(&Value::Bool(false)) {
                    findings.warn(
                        "response-envelope-unconfirmed",
                        format!(
                            "{} has response.envelope: {} marked confirmed: false — it is still the tool's draft; agree it with the user, then set confirmed: true or drop the key",
                            key,
                            get_str(r, "envelope")
                                .map(|e| format!("{:?}", e))
                                .unwrap_or_else(|| "null".to_string())
                        ),
                        Some("selection.yaml"),
                        None,
                    );
                }
                if let Some(env) = get_str(r, "envelope") {
                    let roots = crate::reconcile::root_properties(
                        op,
                        &get_obj(inventory, "shapes").cloned().unwrap_or_default(),
                        Some(r),
                    );
                    if !roots.iter().any(|x| x == env) {
                        findings.error(
                            "unknown-envelope",
                            format!(
                                "{}: response.envelope {:?} is not a root property of its response shape ({})",
                                key,
                                env,
                                if roots.is_empty() {
                                    "the shape has none".to_string()
                                } else {
                                    roots.join(", ")
                                }
                            ),
                            Some("selection.yaml"),
                            None,
                        );
                    }
                }
            }
        }
        let root = get(entry, "graphql").and_then(|g| get_str(g, "root"));
        match root {
            None => findings.error(
                "missing-root",
                format!("{} has no graphql.root; Query vs Mutation is a judgement and is never derived from the HTTP method", key),
                Some("selection.yaml"),
                None,
            ),
            Some("query") if get_str(op, "semantics") == Some("write") => findings.warn(
                "read-root-on-write",
                format!("{} is a {} the inventory reads as a write but is selected as a Query — record why in decisions.json", key, get_str(op, "method").unwrap_or("")),
                Some("selection.yaml"),
                None,
            ),
            _ => {}
        }
        if let Some(name) = get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            if let Some((_, other)) = names.iter().find(|(n, _)| n == name) {
                findings.error(
                    "duplicate-name",
                    format!("{} and {} both map to the field \"{}\"", key, other, name),
                    Some("selection.yaml"),
                    None,
                );
            }
            names.push((name.to_string(), key.clone()));
        }
        let renames: Vec<String> = get(entry, "fields")
            .and_then(|f| get_obj(f, "rename"))
            .map(|r| r.values().map(value_str).collect())
            .unwrap_or_default();
        let distinct: HashSet<&String> = renames.iter().collect();
        if distinct.len() != renames.len() {
            findings.error(
                "duplicate-rename",
                format!("{} renames two fields to the same name", key),
                Some("selection.yaml"),
                None,
            );
        }
    }
    if get(selection, "customized").is_some() {
        findings.error(
            "customized-retired",
            "customized: is retired — codify each hand edit into overrides: (graphos-factory-core codify --key K --reason …) so it carries a reason, a decision and assertions",
            Some("selection.yaml"),
            None,
        );
    }
    let today = crate::today();
    for o in crate::reconcile::read_overrides(selection) {
        if o.key.contains(":/") && by_key(&o.key).is_none() {
            findings.error(
                "override-unknown-key",
                format!("overrides lists {}, which is not in inventory.json", o.key),
                Some("selection.yaml"),
                None,
            );
        }
        if o.assertions.is_empty() {
            findings.warn(
                "override-pinned",
                format!(
                    "override {} has no assertions, so it is pinned byte-for-byte and the agent can never change it; add `assert:` (what must stay true) to let it evolve",
                    o.key
                ),
                Some("selection.yaml"),
                None,
            );
        }
        for (kind, _) in &o.assertions {
            if !crate::spans::ASSERTION_KINDS.contains(&kind.as_str()) {
                findings.error(
                    "override-bad-assertion",
                    format!(
                        "override {} uses unknown assertion {:?} (one of: {})",
                        o.key,
                        kind,
                        crate::spans::ASSERTION_KINDS.join(", ")
                    ),
                    Some("selection.yaml"),
                    None,
                );
            }
        }
        if crate::reconcile::expired(&o, &today) {
            findings.warn(
                "override-expired",
                format!(
                    "override {} expired on {}; revisit it ({}) or extend `expires`",
                    o.key,
                    o.expires.as_deref().unwrap_or(""),
                    o.until.as_deref().unwrap_or("no `until` recorded")
                ),
                Some("selection.yaml"),
                None,
            );
        }
    }
    // links: (ADR 0069). Each entry references a fact — the shape, the
    // property path, the by-id operation — and records a judgement. The
    // reference checks are reconcile's `link_reference_problems`; this only
    // names the rule for each. The draft flag is a warning, never an error,
    // because a draft is the tool's proposal and not a defect; so is a
    // confirmed entry with no host type in the schema yet (`link-no-host`),
    // because applying an operation that returns the shape settles it.
    // (shape, object path, field, link key) of every included link seen so
    // far. The field lands on the object that carries the fk — the shape's
    // root, or the nested object the path's last `>` steps out of — so a
    // name repeated on two different objects is two fields, not a clash.
    let mut fields_seen: Vec<(String, String, String, String)> = Vec::new();
    // What `link_hosts` walks, built once and only when a link needs it.
    let mut host_walk: Option<(crate::op_match::OpHints, crate::sdl_index::SdlIndex)> = None;
    // What `link-target-refused` reads, built once and only when a
    // confirmed link needs it.
    let mut staleness: Option<crate::reconcile::LinkStaleness> = None;
    let mut pasted_links: Option<Vec<crate::reconcile::LinkConnector>> = None;
    for link in crate::reconcile::read_links(selection) {
        let key = link.key();
        let problems = crate::reconcile::link_reference_problems(&link, inventory, selection);
        let sound = problems.is_empty();
        for problem in problems {
            use crate::reconcile::LinkProblem;
            let (rule, message) = match problem {
                LinkProblem::UnknownShape => (
                    "link-unknown-shape",
                    format!(
                        "link {} names shape {}, which is not in inventory.json",
                        key, link.shape
                    ),
                ),
                LinkProblem::UnknownOperation => (
                    "link-unknown-operation",
                    format!(
                        "link {} names {}, which is not in inventory.json",
                        key, link.operation
                    ),
                ),
                LinkProblem::OperationExcluded => (
                    "link-operation-excluded",
                    format!(
                        "link {} resolves through {}, which the selection does not include — the by-id operation is the field's provenance; include it or drop the link",
                        key, link.operation
                    ),
                ),
                LinkProblem::UnknownPath => (
                    "link-unknown-path",
                    format!(
                        "link {}: {} does not resolve in shape {}",
                        key, link.path, link.shape
                    ),
                ),
            };
            findings.error(rule, message, Some("selection.yaml"), None);
        }
        if !link.include {
            continue;
        }
        if !link.confirmed {
            findings.warn(
                "link-unconfirmed",
                link.draft_note(),
                Some("selection.yaml"),
                None,
            );
        } else if sound {
            // A confirmed entry the schema gives no host (R42): `links apply`
            // refuses it `no-host` and reconcile only notes it, so without
            // this nothing a validation run reads would say so. On a
            // components spec the usual cause is a shape the draft reaches
            // through a `$ref` the schema does not declare: gitea's
            // `Organization > username` sits under `Repository.repo_transfer`
            // (excluded) → `RepoTransfer.teams[]` → `Team.organization`.
            let (hints, index) = host_walk.get_or_insert_with(|| {
                (
                    crate::op_match::OpHints::from_selection(
                        Some(workspace),
                        Some(selection),
                        Some(sdl),
                    ),
                    crate::sdl_index::SdlIndex::new(sdl),
                )
            });
            let hosts = crate::reconcile::link_hosts(&link, inventory, sdl, index, hints);
            // `link-target-refused` (ADR 0098): a confirmed entry whose
            // by-id target the current rules refuse, or whose fact is gone.
            // An error once the field is pasted (the schema serves a field
            // that cannot resolve), a warning before; an entry whose
            // `decision:` names a resolved `keep` decision is the user's
            // recorded choice and draws nothing (ADR 0113 §4). It replaces
            // `link-no-host`: the remedy is to decline the link, not to give
            // it a host.
            let verdict = crate::reconcile::link_decision(&link, decisions);
            let stale = if matches!(verdict, crate::reconcile::LinkDecision::Keep(_)) {
                None
            } else {
                staleness
                    .get_or_insert_with(|| crate::reconcile::LinkStaleness::new(inventory))
                    .reason(&link)
            };
            if let Some(reason) = stale {
                let field = link.field_name();
                let pasted: Vec<String> = pasted_links
                    .get_or_insert_with(|| crate::reconcile::link_connectors(sdl))
                    .iter()
                    .filter(|lc| lc.field == field && hosts.iter().any(|(h, _)| h == &lc.type_name))
                    .map(|lc| format!("{}.{} (line {})", lc.type_name, lc.field, lc.span.line))
                    .collect();
                let host_names: Vec<String> = hosts.iter().map(|(h, _)| h.clone()).collect();
                let remedy = crate::reconcile::stale_link_remedy(
                    &link,
                    &verdict,
                    &reason,
                    &host_names,
                    !pasted.is_empty(),
                );
                if pasted.is_empty() {
                    findings.warn(
                        "link-target-refused",
                        format!("link {} is confirmed but {}; {}", key, reason, remedy),
                        Some("selection.yaml"),
                        None,
                    );
                } else {
                    findings.error(
                        "link-target-refused",
                        format!(
                            "link {} is confirmed and pasted as {}, but {}; {}",
                            key,
                            pasted.join(", "),
                            reason,
                            remedy
                        ),
                        Some("selection.yaml"),
                        None,
                    );
                }
            } else if hosts.is_empty() {
                findings.warn(
                    "link-no-host",
                    format!(
                        "link {}: no root field returns a type that reaches this shape, so the link has no host; apply an operation returning the shape or decline the link (include: false)",
                        key
                    ),
                    Some("selection.yaml"),
                    None,
                );
            }
        }
        let field = link.field_name();
        let object = link.object_key();
        if let Some((_, _, _, other)) = fields_seen
            .iter()
            .find(|(s, o, f, _)| s == &link.shape && o == &object && f == &field)
        {
            findings.error(
                "link-duplicate-field",
                format!(
                    "links {} and {} both resolve to field {} on shape {}{}",
                    other,
                    key,
                    field,
                    link.shape,
                    if object.is_empty() {
                        String::new()
                    } else {
                        format!(" at {}", object)
                    }
                ),
                Some("selection.yaml"),
                None,
            );
        }
        fields_seen.push((link.shape.clone(), object, field, key));
    }
}

/// `waivers[]`: each accepts a conformance result the oracle could not judge.
/// A waiver must name exactly one target, a known operation when it names
/// one, a decision, and must still match something — a waiver whose body now
/// conforms (or is gone) is stale acceptance nobody re-read.
fn lint_waivers(
    dir: &Path,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let selection = match selection {
        Some(s) => s,
        None => return,
    };
    let waivers = crate::waivers::read_waivers(selection);
    if waivers.is_empty() {
        return;
    }
    let known = |key: &str| {
        inventory
            .and_then(|inv| get_arr(inv, "operations"))
            .map(|ops| ops.iter().any(|o| get_str(o, "key") == Some(key)))
            .unwrap_or(true)
    };
    let today = crate::today();
    for w in &waivers {
        match (&w.where_, &w.operation) {
            (Some(_), Some(_)) | (None, None) => {
                findings.error(
                    "waiver-bad-target",
                    format!(
                        "waiver {:?} must name exactly one of `where` (one body) or `operation` (every such body of one operation)",
                        w.target()
                    ),
                    Some("selection.yaml"),
                    None,
                );
            }
            (None, Some(op)) if !known(op) => {
                findings.error(
                    "waiver-unknown-key",
                    format!("waiver names {}, which is not in inventory.json", op),
                    Some("selection.yaml"),
                    None,
                );
            }
            _ => {}
        }
        if !crate::waivers::STATUSES.contains(&w.status.as_str()) {
            findings.error(
                "waiver-bad-status",
                format!(
                    "waiver {} accepts status {:?}; a waiver accepts one of: {}",
                    w.target(),
                    w.status,
                    crate::waivers::STATUSES.join(", ")
                ),
                Some("selection.yaml"),
                None,
            );
        }
        if crate::waivers::expired(w, &today) {
            findings.warn(
                "waiver-expired",
                format!(
                    "waiver {} expired on {}; revisit it ({}) or extend `expires`",
                    w.target(),
                    w.expires.as_deref().unwrap_or(""),
                    w.until.as_deref().unwrap_or("no `until` recorded")
                ),
                Some("selection.yaml"),
                None,
            );
        }
    }
    // Unused: run the oracle and see which waivers it did not need.
    if let Ok(report) = crate::cmd::validate::validate_workspace(dir) {
        for u in get(&report.json, "waivers")
            .and_then(|w| get_arr(w, "unused"))
            .into_iter()
            .flatten()
        {
            findings.warn(
                "waiver-unused",
                format!(
                    "waiver {} ({}) matches nothing: no body currently has that status there — the gap closed, or the target is misspelled; remove the waiver or fix it",
                    get_str(u, "target").unwrap_or(""),
                    get_str(u, "status").unwrap_or("")
                ),
                Some("selection.yaml"),
                None,
            );
        }
    }
}

/// The applied lock: every span whose text differs from what the agent last
/// wrote or acknowledged is a hand edit nobody has codified — an error,
/// because the next apply could not tell it from its own delta. Overrides
/// are checked here too, against the span text, so a failing intent fails
/// lint and not only reconcile.
fn lint_lock(
    dir: &Path,
    sdl: &str,
    inventory: Option<&Value>,
    workspace: &Value,
    selection: Option<&Value>,
    findings: &mut Findings,
    schemas_dir: Option<&Path>,
) {
    let hints = crate::op_match::OpHints::from_selection(Some(workspace), selection, Some(sdl));
    let current = crate::spans::spans(sdl, inventory, &hints);
    // A root field keyed `Query.<f>` because its path is a tie the selection
    // does not settle: neither a hand edit to codify nor something `lock`
    // will record (ADR 0044). Reported at its line in the schema; the
    // remedy is in selection.yaml.
    // `lint_workspace` already validated `directory` and read the schema
    // through it before calling here, so this only fails to re-derive a
    // display name in a state the caller could not have reached; the
    // fallback is cosmetic, not a security boundary.
    let schema_file = crate::reconcile::schema_file_of(workspace)
        .unwrap_or_else(|_| "workspace.yaml".to_string());
    for s in current.iter().filter(|s| s.unattributed.is_some()) {
        findings.error(
            "unattributed-span",
            format!(
                "{}: {}; {}",
                s.key,
                s.unattributed.as_deref().unwrap_or(""),
                crate::spans::SETTLE_TIE
            ),
            Some(schema_file.as_str()),
            Some(s.line),
        );
    }
    match crate::spans::read_lock(dir) {
        Err(e) => findings.error(
            "unreadable-file",
            format!("{}: {}", crate::spans::LOCK_FILE, e),
            Some("applied.lock.yaml"),
            None,
        ),
        Ok(None) => findings.warn(
            "no-applied-lock",
            format!(
                "{} is missing; run `graphos-factory-core lock` after an apply so hand edits can be detected",
                crate::spans::LOCK_FILE
            ),
            Some("applied.lock.yaml"),
            None,
        ),
        Ok(Some(lock)) => {
            contract_check(
                &lock,
                "applied-lock.schema.json",
                ".factory/applied.lock.yaml",
                findings,
                schemas_dir,
            );
            // inventory.json is the tool's own output. A hand edit to it
            // is invisible to reconcile, sources status and every other
            // rule, and the next `inventory build` reverts it in silence —
            // so the lock watches it too (ADR 0018).
            if crate::spans::inventory_changed(dir, Some(&lock)) == Some(true) {
                findings.error(
                    "unacknowledged-inventory-edit",
                    format!(
                        "{} changed since applied.lock.yaml — the inventory is built, never edited: rebuild it from the description document, move a spec correction into the pinned document (graphos-factory-core codify --source), or move a judgement into selection.yaml; then run `graphos-factory-core lock`",
                        crate::spans::INVENTORY_FILE
                    ),
                    Some("applied.lock.yaml"),
                    None,
                );
            }
            for e in crate::spans::compare_with_lock(&current, &lock) {
                findings.error(
                    "unacknowledged-edit",
                    format!(
                        "{} {} since applied.lock.yaml — codify it (graphos-factory-core codify --key \"{}\" --reason …) or, if the agent wrote it, acknowledge it (graphos-factory-core lock)",
                        e.key,
                        match e.change {
                            "added" => "was added",
                            "removed" => "was removed",
                            _ => "changed",
                        },
                        e.key
                    ),
                    Some("applied.lock.yaml"),
                    e.line,
                );
            }
        }
    }
    if let Some(sel) = selection {
        for o in crate::reconcile::read_overrides(sel) {
            let span = current.iter().find(|s| s.key == o.key);
            if span.is_none() && !o.key.contains(":/") {
                findings.error(
                    "override-unknown-key",
                    format!(
                        "overrides lists {}, which the schema has no span for (expected type:<Name>, Query.<field> or Mutation.<field>)",
                        o.key
                    ),
                    Some("selection.yaml"),
                    None,
                );
                continue;
            }
            let span = match span {
                Some(s) => s,
                None => continue,
            };
            for (kind, value) in &o.assertions {
                match crate::spans::check_assertion(kind, value, span) {
                    Ok(true) => {}
                    Ok(false) => findings.error(
                        "override-assertion-failed",
                        format!(
                            "override {}: `{} {}` does not hold against the schema — the intent recorded in {} is no longer true; fix the schema or, with the engineer, the override",
                            o.key,
                            kind,
                            serde_json::to_string(value).unwrap_or_default(),
                            o.decision.as_deref().unwrap_or("selection.yaml")
                        ),
                        Some("selection.yaml"),
                        Some(span.line),
                    ),
                    Err(e) => findings.error(
                        "override-bad-assertion",
                        format!("override {}: {}", o.key, e),
                        Some("selection.yaml"),
                        None,
                    ),
                }
            }
        }
    }
}

/// Every e2e case in `tests/cases/*.graphql`, as (file stem, text), in
/// file-name order.
fn read_cases(dir: &Path) -> Vec<(String, String)> {
    let case_dir = dir.join("tests").join("cases");
    let mut cases: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&case_dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "graphql").unwrap_or(false))
            .collect();
        files.sort();
        for f in files {
            let name = f.file_stem().unwrap().to_string_lossy().to_string();
            cases.push((name, read(&f)));
        }
    }
    cases
}

/// Every `rover connector test` suite (`tests/*.connector.yaml`), sorted,
/// and their text joined.
fn read_suites(dir: &Path) -> (Vec<PathBuf>, String) {
    let tests_dir = dir.join("tests");
    let mut suites: Vec<PathBuf> = std::fs::read_dir(&tests_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.to_string_lossy().ends_with(".connector.yaml"))
                .collect()
        })
        .unwrap_or_default();
    suites.sort();
    let suite_text: String = suites
        .iter()
        .map(|f| read(f))
        .collect::<Vec<_>>()
        .join("\n");
    (suites, suite_text)
}

/// Every `(parent type, field name)` an operation in a case document
/// executes, keyed by the type its selection set is on (a named or inline
/// fragment's type condition, a nested field's return type). The walk
/// starts at each operation and follows fragment spreads, so a fragment
/// no operation spreads reads as nothing, and a field, spread or inline
/// fragment under `@skip(if: true)` or `@include(if: false)` is not
/// entered. A condition on a variable is taken as executed. Read with
/// apollo-compiler against the schema, so an alias reads as the field it
/// aliases and a comment or a string reads as nothing. A document that
/// does not build against the schema still yields what its partial parse
/// resolved.
fn selected_fields(
    schema: &apollo_compiler::validation::Valid<apollo_compiler::Schema>,
    text: &str,
    name: &str,
) -> HashSet<(String, String)> {
    use apollo_compiler::ast::{DirectiveList, Value as GqlValue};
    use apollo_compiler::executable::{ExecutableDocument, Selection, SelectionSet};
    /// False when a literal `@skip(if: true)` or `@include(if: false)`
    /// leaves the selection out of every execution.
    fn executed(directives: &DirectiveList) -> bool {
        let literal = |directive: &str| {
            directives
                .get(directive)
                .and_then(|d| d.specified_argument_by_name("if"))
                .and_then(|v| match v.as_ref() {
                    GqlValue::Boolean(b) => Some(*b),
                    _ => None,
                })
        };
        literal("skip") != Some(true) && literal("include") != Some(false)
    }
    fn walk(
        doc: &ExecutableDocument,
        set: &SelectionSet,
        spread: &mut HashSet<String>,
        out: &mut HashSet<(String, String)>,
    ) {
        for sel in &set.selections {
            match sel {
                Selection::Field(f) if executed(&f.directives) => {
                    out.insert((set.ty.to_string(), f.name.to_string()));
                    walk(doc, &f.selection_set, spread, out);
                }
                Selection::InlineFragment(i) if executed(&i.directives) => {
                    walk(doc, &i.selection_set, spread, out)
                }
                Selection::FragmentSpread(s) if executed(&s.directives) => {
                    // Each fragment once per document: its fields are keyed
                    // by its own type condition, so a second visit adds
                    // nothing, and a cycle ends here.
                    if spread.insert(s.fragment_name.to_string()) {
                        if let Some(fragment) = doc.fragments.get(&s.fragment_name) {
                            walk(doc, &fragment.selection_set, spread, out);
                        }
                    }
                }
                _ => {}
            }
        }
    }
    let doc = match ExecutableDocument::parse(schema, text, name) {
        Ok(d) => d,
        Err(with_errors) => with_errors.partial,
    };
    let mut out = HashSet::new();
    let mut spread = HashSet::new();
    for op in doc.operations.iter() {
        walk(&doc, &op.selection_set, &mut spread, &mut out);
    }
    out
}

/// Every `target:` value of every `tests[]` entry in the suites, read as
/// YAML, so a commented-out line or a block scalar's body is not a target.
/// A suite that does not parse contributes none: `rover connector test`
/// cannot run it either.
fn suite_targets(suites: &[PathBuf]) -> HashSet<String> {
    let mut out = HashSet::new();
    for suite in suites {
        let Ok(doc) = crate::yaml::parse(&read(suite)) else {
            continue;
        };
        for entry in doc
            .get("tests")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(t) = entry.get("target").and_then(Value::as_str) {
                out.insert(t.trim().to_string());
            }
        }
    }
    out
}

/// `link-untested` (ADR 0094): every field-level relationship connector
/// (`reconcile::link_connectors`) that no `rover connector test` entry
/// targets (`target: "<Type>.<field>"`) or no e2e case selects on its host
/// type. `missing-unit` and `missing-case` are keyed per selected
/// operation, so without this a workspace whose link fields have no tests
/// lints clean. Unlike `missing-unit`, the unit half is asked for when the
/// workspace has no suite at all, so a field with no test of either kind
/// reads as having neither. A warning, as `missing-unit` is: the
/// tests are hand-written until `scaffold` drafts them, and a
/// field with neither is reported as not validated (ADR 0069), which the
/// message and evidence's report both say.
fn lint_link_coverage(dir: &Path, sdl: &str, schema_file: &str, findings: &mut Findings) {
    let links = crate::reconcile::link_connectors(sdl);
    if links.is_empty() {
        return;
    }
    let (suites, _) = read_suites(dir);
    let targets = suite_targets(&suites);
    // What each case selects, keyed by case name: the null-parent rule asks
    // which case a mapping serves, so the union alone is not enough.
    let mut per_case: Vec<(String, HashSet<(String, String)>)> = Vec::new();
    let cases = read_cases(dir);
    if !cases.is_empty() {
        let schema = assume_valid_schema(sdl, schema_file);
        for (name, text) in &cases {
            per_case.push((
                name.clone(),
                selected_fields(&schema, text, &format!("tests/cases/{}.graphql", name)),
            ));
        }
    }
    let selected: HashSet<(String, String)> = per_case
        .iter()
        .flat_map(|(_, s)| s.iter().cloned())
        .collect();
    let mappings = read_mappings(dir);
    let mut index = crate::sdl_index::SdlIndex::new(sdl);
    for lc in links {
        let coordinate = format!("{}.{}", lc.type_name, lc.field);
        let key = (lc.type_name.clone(), lc.field.clone());
        let mut missing: Vec<String> = Vec::new();
        if !targets.contains(&coordinate) {
            missing.push(format!(
                "no unit entry (target: \"{}\" in tests/*.connector.yaml)",
                coordinate
            ));
        }
        let has_case = selected.contains(&key);
        if !has_case {
            missing.push(format!(
                "no e2e case (a tests/cases/*.graphql selecting {} on {})",
                lc.field, lc.type_name
            ));
        }
        // `link-null-untested` (ADR 0106): a nullable fk needs a case that
        // proves the null parent (connectors-language.md § Relationship
        // fields): a GET mapping on the empty-segment path, serving a case
        // that selects the field. A field with no e2e case at all is
        // `link-untested`'s already, so it is not reported twice.
        let path = lc.span.connect.as_ref().and_then(|c| c.path.clone());
        if let (true, Some(path)) = (has_case, path) {
            for fk in &lc.this_vars {
                let Some(fk_type) = index.field_type(&lc.type_name, fk) else {
                    continue;
                };
                if fk_type.trim_end().ends_with('!') {
                    continue;
                }
                let empty = path.replace(&format!("{{$this.{}}}", fk), "");
                let answered = mappings.iter().any(|(name, mapping)| {
                    answers_get(mapping, &empty)
                        && per_case.iter().any(|(case, fields)| {
                            fields.contains(&key) && mapping_serves(name, mapping, case)
                        })
                });
                if !answered {
                    findings.warn(
                        "link-null-untested",
                        format!(
                            "{} reads {{$this.{}}} and {} is nullable, but no e2e case answers the empty-segment GET {} for a null-{} parent; the field is not validated — add a mapping for that GET (\"urlPath\": \"{}\") serving a case that selects the field on a parent whose {} is null (connectors-language.md § Relationship fields)",
                            coordinate, fk, fk, empty, fk, empty, fk
                        ),
                        Some(schema_file),
                        Some(lc.span.line),
                    );
                }
            }
        }
        if missing.is_empty() {
            continue;
        }
        findings.warn(
            "link-untested",
            format!(
                "{} is a relationship field with {}; {} — write {} by hand (connectors-language.md § Relationship fields; `scaffold` does not draft them yet)",
                coordinate,
                missing.join(" and "),
                if missing.len() == 2 {
                    "with neither it is not validated, whatever evidence/latest.json says (it has no row for the field)"
                } else {
                    "the field is half tested"
                },
                if missing.len() == 2 { "both" } else { "it" }
            ),
            Some(schema_file),
            Some(lc.span.line),
        );
    }
}

/// The schema, parsed for type lookups only: `Schema::parse` +
/// `assume_valid`, as scaffold does. The connector directives come in
/// through `@link`, which the schema validator does not resolve.
fn assume_valid_schema(
    sdl: &str,
    schema_file: &str,
) -> apollo_compiler::validation::Valid<apollo_compiler::Schema> {
    let schema = match apollo_compiler::Schema::parse(sdl, schema_file) {
        Ok(s) => s,
        Err(with_errors) => with_errors.partial,
    };
    apollo_compiler::validation::Valid::assume_valid(schema)
}

/// Every readable `tests/fixtures/mappings/*.json`, sorted, by file name.
/// One that does not parse is `lint_coverage`'s `unreadable-file`.
fn read_mappings(dir: &Path) -> Vec<(String, Value)> {
    let mapping_dir = dir.join("tests").join("fixtures").join("mappings");
    let mut mappings: Vec<(String, Value)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&mapping_dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
            .collect();
        files.sort();
        for f in files {
            if let Ok(m) = crate::json::parse(&read(&f)) {
                mappings.push((f.file_name().unwrap().to_string_lossy().to_string(), m));
            }
        }
    }
    mappings
}

/// Whether e2e.sh loads this mapping for `case`: its `metadata."x-cases"`
/// list names it, `x-shared: true` serves every case, and otherwise the
/// file is named after the case. e2e.sh normalises both sides: hyphens and
/// underscores are one name.
fn mapping_serves(mapping_name: &str, mapping: &Value, case: &str) -> bool {
    let case = case.replace('-', "_");
    let metadata = get(mapping, "metadata");
    if let Some(Value::Array(list)) = metadata.and_then(|m| crate::json::field(m, "x-cases")) {
        return list
            .iter()
            .filter_map(Value::as_str)
            .any(|c| c.replace('-', "_") == case);
    }
    if metadata.and_then(|m| crate::json::field(m, "x-shared")) == Some(&Value::Bool(true)) {
        return true;
    }
    mapping_name.trim_end_matches(".json").replace('-', "_") == case
}

/// Whether a mapping answers `GET <path>`: method `GET` or `ANY`, and
/// `urlPath` equal to the path, or `url` equal to it or to it plus a query
/// string. Fixture paths are connector-relative: e2e renders `BASE_URL` at
/// the WireMock root.
fn answers_get(mapping: &Value, path: &str) -> bool {
    let Some(req) = get(mapping, "request") else {
        return false;
    };
    let method = get_str(req, "method").unwrap_or("");
    if !method.eq_ignore_ascii_case("GET") && !method.eq_ignore_ascii_case("ANY") {
        return false;
    }
    get_str(req, "urlPath") == Some(path)
        || get_str(req, "url").is_some_and(|u| u == path || u.starts_with(&format!("{}?", path)))
}

fn lint_coverage(
    dir: &Path,
    sdl: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let selection = match selection {
        Some(s) => s,
        None => return,
    };
    let prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let query: HashSet<String> = root_fields(sdl, "Query").into_iter().collect();
    let mutation: HashSet<String> = root_fields(sdl, "Mutation").into_iter().collect();

    let cases = read_cases(dir);
    let (suites, suite_text) = read_suites(dir);

    let mapping_dir = dir.join("tests").join("fixtures").join("mappings");
    let mut served: HashSet<String> = HashSet::new();
    if let Ok(entries) = std::fs::read_dir(&mapping_dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
            .collect();
        files.sort();
        for file in files {
            let base = file.file_name().unwrap().to_string_lossy().to_string();
            let mapping = match crate::json::parse(&read(&file)) {
                Ok(m) => m,
                Err(e) => {
                    findings.error(
                        "unreadable-file",
                        format!("tests/fixtures/mappings/{} is not valid JSON: {}", base, e),
                        Some(&base),
                        None,
                    );
                    continue;
                }
            };
            let metadata = get(&mapping, "metadata");
            let tagged = metadata.and_then(|m| crate::json::field(m, "x-cases"));
            let shared = metadata.and_then(|m| crate::json::field(m, "x-shared"));
            if let Some(Value::Array(list)) = tagged {
                for n in list.iter().filter_map(Value::as_str) {
                    served.insert(n.to_string());
                }
            } else if shared == Some(&Value::Bool(true)) {
                served.insert("*".to_string());
            } else if tagged.is_none() && shared.is_none() {
                served.insert(base.trim_end_matches(".json").replace('-', "_"));
            }
        }
    }

    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let name = match get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let field = format!("{}_{}", prefix, name);
        let root = get(entry, "graphql")
            .and_then(|g| get_str(g, "root"))
            .unwrap_or("query");
        let declared = if root == "query" { &query } else { &mutation };
        if !declared.contains(&field) {
            findings.error(
                "missing-field",
                format!(
                    "{} is included but {}.{} is not in the schema",
                    key,
                    if root == "query" { "Query" } else { "Mutation" },
                    field
                ),
                Some(
                    &dir.file_name()
                        .map(|f| f.to_string_lossy().to_string())
                        .unwrap_or_default(),
                ),
                None,
            );
            continue;
        }
        let re = Regex::new(&format!(r"\b{}\b", regex::escape(&field))).unwrap();
        let covering: Vec<&(String, String)> =
            cases.iter().filter(|(_, text)| re.is_match(text)).collect();
        if covering.is_empty() {
            findings.error(
                "missing-case",
                format!(
                    "{} has no test case in tests/cases that selects {}",
                    key, field
                ),
                Some("tests/cases"),
                None,
            );
        } else if !covering
            .iter()
            .any(|(n, _)| served.contains(n) || served.contains("*"))
        {
            findings.error(
                "missing-fixture",
                format!("no WireMock mapping is scoped to a case covering {} (name the file after the case or list it in metadata.\"x-cases\")", field),
                Some("tests/fixtures/mappings"),
                None,
            );
        }
        if !suites.is_empty() {
            let target = Regex::new(&format!(
                r#"target:\s*"?(Query|Mutation)\.{}\b"#,
                regex::escape(&field)
            ))
            .unwrap();
            // A forwarded fields default with a comma cannot be asserted by
            // rover (it percent-encodes the comma), so the e2e cases are the
            // proof and scaffold writes no unit entry (ADR 0045) — only for
            // an operation that really takes a string sparse parameter.
            let root_type = if root == "query" { "Query" } else { "Mutation" };
            let sparse_param = crate::sparse::param_name(workspace);
            let takes_sparse = inventory
                .and_then(|i| get_arr(i, "operations"))
                .into_iter()
                .flatten()
                .find(|o| get_str(o, "key") == Some(key))
                .is_some_and(|op| crate::sparse::string_param(workspace, op).is_some());
            let comma_default = takes_sparse
                && root_field_text(sdl, root_type, &field).is_some_and(|t| {
                    slot_expression(&t, "queryParams", &sparse_param).as_deref()
                        == Some(&format!("$args.{}", sparse_param))
                        && crate::sparse::declared_arg(&t, &sparse_param)
                            .and_then(|d| d.default)
                            .is_some_and(|d| d.contains(','))
                });
            if !target.is_match(&suite_text) && !comma_default {
                findings.warn(
                    "missing-unit",
                    format!(
                        "{} has no `rover connector test` entry asserting the request shape of {}",
                        key, field
                    ),
                    Some("tests"),
                    None,
                );
            }
        }
    }
}

/// One declared argument of a root field.
#[derive(Clone, Debug, PartialEq)]
pub struct Arg {
    pub name: String,
    pub type_: String,
}

impl Arg {
    pub fn is_list(&self) -> bool {
        self.type_.trim_end_matches('!').starts_with('[')
    }
    pub fn is_required(&self) -> bool {
        self.type_.ends_with('!')
    }
}

fn re(cell: &'static std::sync::OnceLock<Regex>, pattern: &str) -> &'static Regex {
    cell.get_or_init(|| Regex::new(pattern).unwrap())
}
static FIELD_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
static ARG_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
static ARG_NAME_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
static DIRECTIVE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
static RETURNS_LINE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

/// Index of the bracket that closes the one at `open` (any of `([{`), or
/// None when the text ends first. Strings and comments must already be
/// blanked.
fn matching_close(code: &[u8], open: usize) -> Option<usize> {
    let mut depth = 0i32;
    for (i, c) in code.iter().enumerate().skip(open) {
        match c {
            b'(' | b'[' | b'{' => depth += 1,
            b')' | b']' | b'}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(i);
                }
            }
            _ => {}
        }
    }
    None
}

/// Blank (offset-preserving) everything in an argument list that is not an
/// argument declaration: default values (`= 10`, `= { status: OPEN }`) and
/// directives on arguments (`@deprecated(reason: "…")`), whose inner
/// `name: value` pairs would otherwise read as arguments.
fn strip_defaults_and_directives(args: &str) -> String {
    let bytes = args.as_bytes();
    let mut out: Vec<u8> = bytes.to_vec();
    let blank = |out: &mut Vec<u8>, from: usize, to: usize| {
        for c in out.iter_mut().take(to.min(bytes.len())).skip(from) {
            if *c != b'\n' {
                *c = b' ';
            }
        }
    };
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'=' => {
                let mut j = i + 1;
                while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                    j += 1;
                }
                let end = if j < bytes.len() && matches!(bytes[j], b'[' | b'{' | b'(') {
                    matching_close(bytes, j)
                        .map(|c| c + 1)
                        .unwrap_or(bytes.len())
                } else {
                    let mut k = j;
                    while k < bytes.len()
                        && !bytes[k].is_ascii_whitespace()
                        && !matches!(bytes[k], b',' | b')' | b'@')
                    {
                        k += 1;
                    }
                    k
                };
                blank(&mut out, i, end);
                i = end.max(i + 1);
            }
            b'@' => {
                let m = re(&DIRECTIVE_RE, r"^@[A-Za-z_][A-Za-z0-9_]*\s*").find(&args[i..]);
                let name_end = i + m.map(|m| m.end()).unwrap_or(1);
                let end = if name_end < bytes.len() && bytes[name_end] == b'(' {
                    matching_close(bytes, name_end)
                        .map(|c| c + 1)
                        .unwrap_or(bytes.len())
                } else {
                    name_end
                };
                blank(&mut out, i, end);
                i = end.max(i + 1);
            }
            _ => i += 1,
        }
    }
    String::from_utf8(out).unwrap_or_else(|e| String::from_utf8_lossy(e.as_bytes()).to_string())
}

/// The root fields of `type <root> { … }` with their declared arguments, read
/// from the blanked SDL so descriptions, comments, default values and
/// argument directives do not confuse the scan. A field with no argument
/// list is not returned. Only the first `type <root>` block is read.
pub fn root_field_args(sdl: &str, root: &str) -> Vec<(String, Vec<Arg>)> {
    match crate::graphql::type_body(sdl, root) {
        Some(body) => body_field_args(&crate::graphql::blank(&body.body)),
        None => vec![],
    }
}

/// `root_field_args` for a type body the caller already holds, given as
/// `graphql::blank` of that body.
pub fn body_field_args(code: &str) -> Vec<(String, Vec<Arg>)> {
    let field_re = re(&FIELD_RE, r"(?m)^\s*([A-Za-z_][A-Za-z0-9_]*)\s*\(");
    // A type: `[ID!]`, `[ [String!]! ]!`, `Int`, with any interior whitespace.
    let arg_re = re(
        &ARG_RE,
        r"([A-Za-z_][A-Za-z0-9_]*)\s*:\s*((?:\[\s*)*[A-Za-z_][A-Za-z0-9_]*\s*!?(?:\s*\]\s*!?)*)",
    );
    let mut out = Vec::new();
    for m in field_re.captures_iter(code) {
        let open = m.get(0).unwrap().end() - 1;
        // A `(` inside a directive argument list is preceded by `@name`,
        // never by a bare identifier at line start, so `^\s*` excludes it.
        let close = match matching_close(code.as_bytes(), open) {
            Some(c) => c,
            None => continue,
        };
        let inner = strip_defaults_and_directives(&code[open + 1..close]);
        let args: Vec<Arg> = arg_re
            .captures_iter(&inner)
            .map(|a| Arg {
                name: a[1].to_string(),
                type_: a[2].chars().filter(|c| !c.is_whitespace()).collect(),
            })
            .collect();
        out.push((m[1].to_string(), args));
    }
    out
}

/// The number of elements in the list literal that starts at `open` (a `[`)
/// in the ORIGINAL document text: a string (quoted or block), an enum, a
/// number, a nested list or object each count once; commas are not needed
/// (GraphQL treats them as whitespace); `#` comments are skipped.
fn list_elements(orig: &[u8], open: usize) -> usize {
    let mut depth = 0i32;
    let mut count = 0usize;
    let mut in_element = false;
    let mut j = open;
    while j < orig.len() {
        let c = orig[j];
        if c == b'"' {
            if depth == 1 && !in_element {
                count += 1;
                in_element = true;
            }
            if orig[j..].starts_with(b"\"\"\"") {
                j = orig[j + 3..]
                    .windows(3)
                    .position(|w| w == b"\"\"\"")
                    .map(|p| j + 3 + p + 3)
                    .unwrap_or(orig.len());
            } else {
                j += 1;
                while j < orig.len() && orig[j] != b'"' && orig[j] != b'\n' {
                    j += if orig[j] == b'\\' { 2 } else { 1 };
                }
                j += 1;
            }
            continue;
        }
        if c == b'#' {
            while j < orig.len() && orig[j] != b'\n' {
                j += 1;
            }
            if depth == 1 {
                in_element = false;
            }
            continue;
        }
        match c {
            b'[' | b'{' | b'(' => {
                if depth == 1 && !in_element {
                    count += 1;
                    in_element = true;
                }
                depth += 1;
            }
            b']' | b'}' | b')' => {
                depth -= 1;
                if depth == 0 {
                    break;
                }
            }
            b',' => {
                if depth == 1 {
                    in_element = false;
                }
            }
            c if c.is_ascii_whitespace() => {
                if depth == 1 {
                    in_element = false;
                }
            }
            _ => {
                if depth == 1 && !in_element {
                    count += 1;
                    in_element = true;
                }
            }
        }
        j += 1;
    }
    count
}

/// The arguments a GraphQL document passes to `field`, with the number of
/// elements each list literal carries (None for a non-list value). Every
/// call of the field in the document counts (aliases included): the result
/// is their union, a list argument keeping its largest element count. None
/// when the document does not call the field with an argument list.
pub fn passed_args(document: &str, field: &str) -> Option<Vec<(String, Option<usize>)>> {
    let code = crate::graphql::blank(document);
    let orig = document.as_bytes();
    let call = Regex::new(&format!(r"\b{}\s*\(", regex::escape(field))).unwrap();
    let name_re = re(&ARG_NAME_RE, r"^([A-Za-z_][A-Za-z0-9_]*)\s*:\s*");
    let mut out: Vec<(String, Option<usize>)> = Vec::new();
    let mut found = false;
    for m in call.find_iter(&code) {
        let open = m.end() - 1;
        let close = match matching_close(code.as_bytes(), open) {
            Some(c) => c,
            None => continue,
        };
        found = true;
        let inner = &code[open + 1..close];
        let base = open + 1;
        let mut depth = 0i32;
        let mut i = 0;
        while i < inner.len() {
            if !inner.is_char_boundary(i) {
                i += 1;
                continue;
            }
            match inner.as_bytes()[i] {
                b'[' | b'{' => depth += 1,
                b']' | b'}' => depth -= 1,
                _ => {}
            }
            if depth == 0 {
                if let Some(m) = name_re.captures(&inner[i..]) {
                    let name = m[1].to_string();
                    let value_at = i + m.get(0).unwrap().end();
                    let count = if inner.as_bytes().get(value_at) == Some(&b'[') {
                        Some(list_elements(orig, base + value_at))
                    } else {
                        None
                    };
                    match out.iter_mut().find(|(n, _)| *n == name) {
                        Some(slot) => {
                            slot.1 = match (slot.1, count) {
                                (Some(a), Some(b)) => Some(a.max(b)),
                                (a, None) => a,
                                (None, b) => b,
                            }
                        }
                        None => out.push((name, count)),
                    }
                    i = value_at.max(i + 1);
                    continue;
                }
            }
            i += 1;
        }
    }
    if found {
        Some(out)
    } else {
        None
    }
}

/// The headers that carry the credential: every `@source` header whose value
/// contains `{{AUTH_EXPR}}`, lower-cased (rover reports headers that way).
/// Either key order (`name` then `value`, or the reverse) is read.
pub fn credential_headers(sdl: &str) -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    for (h, _) in credential_header_values(sdl) {
        if !out.contains(&h) {
            out.push(h);
        }
    }
    out
}

/// The credential headers with their value templates: every `@source` header
/// whose value contains `{{AUTH_EXPR}}`, as (lower-cased name, value as
/// written). `scaffold` renders the value; `credential_headers` keeps the names.
pub fn credential_header_values(sdl: &str) -> Vec<(String, String)> {
    let mut out: Vec<(String, String)> = Vec::new();
    for m in re(
        &CRED_NAME_VALUE_RE,
        r#"name:\s*"([^"]+)"\s*,?\s*value:\s*"([^"]*\{\{\s*AUTH_EXPR\s*\}\}[^"]*)""#,
    )
    .captures_iter(sdl)
    {
        out.push((m[1].to_lowercase(), m[2].to_string()));
    }
    for m in re(
        &CRED_VALUE_NAME_RE,
        r#"value:\s*"([^"]*\{\{\s*AUTH_EXPR\s*\}\}[^"]*)"\s*,?\s*name:\s*"([^"]+)""#,
    )
    .captures_iter(sdl)
    {
        out.push((m[2].to_lowercase(), m[1].to_string()));
    }
    out
}
static CRED_NAME_VALUE_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
static CRED_VALUE_NAME_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

/// The connector's `body` mapping, read as a flat `key: $args.arg` list.
pub enum BodyMapping {
    /// No `body` at all.
    None,
    /// Every entry is `key: $args.arg`: (arg, key) in order — one per line
    /// or several on one line (ADR 0042).
    Flat(Vec<(String, String)>),
    /// Nested objects, literals, methods — not a list of keys.
    NotFlat,
}

pub fn body_mapping(field_text: &str) -> BodyMapping {
    let b = match connector_block(field_text, "body") {
        Some(b) => b,
        None => return BodyMapping::None,
    };
    let mut out = Vec::new();
    for entry in block_entries(&b) {
        match (entry.key, flat_arg(&entry.expression)) {
            (Some(key), Some(arg)) => out.push((arg, key)),
            _ => return BodyMapping::NotFlat,
        }
    }
    if out.is_empty() {
        BodyMapping::NotFlat
    } else {
        BodyMapping::Flat(out)
    }
}

/// The text of one connector block (`queryParams`, `body`): the triple-quoted
/// form, or the one-line `"…"` form.
pub fn connector_block(field_text: &str, name: &str) -> Option<String> {
    Regex::new(&format!(r#"(?s)\b{}\s*:\s*"""(.*?)""""#, name))
        .unwrap()
        .captures(field_text)
        .map(|m| m[1].to_string())
        .or_else(|| {
            Regex::new(&format!(r#"\b{}\s*:\s*"([^"]*)""#, name))
                .unwrap()
                .captures(field_text)
                .map(|m| m[1].to_string())
        })
}

/// One entry of a connector block: `key: expression`, or a bare expression
/// with no key (`$args.input`, a stray `}`).
struct BlockEntry {
    key: Option<String>,
    expression: String,
}

/// The `queryParams` entries of one field's `@connect` that read no
/// `$args`, as `key: expression` in order (`format: $("full")`): the static
/// part of the request, which a relationship field copies from its by-id
/// root field (ADR 0069, R51). A key that is not a bare identifier is quoted
/// again (`"x-mode": $("v2")`); an entry with no key is left out.
pub fn static_query_params(field_text: &str) -> Vec<String> {
    let bare = |k: &str| {
        k.bytes()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == b'_')
            && k.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'_')
    };
    connector_block(field_text, "queryParams")
        .map(|q| block_entries(&q))
        .unwrap_or_default()
        .into_iter()
        .filter(|e| !e.expression.contains("$args"))
        .filter_map(|e| {
            e.key.map(|k| {
                if bare(&k) {
                    format!("{}: {}", k, e.expression)
                } else {
                    format!("\"{}\": {}", k, e.expression)
                }
            })
        })
        .collect()
}

/// The entries of a `queryParams` or `body` block, in order.
///
/// The mapping language separates entries with whitespace, and a newline is
/// whitespace: `a: $args.a b: $args.b` on one line is the same two entries
/// as one per line. Until ADR 0042 this module read a block line by line and
/// saw only the first pair of such a line (`wiring`) or no flat body at all
/// (`body_mapping`); the AppWorld LLM arm writes every block that way, so
/// three spec-facing rules were blind to it.
///
/// A newline still ends an entry, so a block written one pair per line reads
/// exactly as it did. Within a line an entry ends at a run of whitespace
/// outside `(…)`, `[…]`, `{…}` and `"…"` that is followed by another
/// `key:`, so `a: $args.a->match([1, 2], [3, 4]) b: $args.b` is two entries
/// and `size: $args.size ?? 1` is one. A blank line or a `#` comment line is
/// no entry. A `#` later on a line is not a comment to this reader: it stays
/// in the expression it follows, and a `key:` after it starts a new entry —
/// nothing in the pilots or the AppWorld snapshot writes a `#` inside a block.
fn block_entries(block: &str) -> Vec<BlockEntry> {
    let mut out = Vec::new();
    for line in block.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let bytes = line.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                i += 1;
            }
            if i >= bytes.len() {
                break;
            }
            let (key, start) = match entry_key_at(line, i) {
                Some((key, after)) => (Some(key), after),
                None => (None, i),
            };
            let end = expression_end(line, start).max(start);
            out.push(BlockEntry {
                key,
                expression: line[start..end].trim().to_string(),
            });
            // `entry_key_at` consumed at least the key and its colon, and
            // `expression_end` at least one non-blank byte otherwise.
            i = end.max(i + 1);
        }
    }
    out
}

/// `key:` or `"key":` at byte `i` of `line`: the key without its quotes, and
/// the byte where its expression starts (after the colon and any spaces).
/// `None` when what starts at `i` is not a key — a bare expression, a method,
/// a literal.
fn entry_key_at(line: &str, i: usize) -> Option<(String, usize)> {
    let bytes = line.as_bytes();
    let (key, mut j) = if bytes.get(i) == Some(&b'"') {
        let close = i + 1 + line[i + 1..].find('"')?;
        (line[i + 1..close].to_string(), close + 1)
    } else {
        let mut j = i;
        while j < bytes.len()
            && !bytes[j].is_ascii_whitespace()
            && bytes[j] != b'"'
            && bytes[j] != b':'
        {
            j += 1;
        }
        if j == i {
            return None;
        }
        (line[i..j].to_string(), j)
    };
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    if bytes.get(j) != Some(&b':') {
        return None;
    }
    j += 1;
    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
        j += 1;
    }
    Some((key, j))
}

/// Where the expression starting at byte `start` of `line` ends: the end of
/// the line, or the first run of whitespace outside brackets and string
/// literals that is followed by another `key:`.
fn expression_end(line: &str, start: usize) -> usize {
    let bytes = line.as_bytes();
    let mut depth = 0usize;
    let mut in_string = false;
    let mut i = start;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            match c {
                b'\\' => i += 1,
                b'"' => in_string = false,
                _ => {}
            }
        } else {
            match c {
                b'"' => in_string = true,
                b'(' | b'[' | b'{' => depth += 1,
                b')' | b']' | b'}' => depth = depth.saturating_sub(1),
                _ if depth == 0 && c.is_ascii_whitespace() => {
                    let mut j = i;
                    while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                        j += 1;
                    }
                    if j < bytes.len() && entry_key_at(line, j).is_some() {
                        return i;
                    }
                    i = j;
                    continue;
                }
                _ => {}
            }
        }
        i += 1;
    }
    bytes.len()
}

static ARGS_HEAD_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
static ARGS_ONLY_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
static ARGS_PATH_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

/// The argument an expression starts with: `$args.color`,
/// `$args.color->match(…)` and `$args.color.name` all name `color`.
fn args_ident(expression: &str) -> Option<String> {
    re(&ARGS_HEAD_RE, r"^\$args\.([A-Za-z_][A-Za-z0-9_]*)")
        .captures(expression)
        .map(|m| m[1].to_string())
}

/// The argument when the expression is exactly `$args.<arg>` and nothing
/// else — the only shape a flat body pair takes.
/// `$args.x` or `$args.x?` (the `?` drops the key when the value is null).
fn flat_arg(expression: &str) -> Option<String> {
    re(&ARGS_ONLY_RE, r"^\$args\.([A-Za-z_][A-Za-z0-9_]*)\??$")
        .captures(expression)
        .map(|m| m[1].to_string())
}

/// The path when the expression is exactly `$args.<path>` and nothing else:
/// `$args.color` is `[color]`, `$args.filterBy.x` is `[filterBy, x]`, and
/// `$args.color->match(…)` is none.
fn args_path(expression: &str) -> Option<Vec<String>> {
    re(
        &ARGS_PATH_RE,
        r"^\$args\.([A-Za-z_][A-Za-z0-9_]*(?:\.[A-Za-z_][A-Za-z0-9_]*)*)$",
    )
    .captures(expression)
    .map(|m| m[1].split('.').map(str::to_string).collect())
}

/// One root field's declaration text (from its name to the start of the next
/// field or the end of the type body), so the rules can read its `@connect`:
/// the HTTP verb, whether it sends a body, and how each argument maps to a
/// query parameter.
pub fn root_field_text(sdl: &str, root: &str, field: &str) -> Option<String> {
    let body = crate::graphql::type_body(sdl, root)?;
    body_field_text(&body.body, &crate::graphql::blank(&body.body), field)
}

/// `root_field_text` for a type body the caller already holds: `body` is
/// its text and `code` is `graphql::blank(body)`.
pub fn body_field_text(body: &str, code: &str, field: &str) -> Option<String> {
    let start_re = Regex::new(&format!(r"(?m)^[ \t]*{}\s*[(:]", regex::escape(field))).unwrap();
    let start = start_re.find(code)?.start();
    // The next field starts at bracket depth zero: a `http: {` or
    // `queryParams:` line inside the field's own `@connect(…)` does not.
    let next_re = Regex::new(r"(?m)^[ \t]*[A-Za-z_][A-Za-z0-9_]*\s*[(:]").unwrap();
    let bytes = code.as_bytes();
    let end = next_re
        .find_iter(code)
        .map(|m| m.start())
        .filter(|s| *s > start)
        .find(|s| {
            bytes[start..*s].iter().fold(0i32, |d, c| match c {
                b'(' | b'{' | b'[' => d + 1,
                b')' | b'}' | b']' => d - 1,
                _ => d,
            }) == 0
        })
        .unwrap_or(code.len());
    Some(body[start..end].to_string())
}

/// How a root field's arguments reach the wire, read from its `@connect`.
#[derive(Debug, Default, PartialEq)]
pub struct Wiring {
    /// `GET` / `POST` / …, from `http: { VERB: … }`.
    pub verb: Option<String>,
    /// The connector declares an `http.body` mapping.
    pub sends_body: bool,
    /// `queryParams` entries `key: $args.<arg>` → (arg, key), in order — one
    /// per line or several on one line (ADR 0042).
    pub query_keys: Vec<(String, String)>,
    /// The `queryParams` entries whose expression is exactly `$args.<path>`,
    /// no method → (path, key), in order and read like `query_keys`: a list
    /// there goes on the wire as the key repeated, and a dotted path
    /// (`"filter_by.x": $args.filterBy.x`) is one leaf of an input object
    /// (ADR 0051).
    pub plain_query_keys: Vec<(Vec<String>, String)>,
    /// Arguments referenced inside the `body` mapping.
    pub body_args: Vec<String>,
}

pub fn wiring(field_text: &str) -> Wiring {
    let mut w = Wiring {
        verb: Regex::new(r"\b(GET|POST|PUT|PATCH|DELETE|HEAD)\s*:")
            .unwrap()
            .captures(field_text)
            .map(|m| m[1].to_string()),
        ..Wiring::default()
    };
    if let Some(q) = connector_block(field_text, "queryParams") {
        for entry in block_entries(&q) {
            if let (Some(key), Some(arg)) = (entry.key, args_ident(&entry.expression)) {
                if let Some(path) = args_path(&entry.expression) {
                    w.plain_query_keys.push((path, key.clone()));
                }
                w.query_keys.push((arg, key));
            }
        }
    }
    if let Some(b) = connector_block(field_text, "body") {
        w.sends_body = true;
        let arg_re = Regex::new(r"\$args\.([A-Za-z_][A-Za-z0-9_]*)").unwrap();
        for m in arg_re.captures_iter(&b) {
            let a = m[1].to_string();
            if !w.body_args.contains(&a) {
                w.body_args.push(a);
            }
        }
    }
    w
}

/// The shape of the unit test suite itself:
///   unit-no-credential  a unit entry that does not assert the credential header
///   unit-no-response    an operation none of whose unit entries assert the
///                       mapped response
/// The status a WireMock stub answers with (`response.status`), as text. Read
/// only as a string of digits or an unsigned integer: `-1`, `"-1"`, a boolean
/// or a fraction is not a status and answers nothing.
fn stub_status(mapping: &Value) -> Option<String> {
    let r = get(mapping, "response")?;
    match get(r, "status")? {
        Value::String(s) if !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()) => {
            Some(s.clone())
        }
        v => v.as_u64().map(|n| n.to_string()),
    }
}

/// Whether a stub's request is the operation's own: the method of the
/// operation key (`get:/repos/{owner}/{repo}`) and its path template
/// (ADR 0072). A status served to any other request in a case -- a nested
/// lookup answering 404 -- is not the operation's. The method compares
/// ignoring case (`ANY`, or none, matches any). The path is compared segment
/// by segment, a `{parameter}` standing for exactly one non-empty segment; a
/// base path may precede it, so the stub's path must end with the template.
/// Read from `urlPath`, `url` (its query dropped), `urlPathTemplate` (names
/// ignored) and `urlPathPattern` (matched against the template with each
/// parameter filled by a sample segment); any other form, or a pattern that
/// does not compile, matches nothing.
fn stub_request_is_operations_own(mapping: &Value, operation_key: &str) -> bool {
    let Some((method, template)) = operation_key.split_once(':') else {
        return false;
    };
    let Some(request) = get(mapping, "request") else {
        return false;
    };
    if let Some(m) = get_str(request, "method") {
        if !m.eq_ignore_ascii_case("ANY") && !m.eq_ignore_ascii_case(method) {
            return false;
        }
    }
    let segments = |path: &str| -> Vec<String> {
        path.trim_start_matches('/')
            .split('/')
            .map(str::to_string)
            .collect()
    };
    let is_param = |s: &str| s.len() > 2 && s.starts_with('{') && s.ends_with('}');
    let wanted = segments(template);
    if let Some(pattern) = get_str(request, "urlPathPattern") {
        let sample: Vec<String> = wanted
            .iter()
            .map(|s| {
                if is_param(s) {
                    "x".to_string()
                } else {
                    s.clone()
                }
            })
            .collect();
        let sample = format!("/{}", sample.join("/"));
        return regex::Regex::new(&format!("^(?:{})$", pattern))
            .map(|re| re.is_match(&sample))
            .unwrap_or(false);
    }
    let path = ["urlPath", "urlPathTemplate", "url"]
        .iter()
        .find_map(|k| get_str(request, k));
    let Some(path) = path else {
        return false;
    };
    let got = segments(path.split('?').next().unwrap_or(path));
    got.len() >= wanted.len()
        && got[got.len() - wanted.len()..]
            .iter()
            .zip(&wanted)
            .all(|(g, w)| if is_param(w) { !g.is_empty() } else { g == w })
}

/// Ascending order for documented statuses: exact codes numerically, then
/// ranges (`4XX`), then `default`.
fn status_order(status: &str) -> (u8, u32, String) {
    if let Ok(n) = status.parse::<u32>() {
        (0, n, String::new())
    } else if status.eq_ignore_ascii_case("default") {
        (2, 0, String::new())
    } else {
        (1, 0, status.to_ascii_uppercase())
    }
}

/// Whether a stub answering `answered` exercises the documented `status`:
/// equal, a range (`4XX`), or `default` (any non-2xx).
fn status_covers(status: &str, answered: &str) -> bool {
    if answered.starts_with('2') {
        return false;
    }
    if status.eq_ignore_ascii_case("default") {
        return true;
    }
    let s = status.to_ascii_uppercase();
    if s.len() == 3 && s.ends_with("XX") {
        return answered.starts_with(&s[..1]);
    }
    status == answered
}

/// `unknown-tag` (ADR 0072): a `@tag` name outside the target's tag
/// vocabulary is never read by a tag-based policy. Once per name; a target
/// with no vocabulary restricts no name.
fn lint_tags(
    sdl: &str,
    schema_file: &str,
    target: &crate::target::Target,
    findings: &mut Findings,
) {
    let vocabulary = (target.tag_vocabulary)();
    if vocabulary.is_empty() {
        return;
    }
    // Found in the blanked text, so a `@tag` inside a description or a
    // comment is not one; the name is read from the original at the same
    // offset (blanking preserves offsets).
    let code = crate::graphql::blank(sdl);
    // The pattern stops at the colon: in the blanked text the literal is
    // spaces, and a trailing `\s*` would swallow it.
    let directive = Regex::new(r"@tag\s*\(\s*name\s*:").unwrap();
    let literal = Regex::new(r#"^\s*"([^"]*)""#).unwrap();
    let mut seen: HashSet<String> = HashSet::new();
    for m in directive.find_iter(&code) {
        let name = match literal.captures(&sdl[m.end()..]) {
            Some(c) => c[1].to_string(),
            None => continue,
        };
        if !vocabulary.contains(&name) && seen.insert(name.clone()) {
            findings.warn(
                "unknown-tag",
                format!(
                    "@tag(name: \"{}\") is not in this target's tag vocabulary ({}); no tag-based policy reads it — use a listed name",
                    name,
                    vocabulary.join(", ")
                ),
                Some(schema_file),
                None,
            );
        }
    }
}

///
/// list-arg-unproven, mutation-cases, loose-write-body and unit-no-body —
/// each a best-effort, string- or case-existence-based stand-in for outbound
/// proof — are retired (ADR 0079 Step 2): `crate::request_serialization`,
/// wired as the `write_body_proof` evidence layer, proves the same ground
/// (and more: location-specific placement, explicit null vs. omission, the
/// map five-case contract) from the schema, the connector wiring and
/// recorded e2e evidence, not from a case file's own argument list or a
/// stub's matcher type. A dropped argument whose value merely recurs
/// elsewhere in the demanded body — invisible to all four retired rules —
/// is caught by it (`tests/integration/lint.rs`,
/// `the_new_layer_catches_a_dropped_argument_whose_value_recurs_elsewhere_the_old_lints_miss`).
/// failure-case-missing (ADR 0072): every non-2xx status the inventory
/// documents for an operation needs an e2e case behind a stub answering with
/// it. Kept apart from `lint_test_shape` since the write-body proof retired
/// the shallow lints that shared its case and mapping scaffolding (ADR 0079).
fn lint_failure_cases(
    dir: &Path,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let selection = match selection {
        Some(s) => s,
        None => return,
    };
    let prefix = get_str(workspace, "field_prefix").unwrap_or("");
    // The non-2xx statuses the inventory documents for an operation, once
    // each, in order.
    let inventory_error_statuses = |key: &str| -> Vec<(String, String)> {
        let mut out: Vec<(String, String)> = Vec::new();
        for e in inventory
            .and_then(|inv| get_arr(inv, "operations"))
            .into_iter()
            .flatten()
            .find(|o| get_str(o, "key") == Some(key))
            .and_then(|o| get_arr(o, "errors"))
            .into_iter()
            .flatten()
        {
            let status = match get_str(e, "status") {
                Some(s) => s,
                None => continue,
            };
            if !status.starts_with('2') && !out.iter().any(|(s, _)| s == status) {
                out.push((
                    status.to_string(),
                    get_str(e, "description").unwrap_or("").to_string(),
                ));
            }
        }
        out
    };
    // Cases and the mappings that serve them (same classification as e2e.sh).
    let case_dir = dir.join("tests").join("cases");
    let mut cases: Vec<(String, String)> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(&case_dir) {
        let mut files: Vec<PathBuf> = entries
            .flatten()
            .map(|e| e.path())
            .filter(|p| p.extension().map(|e| e == "graphql").unwrap_or(false))
            .collect();
        files.sort();
        for f in files {
            cases.push((
                f.file_stem().unwrap().to_string_lossy().to_string(),
                read(&f),
            ));
        }
    }
    let mappings = read_mappings(dir);
    let serves = mapping_serves;
    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let name = match get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let field = format!("{}_{}", prefix, name);

        // failure-case-missing (ADR 0072): every non-2xx status the
        // inventory documents for the operation needs one e2e case behind a
        // stub answering with it, so the error mapping is exercised and not
        // only declared. Unit suites set no response status in any
        // workspace, so only e2e counts. `4XX` is met by any 4xx stub and
        // `default` by any non-2xx one.
        let error_statuses = inventory_error_statuses(key);
        if !error_statuses.is_empty() {
            let callers: Vec<&String> = cases
                .iter()
                .filter(|(_, text)| passed_args(text, &field).is_some())
                .map(|(n, _)| n)
                .collect();
            let answered: Vec<String> = callers
                .iter()
                .flat_map(|case| {
                    mappings
                        .iter()
                        .filter(|(mn, m)| serves(mn, m, case))
                        .filter(|(_, m)| stub_request_is_operations_own(m, key))
                        .filter_map(|(_, m)| stub_status(m))
                        .collect::<Vec<_>>()
                })
                .collect();
            // One finding per operation (plan item 10a), its missing statuses
            // in ascending order: the count reads as operations to look at.
            let mut missing: Vec<&(String, String)> = error_statuses
                .iter()
                .filter(|(status, _)| !answered.iter().any(|a| status_covers(status, a)))
                .collect();
            missing.sort_by_key(|(status, _)| status_order(status));
            if !missing.is_empty() {
                findings.warn(
                    "failure-case-missing",
                    format!(
                        "{}: no e2e case answers {} — the inventory documents {} for {}, and an error case per status is what proves the error mapping, not only that one exists",
                        key,
                        missing.iter().map(|(s, _)| s.as_str()).collect::<Vec<_>>().join(", "),
                        if missing.len() == 1 { "it" } else { "them" },
                        field
                    ),
                    Some("tests/cases"),
                    None,
                );
                if let Some(f) = findings.items.last_mut() {
                    f.detail = Some(Value::Array(
                        missing
                            .iter()
                            .map(|(status, description)| {
                                crate::json::object(vec![
                                    ("status", Value::from(status.as_str())),
                                    (
                                        "description",
                                        if description.is_empty() {
                                            Value::Null
                                        } else {
                                            Value::from(description.as_str())
                                        },
                                    ),
                                ])
                            })
                            .collect(),
                    ));
                }
            }
        }
    }
}

fn lint_test_shape(
    dir: &Path,
    sdl: &str,
    workspace: &Value,
    selection: Option<&Value>,
    _inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let selection = match selection {
        Some(s) => s,
        None => return,
    };
    let prefix = get_str(workspace, "field_prefix").unwrap_or("");

    // Unit entries.
    let tests_dir = dir.join("tests");
    let mut suites: Vec<PathBuf> = std::fs::read_dir(&tests_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.to_string_lossy().ends_with(".connector.yaml"))
                .collect()
        })
        .unwrap_or_default();
    suites.sort();
    let credentials = credential_headers(sdl);
    let mut response_by_field: HashMap<String, (usize, usize)> = HashMap::new();
    for suite_path in &suites {
        let file = suite_path
            .file_name()
            .unwrap()
            .to_string_lossy()
            .to_string();
        let suite = match crate::yaml::parse(&read(suite_path)) {
            Ok(s) => s,
            Err(_) => continue, // unreadable-file elsewhere
        };
        for entry in get_arr(&suite, "tests").into_iter().flatten() {
            let name = get_str(entry, "name").unwrap_or("");
            let target = get_str(entry, "target").unwrap_or("");
            let expect = get(entry, "expect");
            let cr = expect.and_then(|e| get(e, "connectorRequest"));
            // Only an entry that asserts the request at all is judged on
            // what the request asserts.
            if let (Some(cr), false) = (cr, credentials.is_empty()) {
                let has = get_obj(cr, "headers")
                    .map(|hs| {
                        hs.keys()
                            .any(|k| credentials.iter().any(|h| k.to_lowercase() == *h))
                    })
                    .unwrap_or(false);
                if !has {
                    findings.warn(
                        "unit-no-credential",
                        format!(
                            "tests/{} › {} asserts the request but not the {} header; the one thing every request must carry is the credential, and this entry would pass without it",
                            file, name, credentials.join(" / ")
                        ),
                        Some("tests"),
                        None,
                    );
                }
            }
            let field_name = target
                .strip_prefix("Query.")
                .or_else(|| target.strip_prefix("Mutation."));
            if let Some(field) = field_name {
                let slot = response_by_field.entry(field.to_string()).or_insert((0, 0));
                slot.0 += 1;
                if expect.and_then(|e| get(e, "connectorResponse")).is_some() {
                    slot.1 += 1;
                }
            }
        }
    }
    if !suites.is_empty() {
        for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
            if !truthy(get(entry, "include")) {
                continue;
            }
            let name = match get(entry, "graphql").and_then(|g| get_str(g, "name")) {
                Some(n) => n,
                None => continue,
            };
            let field = format!("{}_{}", prefix, name);
            if let Some((entries, with_response)) = response_by_field.get(&field) {
                if *entries > 0 && *with_response == 0 {
                    findings.warn(
                        "unit-no-response",
                        format!(
                            "{}: {} unit entr{} {} and none asserts connectorResponse; the request shape is proven, the mapping back is not",
                            key,
                            entries,
                            if *entries == 1 { "y targets" } else { "ies target" },
                            field
                        ),
                        Some("tests"),
                        None,
                    );
                }
            }
        }
    }
}

/// Every distinct WireMock `request` matcher must be served by exactly one
/// stub. `e2e.sh` loads every classified stub once, for the whole suite
/// (every stub is mounted flat), so two stubs
/// whose `request` object is structurally identical — the same object
/// WireMock itself matches a real request against — can only ever be
/// answered by whichever one WireMock's own tie-break happens to prefer,
/// never reliably by which case is running. Compares the whole `request`
/// object, not just the URL: two stubs that differ only in an
/// `equalToJson` body, a query parameter, or a header are not a collision.
/// A *partial* overlap (a stub with no `queryParameters` constraint racing
/// a more specific one on the same path) is `fixture-overlap`'s, below.
fn lint_fixture_collisions(dir: &Path, findings: &mut Findings) {
    let mapping_dir = dir.join("tests").join("fixtures").join("mappings");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&mapping_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let mut groups: Vec<(Value, Vec<(String, Value)>)> = Vec::new();
    for f in files {
        let name = f.file_name().unwrap().to_string_lossy().to_string();
        let Ok(mapping) = crate::json::parse(&read(&f)) else {
            continue; // lint_coverage already reports unreadable fixture JSON
        };
        let Some(req) = get(&mapping, "request") else {
            continue;
        };
        match groups.iter_mut().find(|(r, _)| r == req) {
            Some(group) => group.1.push((name, mapping)),
            None => groups.push((req.clone(), vec![(name, mapping)])),
        }
    }
    for (_, members) in groups {
        if members.len() > 1 && !is_scenario_disambiguated(&members) {
            findings.error(
                "fixture-collision",
                format!(
                    "{} stubs match the identical request and only one can ever answer it, WireMock's own tie-break deciding which: {}",
                    members.len(),
                    members
                        .iter()
                        .map(|(n, _)| n.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                Some("tests/fixtures/mappings"),
                None,
            );
        }
    }
}

/// The request keys whose value is a map of named matchers: a stub that
/// names fewer of them constrains less.
const NAMED_MATCHERS: [&str; 5] = [
    "queryParameters",
    "headers",
    "cookies",
    "formParameters",
    "pathParameters",
];

/// Whether stub `a`'s `request` matches every request stub `b`'s does:
/// each constraint `a` declares, `b` declares identically (a named matcher
/// of `a`'s is one of `b`'s, a body pattern of `a`'s is one of `b`'s, any
/// other key equal; `a`'s `ANY` method or no method matches any). Sound,
/// not complete: two matchers that overlap without one containing the
/// other (`equalTo` against `matches`) are not seen.
fn request_subsumes(a: &Value, b: &Value) -> bool {
    let (Some(a), Some(b)) = (a.as_object(), b.as_object()) else {
        return false;
    };
    a.iter().all(|(key, av)| {
        let bv = b.get(key);
        if NAMED_MATCHERS.contains(&key.as_str()) {
            let (Some(am), Some(bm)) = (av.as_object(), bv.and_then(Value::as_object)) else {
                return av.as_object().is_some_and(|m| m.is_empty());
            };
            am.iter().all(|(k, m)| bm.get(k) == Some(m))
        } else if key == "bodyPatterns" || key == "multipartPatterns" {
            let (Some(ap), Some(bp)) = (av.as_array(), bv.and_then(Value::as_array)) else {
                return av.as_array().is_some_and(|p| p.is_empty());
            };
            ap.iter().all(|p| bp.contains(p))
        } else if key == "method" && av.as_str() == Some("ANY") {
            true
        } else {
            bv == Some(av)
        }
    })
}

/// A stub's WireMock priority: lower answers first, 5 when unset.
fn stub_priority(mapping: &Value) -> u64 {
    get(mapping, "priority")
        .and_then(Value::as_u64)
        .unwrap_or(5)
}

/// The partial overlap `fixture-collision` cannot see (ADR 0115): stub A's
/// request matcher constrains a subset of what stub B's does, so A also
/// answers every request B was written for, and with every stub loaded at
/// once WireMock's own tie-break (the most recently loaded stub, an order
/// `e2e.sh` once took from the shell's locale) decides which answers it.
/// thecatapi's `generate_genealogy_minimal` stub, written without its
/// sibling's optional `lang` parameter, answered `generate_genealogy`'s
/// request under `LC_ALL=C` and not under `en_US.UTF-8`. A downstream
/// runner that mounts the whole mappings directory and reads no journal
/// lets the wrong stub answer silently, so every mapping is compared, the
/// ones loaded for no case too. Not a defect when B outranks A by
/// `priority` (B answers its own requests, A the rest), or when A answers
/// only in a scenario state a case sets on purpose.
fn lint_fixture_overlaps(dir: &Path, findings: &mut Findings) {
    let mapping_dir = dir.join("tests").join("fixtures").join("mappings");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&mapping_dir)
        .map(|rd| {
            rd.flatten()
                .map(|e| e.path())
                .filter(|p| p.extension().map(|e| e == "json").unwrap_or(false))
                .collect()
        })
        .unwrap_or_default();
    files.sort();
    let stubs: Vec<(String, Value)> = files
        .iter()
        .filter_map(|f| {
            let mapping = crate::json::parse(&read(f)).ok()?;
            get(&mapping, "request")?;
            Some((f.file_name()?.to_string_lossy().to_string(), mapping))
        })
        .collect();
    for (an, a) in &stubs {
        if get_str(a, "requiredScenarioState").is_some() {
            continue;
        }
        let ar = get(a, "request").unwrap();
        for (bn, b) in &stubs {
            let br = get(b, "request").unwrap();
            if an == bn || ar == br || stub_priority(b) < stub_priority(a) {
                continue; // identical requests are fixture-collision's
            }
            if !request_subsumes(ar, br) {
                continue;
            }
            let missing: Vec<String> = NAMED_MATCHERS
                .iter()
                .flat_map(|group| {
                    let have = get_obj(ar, group);
                    get_obj(br, group)
                        .into_iter()
                        .flatten()
                        .filter(move |(k, _)| !have.is_some_and(|h| h.contains_key(*k)))
                        .map(move |(k, _)| format!("{}.{}", group, k))
                })
                .collect();
            let fix = if missing.is_empty() {
                format!("give {} a higher priority (a lower number) than {}", bn, an)
            } else {
                format!(
                    "add {{\"absent\": true}} to {} for {}, or give {} a higher priority",
                    an,
                    missing.join(", "),
                    bn
                )
            };
            findings.error(
                "fixture-overlap",
                format!(
                    "{} matches every request {} matches, so with every stub loaded WireMock's tie-break, not the running case, decides which one answers it: {}",
                    an, bn, fix
                ),
                Some("tests/fixtures/mappings"),
                None,
            );
        }
    }
}

/// A colliding group is a legitimate WireMock Scenario, not a defect, when
/// every member names the same `scenarioName` and each declares its own
/// `requiredScenarioState`, no two the same — the only way to answer two
/// requests that are identical by definition (an RPC-style
/// `customFields.fetch` takes no body at all) differently. Anything less — no scenario, mixed
/// scenario names, a missing or repeated state — is still a collision.
fn is_scenario_disambiguated(members: &[(String, Value)]) -> bool {
    let Some(scenario) = get_str(&members[0].1, "scenarioName") else {
        return false;
    };
    if !members
        .iter()
        .all(|(_, m)| get_str(m, "scenarioName") == Some(scenario))
    {
        return false;
    }
    let mut states: Vec<&str> = members
        .iter()
        .filter_map(|(_, m)| get_str(m, "requiredScenarioState"))
        .collect();
    if states.len() != members.len() {
        return false; // every member must declare its own state
    }
    states.sort_unstable();
    states.dedup();
    states.len() == members.len() // every state distinct
}

// ── Error mapping paths ──────────────────────────────────────────────────────

/// One step of a `$.`-rooted path read from an `errors` mapping.
#[derive(Debug, Clone, PartialEq)]
enum ErrorPathStep {
    /// `.name` or `?.name`.
    Prop(String),
    /// `->first`, `->last`, `->get(<n>)` (and their `?->` forms): one
    /// element of an array.
    Item,
}

/// A `$.`-rooted path in an errors-mapping expression: the prefix the rule
/// can read, as written, its steps, and its byte offset in the scanned text.
struct ErrorPath {
    text: String,
    steps: Vec<ErrorPathStep>,
    offset: usize,
}

fn is_ident_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// The identifier starting at byte `i`, and the byte after it.
fn ident_at(b: &[u8], i: usize) -> Option<(String, usize)> {
    if i >= b.len() || !(b[i].is_ascii_alphabetic() || b[i] == b'_') {
        return None;
    }
    let mut j = i;
    while j < b.len() && is_ident_byte(b[j]) {
        j += 1;
    }
    Some((String::from_utf8_lossy(&b[i..j]).to_string(), j))
}

/// Read the path starting at the `$` of `$.` at byte `start`. The readable
/// grammar is `.name` / `?.name`, and `->first` / `->last` / `->get(<n>)`
/// with an optional `?` before the arrow. Anything else — another method,
/// `??`, `{`, a quoted key — ends the path: what was read so far is the
/// path, and nothing past it is judged. Returns the steps and the byte after
/// the last step read.
fn parse_error_path(b: &[u8], start: usize) -> (Vec<ErrorPathStep>, usize) {
    let mut steps = Vec::new();
    let mut i = start + 1; // the `.` after `$`
    let mut end = i;
    loop {
        let (optional_dot, arrow) = if b[i..].starts_with(b"?.") {
            (Some(i + 2), None)
        } else if b[i..].starts_with(b".") {
            (Some(i + 1), None)
        } else if b[i..].starts_with(b"?->") {
            (None, Some(i + 3))
        } else if b[i..].starts_with(b"->") {
            (None, Some(i + 2))
        } else {
            break;
        };
        if let Some(at) = optional_dot {
            match ident_at(b, at) {
                Some((name, after)) => {
                    steps.push(ErrorPathStep::Prop(name));
                    i = after;
                    end = after;
                }
                None => break,
            }
            continue;
        }
        let at = arrow.unwrap_or(i);
        match ident_at(b, at) {
            Some((name, after)) if name == "first" || name == "last" => {
                if b.get(after) == Some(&b'(') {
                    break;
                }
                steps.push(ErrorPathStep::Item);
                i = after;
                end = after;
            }
            Some((name, after)) if name == "get" && b.get(after) == Some(&b'(') => {
                let mut j = after + 1;
                while j < b.len() && b[j].is_ascii_digit() {
                    j += 1;
                }
                if j == after + 1 || b.get(j) != Some(&b')') {
                    break;
                }
                steps.push(ErrorPathStep::Item);
                i = j + 1;
                end = j + 1;
            }
            _ => break,
        }
    }
    (steps, end)
}

/// Every `$.`-rooted path in one errors-mapping value (the `message`
/// string's content, or the `extensions` block). Paths inside string
/// literals (`'…'`, `"…"`) and `#` comments are not paths. Only paths at the
/// top level count — directly, or inside a `$(…)` expression: inside a
/// method's arguments, a list or a `{ … }` subselection `$` is not
/// necessarily the response body, so those are left alone. A `??` chain
/// yields one path per alternative.
fn error_mapping_paths(expr: &str) -> Vec<ErrorPath> {
    let b = expr.as_bytes();
    let mut out = Vec::new();
    // One entry per open bracket: true when it hides `$` (a method's
    // arguments, a list, a subselection), false for a `$(…)` expression.
    let mut stack: Vec<bool> = Vec::new();
    let mut i = 0;
    while i < b.len() {
        let c = b[i];
        match c {
            b'\'' | b'"' => {
                let mut j = i + 1;
                while j < b.len() && b[j] != c {
                    j += if b[j] == b'\\' { 2 } else { 1 };
                }
                i = j + 1;
                continue;
            }
            b'#' => {
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                continue;
            }
            b'$' if b.get(i + 1) == Some(&b'(') => {
                stack.push(false);
                i += 2;
                continue;
            }
            b'$' if b.get(i + 1) == Some(&b'.')
                && !stack.iter().any(|hides| *hides)
                && (i == 0 || !is_ident_byte(b[i - 1])) =>
            {
                let (steps, end) = parse_error_path(b, i);
                if !steps.is_empty() {
                    out.push(ErrorPath {
                        text: expr[i..end].to_string(),
                        steps,
                        offset: i,
                    });
                }
                i = end.max(i + 2);
                continue;
            }
            b'(' | b'[' | b'{' => stack.push(true),
            b')' | b']' | b'}' => {
                stack.pop();
            }
            _ => {}
        }
        i += 1;
    }
    out
}

/// The `{ … }` contents of the `errors:` argument in a directive's argument
/// text or a root field's text, and the byte offset of those contents.
fn errors_block(text: &str) -> Option<(String, usize)> {
    let code = crate::graphql::blank(text);
    let m = Regex::new(r"\berrors\s*:\s*\{").unwrap().find(&code)?;
    let open = m.end() - 1;
    let close = matching_close(code.as_bytes(), open)?;
    Some((text[open + 1..close].to_string(), open + 1))
}

/// The `message` and `extensions` values of an errors block, each as the
/// string literal's content and its byte offset in the block. The keys are
/// found in the blanked block, so a `message:` inside the extensions
/// string is not mistaken for the block's own.
fn errors_block_values(block: &str) -> Vec<(String, usize)> {
    let code = crate::graphql::blank(block);
    let bytes = block.as_bytes();
    // No trailing `\s*`: the blanked string after the colon is spaces too,
    // so the value's opening quote is found in the original bytes.
    let key_re = Regex::new(r"\b(?:message|extensions)\s*:").unwrap();
    let mut out = Vec::new();
    for m in key_re.find_iter(&code) {
        let mut at = m.end();
        while at < bytes.len() && bytes[at].is_ascii_whitespace() {
            at += 1;
        }
        if bytes[at..].starts_with(b"\"\"\"") {
            let start = at + 3;
            if let Some(len) = block[start..].find("\"\"\"") {
                out.push((block[start..start + len].to_string(), start));
            }
        } else if bytes.get(at) == Some(&b'"') {
            let start = at + 1;
            let mut j = start;
            while j < bytes.len() && bytes[j] != b'"' && bytes[j] != b'\n' {
                j += if bytes[j] == b'\\' { 2 } else { 1 };
            }
            // An escaped quote is a quote in the mapping text; keep the byte
            // offsets by swapping the backslash for a space.
            let content = block[start..j.min(bytes.len())].replace("\\\"", " \"");
            out.push((content, start));
        }
    }
    out
}

/// Can this shape say for certain which properties a body carries? An
/// object with listed `properties` and nothing that admits others
/// (`additionalProperties` other than `false`, `patternProperties`, a
/// `oneOf`/`anyOf`/`not` this reader does not resolve), or an `allOf` of
/// such parts. Anything else — a free-form object, a scalar, an unresolved
/// `$ref` — cannot tell, and a path into it is not reported.
fn readable_object(shape: &Value, shapes: &Object, depth: usize) -> bool {
    if depth > 8 {
        return false;
    }
    let s = match deref_shape(shape, shapes) {
        Some(s) => s,
        None => return false,
    };
    if ["oneOf", "anyOf", "not", "patternProperties"]
        .iter()
        .any(|k| get(s, k).is_some())
    {
        return false;
    }
    if matches!(get(s, "additionalProperties"), Some(v) if v != &Value::Bool(false)) {
        return false;
    }
    let parts = get_arr(s, "allOf");
    if !parts
        .into_iter()
        .flatten()
        .all(|p| readable_object(p, shapes, depth + 1))
    {
        return false;
    }
    get_obj(s, "properties")
        .map(|p| !p.is_empty())
        .unwrap_or(false)
        || get_arr(s, "allOf").map(|a| !a.is_empty()).unwrap_or(false)
}

#[derive(Debug, PartialEq)]
enum PathResolves {
    Yes,
    No,
    CannotTell,
}

/// Walk an error-mapping path into one documented error body. A property
/// step over an array steps into its items first (the mapping language maps
/// a path over a list); an item step over a non-array stays put.
fn resolve_error_path(shape: &Value, shapes: &Object, steps: &[ErrorPathStep]) -> PathResolves {
    let mut cur = match deref_shape(shape, shapes) {
        Some(s) => s,
        None => return PathResolves::CannotTell,
    };
    for step in steps {
        if is_array_shape(cur) {
            cur = match get(cur, "items").and_then(|i| deref_shape(i, shapes)) {
                Some(s) => s,
                None => return PathResolves::CannotTell,
            };
            if *step == ErrorPathStep::Item {
                continue;
            }
        }
        let name = match step {
            ErrorPathStep::Prop(name) => name,
            ErrorPathStep::Item => continue,
        };
        if !readable_object(cur, shapes, 0) {
            return PathResolves::CannotTell;
        }
        cur = match property_of(cur, shapes, name, 0) {
            Some(p) => match deref_shape(p, shapes) {
                Some(s) => s,
                None => return PathResolves::CannotTell,
            },
            None => return PathResolves::No,
        };
    }
    PathResolves::Yes
}

/// The top-level property names a documented error body carries (through
/// `allOf` parts, and an array body's items), in first-seen order.
fn body_properties(shape: &Value, shapes: &Object, depth: usize, out: &mut Vec<String>) {
    if depth > 8 {
        return;
    }
    let mut s = match deref_shape(shape, shapes) {
        Some(s) => s,
        None => return,
    };
    if is_array_shape(s) {
        s = match get(s, "items").and_then(|i| deref_shape(i, shapes)) {
            Some(i) => i,
            None => return,
        };
    }
    for name in get_obj(s, "properties").into_iter().flat_map(|p| p.keys()) {
        if !out.contains(name) {
            out.push(name.clone());
        }
    }
    for part in get_arr(s, "allOf").into_iter().flatten() {
        body_properties(part, shapes, depth + 1, out);
    }
}

/// One warning rule, `error-path-unresolved`: every `$.`-rooted path in a
/// connector `errors` block — the `message` expression and every
/// `extensions` expression — must resolve in at least one of the error
/// bodies the inventory documents for the operations the block covers.
///
/// The `@source` block covers every included operation whose `@connect`
/// names that source; a `@connect`'s own `errors` block covers that one
/// operation. The documented bodies are the non-null `errors[].shape_ref`s
/// of those operations (ADR 0043 keeps inline ones). With no documented
/// body there is nothing to check against and the block is silent. A body
/// that cannot say which properties it carries (free-form, `oneOf`,
/// `additionalProperties`) counts as resolving: the rule must never fire on
/// a correct schema. The mapping that motivated it read `$.detail` while
/// every documented body was `{ message }`: a green unit suite, and every
/// error surfaced as the fallback string.
fn lint_error_paths(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };
    let shapes = match get(inventory, "shapes").and_then(Value::as_object) {
        Some(s) => s,
        None => return,
    };
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let operations: HashMap<&str, &Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .filter_map(|o| get_str(o, "key").map(|k| (k, o)))
        .collect();
    let source_re = Regex::new(r#"\bsource\s*:\s*"([^"]*)""#).unwrap();
    let error_refs = |op: &Value| -> Vec<String> {
        get_arr(op, "errors")
            .into_iter()
            .flatten()
            .filter_map(|e| get_str(e, "shape_ref").map(str::to_string))
            .collect()
    };

    // Judge one errors block against the documented bodies of the
    // operations it covers. `line_at` maps a byte offset in `block` to its
    // line in the schema file.
    let judge = |label: &str,
                 block: &str,
                 line_at: &dyn Fn(usize) -> usize,
                 refs: &[String],
                 covered: usize,
                 findings: &mut Findings| {
        let mut bodies: Vec<Value> = Vec::new();
        let mut seen: HashSet<String> = HashSet::new();
        for r in refs {
            let reference = crate::json::object(vec![("$ref", Value::from(r.as_str()))]);
            let key = deref_shape(&reference, shapes)
                .map(|s| s.to_string())
                .unwrap_or_else(|| r.clone());
            if seen.insert(key) {
                bodies.push(reference);
            }
        }
        if bodies.is_empty() {
            return;
        }
        let mut reported: HashSet<String> = HashSet::new();
        for (content, at) in errors_block_values(block) {
            for path in error_mapping_paths(&content) {
                if reported.contains(&path.text) {
                    continue;
                }
                let unresolved = bodies
                    .iter()
                    .all(|b| resolve_error_path(b, shapes, &path.steps) == PathResolves::No);
                if !unresolved {
                    continue;
                }
                reported.insert(path.text.clone());
                let mut carried = Vec::new();
                for b in &bodies {
                    body_properties(b, shapes, 0, &mut carried);
                }
                findings.warn(
                    "error-path-unresolved",
                    format!(
                        "`{}` in {} resolves in no documented error body ({} distinct {} checked across the {} {} it covers; documented error bodies carry: {}); take the path from the operation's errors[].shape_ref in .factory/inventory.json (schema-authoring.md § Errors)",
                        path.text,
                        label,
                        bodies.len(),
                        if bodies.len() == 1 { "body" } else { "bodies" },
                        covered,
                        if covered == 1 { "operation" } else { "operations" },
                        if carried.is_empty() { "no named properties".to_string() } else { carried.join(", ") },
                    ),
                    Some(schema_file),
                    Some(line_at(at + path.offset)),
                );
            }
        }
    };

    // (source name) -> documented error refs and covered-operation count.
    let mut by_source: HashMap<String, (Vec<String>, usize)> = HashMap::new();
    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let name = match get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let root = get(entry, "graphql")
            .and_then(|g| get_str(g, "root"))
            .unwrap_or("query");
        let root_type = if root == "query" { "Query" } else { "Mutation" };
        let field = format!("{}_{}", field_prefix, name);
        let field_text = match root_field_text(sdl, root_type, &field) {
            Some(t) => t,
            None => continue,
        };
        let op = match operations.get(key.as_str()) {
            Some(o) => *o,
            None => continue,
        };
        let refs = error_refs(op);
        if let Some(source) = source_re.captures(&field_text).map(|m| m[1].to_string()) {
            let slot = by_source.entry(source).or_default();
            slot.0.extend(refs.iter().cloned());
            slot.1 += 1;
        }
        // Rule: error-path-unresolved (a @connect's own errors block)
        if let Some((block, at)) = errors_block(&field_text) {
            let first_line = field_line(sdl, root_type, &field).unwrap_or(1);
            let line_at = |off: usize| {
                first_line
                    + field_text.as_bytes()[..(at + off).min(field_text.len())]
                        .iter()
                        .filter(|c| **c == b'\n')
                        .count()
            };
            judge(
                &format!("the `{}.{}` @connect errors block", root_type, field),
                &block,
                &line_at,
                &refs,
                1,
                findings,
            );
        }
    }

    // Rule: error-path-unresolved (the @source errors block)
    let name_re = Regex::new(r#"\bname\s*:\s*"([^"]*)""#).unwrap();
    for d in directives(sdl, "source") {
        let args_start = match sdl[d.index..].find('(') {
            Some(p) => d.index + p + 1,
            None => continue,
        };
        let name = match name_re.captures(&d.args) {
            Some(m) => m[1].to_string(),
            None => continue,
        };
        let (block, at) = match errors_block(&d.args) {
            Some(b) => b,
            None => continue,
        };
        let (refs, covered) = match by_source.get(&name) {
            Some(x) => x,
            None => continue,
        };
        let line_at = |off: usize| line_of(sdl, args_start + at + off);
        judge(
            &format!("the @source(name: \"{}\") errors block", name),
            &block,
            &line_at,
            refs,
            *covered,
            findings,
        );
    }
}

/// Whether an `isSuccess` path is missing from a success body, and if so the
/// properties the body (or the branch that lacks it) does carry. A plain
/// body lacks it when the path resolves to nothing. A `oneOf`/`anyOf` body
/// lacks it when no branch carries it, or when one branch carries it and
/// another does not: the branch without it reads null, and that branch may
/// be the success payload.
fn success_lacks(body: &Value, shapes: &Object, steps: &[ErrorPathStep]) -> Option<Vec<String>> {
    let mut carried = Vec::new();
    if resolve_error_path(body, shapes, steps) == PathResolves::No {
        body_properties(body, shapes, 0, &mut carried);
        return Some(carried);
    }
    let root = deref_shape(body, shapes)?;
    let branches = get_arr(root, "oneOf").or_else(|| get_arr(root, "anyOf"))?;
    let verdicts: Vec<(PathResolves, &Value)> = branches
        .iter()
        .map(|b| (resolve_error_path(b, shapes, steps), b))
        .collect();
    let carries = verdicts.iter().any(|(v, _)| *v == PathResolves::Yes);
    let lacking = verdicts.iter().find(|(v, _)| *v == PathResolves::No)?;
    if !carries && verdicts.iter().any(|(v, _)| *v == PathResolves::CannotTell) {
        return None;
    }
    body_properties(lacking.1, shapes, 0, &mut carried);
    Some(carried)
}

/// The string literal (`"…"` or `"""…"""`) after the `isSuccess:` key in a
/// directive's argument text or a root field's text, and its byte offset.
/// The key is found in the blanked text, so one inside a string or a comment
/// is not read.
fn is_success_value(text: &str) -> Option<(String, usize)> {
    let code = crate::graphql::blank(text);
    let m = Regex::new(r"\bisSuccess\s*:").unwrap().find(&code)?;
    let bytes = text.as_bytes();
    let mut at = m.end();
    while at < bytes.len() && bytes[at].is_ascii_whitespace() {
        at += 1;
    }
    if bytes[at..].starts_with(b"\"\"\"") {
        let start = at + 3;
        let len = text[start..].find("\"\"\"")?;
        return Some((text[start..start + len].to_string(), start));
    }
    if bytes.get(at) != Some(&b'"') {
        return None;
    }
    let start = at + 1;
    let mut j = start;
    while j < bytes.len() && bytes[j] != b'"' && bytes[j] != b'\n' {
        j += if bytes[j] == b'\\' { 2 } else { 1 };
    }
    Some((text[start..j.min(bytes.len())].to_string(), start))
}

/// One warning rule, `success-path-unresolved`: every `$.`-rooted path an
/// `isSuccess` expression reads must resolve in the documented success body
/// of each operation it decides. The `@source` expression decides every
/// included operation on that source without an `isSuccess` of its own; a
/// `@connect`'s decides that one operation. A path absent from the body
/// reads null, so the expression is false and every success of that
/// operation becomes a connector error: a recruiting API's source-wide `$.success` did
/// this to an operation whose body has no `success` key, and only an e2e
/// snapshot full of nulls showed it. A path followed by `??` has a fallback
/// and is not reported, nor is an operation with no documented success
/// body, nor a body that cannot say which properties it carries. A 200 body
/// that is a `oneOf`/`anyOf` of a success and an error branch is judged on
/// the success branch the inventory resolved (`referenced_shape`); with
/// none resolved, it is reported when some branches carry the path and
/// another lacks it, since a branch without it always reads as a failure.
/// Its custom-fields read was that: the error branch carries
/// `success`, the success branch does not.
fn lint_success_paths(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };
    let shapes = match get(inventory, "shapes").and_then(Value::as_object) {
        Some(s) => s,
        None => return,
    };
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let operations: HashMap<&str, &Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .filter_map(|o| get_str(o, "key").map(|k| (k, o)))
        .collect();
    let source_re = Regex::new(r#"\bsource\s*:\s*"([^"]*)""#).unwrap();
    let success_body = |op: &Value| -> Option<Value> {
        let response = get(op, "response")?;
        if let Some(branch) = get(response, "referenced_shape") {
            return Some(match branch.as_str() {
                Some(r) => crate::json::object(vec![("$ref", Value::from(r))]),
                None => branch.clone(),
            });
        }
        let r = get_str(response, "shape_ref")?;
        Some(crate::json::object(vec![("$ref", Value::from(r))]))
    };
    // The paths of one expression that have no `??` fallback after them.
    let unguarded = |content: &str| -> Vec<ErrorPath> {
        error_mapping_paths(content)
            .into_iter()
            .filter(|p| {
                !content[p.offset + p.text.len()..]
                    .trim_start()
                    .starts_with("??")
            })
            .collect()
    };

    // (source name) -> (operation key, success body) for the operations the
    // source's expression decides.
    let mut by_source: HashMap<String, Vec<(String, Value)>> = HashMap::new();
    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let name = match get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let root = get(entry, "graphql")
            .and_then(|g| get_str(g, "root"))
            .unwrap_or("query");
        let root_type = if root == "query" { "Query" } else { "Mutation" };
        let field = format!("{}_{}", field_prefix, name);
        let Some(field_text) = root_field_text(sdl, root_type, &field) else {
            continue;
        };
        let Some(op) = operations.get(key.as_str()) else {
            continue;
        };
        let own = is_success_value(&field_text);
        let Some(body) = success_body(op) else {
            continue;
        };
        match own {
            // Rule: success-path-unresolved (a @connect's own isSuccess)
            Some((content, at)) => {
                for path in unguarded(&content) {
                    let Some(carried) = success_lacks(&body, shapes, &path.steps) else {
                        continue;
                    };
                    let first_line = field_line(sdl, root_type, &field).unwrap_or(1);
                    let line = first_line
                        + field_text.as_bytes()[..(at + path.offset).min(field_text.len())]
                            .iter()
                            .filter(|c| **c == b'\n')
                            .count();
                    findings.warn(
                        "success-path-unresolved",
                        format!(
                            "{}: `{}` in the `{}.{}` @connect isSuccess is not in the documented success body (it carries: {}); it reads null there, so every success of this operation is reported as a connector error — read a property the body has, or give it a fallback (`{} ?? true`) (schema-authoring.md § Errors)",
                            key,
                            path.text,
                            root_type,
                            field,
                            if carried.is_empty() { "no named properties".to_string() } else { carried.join(", ") },
                            path.text,
                        ),
                        Some(schema_file),
                        Some(line),
                    );
                }
            }
            None => {
                if let Some(source) = source_re.captures(&field_text).map(|m| m[1].to_string()) {
                    by_source
                        .entry(source)
                        .or_default()
                        .push((key.clone(), body));
                }
            }
        }
    }

    // Rule: success-path-unresolved (the @source isSuccess)
    let name_re = Regex::new(r#"\bname\s*:\s*"([^"]*)""#).unwrap();
    for d in directives(sdl, "source") {
        let Some(args_start) = sdl[d.index..].find('(').map(|p| d.index + p + 1) else {
            continue;
        };
        let Some(name) = name_re.captures(&d.args).map(|m| m[1].to_string()) else {
            continue;
        };
        let Some((content, at)) = is_success_value(&d.args) else {
            continue;
        };
        let Some(decided) = by_source.get(&name) else {
            continue;
        };
        for path in unguarded(&content) {
            let missing: Vec<&str> = decided
                .iter()
                .filter(|(_, body)| success_lacks(body, shapes, &path.steps).is_some())
                .map(|(k, _)| k.as_str())
                .collect();
            if missing.is_empty() {
                continue;
            }
            const SHOWN: usize = 5;
            let mut named = missing
                .iter()
                .take(SHOWN)
                .copied()
                .collect::<Vec<_>>()
                .join(", ");
            if missing.len() > SHOWN {
                named.push_str(&format!(" and {} more", missing.len() - SHOWN));
            }
            findings.warn(
                "success-path-unresolved",
                format!(
                    "`{}` in the @source(name: \"{}\") isSuccess is not in the documented success body of {} of the {} {} it decides ({}); it reads null there, so every success of those operations is reported as a connector error — give it a fallback (`{} ?? true`), or set isSuccess on those operations' @connect (schema-authoring.md § Errors)",
                    path.text,
                    name,
                    missing.len(),
                    decided.len(),
                    if decided.len() == 1 { "operation" } else { "operations" },
                    named,
                    path.text,
                ),
                Some(schema_file),
                Some(line_of(sdl, args_start + at + path.offset)),
            );
        }
    }
}

/// One warning rule, `wire-key-undocumented`: a query key a selected
/// operation's connector sends — a `queryParams` key, or a `name=` pair in
/// the URL template's query string — that the inventory does not document
/// as one of that operation's query parameters. A stub `scaffold` drafts
/// takes its query matchers from the connector, so it demands the same key
/// and every offline layer passes: incidentio's schedule entries sent
/// `after`/`before` where the spec has `entry_window_start`/`_end`. A key
/// the vendor accepts without documenting it is a spec correction: patch
/// the pinned document so the inventory records it. Keys built from an
/// expression, and an operation the inventory does not have, are left alone.
fn lint_wire_keys(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let operations: HashMap<&str, &Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .filter_map(|o| get_str(o, "key").map(|k| (k, o)))
        .collect();
    let path_re = Regex::new(r#"\b(?:GET|POST|PUT|PATCH|DELETE|HEAD)\s*:\s*"([^"]+)""#).unwrap();
    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let Some(name) = get(entry, "graphql").and_then(|g| get_str(g, "name")) else {
            continue;
        };
        let root = get(entry, "graphql")
            .and_then(|g| get_str(g, "root"))
            .unwrap_or("query");
        let root_type = if root == "query" { "Query" } else { "Mutation" };
        let field = format!("{}_{}", field_prefix, name);
        let Some(field_text) = root_field_text(sdl, root_type, &field) else {
            continue;
        };
        let Some(op) = operations.get(key.as_str()) else {
            continue;
        };
        // The documented query parameters, and any pair the operation's own
        // path carries (`/?operation=user` keys an RPC-style endpoint).
        let mut documented: Vec<&str> = get_arr(op, "parameters")
            .into_iter()
            .flatten()
            .filter(|p| get_str(p, "in") == Some("query"))
            .filter_map(|p| get_str(p, "name"))
            .collect();
        if let Some((_, q)) = get_str(op, "path").and_then(|p| p.split_once('?')) {
            documented.extend(q.split('&').filter_map(|pair| pair.split('=').next()));
        }
        let mut sent: Vec<String> = connector_block(&field_text, "queryParams")
            .map(|q| block_entries(&q))
            .unwrap_or_default()
            .into_iter()
            .filter_map(|e| e.key)
            .collect();
        if let Some(query) = path_re
            .captures(&field_text)
            .and_then(|m| m[1].split_once('?').map(|(_, q)| q.to_string()))
        {
            sent.extend(
                query
                    .split('&')
                    .map(|pair| pair.split('=').next().unwrap_or("").to_string()),
            );
        }
        let mut undocumented: Vec<String> = Vec::new();
        for k in sent {
            let k = k.trim().trim_matches('\'').to_string();
            let dynamic = k.is_empty() || k.contains('{') || k.contains('$');
            // The documented name as written (`statuses[]` for `statuses[]`),
            // or with a list suffix the source leaves off (`ids[]` for `ids`).
            // A suffix the source requires and the connector drops is a
            // different key on the wire, and is reported.
            // A member of a documented object parameter, spelled
            // `filter_by.state` or `filter[state]` (deepObject), is that
            // parameter on the wire.
            let object_member = k
                .find(['.', '['])
                .filter(|at| !k[*at..].starts_with("[]"))
                .is_some_and(|at| documented.contains(&&k[..at]));
            let matches = documented.contains(&k.as_str())
                || k.strip_suffix("[]")
                    .is_some_and(|base| documented.contains(&base))
                || object_member;
            if dynamic || matches || undocumented.contains(&k) {
                continue;
            }
            undocumented.push(k);
        }
        if undocumented.is_empty() {
            continue;
        }
        documented.sort_unstable();
        documented.dedup();
        findings.warn(
            "wire-key-undocumented",
            format!(
                "{}: {}.{} sends query {} {} that the source does not document for this operation (documented: {}); a stub scaffolded from the connector demands the same {}, so no offline layer can tell — send the documented name, or, when the vendor accepts it undocumented, correct the pinned document and `codify --source` so the inventory records it (spec-intake.md § Pinned specs and patches)",
                key,
                root_type,
                field,
                if undocumented.len() == 1 { "key" } else { "keys" },
                undocumented
                    .iter()
                    .map(|k| format!("`{}`", k))
                    .collect::<Vec<_>>()
                    .join(", "),
                if documented.is_empty() { "none".to_string() } else { documented.join(", ") },
                if undocumented.len() == 1 { "key" } else { "keys" },
            ),
            Some(schema_file),
            field_line(sdl, root_type, &field),
        );
    }
}

/// One warning rule, `connector-path-undocumented`: a selected operation's
/// connector selects a path its documented success body does not have —
/// the same fact `reconcile` reports as fields drift, here where `evidence`
/// records it. A stub `scaffold` drafts carries every path the selection
/// names, so the e2e case maps it and passes; the vendor sends no such
/// field and the agent reads null. A field the vendor does send without
/// documenting it is a spec correction (patch the pinned document). Silent
/// where the shapes were inferred from samples, and for an open shape
/// (`additionalProperties`, `oneOf`), which `reconcile` cannot walk either.
fn lint_connector_paths(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };
    if get_str(workspace, "intake") == Some("discovered") {
        return;
    }
    let Some(shapes) = get(inventory, "shapes").and_then(Value::as_object) else {
        return;
    };
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let operations: HashMap<&str, &Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .filter_map(|o| get_str(o, "key").map(|k| (k, o)))
        .collect();
    let spans: HashMap<&str, Vec<crate::reconcile::FieldSpan>> = ["Query", "Mutation"]
        .iter()
        .map(|r| (*r, crate::reconcile::field_spans(sdl, r)))
        .collect();
    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let Some(name) = get(entry, "graphql").and_then(|g| get_str(g, "name")) else {
            continue;
        };
        let root = get(entry, "graphql")
            .and_then(|g| get_str(g, "root"))
            .unwrap_or("query");
        let root_type = if root == "query" { "Query" } else { "Mutation" };
        let field = format!("{}_{}", field_prefix, name);
        let Some(op) = operations.get(key.as_str()) else {
            continue;
        };
        let Some(shape_ref) = get(op, "response").and_then(|r| get_str(r, "shape_ref")) else {
            continue;
        };
        let Some(span) = spans
            .get(root_type)
            .and_then(|s| s.iter().find(|f| f.name == field))
        else {
            continue;
        };
        let Some(sel_text) = span.connect.as_ref().and_then(|c| c.selection.clone()) else {
            continue;
        };
        let shape = crate::json::object(vec![("$ref", Value::from(shape_ref))]);
        let resolved = crate::reconcile::resolve_paths(
            &crate::reconcile::parse_selection(&sel_text),
            &shape,
            shapes,
        );
        if resolved.unknown.is_empty() {
            continue;
        }
        const SHOWN: usize = 8;
        let mut named = resolved
            .unknown
            .iter()
            .take(SHOWN)
            .map(|p| format!("`{}`", p))
            .collect::<Vec<_>>()
            .join(", ");
        if resolved.unknown.len() > SHOWN {
            named.push_str(&format!(" and {} more", resolved.unknown.len() - SHOWN));
        }
        findings.warn(
            "connector-path-undocumented",
            format!(
                "{}: the {}.{} connector selects {} {} that the documented success body ({}) does not have: {}; a scaffolded stub carries every path the selection names, so the e2e case passes while the vendor sends nothing there — select the documented name, or, when the vendor does send it, correct the pinned document and `codify --source` (spec-intake.md § Pinned specs and patches)",
                key,
                root_type,
                field,
                resolved.unknown.len(),
                if resolved.unknown.len() == 1 { "path" } else { "paths" },
                shape_ref.trim_start_matches("#/shapes/"),
                named,
            ),
            Some(schema_file),
            Some(span.line),
        );
    }
}

/// One warning rule, `auth-scheme-drift`: each credential header the schema
/// sends (a header whose value carries `{{AUTH_EXPR}}`) against the
/// inventory's documented schemes (`api.auth[]`). It fires when the header
/// is one no scheme documents while the source documents a header-borne
/// credential, or when every scheme documented for that header gives a
/// literal prefix (`Bearer `, `Basic `) and the schema's prefix is none of
/// them. Every offline layer renders the same header into the request and
/// the stub demands it back, so a wrong scheme passes everything until the
/// vendor answers 401. Silent when the source documents no credential in a
/// header, and for a scheme whose prefix it does not record.
fn lint_auth_scheme(
    sdl: &str,
    schema_file: &str,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let Some(inventory) = inventory else {
        return;
    };
    let schemes: Vec<&Value> = get(inventory, "api")
        .and_then(|a| get_arr(a, "auth"))
        .into_iter()
        .flatten()
        .collect();
    let header_of = |s: &Value| get_str(s, "header").map(str::to_lowercase);
    let documented_headers: Vec<String> = {
        let mut h: Vec<String> = schemes.iter().filter_map(|s| header_of(s)).collect();
        h.sort();
        h.dedup();
        h
    };
    if documented_headers.is_empty() {
        return;
    }
    let mut seen: HashSet<(String, String)> = HashSet::new();
    for (header, value) in credential_header_values(sdl) {
        let prefix = value.split("{{").next().unwrap_or("").to_string();
        if !seen.insert((header.clone(), prefix.clone())) {
            continue;
        }
        let line = sdl
            .find(&format!("value: \"{}", value))
            .map(|at| line_of(sdl, at));
        let for_header: Vec<&&Value> = schemes
            .iter()
            .filter(|s| header_of(s).as_deref() == Some(header.as_str()))
            .collect();
        if for_header.is_empty() {
            findings.warn(
                "auth-scheme-drift",
                format!(
                    "the schema sends its credential in the `{}` header, which no scheme in the inventory's api.auth documents (documented credential headers: {}); every offline layer sends and demands the same header, so only the vendor can refuse it — send it where the source says (spec-intake.md § What the reader records about authentication)",
                    header,
                    documented_headers.join(", ")
                ),
                Some(schema_file),
                line,
            );
            continue;
        }
        let prefixes: Vec<String> = for_header
            .iter()
            .filter_map(|s| get_str(s, "prefix"))
            .map(|p| p.trim().to_string())
            .collect();
        if prefixes.len() < for_header.len() || prefixes.iter().any(|p| p.is_empty()) {
            continue;
        }
        if prefixes
            .iter()
            .any(|p| p.eq_ignore_ascii_case(prefix.trim()))
        {
            continue;
        }
        let mut listed = prefixes.clone();
        listed.sort();
        listed.dedup();
        findings.warn(
            "auth-scheme-drift",
            format!(
                "the `{}` header is sent as `{}{{{{AUTH_EXPR}}}}`, but every scheme the inventory documents for it gives another prefix ({}); every offline layer sends and demands the same header, so only the vendor can refuse it — write the documented prefix outside the placeholder (spec-intake.md § What the reader records about authentication)",
                header,
                prefix,
                listed
                    .iter()
                    .map(|p| format!("`{} `", p))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Some(schema_file),
            line,
        );
    }
}

// ── Casing and the wire vocabulary ───────────────────────────────────────────

/// Follow `$ref` chains through the inventory's shapes.
fn deref_shape<'a>(shape: &'a Value, shapes: &'a Object) -> Option<&'a Value> {
    let mut cur = shape;
    for _ in 0..32 {
        match get_str(cur, "$ref") {
            Some(r) => cur = shapes.get(r.trim_start_matches("#/shapes/"))?,
            None => return Some(cur),
        }
    }
    None
}

/// An array shape: `type: array`, a `["array", "null"]` union, or bare
/// `items` — the same reading `reconcile` takes.
fn is_array_shape(s: &Value) -> bool {
    get_str(s, "type") == Some("array")
        || get_arr(s, "type")
            .map(|t| t.iter().any(|x| x.as_str() == Some("array")))
            .unwrap_or(false)
        || (get(s, "items").is_some() && get(s, "properties").is_none())
}

/// The property `name` of an object shape, looking through `allOf` parts.
/// Bounded: `openapi.rs` leaves a residual `allOf` where a composition
/// cycles, and the walk must not follow it forever.
fn property_of<'a>(
    shape: &'a Value,
    shapes: &'a Object,
    name: &str,
    depth: usize,
) -> Option<&'a Value> {
    if depth > 8 {
        return None;
    }
    let s = deref_shape(shape, shapes)?;
    if let Some(p) = get_obj(s, "properties").and_then(|pp| pp.get(name)) {
        return Some(p);
    }
    for part in get_arr(s, "allOf").into_iter().flatten() {
        if let Some(p) = property_of(part, shapes, name, depth + 1) {
            return Some(p);
        }
    }
    None
}

/// Walk `segs` into a shape: through arrays' items and `$ref`s. The result
/// is dereferenced; with `into_items`, an array at the end yields its items'
/// shape (the selection descends per element).
fn shape_at<'a>(
    shape: &'a Value,
    shapes: &'a Object,
    segs: &[String],
    into_items: bool,
) -> Option<&'a Value> {
    let mut cur = deref_shape(shape, shapes)?;
    for seg in segs {
        if is_array_shape(cur) {
            cur = deref_shape(get(cur, "items")?, shapes)?;
        }
        cur = deref_shape(property_of(cur, shapes, seg, 0)?, shapes)?;
    }
    if into_items && is_array_shape(cur) {
        cur = deref_shape(get(cur, "items")?, shapes)?;
    }
    Some(cur)
}

/// The object a spread arm reads (ADR 0058): the `oneOf`/`anyOf` variant of
/// `shape` whose discriminator property's `enum` holds the arm's candidate
/// (`"book"` against `kind: {enum: [book]}`). `None` when no variant says
/// so — a catch-all `@` arm, or a variant with no such `enum` — so the arm's
/// leaves are not compared against another member's property.
fn arm_shape<'a>(
    shape: &'a Value,
    shapes: &'a Object,
    discriminator: Option<&[String]>,
    candidate: Option<&str>,
) -> Option<&'a Value> {
    let (disc, candidate) = (discriminator?, candidate?);
    let [disc] = disc else { return None };
    let mut s = deref_shape(shape, shapes)?;
    if is_array_shape(s) {
        s = deref_shape(get(s, "items")?, shapes)?;
    }
    let variants = get_arr(s, "oneOf").or_else(|| get_arr(s, "anyOf"))?;
    variants.iter().find_map(|v| {
        let v = deref_shape(v, shapes)?;
        let values = get_arr(
            deref_shape(property_of(v, shapes, disc, 0)?, shapes)?,
            "enum",
        )?;
        values
            .iter()
            .any(|x| x.as_str() == Some(candidate))
            .then_some(v)
    })
}

/// The `enum` a spec property declares, when every member is a string — a
/// numeric vocabulary has no GraphQL enum spelling to compare with. An
/// array property's enum lives on its items.
fn spec_enum(v: &Value, shapes: &Object) -> Option<Vec<String>> {
    let mut node = deref_shape(v, shapes)?;
    if is_array_shape(node) {
        node = deref_shape(get(node, "items")?, shapes)?;
    }
    string_enum(get_arr(node, "enum")?)
}

/// A parameter's `enum` (the inventory keeps it on the parameter itself,
/// whatever the parameter's type).
fn param_enum(p: &Value) -> Option<Vec<String>> {
    string_enum(get_arr(p, "enum")?)
}

fn string_enum(values: &[Value]) -> Option<Vec<String>> {
    let mut out = Vec::new();
    for v in values {
        match v {
            Value::String(s) => out.push(s.clone()),
            Value::Null => {}
            _ => return None,
        }
    }
    if out.is_empty() {
        None
    } else {
        Some(out)
    }
}

fn list(values: &[String]) -> String {
    values
        .iter()
        .map(|v| format!("`{}`", v))
        .collect::<Vec<_>>()
        .join(", ")
}

/// The expression a connector block (`queryParams`, `body`) maps `key` to,
/// as written: `$args.color`, `$args.color->match(…)`, a literal. Exactly
/// that entry's expression, however many entries share its line (ADR 0042),
/// so a `->match` on one pair marks that slot as mapped and no other.
pub(crate) fn slot_expression(field_text: &str, block: &str, key: &str) -> Option<String> {
    let text = connector_block(field_text, block)?;
    block_entries(&text)
        .into_iter()
        .find(|e| e.key.as_deref() == Some(key))
        .map(|e| e.expression)
}

fn uses_mapping_language(expression: &str) -> bool {
    expression.contains("->") || expression.contains("$(")
}

/// Extract the doc comment for a specific argument within a root field's text.
/// `field_text` is the full text of the field definition including @connect.
/// Returns the doc comment string if found, or None.
fn arg_doc_comment(sdl: &str, root: &str, field: &str, arg_name: &str) -> Option<String> {
    let field_text = root_field_text(sdl, root, field)?;
    // Find where the argument list starts (after the field name)
    let open_paren = field_text.find('(')?;
    let close_paren = matching_close(field_text.as_bytes(), open_paren)?;
    let args_text = &field_text[open_paren + 1..close_paren];

    // Build a regex to find this specific argument
    let arg_re = Regex::new(&format!(
        r#"(?s)(""".*?"""|"[^"\n]*")?\s*{}\s*:"#,
        regex::escape(arg_name)
    ))
    .unwrap();

    if let Some(m) = arg_re.captures(args_text) {
        if let Some(doc) = m.get(1) {
            let doc_str = doc.as_str();
            // Extract the content from the doc string
            if doc_str.starts_with("\"\"\"") && doc_str.ends_with("\"\"\"") {
                return Some(doc_str[3..doc_str.len() - 3].trim().to_string());
            } else if doc_str.starts_with('"') && doc_str.ends_with('"') {
                return Some(doc_str[1..doc_str.len() - 1].trim().to_string());
            }
        }
    }
    None
}

/// Argument names that carry a credential the service mints itself: the
/// AppWorld spelling (`access_token`), its camelCase form, and the other
/// token and API-key spellings REST APIs use for a credential passed as a
/// parameter. Deliberately not here: `password` (a login input, not a minted
/// credential), and the bare `key`, `secret` or `session`, which name sort
/// keys, key-value keys and resources as often as credentials (ADR 0064).
pub const CREDENTIAL_ARG_NAMES: [&str; 9] = [
    "access_token",
    "accessToken",
    "token",
    "api_key",
    "apiKey",
    "auth_token",
    "authToken",
    "bearer_token",
    "session_token",
];

/// The named type a root field returns, list and non-null wrappers removed:
/// `[Widget!]!` → `Widget`. Read from the field's blanked text, past its
/// argument list when it has one.
fn root_field_return_type(sdl: &str, root: &str, field: &str) -> Option<String> {
    let code = crate::graphql::blank(&root_field_text(sdl, root, field)?);
    let after_name = code.find(field)? + field.len();
    let rest = code[after_name..].trim_start();
    let offset = code.len() - rest.len();
    let after_args = if rest.starts_with('(') {
        matching_close(code.as_bytes(), offset)? + 1
    } else {
        offset
    };
    let rest = code[after_args..].trim_start().strip_prefix(':')?;
    let name: String = rest
        .trim_start_matches(|c: char| c == '[' || c.is_whitespace())
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || *c == '_')
        .collect();
    (!name.is_empty()).then_some(name)
}

/// A root field's own doc comment and the line the field starts on.
fn root_field_doc(sdl: &str, root: &str, field: &str) -> (Option<String>, Option<usize>) {
    let body = match crate::graphql::type_body(sdl, root) {
        Some(b) => b,
        None => return (None, None),
    };
    let code = crate::graphql::blank(&body.body);
    let start_re = Regex::new(&format!(r"(?m)^[ \t]*{}\s*[(:]", regex::escape(field))).unwrap();
    match start_re.find(&code) {
        Some(m) => {
            let leading = m.as_str().len() - m.as_str().trim_start().len();
            let at = body.start + m.start() + leading;
            (doc_comment_before(sdl, at), Some(line_of(sdl, at)))
        }
        None => (None, None),
    }
}

/// The part of a root field's doc comment before its Returns line (ADR
/// 0032): `Returns: …` or `Returns a list of items with: …`, and the
/// `Each item in … has:` line after it. The Returns line of a login root
/// names the credential as a field of the result, which says nothing about
/// where it goes next, so it cannot satisfy `credential-source-undocumented`.
fn doc_before_returns_line(doc: &str) -> &str {
    let returns = Regex::new(r"Returns(?::| a list of items with:)").unwrap();
    match returns.find_iter(doc).last() {
        Some(m) => &doc[..m.start()],
        None => doc,
    }
}

/// `credential-source-undocumented` (ADR 0064, schema-authoring.md §
/// Descriptions). A service that mints its own credential through a
/// selected operation states the rule once, on the minting root field's doc
/// comment: a composed supergraph keeps one schema description, so the
/// consumer never sees one written there. Fires when (a) two or more
/// selected root fields declare an argument with the same name from
/// [`CREDENTIAL_ARG_NAMES`], (b) a selected root field's unwrapped return
/// type declares a field of that name (or its camelCase form), and (c) no
/// such minting root names the argument in its doc comment before the
/// Returns line. One finding per credential name, at the first minting root.
fn lint_credential_source(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    findings: &mut Findings,
) {
    let selection = match selection {
        Some(s) => s,
        None => return,
    };
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let mut selected: Vec<(&str, String)> = Vec::new();
    for (_, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let graphql = get(entry, "graphql");
        let name = match graphql.and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let root_type = match graphql.and_then(|g| get_str(g, "root")) {
            Some("mutation") => "Mutation",
            _ => "Query",
        };
        let field = if field_prefix.is_empty() {
            name.to_string()
        } else {
            format!("{}_{}", field_prefix, name)
        };
        if root_field_text(sdl, root_type, &field).is_some() {
            selected.push((root_type, field));
        }
    }
    let args: HashMap<&str, Vec<(String, Vec<Arg>)>> = ["Query", "Mutation"]
        .into_iter()
        .map(|r| (r, root_field_args(sdl, r)))
        .collect();
    for credential in CREDENTIAL_ARG_NAMES {
        let takers: Vec<String> = selected
            .iter()
            .filter(|(root, field)| {
                args[root]
                    .iter()
                    .any(|(f, a)| f == field && a.iter().any(|arg| arg.name == credential))
            })
            .map(|(root, field)| format!("{}.{}", root, field))
            .collect();
        if takers.len() < 2 {
            continue;
        }
        let camel_form = camel(credential);
        let minters: Vec<&(&str, String)> = selected
            .iter()
            .filter(|(root, field)| {
                root_field_return_type(sdl, root, field)
                    .and_then(|t| crate::graphql::type_body(sdl, &t))
                    .map(|b| {
                        crate::graphql::field_names(&b.body)
                            .iter()
                            .any(|n| n == credential || *n == camel_form)
                    })
                    .unwrap_or(false)
            })
            .collect();
        if minters.is_empty() {
            continue;
        }
        let documented = minters.iter().any(|(root, field)| {
            root_field_doc(sdl, root, field)
                .0
                .map(|d| doc_before_returns_line(&d).contains(credential))
                .unwrap_or(false)
        });
        if documented {
            continue;
        }
        let (root, field) = minters[0];
        findings.warn(
            "credential-source-undocumented",
            format!(
                "{}.{} mints `{}` (its return type declares it) and {} selected root fields take it as an argument ({}), but its doc comment does not name `{}` before the Returns line: state there that every secured operation takes `{}` from this operation's result and, when the inventory's account-creation shape shows it, which account property is the username (schema-authoring.md § Descriptions; a schema description or a # comment never reaches a consumer)",
                root,
                field,
                credential,
                takers.len(),
                takers.join(", "),
                credential,
                credential
            ),
            Some(schema_file),
            root_field_doc(sdl, root, field).1,
        );
    }
}

/// Five rules about pagination and copy/update semantics in the schema:
///
/// - `pagination-bounds-undocumented`: inventory declares default/maximum for
///   a pagination size param, but the schema arg's doc comment doesn't mention
///   the number. A bound outside `Int` may be stated as "no practical
///   maximum" / "no practical default" instead (ADR 0065). Generation defect.
/// - `pagination-bounds-unknown`: inventory has NEITHER default NOR maximum,
///   and the arg doc comment doesn't state the gap. Source-contract gap.
/// - `page-limit-not-int`: inventory says the size param is integer/number,
///   but the schema arg type is not Int. Generation defect.
/// - `list-completion-missing`: paginated operation's root-field doc comment
///   lacks keywords about pagination. Generation defect.
/// - `copy-state-undocumented`: mutation whose name contains copy/clone/duplicate
///   lacks preservation keywords in its description. Advisory heuristic.
fn lint_pagination(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };

    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let operations: HashMap<&str, &Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .filter_map(|o| get_str(o, "key").map(|k| (k, o)))
        .collect();

    // Keywords for list-completion-missing (case-insensitive)
    let completion_keywords = [
        "page",
        "iterate",
        "collection",
        "total",
        "cursor",
        "complete",
    ];

    // Keywords for copy-state-undocumented (case-insensitive)
    let copy_keywords = ["carry over", "preserve", "omitted", "default"];
    let copy_name_patterns = ["copy", "clone", "duplicate"];

    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let name = match get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let root = get(entry, "graphql")
            .and_then(|g| get_str(g, "root"))
            .unwrap_or("query");
        let root_type = if root == "query" { "Query" } else { "Mutation" };
        let field = format!("{}_{}", field_prefix, name);
        // Validate field exists in schema; skip if not found
        if root_field_text(sdl, root_type, &field).is_none() {
            continue;
        }
        let op = match operations.get(key.as_str()) {
            Some(o) => *o,
            None => continue,
        };

        // Get the operation description (doc comment before the root field)
        let op_doc = {
            if let Some(body) = crate::graphql::type_body(sdl, root_type) {
                let code = crate::graphql::blank(&body.body);
                let start_re =
                    Regex::new(&format!(r"(?m)^[ \t]*{}\s*[(:]", regex::escape(&field))).unwrap();
                if let Some(m) = start_re.find(&code) {
                    doc_comment_before(sdl, body.start + m.start())
                } else {
                    None
                }
            } else {
                None
            }
        };

        // A root field with no doc comment at all, and vendor text to give
        // it, is `undocumented-root-field`'s finding (ADR 0062); these two
        // rules read the comment's content and would only repeat it.
        let described_elsewhere =
            op_doc.is_none() && vendor_root_text(get(entry, "graphql"), op).is_some();

        // Check pagination rules
        let pagination = get(op, "pagination");
        let pagination_style = pagination.and_then(|p| get_str(p, "style"));
        let has_pagination =
            pagination_style.is_some() && !matches!(pagination_style, Some("none" | "unknown"));

        if has_pagination {
            // Rule: list-completion-missing
            let op_doc_lower = op_doc.as_deref().unwrap_or("").to_lowercase();
            let has_completion_keyword = completion_keywords
                .iter()
                .any(|kw| op_doc_lower.contains(kw));

            if !has_completion_keyword && !described_elsewhere {
                findings.warn(
                    "list-completion-missing",
                    format!(
                        "{}: paginated operation's description must state the result is one page and give the stop condition (schema-authoring.md § Paginated list operation descriptions)",
                        key
                    ),
                    Some(schema_file),
                    None,
                );
            }

            // Check size parameter bounds
            if let Some(size_param_name) = pagination.and_then(|p| get_str(p, "size_param")) {
                // Find the inventory parameter
                let inv_param = get_arr(op, "parameters")
                    .into_iter()
                    .flatten()
                    .find(|p| get_str(p, "name") == Some(size_param_name));

                if let Some(inv_param) = inv_param {
                    let inv_type = get_str(inv_param, "type");
                    let inv_default = get(inv_param, "default");
                    let inv_maximum = get(inv_param, "maximum");

                    let has_default = inv_default.is_some();
                    let has_maximum = inv_maximum.is_some();

                    // The schema argument that feeds the size parameter: the
                    // one the connector's `queryParams` maps to that wire key
                    // (`page_size: $args.pageSize`), else the same name.
                    let size_arg = root_field_text(sdl, root_type, &field)
                        .map(|t| wiring(&t))
                        .and_then(|w| {
                            w.query_keys.into_iter().find(|(_, k)| {
                                k == size_param_name || k.trim_end_matches("[]") == size_param_name
                            })
                        })
                        .map(|(arg, _)| arg)
                        .unwrap_or_else(|| size_param_name.to_string());

                    // Find the schema argument and its doc comment
                    let arg_doc = arg_doc_comment(sdl, root_type, &field, &size_arg);
                    let arg_doc_str = arg_doc.as_deref().unwrap_or("");

                    // Find schema arg type
                    let schema_arg_type = {
                        let query_args = root_field_args(sdl, root_type);
                        query_args
                            .iter()
                            .find(|(f, _)| *f == field)
                            .and_then(|(_, args)| {
                                args.iter()
                                    .find(|a| a.name == size_arg)
                                    .map(|a| a.type_.clone())
                            })
                    };

                    // Rule: page-limit-not-int
                    if matches!(inv_type, Some("integer") | Some("number")) {
                        if let Some(ref schema_type) = schema_arg_type {
                            let base_type = crate::sdl_index::base_type(schema_type);
                            if base_type != "Int" {
                                findings.warn(
                                    "page-limit-not-int",
                                    format!(
                                        "{}: `{}` is {} in the inventory but {} in the schema; use Int (schema-authoring.md)",
                                        key, size_param_name, inv_type.unwrap_or("unknown"), schema_type
                                    ),
                                    Some(schema_file),
                                    None,
                                );
                            }
                        }
                    }

                    if has_default || has_maximum {
                        // Rule: pagination-bounds-undocumented
                        let mut missing_bounds = Vec::new();

                        // A bound outside `Int` may be stated as its gap phrase
                        // instead of the number (ADR 0065).
                        let arg_doc_lower = arg_doc_str.to_lowercase();
                        for (name, val, phrase) in [
                            ("default", inv_default, NO_PRACTICAL_DEFAULT),
                            ("maximum", inv_maximum, NO_PRACTICAL_MAXIMUM),
                        ] {
                            let Some(val) = val else { continue };
                            if doc_states_bound(arg_doc_str, val) {
                                continue;
                            }
                            if !outside_int(val) {
                                missing_bounds.push(format!("{}: {}", name, value_str(val)));
                            } else if !arg_doc_lower.contains(phrase) {
                                missing_bounds.push(format!(
                                    "{}: {} (outside Int, so \"{}\" also satisfies)",
                                    name,
                                    value_str(val),
                                    phrase
                                ));
                            }
                        }

                        if !missing_bounds.is_empty() {
                            findings.warn(
                                "pagination-bounds-undocumented",
                                format!(
                                    "{}: `{}` doc comment must state {} (schema-authoring.md § Pagination)",
                                    key, size_param_name, missing_bounds.join(" and ")
                                ),
                                Some(schema_file),
                                None,
                            );
                        }
                    } else {
                        // Rule: pagination-bounds-unknown
                        // Check if the doc comment mentions the gap
                        let unknown_keywords =
                            ["no documented", "unknown", "not specified", "no bound"];
                        let mentions_gap = unknown_keywords
                            .iter()
                            .any(|kw| arg_doc_str.to_lowercase().contains(kw));

                        if !mentions_gap {
                            findings.warn(
                                "pagination-bounds-unknown",
                                format!(
                                    "{}: `{}` has no documented default or maximum in the source; state 'no documented maximum' in the argument doc comment (schema-authoring.md)",
                                    key, size_param_name
                                ),
                                Some(schema_file),
                                None,
                            );
                        }
                    }
                }
            }
        }

        // Rule: copy-state-undocumented (mutations only)
        if root == "mutation" {
            let name_lower = name.to_lowercase();
            let key_lower = key.to_lowercase();
            let is_copy_operation = copy_name_patterns
                .iter()
                .any(|p| name_lower.contains(p) || key_lower.contains(p));

            if is_copy_operation {
                let op_doc_lower = op_doc.as_deref().unwrap_or("").to_lowercase();
                let has_preservation_keyword =
                    copy_keywords.iter().any(|kw| op_doc_lower.contains(kw));

                if !has_preservation_keyword && !described_elsewhere {
                    findings.warn(
                        "copy-state-undocumented",
                        format!(
                            "{}: mutation name suggests a copy/clone operation; description should state which fields carry over from the source (heuristic check; schema-authoring.md § Copy and update workflows)",
                            key
                        ),
                        Some(schema_file),
                        None,
                    );
                }
            }
        }
    }
}

/// One name in a Returns line: its byte offset in the SDL, and the names in
/// its braces when it carries any.
struct ReturnsEntry {
    name: String,
    at: usize,
    braces: Option<Vec<ReturnsEntry>>,
}

/// The deepest Returns-line level whose entries are read one by one. The
/// rule owes braces two levels down, so a third-level name is always a leaf
/// and its braces are reported without being read.
const RETURNS_MAX_DEPTH: usize = 3;

/// Read a Returns-line entry list — `id, owner { id, login (+16 more) },
/// name (+3 more).` — from `text`, whose first byte sits at `base` in the
/// SDL. Stops at `}` (the caller's closing brace), at the sentence's `.` or
/// at the first thing that is not part of the list, and keeps what it read:
/// the line is prose, so what follows it is not judged. `depth` is the
/// list's level (1 for the line itself); a brace group opened at
/// [`RETURNS_MAX_DEPTH`] is skipped to its close and recorded as empty
/// braces rather than read, so a nest of any depth costs no stack.
fn returns_entries(text: &[u8], i: &mut usize, base: usize, depth: usize) -> Vec<ReturnsEntry> {
    let skip_ws = |i: &mut usize| {
        while *i < text.len() && text[*i].is_ascii_whitespace() {
            *i += 1;
        }
    };
    let mut out = Vec::new();
    loop {
        skip_ws(i);
        if text[*i..].starts_with(b"(+") {
            match text[*i..].iter().position(|&c| c == b')') {
                Some(p) => *i += p + 1,
                None => return out,
            }
            skip_ws(i);
        }
        let start = *i;
        while *i < text.len() && (text[*i].is_ascii_alphanumeric() || text[*i] == b'_') {
            *i += 1;
        }
        if *i == start {
            return out;
        }
        let name = String::from_utf8_lossy(&text[start..*i]).to_string();
        skip_ws(i);
        let braces = if *i < text.len() && text[*i] == b'{' && depth >= RETURNS_MAX_DEPTH {
            let mut open = 0usize;
            while *i < text.len() {
                match text[*i] {
                    b'{' => open += 1,
                    b'}' => open -= 1,
                    _ => {}
                }
                *i += 1;
                if open == 0 {
                    break;
                }
            }
            Some(Vec::new())
        } else if *i < text.len() && text[*i] == b'{' {
            *i += 1;
            let inner = returns_entries(text, i, base, depth + 1);
            skip_ws(i);
            if *i < text.len() && text[*i] == b'}' {
                *i += 1;
            }
            Some(inner)
        } else {
            None
        };
        out.push(ReturnsEntry {
            name,
            at: base + start,
            braces,
        });
        skip_ws(i);
        if text[*i..].starts_with(b"(+") {
            continue;
        }
        if *i < text.len() && text[*i] == b',' {
            *i += 1;
            continue;
        }
        return out;
    }
}

/// The ordered `(field, declared type)` pairs of an object or interface
/// type, or None when `name` is not one.
fn object_fields(
    sdl: &str,
    kinds: &HashMap<String, String>,
    index: &mut crate::sdl_index::SdlIndex,
    name: &str,
) -> Option<Vec<(String, String)>> {
    if !matches!(
        kinds.get(name).map(String::as_str),
        Some("type" | "interface")
    ) {
        return None;
    }
    let body = crate::graphql::type_body(sdl, name)?;
    Some(
        crate::graphql::field_names(&body.body)
            .into_iter()
            .map(|f| {
                let t = index.field_type(name, &f).unwrap_or_default();
                (f, crate::sdl_index::base_type(&t))
            })
            .collect(),
    )
}

/// `returns-line-nesting` (warn, ADR 0067): a selected root field's Returns
/// line — `Returns:`, `Returns a list of items with:`, and the envelope's
/// ``Each item in `x` has:`` — read against the SDL return type. It reports
/// (a) a name the type it is listed under does not declare, (b) an
/// object-typed first-level entry left bare (the envelope's payload field in
/// the first line excepted: the item line is its expansion), (c) an
/// object-typed second-level entry whose type has at most 6 fields, every
/// one a leaf, left bare, and (d) a second-level entry braced although its
/// type has an object-typed field or more than 6 fields; braces on a name
/// that is not an object are reported too. Order, the caps and `(+N more)`
/// are not checked; a doc comment without a Returns line is not this rule's
/// business, and a line the reader cannot follow is judged only as far as
/// it reads.
fn lint_returns_line(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    findings: &mut Findings,
) {
    const NESTED_CAP: usize = 6;
    let Some(selection) = selection else {
        return;
    };
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let kinds: HashMap<String, String> = type_declarations(sdl)
        .into_iter()
        .map(|d| (d.name, d.kind))
        .collect();
    let mut index = crate::sdl_index::SdlIndex::new(sdl);
    let returns_re = re(
        &RETURNS_LINE_RE,
        r"\bReturns(?::| a list of items with:)|Each item in `([A-Za-z_][A-Za-z0-9_]*)` has:",
    );
    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let Some(name) = get(entry, "graphql").and_then(|g| get_str(g, "name")) else {
            continue;
        };
        let root_type = match get(entry, "graphql").and_then(|g| get_str(g, "root")) {
            Some("mutation") => "Mutation",
            _ => "Query",
        };
        let field = format!("{}_{}", field_prefix, name);
        let Some(body) = crate::graphql::type_body(sdl, root_type) else {
            continue;
        };
        let code = crate::graphql::blank(&body.body);
        let start_re =
            Regex::new(&format!(r"(?m)^[ \t]*{}\s*[(:]", regex::escape(&field))).unwrap();
        let Some(m) = start_re.find(&code) else {
            continue;
        };
        let field_at = body.start + m.start() + (m.as_str().len() - m.as_str().trim_start().len());
        let Some(doc) = doc_comment_before(sdl, field_at) else {
            continue;
        };
        let Some(doc_at) = sdl[..field_at].rfind(doc.as_str()) else {
            continue;
        };
        let Some(returns) = index
            .field_type(root_type, &field)
            .map(|t| crate::sdl_index::base_type(&t))
        else {
            continue;
        };
        // (list type, its entries, the payload field the first line may leave bare)
        let marks: Vec<_> = returns_re.captures_iter(&doc).collect();
        let payload = marks
            .iter()
            .find_map(|c| c.get(1).map(|g| g.as_str().to_string()));
        let mut lists: Vec<(Option<String>, Vec<ReturnsEntry>, bool)> = Vec::new();
        for (n, c) in marks.iter().enumerate() {
            let whole = c.get(0).unwrap();
            let end = marks
                .get(n + 1)
                .map(|next| next.get(0).unwrap().start())
                .unwrap_or(doc.len());
            let text = &doc.as_bytes()[whole.end()..end];
            let mut i = 0;
            let entries = returns_entries(text, &mut i, doc_at + whole.end(), 1);
            match c.get(1) {
                None => lists.push((Some(returns.clone()), entries, true)),
                Some(item) => {
                    let item_type = index
                        .field_type(&returns, item.as_str())
                        .map(|t| crate::sdl_index::base_type(&t));
                    if item_type.is_none() {
                        findings.warn(
                            "returns-line-nesting",
                            format!(
                                "{}: `{}`'s Returns line names `{}`, which `{}` does not declare (schema-authoring.md § Returned field names)",
                                key, field, item.as_str(), returns
                            ),
                            Some(schema_file),
                            Some(line_of(sdl, doc_at + item.start())),
                        );
                    }
                    lists.push((item_type, entries, false));
                }
            }
        }
        for (owner, entries, first_line) in lists {
            let Some(owner) = owner else {
                continue;
            };
            check_returns_entries(
                sdl,
                &kinds,
                &mut index,
                key,
                &field,
                schema_file,
                &owner,
                &entries,
                1,
                if first_line { payload.as_deref() } else { None },
                findings,
            );
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn check_returns_entries(
        sdl: &str,
        kinds: &HashMap<String, String>,
        index: &mut crate::sdl_index::SdlIndex,
        key: &str,
        field: &str,
        schema_file: &str,
        owner: &str,
        entries: &[ReturnsEntry],
        level: usize,
        payload: Option<&str>,
        findings: &mut Findings,
    ) {
        let Some(fields) = object_fields(sdl, kinds, index, owner) else {
            return;
        };
        let rendered = |fields: &[(String, String)]| {
            let names: Vec<&str> = fields
                .iter()
                .take(NESTED_CAP)
                .map(|f| f.0.as_str())
                .collect();
            let more = fields.len().saturating_sub(NESTED_CAP);
            if more > 0 {
                format!("{} (+{} more)", names.join(", "), more)
            } else {
                names.join(", ")
            }
        };
        for e in entries {
            let mut warn = |message: String| {
                findings.warn(
                    "returns-line-nesting",
                    format!(
                        "{}: `{}`'s Returns line: {} (schema-authoring.md § Returned field names)",
                        key, field, message
                    ),
                    Some(schema_file),
                    Some(line_of(sdl, e.at)),
                );
            };
            let Some((_, ty)) = fields.iter().find(|(f, _)| *f == e.name) else {
                warn(format!("`{}` is not a field of `{}`", e.name, owner));
                continue;
            };
            let nested = object_fields(sdl, kinds, index, ty);
            let Some(nested) = nested else {
                if e.braces.is_some() {
                    warn(format!(
                        "`{}` is a `{}`, not an object, and takes no braces",
                        e.name, ty
                    ));
                }
                continue;
            };
            let leaf_only = nested.iter().all(|(_, t)| {
                !matches!(
                    kinds.get(t).map(String::as_str),
                    Some("type" | "interface" | "union")
                )
            });
            let owed = match level {
                1 => payload != Some(e.name.as_str()),
                2 => leaf_only && nested.len() <= NESTED_CAP,
                _ => false,
            };
            match (&e.braces, owed) {
                (None, true) => warn(format!(
                    "`{}` is a `{}` and reads as a leaf; brace it: `{} {{ {} }}`",
                    e.name,
                    ty,
                    e.name,
                    rendered(&nested)
                )),
                (Some(_), false) if level >= 2 => {
                    let why = if let Some((f, t)) = nested.iter().find(|(_, t)| {
                        matches!(
                            kinds.get(t).map(String::as_str),
                            Some("type" | "interface" | "union")
                        )
                    }) {
                        format!("`{}` has the object-typed field `{}: {}`", ty, f, t)
                    } else {
                        format!(
                            "`{}` has {} fields, more than {}",
                            ty,
                            nested.len(),
                            NESTED_CAP
                        )
                    };
                    warn(format!(
                        "`{}` is braced at the second level, but {}; name it bare",
                        e.name, why
                    ))
                }
                (Some(inner), _) => check_returns_entries(
                    sdl,
                    kinds,
                    index,
                    key,
                    field,
                    schema_file,
                    ty,
                    inner,
                    level + 1,
                    None,
                    findings,
                ),
                (None, false) => {}
            }
        }
    }
}

/// `sparse-fieldsets` (error): a selected GET whose operation takes a string
/// query parameter naming its fields (`workspace.yaml`
/// `sparse_fieldsets.param`, default `fields`) declares that argument as
/// `String`, with the default its own connector implies (`crate::sparse`) or
/// the literal a decision naming the operation records in backticks
/// (`crate::sparse::decision_for`; when the list is derivable, only a
/// literal that keeps every derived part), and forwards it as
/// `<param>: $args.<param>`. It checks the declared default and the
/// forwarding, not what a caller passes at run time (ADR 0045).
/// `sparse_fieldsets.enabled: false` turns the rule off.
fn lint_sparse_fieldsets(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
    union: Option<&Value>,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) if crate::sparse::enabled(workspace) => (s, i),
        _ => return,
    };
    let param = crate::sparse::param_name(workspace);
    let prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let shapes = get_obj(inventory, "shapes").cloned().unwrap_or_default();
    // A resolved decision or a current finding (ADR 0113 §2); a log that
    // does not load is an empty one here.
    let empty = crate::decisions::empty();
    let decisions = union.unwrap_or(&empty);
    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let Some(op) = get_arr(inventory, "operations")
            .into_iter()
            .flatten()
            .find(|o| get_str(o, "key") == Some(key))
        else {
            continue;
        };
        let Some(p) = crate::sparse::query_param(op, &param) else {
            continue;
        };
        let ty = crate::sparse::param_type(p);
        if ty != "string" {
            findings.add(
                "info",
                "sparse-fieldsets",
                format!(
                    "{}: the `{}` query parameter is {}, not a string; the sparse-fieldsets rule does not apply",
                    key, param, ty
                ),
                Some(schema_file),
                None,
            );
            continue;
        }
        let Some(name) = get(entry, "graphql").and_then(|g| get_str(g, "name")) else {
            continue;
        };
        let root = if get(entry, "graphql").and_then(|g| get_str(g, "root")) == Some("mutation") {
            "Mutation"
        } else {
            "Query"
        };
        let field = format!("{}_{}", prefix, name);
        let Some(text) = root_field_text(sdl, root, &field) else {
            continue;
        };
        let selection_text = crate::reconcile::field_spans(sdl, root)
            .into_iter()
            .find(|f| f.name == field)
            .and_then(|f| f.connect)
            .and_then(|c| c.selection)
            .unwrap_or_default();
        let derived = crate::sparse::derive(op, &shapes, &selection_text);
        let want = match &derived {
            crate::sparse::Derived::Fields(d) => format!("\"{}\"", d),
            crate::sparse::Derived::NotDerivable(_) => "\"<the recorded literal>\"".to_string(),
        };
        let at = format!("{}: {}.{}", key, root, field);
        let forwarded = slot_expression(&text, "queryParams", &param);
        if forwarded.as_deref() != Some(&format!("$args.{}", param)) {
            findings.error(
                "sparse-fieldsets",
                format!(
                    "{} does not forward the argument: queryParams must carry `{}: $args.{}` ({})",
                    at,
                    param,
                    param,
                    match &forwarded {
                        Some(e) => format!("it sends `{}`", e),
                        None => format!("it sends no `{}`", param),
                    }
                ),
                Some(schema_file),
                None,
            );
        }
        let declared = match crate::sparse::declared_arg(&text, &param) {
            None => {
                findings.error(
                    "sparse-fieldsets",
                    format!(
                        "{} declares no `{}` argument; the operation takes a `{}` query parameter, so declare `{}: String = {}`",
                        at, param, param, param, want
                    ),
                    Some(schema_file),
                    None,
                );
                continue;
            }
            Some(d) => d,
        };
        if declared.type_ != "String" {
            findings.error(
                "sparse-fieldsets",
                format!(
                    "{}: `{}` is {}; declare it `String`",
                    at, param, declared.type_
                ),
                Some(schema_file),
                None,
            );
            continue;
        }
        let Some(default) = declared.default else {
            findings.error(
                "sparse-fieldsets",
                format!("{}: `{}` has no default; declare `= {}`", at, param, want),
                Some(schema_file),
                None,
            );
            continue;
        };
        if matches!(&derived, crate::sparse::Derived::Fields(d) if *d == default) {
            continue;
        }
        let recorded = crate::sparse::decision_for(decisions, key, &default);
        // A decision records what the derivation cannot express; when the
        // list is derivable it may add to it, never drop what the
        // connector maps (ADR 0045).
        if let (Some(id), crate::sparse::Derived::Fields(d)) = (recorded, &derived) {
            let dropped = crate::sparse::dropped_parts(&default, d);
            if !dropped.is_empty() {
                findings.error(
                    "sparse-fieldsets",
                    format!(
                        "{}: the `{}` default \"{}\" that {} records drops `{}`, which the connector maps (derived \"{}\"); a decision may add to the derived list, never remove from it",
                        at, param, default, id, dropped.join("`, `"), d
                    ),
                    Some(schema_file),
                    None,
                );
                continue;
            }
        }
        if recorded.is_some() {
            continue;
        }
        let message = match &derived {
            crate::sparse::Derived::Fields(d) => format!(
                "{}: the `{}` default \"{}\" is not the list its connector maps, \"{}\", and no resolved decision naming {} records it in backticks",
                at, param, default, d, key
            ),
            crate::sparse::Derived::NotDerivable(why) => format!(
                "{}: the `{}` default cannot be derived ({}); record `{}`, in backticks, in a resolved decision that names {}",
                at, param, why, default, key
            ),
        };
        findings.error("sparse-fieldsets", message, Some(schema_file), None);
    }
}

// ── Numeric range ────────────────────────────────────────────────────────────

/// GraphQL `Int` is a signed 32-bit integer; a larger value fails coercion
/// and the router hands back `null` with an error, not the number.
const GQL_INT_MIN: f64 = -2_147_483_648.0;
const GQL_INT_MAX: f64 = 2_147_483_647.0;

/// The prefix of schema-authoring.md § Argument constraints' spelling for a
/// default outside `Int`, which clears `int-overflow` on an argument's
/// default and nothing else (ADR 0065).
const UNBOUNDED_DEFAULT: &str = "default: unbounded";

/// The phrases § Pagination accepts in place of a page-size bound outside
/// `Int`, which no `Int` argument reaches (ADR 0065).
const NO_PRACTICAL_MAXIMUM: &str = "no practical maximum";
const NO_PRACTICAL_DEFAULT: &str = "no practical default";

/// True when `v` is a number outside GraphQL `Int`.
fn outside_int(v: &Value) -> bool {
    v.as_f64()
        .map(|n| !(GQL_INT_MIN..=GQL_INT_MAX).contains(&n))
        .unwrap_or(false)
}

/// Why an `Int`-typed slot cannot hold what the source declares, or `None`.
/// A declared `minimum`/`maximum` is the source's own word and beats the
/// format hint in both directions: a bound outside `Int` is a finding
/// whatever the format says, and a pair inside `Int` clears an `int64` one
/// (so `page-limit-not-int`'s bounded size parameter stays `Int`). An
/// exclusive bound is read as the bound it is (ADR 0065): OpenAPI 3.1's
/// numeric `exclusiveMaximum: N` admits up to N - 1, and 3.0's boolean
/// `exclusiveMaximum: true` makes `maximum` exclusive. A `default` outside
/// `Int` is a finding too.
fn int_overflow_gap(shape: &Value, shapes: &Object) -> Option<String> {
    int_overflow_gap_at(shape, shapes, 0, true)
}

/// `int_overflow_gap` without the `default` check, for an argument: whether
/// its default clears depends on the doc comment (`int_overflow_default`).
fn int_overflow_bound_gap(shape: &Value, shapes: &Object) -> Option<String> {
    int_overflow_gap_at(shape, shapes, 0, false)
}

/// The numeric shape `int_overflow_gap` judges: the dereferenced shape, or
/// its `items` for an array. `None` when it is not numeric.
fn int_overflow_numeric<'a>(
    shape: &'a Value,
    shapes: &'a Object,
    depth: usize,
) -> Option<&'a Value> {
    if depth > 8 {
        return None;
    }
    let s = deref_shape(shape, shapes)?;
    // A `[Int]` leaf over an array of `int64` has the same defect once per
    // element, and the element shape is where the source declares it.
    if is_array_shape(s) {
        return int_overflow_numeric(get(s, "items")?, shapes, depth + 1);
    }
    let numeric = matches!(get_str(s, "type"), Some("integer") | Some("number"))
        || get_arr(s, "type")
            .map(|t| {
                t.iter()
                    .any(|x| matches!(x.as_str(), Some("integer") | Some("number")))
            })
            .unwrap_or(false);
    numeric.then_some(s)
}

/// A numeric `default` outside `Int`, as the number the source wrote.
fn int_overflow_default_value(shape: &Value, shapes: &Object) -> Option<String> {
    let s = int_overflow_numeric(shape, shapes, 0)?;
    let d = get(s, "default")?;
    outside_int(d).then(|| value_str(d))
}

/// A numeric `default` outside `Int`, as the reason `int-overflow` gives.
fn int_overflow_default(shape: &Value, shapes: &Object) -> Option<String> {
    int_overflow_default_value(shape, shapes).map(|n| format!("declares default {}", n))
}

/// A clause in an argument's doc comment that states the source's
/// out-of-range default in a wording written before ADR 0065, which
/// `int-overflow` quotes as the text to replace or delete (ADR 0093, ADR
/// 0104).
struct LegacyDefaultClause {
    /// The clause exactly as the doc comment spells it.
    text: String,
    /// Why the clause harms a caller, as the finding says it.
    harm: &'static str,
}

/// Every legacy default clause in `doc`, in the order it appears:
/// - `default N` or `default: N`, where N is the source's number: the
///   spelling § Argument constraints prescribed before ADR 0065. The number
///   must end there: a following digit, exponent or decimal part makes it a
///   longer number, while a period that ends the sentence does not (ADR
///   0093).
/// - `default no upper bound` or `default: no upper bound`, the wording
///   AppWorld workspaces built at crate 0.5.1 carry (ADR 0104).
///
/// Both are matched case-insensitively and quoted as written.
fn int_overflow_legacy_default_clauses(doc: &str, number: &str) -> Vec<LegacyDefaultClause> {
    let numeric = Regex::new(&format!(
        r"(?i)\bdefault\s*:?\s*{}(?:[^0-9.eE]|\.(?:[^0-9]|$)|$)",
        regex::escape(number)
    ))
    .expect("int-overflow numeric clause pattern");
    let no_upper_bound = Regex::new(r"(?i)\bdefault\s*:?\s*no\s+upper\s+bound\b")
        .expect("int-overflow no-upper-bound clause pattern");
    let mut found: Vec<(usize, LegacyDefaultClause)> = Vec::new();
    for m in numeric.find_iter(doc) {
        found.push((
            m.start(),
            LegacyDefaultClause {
                text: m
                    .as_str()
                    .trim_end_matches(|c: char| !c.is_ascii_digit())
                    .to_string(),
                harm: "states a value an Int argument cannot carry, so a caller who copies it gets a coercion error",
            },
        ));
    }
    for m in no_upper_bound.find_iter(doc) {
        found.push((
            m.start(),
            LegacyDefaultClause {
                text: m.as_str().to_string(),
                harm: "does not tell a caller to omit the argument, and an Int cannot carry the source's default",
            },
        ));
    }
    found.sort_by_key(|(at, _)| *at);
    found.into_iter().map(|(_, c)| c).collect()
}

/// Quote each clause in backticks and join them as prose: `a`, `a` and
/// `b`, `a`, `b` and `c`.
fn quoted_list(items: &[&str]) -> String {
    let q: Vec<String> = items.iter().map(|i| format!("`{}`", i)).collect();
    match q.len() {
        0 => String::new(),
        1 => q[0].clone(),
        n => format!("{} and {}", q[..n - 1].join(", "), q[n - 1]),
    }
}

/// The remedy `int-overflow` gives for an argument whose doc comment holds
/// `clauses` (non-empty): replace the first with `spelling` and delete the
/// rest, or, when `default: unbounded` is already there, delete them all.
fn int_overflow_legacy_remedy(
    clauses: &[LegacyDefaultClause],
    unbounded: bool,
    spelling: &str,
    advice: &str,
) -> String {
    let why: Vec<String> = clauses
        .iter()
        .map(|c| format!("`{}` {}", c.text, c.harm))
        .collect();
    let texts: Vec<&str> = clauses.iter().map(|c| c.text.as_str()).collect();
    let action = if unbounded {
        format!(
            "the doc comment already says `{}`, so delete {}",
            UNBOUNDED_DEFAULT,
            quoted_list(&texts)
        )
    } else if texts.len() == 1 {
        format!("replace `{}` with `{}`", texts[0], spelling)
    } else {
        format!(
            "replace `{}` with `{}` and delete {}",
            texts[0],
            spelling,
            quoted_list(&texts[1..])
        )
    };
    format!(
        "the doc comment's {}: {}, or retype the argument: {}",
        why.join("; its "),
        action,
        advice
    )
}

/// One side of a declared range: the tightest bound among `inclusive`
/// (`minimum`/`maximum`, made exclusive by a boolean `exclusive` key) and a
/// numeric `exclusive` key, as (the integer it admits, the reason naming the
/// key that set it). `upper` picks the smaller, a lower bound the larger.
fn int_overflow_side(
    s: &Value,
    inclusive: &str,
    exclusive: &str,
    upper: bool,
) -> Option<(f64, String)> {
    let step = if upper { -1.0 } else { 1.0 };
    let mut sides: Vec<(f64, String)> = Vec::new();
    if let Some(raw) = get(s, inclusive) {
        if let Some(v) = raw.as_f64() {
            if get(s, exclusive) == Some(&Value::Bool(true)) {
                sides.push((
                    v + step,
                    format!(
                        "declares {} {} with {}: true",
                        inclusive,
                        value_str(raw),
                        exclusive
                    ),
                ));
            } else {
                sides.push((v, format!("declares {} {}", inclusive, value_str(raw))));
            }
        }
    }
    if let Some(raw) = get(s, exclusive).filter(|v| v.is_number()) {
        let v = raw.as_f64()?;
        sides.push((
            v + step,
            format!("declares {} {}", exclusive, value_str(raw)),
        ));
    }
    sides.into_iter().reduce(|a, b| {
        if (upper && b.0 < a.0) || (!upper && b.0 > a.0) {
            b
        } else {
            a
        }
    })
}

/// `depth` bounds the walk into `items`, so an `items: {$ref: itself}` cycle
/// cannot recurse forever.
fn int_overflow_gap_at(
    shape: &Value,
    shapes: &Object,
    depth: usize,
    with_default: bool,
) -> Option<String> {
    let s = int_overflow_numeric(shape, shapes, depth)?;
    let lower = int_overflow_side(s, "minimum", "exclusiveMinimum", false);
    let upper = int_overflow_side(s, "maximum", "exclusiveMaximum", true);
    if let Some((v, reason)) = &lower {
        if *v < GQL_INT_MIN {
            return Some(reason.clone());
        }
    }
    if let Some((v, reason)) = &upper {
        if *v > GQL_INT_MAX {
            return Some(reason.clone());
        }
    }
    if with_default {
        if let Some(reason) = int_overflow_default(s, shapes) {
            return Some(reason);
        }
    }
    // `int64`, `uint64` and `uint32` all reach past `Int`'s
    // [-2147483648, 2147483647]: `uint32` is unsigned, so its upper half
    // (2147483648 .. 4294967295) does not fit. `int32` and an absent
    // format do fit.
    let format = get_str(s, "format").unwrap_or("");
    if !matches!(format, "int64" | "uint64" | "uint32") {
        return None;
    }
    // Both sides are declared and neither returned above, so both fit.
    if lower.is_some() && upper.is_some() {
        return None;
    }
    Some(format!("declares `format: {}`", format))
}

/// The line `field` of `type_name` is declared on. Serves an object field
/// and a root field alike.
fn field_line(sdl: &str, type_name: &str, field: &str) -> Option<usize> {
    let body = crate::graphql::type_body(sdl, type_name)?;
    let code = crate::graphql::blank(&body.body);
    let re = Regex::new(&format!(r"(?m)^[ \t]*{}\s*[(:]", regex::escape(field))).unwrap();
    let m = re.find(&code)?;
    Some(crate::graphql::line_of(sdl, body.start + m.start()))
}

/// The doc comment written inline before one argument of a root field, in
/// `field_text` (which starts at the field name). Only whitespace, commas
/// and the string itself may sit between the argument's name and the end of
/// the previous argument; a string that follows `=` is that argument's
/// default value, not this one's description.
fn inline_arg_doc(field_text: &str, arg_name: &str) -> Option<String> {
    let code = crate::graphql::blank(field_text);
    let open = code.find('(')?;
    let close = matching_close(code.as_bytes(), open)?;
    let inner = strip_defaults_and_directives(&code[open + 1..close]);
    let name_re = Regex::new(&format!(r"(?:^|[\s,(])({})\s*:", regex::escape(arg_name))).unwrap();
    let pos = open + 1 + name_re.captures(&inner)?.get(1)?.start();
    let code_bytes = code.as_bytes();
    let previous = (open..pos)
        .rev()
        .find(|&i| !code_bytes[i].is_ascii_whitespace() && code_bytes[i] != b',')?;
    let gap =
        field_text[previous + 1..pos].trim_end_matches(|c: char| c.is_whitespace() || c == ',');
    const FENCE: &str = "\"\"\"";
    let (start, text) = if gap.len() >= 2 * FENCE.len() && gap.ends_with(FENCE) {
        let body_end = gap.len() - FENCE.len();
        let open_fence = gap[..body_end].rfind(FENCE)?;
        (open_fence, &gap[open_fence + FENCE.len()..body_end])
    } else if gap.ends_with('"') {
        let line_start = gap.rfind('\n').map(|p| p + 1).unwrap_or(0);
        // The opening quote is the last one no backslash escapes: `\"alpha\"`
        // inside the string is text (ADR 0083).
        let line = &gap.as_bytes()[line_start..gap.len() - 1];
        let open_quote = line_start
            + (0..line.len()).rev().find(|&i| {
                line[i] == b'"'
                    && line[..i].iter().rev().take_while(|&&b| b == b'\\').count() % 2 == 0
            })?;
        (open_quote, &gap[open_quote + 1..gap.len() - 1])
    } else {
        return None;
    };
    if gap[..start].trim().is_empty() && code_bytes[previous] == b'=' {
        return None; // the string is the previous argument's default value
    }
    Some(text.trim().to_string())
}

/// The source parameter or request-body property an argument reaches (ADR
/// 0095 names it in a decision).
pub struct ArgSource {
    /// For a message: "query parameter `x`", "body key `a.b`".
    pub origin: String,
    /// The name a decision waives it by: `query:x`, `path:x`,
    /// `header:X-Card`, `body:a.b`. Unambiguous within one operation.
    pub key: String,
    /// The parameter's name, or a body property's last key.
    pub name: String,
    /// The inventory parameter, or the body property's shape.
    pub entry: Value,
    /// The body keys beside a body property, the sibling names an omission
    /// sentence may name at that depth (ADR 0083).
    pub peers: Vec<String>,
    /// Whether the source requires it: a parameter's `required`, or the
    /// enclosing object's `required` list for a body key. None when the
    /// inventory does not say.
    pub required: Option<bool>,
}

impl ArgSource {
    fn new(
        kind: &str,
        noun: &str,
        label: &str,
        name: &str,
        entry: Value,
        peers: Vec<String>,
    ) -> ArgSource {
        let required = entry.get("required").and_then(Value::as_bool);
        ArgSource {
            origin: format!("{} `{}`", noun, label),
            key: format!("{}:{}", kind, label),
            name: name.to_string(),
            entry,
            peers,
            required,
        }
    }
}

/// The source parameter (or request-body property) an argument reaches on
/// the wire, as `(where, source name, entry, peers)`: a `queryParams` key, a
/// path segment `{$args.x}` aligned with the inventory path's `{param}`, a
/// header whose value is the argument, or a body key, flat or nested in
/// object literals (ADR 0083). `peers` are the body keys beside it, the
/// sibling names an omission sentence may name at that depth. None when the
/// argument is wired some other way (an input field of an argument, an
/// expression the body walk does not read).
fn argument_source(
    field_text: &str,
    op: &Value,
    shapes: Option<&Object>,
    arg_name: &str,
) -> Option<ArgSource> {
    let params: Vec<&Value> = get_arr(op, "parameters").into_iter().flatten().collect();
    let param = |location: &str, name: &str| {
        params
            .iter()
            .find(|p| get_str(p, "in") == Some(location) && get_str(p, "name") == Some(name))
            .map(|p| (*p).clone())
    };
    let wire = wiring(field_text);
    if let Some((_, key)) = wire.query_keys.iter().find(|(a, _)| a == arg_name) {
        let base = key.trim_end_matches("[]");
        let p = param("query", base).or_else(|| param("query", key))?;
        return Some(ArgSource::new(
            "query",
            "query parameter",
            base,
            base,
            p,
            Vec::new(),
        ));
    }
    let http_path = Regex::new(r#"\b(?:GET|POST|PUT|PATCH|DELETE|HEAD)\s*:\s*"([^"]+)""#)
        .unwrap()
        .captures(field_text)
        .map(|m| m[1].to_string())
        .unwrap_or_default();
    let placeholder = format!("{{$args.{}}}", arg_name);
    if http_path.contains(&placeholder) {
        let connector: Vec<&str> = http_path.split('?').next()?.split('/').collect();
        let source: Vec<&str> = get_str(op, "path")?.split('/').collect();
        let name = if connector.len() == source.len() {
            connector
                .iter()
                .zip(&source)
                .find(|(c, _)| **c == placeholder)
                .and_then(|(_, s)| s.strip_prefix('{')?.strip_suffix('}'))
        } else {
            None
        }
        .unwrap_or(arg_name);
        return param("path", name)
            .map(|p| ArgSource::new("path", "path parameter", name, name, p, Vec::new()));
    }
    if let Some(header) = header_param(field_text, arg_name) {
        return param("header", &header)
            .map(|p| ArgSource::new("header", "header", &header, &header, p, Vec::new()));
    }
    let reference = crate::json::object(vec![(
        "$ref",
        Value::from(get(op, "request_body").and_then(|r| get_str(r, "shape_ref"))?),
    )]);
    let shapes = shapes?;
    let path = match body_mapping(field_text) {
        BodyMapping::Flat(map) => vec![map.into_iter().find(|(a, _)| a == arg_name)?.1],
        BodyMapping::NotFlat => nested_body_path(&connector_block(field_text, "body")?, arg_name)?,
        BodyMapping::None => return None,
    };
    let (last, parents) = path.split_last()?;
    let parent = shape_at(&reference, shapes, parents, false)?;
    let prop = property_of(parent, shapes, last, 0)?;
    // The keys beside it: an omission sentence that names one of them is
    // about the two together (ADR 0066), at any depth of the body.
    let peers = deref_shape(parent, shapes)
        .and_then(|p| get_obj(p, "properties"))
        .map(|props| props.keys().cloned().collect())
        .unwrap_or_default();
    let mut source = ArgSource::new(
        "body",
        "body key",
        &path.join("."),
        last,
        prop.clone(),
        peers,
    );
    // A body property has no `required` of its own: the enclosing object
    // lists the keys it requires.
    source.required = deref_shape(parent, shapes)
        .and_then(|p| get_arr(p, "required"))
        .map(|list| list.iter().any(|k| k.as_str() == Some(last.as_str())));
    Some(source)
}

/// The header an argument fills: `headers: [{ name: "From", value:
/// "{$args.from}" }]` gives `From` for `from` (ADR 0083). Only a value that
/// is the argument alone counts.
fn header_param(field_text: &str, arg_name: &str) -> Option<String> {
    let entry = Regex::new(
        r#"\{\s*name\s*:\s*"([^"]+)"\s*,\s*value\s*:\s*"\{\$args\.([A-Za-z_][A-Za-z0-9_]*)\}"\s*\}"#,
    )
    .unwrap();
    let found = entry
        .captures_iter(field_text)
        .find(|m| &m[2] == arg_name)
        .map(|m| m[1].to_string());
    found
}

/// Where a nested request body sends an argument (ADR 0083): the key path
/// to the first value that is the argument itself, optionally through
/// methods (`urgency: $args.urgency` inside `incident: { … }` gives
/// `incident.urgency`; `priority: $args.priorityId->map(…)->first` gives
/// `priority`). Object literals, `$({ … })` included, are walked with
/// `source-coverage`'s own pair reader. A deeper path (`$args.input.x`) or an
/// argument sub-selection (`$args.x { … }`) sends an input field, not the
/// argument, and is not followed.
fn nested_body_path(body: &str, arg_name: &str) -> Option<Vec<String>> {
    fn walk(expr: &str, prefix: &[String], arg_name: &str) -> Option<Vec<String>> {
        for (key, value) in crate::obligations::parse_body_pairs(expr) {
            let mut path = prefix.to_vec();
            path.push(key);
            let value = value.trim();
            if value.starts_with('{') || value.starts_with("$({") {
                if let Some(found) = walk(value, &path, arg_name) {
                    return Some(found);
                }
                continue;
            }
            let rest = value
                .strip_prefix("$args.")
                .and_then(|r| r.strip_prefix(arg_name));
            if rest.is_some_and(|r| r.trim().is_empty() || r.trim_start().starts_with("->")) {
                return Some(path);
            }
        }
        None
    }
    walk(body, &[], arg_name)
}

/// The § Argument constraints clause (ADR 0031) for a source parameter or
/// property: `(default asc, one of asc|desc)`, `(default 25, min 1, max
/// 100)`. None when the source states none of the four keys.
fn constraint_clause(source: &Value) -> Option<String> {
    let scalar = |v: &Value| match v {
        Value::String(t) if t.is_empty() => "\"\"".to_string(),
        Value::String(t) => t.clone(),
        other => other.to_string(),
    };
    let mut parts: Vec<String> = Vec::new();
    if let Some(d) = get(source, "default").filter(|d| !d.is_null()) {
        parts.push(format!("default {}", scalar(d)));
    }
    if let Some(m) = get(source, "minimum").filter(|m| !m.is_null()) {
        parts.push(format!("min {}", scalar(m)));
    }
    if let Some(m) = get(source, "maximum").filter(|m| !m.is_null()) {
        parts.push(format!("max {}", scalar(m)));
    }
    let values: Vec<String> = get_arr(source, "enum")
        .into_iter()
        .flatten()
        .filter(|v| !v.is_null())
        .map(scalar)
        .collect();
    if !values.is_empty() {
        let shown = values.iter().take(8).cloned().collect::<Vec<_>>().join("|");
        let more = if values.len() > 8 {
            format!(" (+{} more)", values.len() - 8)
        } else {
            String::new()
        };
        parts.push(format!("one of {}{}", shown, more));
    }
    (!parts.is_empty()).then(|| format!("({})", parts.join(", ")))
}

/// The phrasings that mark a sentence as saying what happens when its
/// argument is omitted (ADR 0066, widened by ADR 0083; schema-authoring.md
/// § Argument constraints). Matched case-insensitively anywhere in the
/// sentence; generic, not per vendor. Four families:
///
/// - a default: `by default`, `defaults to`, `will default to`, `default
///   is`, `default value is`;
/// - a condition on absence: `if`/`when`, up to four words (`this
///   parameter is`), then `not passed|provided|specified|
///   given|set|supplied|sent|present|included`, or `omitted`, `absent`,
///   `unset`, `left empty|blank|out`; or `if no …` (up to three words)
///   `is|are passed|provided|specified|given|set|supplied|sent`;
/// - `unless (otherwise) specified|provided|set|given`;
/// - `omit (it|this) to …`.
///
/// ADR 0066's eight phrases are all in it. `missing` is not: "if the user is
/// missing a role" is about something else.
fn omission_cue() -> &'static Regex {
    static CUE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();
    re(
        &CUE,
        r"(?ix)
        \bby\s+default\b
        | \bdefaults?\s+(?:to|is)\b
        | \bdefault\s+value\s+is\b
        | \b(?:if|when)\s+(?:[\w`'\x22-]+\s+){0,4}?
            (?:not\s+(?:passed|provided|specified|given|set|supplied|sent|present|included)
              | omitted | absent | unset | left\s+(?:empty|blank|out))\b
        | \bif\s+no\s+(?:[\w`'\x22-]+\s+){1,3}?(?:is|are)\s+
            (?:passed|provided|specified|given|set|supplied|sent)\b
        | \bunless\s+(?:otherwise\s+)?(?:specified|provided|set|given)\b
        | \bomit\s+(?:it\s+|this\s+)?to\b",
    )
}

/// Abbreviations whose period does not end a sentence.
const ABBREVIATIONS: [&str; 7] = ["e.g", "i.e", "etc", "vs", "cf", "approx", "incl"];

/// A description's sentences, each whitespace-normalised and keeping its
/// terminal punctuation. A blank line or a list item (`- `, `* `) starts a
/// new sentence; inside a paragraph a sentence ends at `.`, `!` or `?`
/// followed by whitespace and a character that is not a lowercase letter,
/// unless the word before the period is an abbreviation (`e.g.`) or the
/// period is inside parentheses or brackets (`(max. 100 per page)`, ADR
/// 0083).
fn sentences(description: &str) -> Vec<String> {
    let mut paragraphs: Vec<String> = vec![String::new()];
    for line in description.lines() {
        let trimmed = line.trim();
        let item = trimmed
            .strip_prefix("- ")
            .or_else(|| trimmed.strip_prefix("* "));
        if trimmed.is_empty() || item.is_some() {
            paragraphs.push(String::new());
        }
        let text = item.unwrap_or(trimmed);
        let current = paragraphs.last_mut().unwrap();
        if !text.is_empty() {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(text);
        }
    }
    let mut out = Vec::new();
    for paragraph in paragraphs {
        let normalised = paragraph.split_whitespace().collect::<Vec<_>>().join(" ");
        let chars: Vec<char> = normalised.chars().collect();
        let mut start = 0;
        let mut depth = 0i32;
        for i in 0..chars.len() {
            match chars[i] {
                '(' | '[' => depth += 1,
                ')' | ']' => depth = (depth - 1).max(0),
                _ => {}
            }
            if depth > 0 || !matches!(chars[i], '.' | '!' | '?') || chars.get(i + 1) != Some(&' ') {
                continue;
            }
            if chars.get(i + 2).is_some_and(|c| c.is_lowercase()) {
                continue;
            }
            let word: String = chars[start..i]
                .iter()
                .rev()
                .take_while(|c| !c.is_whitespace())
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            let word = word.trim_start_matches(|c: char| !c.is_alphanumeric());
            if chars[i] == '.' && ABBREVIATIONS.iter().any(|a| word.eq_ignore_ascii_case(a)) {
                continue;
            }
            out.push(chars[start..=i].iter().collect::<String>());
            start = i + 2;
        }
        let rest: String = chars[start.min(chars.len())..].iter().collect();
        if !rest.trim().is_empty() {
            out.push(rest.trim().to_string());
        }
    }
    out
}

/// Whether `sentence` names the parameter `name`: the name (a trailing `[]`
/// dropped) appears case-sensitively with no letter, digit, `_` or `-`
/// against either side. `sort` is named in `if "sort" is not specified`, not
/// in `Sort direction` or `sort-order`.
fn names_parameter(sentence: &str, name: &str) -> bool {
    let name = name.trim_end_matches("[]");
    if name.is_empty() {
        return false;
    }
    let word = |c: char| c.is_alphanumeric() || c == '_' || c == '-';
    sentence.match_indices(name).any(|(at, _)| {
        let before = sentence[..at].chars().next_back();
        let after = sentence[at + name.len()..].chars().next();
        !before.is_some_and(word) && !after.is_some_and(word)
    })
}

/// The omission sentence of a parameter's (or body property's) source
/// `description` (ADR 0066): the first sentence carrying an
/// [`omission_cue`] phrasing that does not name one of `siblings` (the
/// operation's other parameters and body properties; `own` is never a
/// sibling). A sentence that names another parameter describes how two
/// arguments interact, not what omitting this one does. Returned
/// whitespace-normalised, with the source's own punctuation.
pub fn omission_sentence(description: &str, own: &str, siblings: &[&str]) -> Option<String> {
    let own = own.trim_end_matches("[]");
    sentences(description).into_iter().find(|sentence| {
        omission_cue().is_match(sentence)
            && !siblings
                .iter()
                .filter(|s| s.trim_end_matches("[]") != own)
                .any(|s| names_parameter(sentence, s))
    })
}

/// Whether an argument's doc comment carries `sentence`: the sentence
/// without its final period is a substring of the doc comment, both
/// whitespace-normalised and compared case-insensitively, the doc
/// comment's `\"` escapes read as quotes.
fn carries_sentence(doc: &str, sentence: &str) -> bool {
    let normalise = |t: &str| {
        t.replace("\\\"", "\"")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
            .to_lowercase()
    };
    let wanted = normalise(sentence.strip_suffix('.').unwrap_or(sentence));
    !wanted.is_empty() && normalise(doc).contains(&wanted)
}

/// Every parameter name of an operation and every top-level property of its
/// request body: the names an omission sentence may name (ADR 0066).
fn sibling_names<'a>(op: &'a Value, shapes: Option<&'a Object>) -> Vec<&'a str> {
    let mut names: Vec<&str> = get_arr(op, "parameters")
        .into_iter()
        .flatten()
        .filter_map(|p| get_str(p, "name"))
        .collect();
    let body = get(op, "request_body")
        .and_then(|r| get_str(r, "shape_ref"))
        .zip(shapes)
        .and_then(|(r, shapes)| shapes.get(r.trim_start_matches("#/shapes/")))
        .zip(shapes)
        .and_then(|(shape, shapes)| deref_shape(shape, shapes));
    if let Some(props) = body.and_then(|b| get_obj(b, "properties")) {
        names.extend(props.keys().map(String::as_str));
    }
    names
}

/// A sentence as written into a doc comment: its own terminal punctuation,
/// or a period when the source has none.
fn terminated(sentence: &str) -> String {
    if sentence.ends_with(['.', '!', '?']) {
        sentence.to_string()
    } else {
        format!("{}.", sentence)
    }
}

/// The text a root field's doc comment starts from, and where it comes from:
/// the selection's `graphql.description` override, else the inventory
/// operation's `summary`, else its `description` (workspace-contract.md).
/// Google discovery-derived specs carry no summaries, so the fallback is
/// what they have.
fn vendor_root_text(graphql: Option<&Value>, op: &Value) -> Option<(&'static str, String)> {
    let text_of = |v: Option<&Value>, key: &str| {
        v.and_then(|v| get_str(v, key))
            .map(str::trim)
            .filter(|t| !t.is_empty())
            .map(str::to_string)
    };
    text_of(graphql, "description")
        .map(|t| ("selection.yaml's graphql.description", t))
        .or_else(|| text_of(Some(op), "summary").map(|t| ("the source's summary", t)))
        .or_else(|| text_of(Some(op), "description").map(|t| ("the source's description", t)))
}

/// Two description rules (ADR 0062), for the default-on halves of
/// schema-authoring.md § Descriptions and § Argument constraints. Nothing
/// enforced either, and a whole service shipped with no root-field doc
/// comment while the inventory held the text for every one.
///
/// - `undocumented-root-field` (error): a selected operation's root field has
///   no doc comment while there is text to give it ([`vendor_root_text`]).
///   The doc comment is what an MCP client shows a model choosing between
///   tools, so a missing one is a defect of the published contract.
/// - `argument-constraints-undocumented` (warning): a root-field argument has
///   no doc comment while the source parameter or body property it reaches
///   carries a `default`, `minimum`, `maximum` or `enum`. The page-size and
///   page-index parameters are `pagination-bounds-*`'s and are skipped.
///   It also fires (ADR 0066) when an optional argument's source
///   description has an omission sentence ([`omission_sentence`]) that the
///   doc comment does not carry ([`carries_sentence`]), with or without a
///   doc comment; this half reads the comment's content.
///
/// The source-sentence half of an argument's doc comment is opt-in per
/// service (ADR 0040) with no machine-readable marker, so it is not checked.
/// Both findings carry the text to write, so the fix is a copy.
fn lint_descriptions(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    waived: &HashSet<(String, String)>,
    findings: &mut Findings,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };
    let shapes = get(inventory, "shapes").and_then(Value::as_object);
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let operations: HashMap<&str, &Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .filter_map(|o| get_str(o, "key").map(|k| (k, o)))
        .collect();
    let query_args = root_field_args(sdl, "Query");
    let mutation_args = root_field_args(sdl, "Mutation");
    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let graphql = get(entry, "graphql");
        let name = match graphql.and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let root = graphql.and_then(|g| get_str(g, "root")).unwrap_or("query");
        let root_type = if root == "query" { "Query" } else { "Mutation" };
        let field = format!("{}_{}", field_prefix, name);
        let field_text = match root_field_text(sdl, root_type, &field) {
            Some(t) => t,
            None => continue,
        };
        let op = match operations.get(key.as_str()) {
            Some(o) => *o,
            None => continue,
        };
        let line = field_line(sdl, root_type, &field);

        let field_doc = crate::graphql::type_body(sdl, root_type).and_then(|body| {
            let code = crate::graphql::blank(&body.body);
            let start_re =
                Regex::new(&format!(r"(?m)^[ \t]*{}\s*[(:]", regex::escape(&field))).unwrap();
            let m = start_re.find(&code)?;
            doc_comment_before(sdl, body.start + m.start())
        });
        if field_doc.is_none() {
            if let Some((origin, text)) = vendor_root_text(graphql, op) {
                findings.error(
                    "undocumented-root-field",
                    format!(
                        "{}: {}.{} has no doc comment; {} gives it one — add above the field: \"\"\"{}\"\"\" (schema-authoring.md § Descriptions)",
                        key, root_type, field, origin, text
                    ),
                    Some(schema_file),
                    line,
                );
            }
        }

        let args = if root == "query" {
            &query_args
        } else {
            &mutation_args
        }
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, a)| a.clone())
        .unwrap_or_default();
        for fact in argument_facts(&field_text, &args, op, shapes) {
            // `!` on an argument the source leaves optional hides its
            // omission behaviour: the caller can no longer leave it out. An
            // error, because a warning would make it a way past the
            // behaviour gate (ADR 0096); a resolved `behaviour` waiver on the
            // source is the recorded way out, as for the sentence.
            if fact.required_in_schema
                && fact.required_in_source == Some(false)
                && !waived.contains(&(key.clone(), fact.key.clone()))
            {
                findings.error(
                    "argument-required-optional-in-source",
                    format!(
                        "{}: argument `{}` of {}.{} is required (`!`) but the source's {} is optional; a caller with no value for it cannot leave it out, so what omitting it does is unreachable — make it optional, or record the decision with `decisions add --question … --choice … --resolved … --omit '{}|behaviour|{}|editorial'` (schema-authoring.md § Argument constraints)",
                        key, fact.arg, root_type, field, fact.origin, key, fact.key
                    ),
                    Some(schema_file),
                    line,
                );
            }
            // The mirror: the source requires a value the schema lets the
            // caller leave out, so a call without it reaches the vendor
            // without it and fails there. Quiet when something else fills
            // the value (a GraphQL default, a `$args.x ??` fallback in the
            // connector), and for a key nested in a body object, whose
            // `required` holds only when its parent is sent.
            if !fact.required_in_schema
                && fact.required_in_source == Some(true)
                && !(fact.key.starts_with("body:") && fact.key.contains('.'))
                && !argument_has_default(&field_text, &fact.arg)
                && !connector_falls_back(&field_text, &fact.arg)
                && !waived.contains(&(key.clone(), fact.key.clone()))
            {
                findings.warn(
                    "argument-optional-required-in-source",
                    format!(
                        "{}: argument `{}` of {}.{} is optional but the source's {} is required; a call that leaves it out is sent without it and fails at the vendor — make it required (`!`), give it a default, or record why the source's `required` does not hold with `decisions add --question … --choice … --resolved … --omit '{}|behaviour|{}|editorial'` (schema-authoring.md § Argument constraints)",
                        key, fact.arg, root_type, field, fact.origin, key, fact.key
                    ),
                    Some(schema_file),
                    line,
                );
            }
            // A waived omission sentence is not owed (ADR 0095); the
            // constraint clause still is.
            let owed = fact
                .sentence
                .as_ref()
                .filter(|_| !fact.carried)
                .filter(|_| !waived.contains(&(key.clone(), fact.key.clone())));
            let arg_name = &fact.arg;
            let origin = &fact.origin;
            let message = match (&fact.doc, &fact.clause, owed) {
                (None, Some(clause), None) => format!(
                    "{}: argument `{}` of {}.{} has no doc comment; the source's {} states {} — write it as the argument's doc comment (schema-authoring.md § Argument constraints)",
                    key, arg_name, root_type, field, origin, clause
                ),
                (None, clause, Some(sentence)) => {
                    let text = match clause {
                        Some(c) => format!("{}. {}", c, terminated(sentence)),
                        None => terminated(sentence),
                    };
                    format!(
                        "{}: argument `{}` of {}.{} has no doc comment; the source's {} says what omitting it does, \"{}\" — write \"{}\" as the argument's doc comment (schema-authoring.md § Argument constraints)",
                        key, arg_name, root_type, field, origin, sentence, text
                    )
                }
                (Some(_), _, Some(sentence)) => format!(
                    "{}: the doc comment on argument `{}` of {}.{} leaves out what omitting it does; the source's {} says \"{}\" — keep it verbatim after the constraint clause, or record why it does not apply with `findings add --title … --body … --omit '{}|behaviour|{}|not-applicable'` (schema-authoring.md § Argument constraints)",
                    key, arg_name, root_type, field, origin, sentence, key, fact.key
                ),
                _ => continue,
            };
            findings.warn(
                "argument-constraints-undocumented",
                message,
                Some(schema_file),
                line,
            );
        }
    }
}

/// The argument's declaration in the field text carries a default value
/// (`limit: Int = 25`): the router fills it when the caller leaves it out.
fn argument_has_default(field_text: &str, arg: &str) -> bool {
    let code = crate::graphql::blank(field_text);
    Regex::new(&format!(
        r"(?:^|[(,\s]){}\s*:\s*[^,)=@]*=",
        regex::escape(arg)
    ))
    .map(|re| re.is_match(&code))
    .unwrap_or(false)
}

/// The connector supplies a value of its own when the argument is absent
/// (`$args.limit ?? 25`).
fn connector_falls_back(field_text: &str, arg: &str) -> bool {
    Regex::new(&format!(r"\$args\.{}\s*\?\?", regex::escape(arg)))
        .map(|re| re.is_match(field_text))
        .unwrap_or(false)
}

/// What one root-field argument's source says, judged against the argument's
/// doc comment (ADR 0062, 0066, 0083, 0095). The single reading `lint`
/// warns from and `source-coverage` classifies from.
pub struct ArgFact {
    pub arg: String,
    /// For a message: "query parameter `x`".
    pub origin: String,
    /// The name a decision waives the omission sentence by (`query:x`).
    pub key: String,
    /// The argument's own doc comment.
    pub doc: Option<String>,
    /// The § Argument constraints clause the source states, if any.
    pub clause: Option<String>,
    /// The source's omission sentence for an optional argument, whether or
    /// not the doc comment carries it.
    pub sentence: Option<String>,
    /// The doc comment carries `sentence`.
    pub carried: bool,
    /// The argument is required in the schema (`!`).
    pub required_in_schema: bool,
    /// The source requires it, when the inventory says.
    pub required_in_source: Option<bool>,
}

/// An operation's arguments traced to their source, one [`ArgFact`] each.
/// An argument wired some way the trace does not follow, and the page-size
/// and page-index arguments (`pagination-bounds-*`'s), give none.
pub fn argument_facts(
    field_text: &str,
    args: &[Arg],
    op: &Value,
    shapes: Option<&Object>,
) -> Vec<ArgFact> {
    let pagination = get(op, "pagination");
    let paging_params: Vec<&str> = ["size_param", "request"]
        .iter()
        .filter_map(|k| pagination.and_then(|p| get_str(p, k)))
        .collect();
    let operation_siblings = sibling_names(op, shapes);
    let mut facts = Vec::new();
    for arg in args {
        let doc = inline_arg_doc(field_text, &arg.name);
        let Some(source) = argument_source(field_text, op, shapes, &arg.name) else {
            continue;
        };
        // A body property has no `name`: its key is the page-size or
        // page-index parameter's name (ADR 0083).
        let wire_name = get_str(&source.entry, "name").unwrap_or(&source.name);
        if paging_params.contains(&wire_name) {
            continue;
        }
        let siblings: Vec<&str> = operation_siblings
            .iter()
            .copied()
            .chain(source.peers.iter().map(String::as_str))
            .collect();
        let sentence = get_str(&source.entry, "description")
            .filter(|_| !arg.is_required())
            .and_then(|d| omission_sentence(d, &source.name, &siblings));
        let carried = sentence
            .as_deref()
            .is_some_and(|s| doc.as_deref().is_some_and(|d| carries_sentence(d, s)));
        facts.push(ArgFact {
            arg: arg.name.clone(),
            origin: source.origin.clone(),
            key: source.key.clone(),
            clause: constraint_clause(&source.entry),
            doc,
            sentence,
            carried,
            required_in_schema: arg.is_required(),
            required_in_source: source.required,
        });
    }
    facts
}

/// One numeric-range rule, an error rather than a warning because the value
/// is wrong at runtime rather than merely undocumented:
///
/// - `int-overflow`: a selected operation's response leaf or argument is
///   `Int` in the schema while the source declares a format whose range
///   reaches past `Int` (`int64`, `uint64` or `uint32`, whose unsigned
///   upper half does not fit), or a `minimum`/`maximum` (either one made
///   exclusive, or a numeric `exclusiveMinimum`/`exclusiveMaximum`) or a
///   `default` outside `Int`'s [-2147483648, 2147483647]. GraphQL `Int` is 32-bit, so the
///   router nulls the field and reports a coercion error rather than
///   returning the number — a silent wrong answer on the small values and a
///   failure on the large ones. `ID` is not `Int` and is never reported;
///   nor is a leaf the connector already maps (`->jsonStringify`), nor an
///   operation's pagination size parameter, which `page-limit-not-int`
///   owns. There is no doc-comment exemption: a sentence beside the field
///   does not change what the router coerces (ADR 0030). The one exception
///   is an argument's `default`, which the router never sends: the
///   `default: unbounded` spelling in its doc comment clears it (ADR 0065).
///   A doc comment that states the number instead (`default N`, the spelling
///   before ADR 0065) stays an error; the finding quotes that clause and
///   gives the replacement with N filled in (ADR 0093). `default: no upper
///   bound` is quoted the same way, and every legacy clause is named, to
///   replace or to delete (ADR 0104).
fn lint_int_overflow(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };
    let shapes = match get(inventory, "shapes").and_then(Value::as_object) {
        Some(s) => s,
        None => return,
    };
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let operations: HashMap<&str, &Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .filter_map(|o| get_str(o, "key").map(|k| (k, o)))
        .collect();
    let mut index = crate::sdl_index::SdlIndex::new(sdl);
    let query_args = root_field_args(sdl, "Query");
    let mutation_args = root_field_args(sdl, "Mutation");
    let return_re = Regex::new(r"\)\s*:\s*([\[\]!A-Za-z0-9_]+)").unwrap();
    let selection_block = Regex::new(r#"(?s)\bselection\s*:\s*"""(.*?)""""#).unwrap();
    let selection_line = Regex::new(r#"\bselection\s*:\s*"([^"]*)""#).unwrap();
    let http_path_re =
        Regex::new(r#"\b(?:GET|POST|PUT|PATCH|DELETE|HEAD)\s*:\s*"([^"]+)""#).unwrap();
    // One finding per schema slot, however many operations reach it: a
    // shared type is declared once and fixed once.
    let mut reported: HashSet<String> = HashSet::new();
    let advice = "GraphQL Int is 32-bit — use ID for an identifier, or String mapped with path->match([null, null], [@, @->jsonStringify]) for a magnitude (schema-authoring.md § Scalar choice)";

    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let name = match get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let root = get(entry, "graphql")
            .and_then(|g| get_str(g, "root"))
            .unwrap_or("query");
        let root_type = if root == "query" { "Query" } else { "Mutation" };
        let field = format!("{}_{}", field_prefix, name);
        let field_text = match root_field_text(sdl, root_type, &field) {
            Some(t) => t,
            None => continue,
        };
        let op = match operations.get(key.as_str()) {
            Some(o) => *o,
            None => continue,
        };

        // ── Arguments: the value the router puts on the wire ──────────────
        let wire = wiring(&field_text);
        let body = body_mapping(&field_text);
        let http_path = http_path_re
            .captures(&field_text)
            .map(|m| m[1].to_string())
            .unwrap_or_default();
        // A pagination size parameter belongs to `page-limit-not-int`, which
        // wants it `Int`; a page size is small whatever the spec's format.
        let size_param = get(op, "pagination").and_then(|p| get_str(p, "size_param"));
        let args = if root == "query" {
            &query_args
        } else {
            &mutation_args
        }
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, a)| a.clone())
        .unwrap_or_default();

        for arg in &args {
            if crate::sdl_index::base_type(&arg.type_) != "Int" {
                continue;
            }
            let mut source: Option<(String, Value)> = None;
            if let Some((_, k)) = wire.query_keys.iter().find(|(a, _)| a == &arg.name) {
                if slot_expression(&field_text, "queryParams", k)
                    .map(|e| uses_mapping_language(&e))
                    .unwrap_or(false)
                {
                    continue;
                }
                let base_key = k.trim_end_matches("[]");
                if size_param == Some(base_key) || size_param == Some(k.as_str()) {
                    continue;
                }
                if let Some(p) = get_arr(op, "parameters").into_iter().flatten().find(|p| {
                    get_str(p, "in") == Some("query")
                        && (get_str(p, "name") == Some(base_key) || get_str(p, "name") == Some(k))
                }) {
                    source = Some((format!("query parameter `{}`", base_key), p.clone()));
                }
            }
            if source.is_none() && http_path.contains(&format!("{{$args.{}}}", arg.name)) {
                if size_param == Some(arg.name.as_str()) {
                    continue;
                }
                if let Some(p) = get_arr(op, "parameters").into_iter().flatten().find(|p| {
                    get_str(p, "in") == Some("path") && get_str(p, "name") == Some(&arg.name)
                }) {
                    source = Some((format!("path parameter `{}`", arg.name), p.clone()));
                }
            } else if source.is_none() && http_path.contains(&format!("{{$args.{}->", arg.name)) {
                continue;
            }
            if source.is_none() {
                if let BodyMapping::Flat(map) = &body {
                    if let Some((_, k)) = map.iter().find(|(a, _)| a == &arg.name) {
                        if let Some(prop) = get(op, "request_body")
                            .and_then(|r| get_str(r, "shape_ref"))
                            .and_then(|sr| {
                                let reference =
                                    crate::json::object(vec![("$ref", Value::from(sr))]);
                                property_of(&reference, shapes, k, 0).cloned()
                            })
                        {
                            source = Some((format!("body key `{}`", k), prop));
                        }
                    }
                }
            }
            let (where_, prop) = match source {
                Some(x) => x,
                None => continue,
            };
            // A default outside `Int` is never sent by the router (`= literal`
            // is banned), so it reaches coercion only if a caller copies it
            // out of the prose; the `default: unbounded` spelling tells the
            // caller to omit the argument instead, and clears it (ADR 0065).
            // A doc comment that states the number (the spelling before ADR
            // 0065) is quoted as the clause to replace, and the replacement
            // carries the source's number; while that clause stays, the
            // `default: unbounded` spelling beside it does not clear the
            // finding, since the number is still there to copy (ADR 0093).
            // `default: no upper bound` is a legacy clause too: it is named
            // for replacement, and one left beside `default: unbounded` is
            // named for deletion, so a finding followed literally leaves one
            // default clause (ADR 0104).
            let (reason, remedy) = match int_overflow_bound_gap(&prop, shapes) {
                Some(r) => (r, advice.to_string()),
                None => {
                    let doc =
                        arg_doc_comment(sdl, root_type, &field, &arg.name).unwrap_or_default();
                    let n = match int_overflow_default_value(&prop, shapes) {
                        Some(n) => n,
                        None => continue,
                    };
                    let unbounded = doc.to_lowercase().contains(UNBOUNDED_DEFAULT);
                    let spelling = format!(
                        "{}: the source's {} does not fit Int; omit the argument to accept it",
                        UNBOUNDED_DEFAULT, n
                    );
                    let clauses = int_overflow_legacy_default_clauses(&doc, &n);
                    let remedy = match (clauses.is_empty(), unbounded) {
                        (false, _) => {
                            int_overflow_legacy_remedy(&clauses, unbounded, &spelling, advice)
                        }
                        (true, false) => format!("state it as `({})` in the argument's doc comment, or retype the argument: {}", spelling, advice),
                        (true, true) => continue,
                    };
                    (format!("declares default {}", n), remedy)
                }
            };
            if reported.insert(format!("arg:{}.{}", field, arg.name)) {
                let line = field_line(sdl, root_type, &field);
                findings.error(
                    "int-overflow",
                    format!(
                        "{}: argument `{}` is Int, but the source's {} {}; {}",
                        key, arg.name, where_, reason, remedy
                    ),
                    Some(schema_file),
                    line,
                );
            }
        }

        // ── Response leaves: the value the router coerces on the way back ──
        let response_shape = match get(op, "response").and_then(|r| get_str(r, "shape_ref")) {
            Some(sr) => crate::json::object(vec![("$ref", Value::from(sr))]),
            None => continue,
        };
        let blanked = crate::graphql::blank(&field_text);
        let return_type = return_re
            .captures(&blanked)
            .map(|m| crate::sdl_index::base_type(&m[1]))
            .or_else(|| {
                Regex::new(&format!(
                    r"\b{}\s*:\s*([\[\]!A-Za-z0-9_]+)",
                    regex::escape(&field)
                ))
                .unwrap()
                .captures(&blanked)
                .map(|m| crate::sdl_index::base_type(&m[1]))
            });
        let return_type = match return_type {
            Some(t) => t,
            None => continue,
        };
        let selection_text = selection_block
            .captures(&field_text)
            .map(|m| m[1].to_string())
            .or_else(|| {
                selection_line
                    .captures(&field_text)
                    .map(|m| m[1].to_string())
            })
            .unwrap_or_default();
        let nodes = crate::reconcile::parse_selection(&selection_text);
        // (owner type — empty for the root field itself, field, path, reason)
        let mut hits: Vec<(String, String, String, String)> = Vec::new();
        walk_int_overflow(
            &nodes,
            Some(&response_shape),
            shapes,
            &return_type,
            "",
            &field,
            &mut index,
            &mut hits,
        );
        for (owner, leaf, path, reason) in hits {
            let owner_type = if owner.is_empty() {
                root_type.to_string()
            } else {
                owner.clone()
            };
            let line = field_line(sdl, &owner_type, &leaf);
            if reported.insert(format!("leaf:{}.{}", owner_type, leaf)) {
                findings.error(
                    "int-overflow",
                    format!(
                        "{}: `{}.{}` is Int, but the source property at `{}` {}; {}",
                        key, owner_type, leaf, path, reason, advice
                    ),
                    Some(schema_file),
                    line,
                );
            }
        }
    }
}

/// Selection nodes beside the response shape and the GraphQL type they land
/// in; every `Int`-typed leaf whose source property `int_overflow_gap`
/// rejects is collected as (owner type, field, path, reason). A leaf the
/// connector maps is skipped — the mapping, not the source, decides what
/// reaches the schema — and `reconcile::parse_selection` is what records
/// that: it sets `opaque` on any node carrying a method, so the single
/// `n.opaque` test covers a mapped leaf and a `$(…)` expression alike.
/// `root_field` names the root field, which owns a `$.x` leaf selected
/// straight into the root type.
#[allow(clippy::too_many_arguments)]
fn walk_int_overflow(
    nodes: &[crate::reconcile::Node],
    shape: Option<&Value>,
    shapes: &Object,
    gql_type: &str,
    prefix: &str,
    root_field: &str,
    index: &mut crate::sdl_index::SdlIndex,
    hits: &mut Vec<(String, String, String, String)>,
) {
    let shape = match shape {
        Some(s) => s,
        None => return,
    };
    for n in nodes {
        // A spread's arms land in the member type each one names, and read
        // the variant its candidate selects (ADR 0058).
        if let Some(crate::reconcile::Spread::Match(arms)) = &n.spread {
            for arm in arms {
                let Some(member) = arm.typename.as_deref() else {
                    continue;
                };
                let inner = arm_shape(shape, shapes, n.key.as_deref(), arm.candidate.as_deref());
                walk_int_overflow(
                    &arm.children,
                    inner,
                    shapes,
                    member,
                    prefix,
                    root_field,
                    index,
                    hits,
                );
            }
        }
        if n.opaque {
            continue;
        }
        let key = match &n.key {
            Some(k) => k,
            None => continue,
        };
        let path = if prefix.is_empty() {
            key.join(".")
        } else {
            format!("{}.{}", prefix, key.join("."))
        };
        if n.rooted && n.alias.is_none() {
            match &n.children {
                // `$.data { … }`: the children live in the same GraphQL type.
                Some(children) => {
                    let inner = shape_at(shape, shapes, key, true);
                    walk_int_overflow(
                        children, inner, shapes, gql_type, &path, root_field, index, hits,
                    );
                }
                // `$.total` alone: the root field itself is the leaf.
                None => {
                    if gql_type != "Int" {
                        continue;
                    }
                    if let Some(prop) = shape_at(shape, shapes, key, false) {
                        if let Some(reason) = int_overflow_gap(prop, shapes) {
                            hits.push((String::new(), root_field.to_string(), path, reason));
                        }
                    }
                }
            }
            continue;
        }
        let exposed = match n.alias.clone().or_else(|| key.last().cloned()) {
            Some(x) => x,
            None => continue,
        };
        let ftype = match index.field_type(gql_type, &exposed) {
            Some(t) => crate::sdl_index::base_type(&t),
            None => continue,
        };
        match &n.children {
            Some(children) => {
                let inner = shape_at(shape, shapes, key, true);
                walk_int_overflow(
                    children, inner, shapes, &ftype, &path, root_field, index, hits,
                );
            }
            None => {
                if ftype != "Int" {
                    continue;
                }
                if let Some(prop) = shape_at(shape, shapes, key, false) {
                    if let Some(reason) = int_overflow_gap(prop, shapes) {
                        hits.push((gql_type.to_string(), exposed, path, reason));
                    }
                }
            }
        }
    }
}

/// The two rules about names and values on the wire:
///
/// - `field-casing`: a field declared in snake_case on an object, interface
///   or input type, or a root field whose name after the prefix is
///   snake_case. GraphQL fields are camelCase; a type's rename belongs in
///   the selection (`fooBar: foo_bar`), a root field's in `selection.yaml`'s
///   `graphql.name`. A doc comment on the field records a deliberate
///   exception (naming.md).
/// - `wire-enum-drift`: a GraphQL enum whose values are not the spec's for
///   the parameter or property it is mapped to. An argument's value goes on
///   the wire as spelled, so an argument value the spec does not list is
///   reported; a response value is handed back as spelled, so a spec value
///   the enum lacks is reported (the payload fails coercion). A slot the
///   connector maps through the language (`->match`) is the mapping's to
///   spell, and is skipped.
fn lint_casing(
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let roots = ["Query", "Mutation", "Subscription"];
    let field_decl = Regex::new(r"^([A-Za-z_][A-Za-z0-9_]*)\s*[(:]").unwrap();
    for decl in type_declarations(sdl) {
        if !matches!(decl.kind.as_str(), "type" | "interface" | "input") {
            continue;
        }
        let body = match crate::graphql::type_body_at(sdl, decl.index) {
            Some(b) => b,
            None => continue,
        };
        let is_root = roots.contains(&decl.name.as_str());
        // Blanked text keeps every offset; depth tracking keeps argument
        // lists (and anything nested) from reading as fields.
        let code = crate::graphql::blank(&body.body);
        let mut depth = 0i32;
        let mut offset = 0usize;
        for raw in code.split_inclusive('\n') {
            let line = raw.trim_end_matches('\n');
            let leading = line.len() - line.trim_start().len();
            if depth == 0 {
                if let Some(m) = field_decl.captures(line.trim_start()) {
                    let name = m[1].to_string();
                    let local: Option<String> = if is_root {
                        if field_prefix.is_empty() {
                            None
                        } else {
                            name.strip_prefix(&format!("{}_", field_prefix))
                                .map(str::to_string)
                        }
                    } else {
                        Some(name.clone())
                    };
                    if let Some(local) = local.filter(|l| l.contains('_')) {
                        let index = body.start + offset + leading;
                        if doc_comment_before(sdl, index).is_none() {
                            let message = if is_root {
                                format!(
                                    "{}.{} is snake_case after the prefix; GraphQL fields are camelCase — rename it in selection.yaml's graphql.name (`{}`), or say in a doc comment why the wire name is kept",
                                    decl.name,
                                    name,
                                    camel(&local)
                                )
                            } else {
                                format!(
                                    "{}.{} is snake_case; GraphQL fields are camelCase — alias it in the selection (`{}: {}`), or say in a doc comment why the wire name is kept",
                                    decl.name,
                                    name,
                                    camel(&local),
                                    local
                                )
                            };
                            findings.warn(
                                "field-casing",
                                message,
                                Some(schema_file),
                                Some(line_of(sdl, index)),
                            );
                        }
                    }
                }
            }
            let opens = line.chars().filter(|c| matches!(c, '(' | '{')).count() as i32;
            let closes = line.chars().filter(|c| matches!(c, ')' | '}')).count() as i32;
            depth = (depth + opens - closes).max(0);
            offset += raw.len();
        }
    }

    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };
    let shapes = match get(inventory, "shapes").and_then(Value::as_object) {
        Some(s) => s,
        None => return,
    };
    let operations: HashMap<&str, &Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .filter_map(|o| get_str(o, "key").map(|k| (k, o)))
        .collect();
    let enum_lines: HashMap<String, usize> = type_declarations(sdl)
        .into_iter()
        .filter(|d| d.kind == "enum")
        .map(|d| (d.name, d.line))
        .collect();
    let mut index = crate::sdl_index::SdlIndex::new(sdl);
    let query_args = root_field_args(sdl, "Query");
    let mutation_args = root_field_args(sdl, "Mutation");
    let return_re = Regex::new(r"\)\s*:\s*([\[\]!A-Za-z0-9_]+)").unwrap();
    let selection_block = Regex::new(r#"(?s)\bselection\s*:\s*"""(.*?)""""#).unwrap();
    let selection_line = Regex::new(r#"\bselection\s*:\s*"([^"]*)""#).unwrap();
    let http_path_re =
        Regex::new(r#"\b(?:GET|POST|PUT|PATCH|DELETE|HEAD)\s*:\s*"([^"]+)""#).unwrap();
    let mut reported: HashSet<String> = HashSet::new();

    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let name = match get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let root = get(entry, "graphql")
            .and_then(|g| get_str(g, "root"))
            .unwrap_or("query");
        let root_type = if root == "query" { "Query" } else { "Mutation" };
        let field = format!("{}_{}", field_prefix, name);
        let field_text = match root_field_text(sdl, root_type, &field) {
            Some(t) => t,
            None => continue,
        };
        let op = match operations.get(key.as_str()) {
            Some(o) => *o,
            None => continue,
        };
        let wire = wiring(&field_text);
        let body = body_mapping(&field_text);
        let http_path = http_path_re
            .captures(&field_text)
            .map(|m| m[1].to_string())
            .unwrap_or_default();
        let args = if root == "query" {
            &query_args
        } else {
            &mutation_args
        }
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, a)| a.clone())
        .unwrap_or_default();

        // Arguments: the value goes on the wire as spelled, unless the slot's
        // own expression translates it.
        for arg in &args {
            let base = crate::sdl_index::base_type(&arg.type_);
            let schema_values = match index.enum_values(&base) {
                Some(v) => v.clone(),
                None => continue,
            };
            let mut spec_values: Option<(String, Vec<String>)> = None;
            if let Some((_, k)) = wire.query_keys.iter().find(|(a, _)| a == &arg.name) {
                if slot_expression(&field_text, "queryParams", k)
                    .map(|e| uses_mapping_language(&e))
                    .unwrap_or(false)
                {
                    continue;
                }
                let base_key = k.trim_end_matches("[]");
                if let Some(p) = get_arr(op, "parameters").into_iter().flatten().find(|p| {
                    get_str(p, "in") == Some("query")
                        && (get_str(p, "name") == Some(base_key) || get_str(p, "name") == Some(k))
                }) {
                    spec_values =
                        param_enum(p).map(|v| (format!("query parameter `{}`", base_key), v));
                }
            }
            if spec_values.is_none() && http_path.contains(&format!("{{$args.{}}}", arg.name)) {
                if let Some(p) = get_arr(op, "parameters").into_iter().flatten().find(|p| {
                    get_str(p, "in") == Some("path") && get_str(p, "name") == Some(&arg.name)
                }) {
                    spec_values =
                        param_enum(p).map(|v| (format!("path parameter `{}`", arg.name), v));
                }
            } else if spec_values.is_none()
                && http_path.contains(&format!("{{$args.{}->", arg.name))
            {
                continue;
            }
            if spec_values.is_none() {
                if let BodyMapping::Flat(map) = &body {
                    if let Some((_, k)) = map.iter().find(|(a, _)| a == &arg.name) {
                        if let Some(prop) = get(op, "request_body")
                            .and_then(|r| get_str(r, "shape_ref"))
                            .and_then(|sr| {
                                let reference =
                                    crate::json::object(vec![("$ref", Value::from(sr))]);
                                property_of(&reference, shapes, k, 0).cloned()
                            })
                        {
                            spec_values =
                                spec_enum(&prop, shapes).map(|v| (format!("body key `{}`", k), v));
                        }
                    }
                }
            }
            let (where_, spec) = match spec_values {
                Some(x) => x,
                None => continue,
            };
            let extra: Vec<String> = schema_values
                .iter()
                .filter(|v| !spec.contains(v))
                .cloned()
                .collect();
            if extra.is_empty() {
                continue;
            }
            let message = format!(
                "{} declares {} which the spec does not list for {}'s {} ({}); the router sends an argument value as spelled — use the wire spelling, or map it with ->match",
                base,
                list(&extra),
                key,
                where_,
                list(&spec)
            );
            // One finding per enum and gap: the same argument type is
            // reached from every operation that takes it.
            if reported.insert(format!("arg:{}:{}", base, extra.join(","))) {
                findings.warn(
                    "wire-enum-drift",
                    message,
                    Some(schema_file),
                    enum_lines.get(&base).copied(),
                );
            }
        }

        // Response leaves: the payload value is handed back as spelled, so a
        // spec value the enum lacks fails coercion.
        let response_shape = match get(op, "response").and_then(|r| get_str(r, "shape_ref")) {
            Some(sr) => crate::json::object(vec![("$ref", Value::from(sr))]),
            None => continue,
        };
        let blanked = crate::graphql::blank(&field_text);
        let return_type = return_re
            .captures(&blanked)
            .map(|m| crate::sdl_index::base_type(&m[1]))
            .or_else(|| {
                Regex::new(&format!(
                    r"\b{}\s*:\s*([\[\]!A-Za-z0-9_]+)",
                    regex::escape(&field)
                ))
                .unwrap()
                .captures(&blanked)
                .map(|m| crate::sdl_index::base_type(&m[1]))
            });
        let return_type = match return_type {
            Some(t) => t,
            None => continue,
        };
        let selection_text = selection_block
            .captures(&field_text)
            .map(|m| m[1].to_string())
            .or_else(|| {
                selection_line
                    .captures(&field_text)
                    .map(|m| m[1].to_string())
            })
            .unwrap_or_default();
        let nodes = crate::reconcile::parse_selection(&selection_text);
        let mut hits: Vec<(String, String, Vec<String>)> = Vec::new();
        walk_enums(
            &nodes,
            Some(&response_shape),
            shapes,
            &return_type,
            "",
            &mut index,
            &mut hits,
        );
        for (path, enum_name, missing) in hits {
            let message = format!(
                "{} lacks {} which the spec lists at {}'s `{}`; the router hands a payload value back as spelled, and one the enum does not declare fails coercion — add the wire spelling, or map it with ->match",
                enum_name,
                list(&missing),
                key,
                path
            );
            // One finding per enum and gap, however many operations select
            // the field: the enum is declared once and fixed once.
            if reported.insert(format!("leaf:{}:{}", enum_name, missing.join(","))) {
                findings.warn(
                    "wire-enum-drift",
                    message,
                    Some(schema_file),
                    enum_lines.get(&enum_name).copied(),
                );
            }
        }
    }
}

/// Compare one enum-typed leaf: the spec values the enum lacks.
fn enum_gap(
    index: &mut crate::sdl_index::SdlIndex,
    gql_type: &str,
    prop: &Value,
    shapes: &Object,
) -> Option<Vec<String>> {
    let schema_values = index.enum_values(gql_type)?.clone();
    let spec = spec_enum(prop, shapes)?;
    let missing: Vec<String> = spec
        .iter()
        .filter(|v| !schema_values.contains(v))
        .cloned()
        .collect();
    if missing.is_empty() {
        None
    } else {
        Some(missing)
    }
}

/// Selection nodes beside the response shape and the GraphQL type they land
/// in; every enum-typed leaf whose spec property carries `enum` is compared.
/// A leaf the connector maps is skipped — the mapping, not the source,
/// decides what reaches the schema — and `n.opaque` is the whole test:
/// `reconcile::parse_item` promotes every node carrying a method to `opaque`
/// before it returns, so a `!n.methods.is_empty()` clause beside it can never
/// be reached. `wire_enum_drift_quiet_when_an_enum_leaf_carries_a_mapping`
/// pins the skip (ADR 0030).
fn walk_enums(
    nodes: &[crate::reconcile::Node],
    shape: Option<&Value>,
    shapes: &Object,
    gql_type: &str,
    prefix: &str,
    index: &mut crate::sdl_index::SdlIndex,
    hits: &mut Vec<(String, String, Vec<String>)>,
) {
    let shape = match shape {
        Some(s) => s,
        None => return,
    };
    for n in nodes {
        // A spread's arms land in the member type each one names, and read
        // the variant its candidate selects (ADR 0058).
        if let Some(crate::reconcile::Spread::Match(arms)) = &n.spread {
            for arm in arms {
                let Some(member) = arm.typename.as_deref() else {
                    continue;
                };
                let inner = arm_shape(shape, shapes, n.key.as_deref(), arm.candidate.as_deref());
                walk_enums(&arm.children, inner, shapes, member, prefix, index, hits);
            }
        }
        if n.opaque {
            continue;
        }
        let key = match &n.key {
            Some(k) => k,
            None => continue,
        };
        let path = if prefix.is_empty() {
            key.join(".")
        } else {
            format!("{}.{}", prefix, key.join("."))
        };
        if n.rooted && n.alias.is_none() {
            match &n.children {
                // `$.data { … }`: the children live in the same GraphQL type.
                Some(children) => {
                    let inner = shape_at(shape, shapes, key, true);
                    walk_enums(children, inner, shapes, gql_type, &path, index, hits);
                }
                // `$.status` alone: the field itself is the leaf.
                None => {
                    if let Some(prop) = shape_at(shape, shapes, key, false) {
                        if let Some(missing) = enum_gap(index, gql_type, prop, shapes) {
                            hits.push((path, gql_type.to_string(), missing));
                        }
                    }
                }
            }
            continue;
        }
        let exposed = match n.alias.clone().or_else(|| key.last().cloned()) {
            Some(x) => x,
            None => continue,
        };
        let ftype = match index.field_type(gql_type, &exposed) {
            Some(t) => crate::sdl_index::base_type(&t),
            None => continue,
        };
        match &n.children {
            Some(children) => {
                let inner = shape_at(shape, shapes, key, true);
                walk_enums(children, inner, shapes, &ftype, &path, index, hits);
            }
            None => {
                if let Some(prop) = shape_at(shape, shapes, key, false) {
                    if let Some(missing) = enum_gap(index, &ftype, prop, shapes) {
                        hits.push((path, ftype, missing));
                    }
                }
            }
        }
    }
}

/// `foo_bar_baz` → `fooBarBaz`.
fn camel(snake: &str) -> String {
    let mut out = String::new();
    let mut upper = false;
    for c in snake.chars() {
        if c == '_' {
            upper = !out.is_empty();
            continue;
        }
        if upper {
            out.extend(c.to_uppercase());
            upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// A closed vocabulary is one the schema can spell as a GraphQL enum: every
/// value is a valid enum value name (`^[_A-Za-z][_0-9A-Za-z]*$`, and not
/// `true`, `false` or `null`, which the GraphQL grammar reserves), and there
/// are **two or more** of them. A vocabulary with a leading digit or a hyphen
/// is not — there `String` is the right mapping (schema-authoring.md § Enums)
/// — an empty list is not a vocabulary at all, and a one-member list is a
/// constant discriminator (pagerduty's `Service.type` is always `service`),
/// not a choice: a one-member GraphQL enum documents nothing the field does
/// not already say. The floor applies to arguments and leaves alike.
fn closed_enum_values(values: &[String]) -> bool {
    values.len() >= 2
        && values.iter().all(|v| {
            let mut chars = v.chars();
            let head = matches!(chars.next(), Some(c) if c == '_' || c.is_ascii_alphabetic());
            head && chars.all(|c| c == '_' || c.is_ascii_alphanumeric())
                && !matches!(v.as_str(), "true" | "false" | "null")
        })
}

/// `list()` for a message that must stay readable when the vocabulary is
/// long: the first `cap` values, then `(+N more)`. Its own function rather
/// than a change to `list()`, whose callers' messages are pinned by their
/// tests and by the pilots' lint output.
fn list_capped(values: &[String], cap: usize) -> String {
    if values.len() <= cap {
        return list(values);
    }
    format!("{} (+{} more)", list(&values[..cap]), values.len() - cap)
}

/// `entry_type` → `EntryType`: the name half of the `<Prefix>_<Name>` enum
/// the rule's advice proposes.
fn pascal(name: &str) -> String {
    let c = camel(&name.replace('-', "_"));
    let mut chars = c.chars();
    match chars.next() {
        Some(f) => f.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// `decision-without-alternative` (warn, ADR 0113): an open or resolved
/// record in decisions.json with no `question` and fewer than two
/// `choices`. `decisions add` refuses such a record now; one written
/// before, or kept by `decisions migrate --split` because it carries an
/// editorial omit, is asked to gain its alternative, which the split's
/// `--sorted` entry (`as: decision`) adds. A superseded record no longer
/// counts and draws nothing; an unreadable log is reported elsewhere.
fn lint_decision_alternatives(decisions: Option<&Value>, findings: &mut Findings) {
    let Some(doc) = decisions else {
        return;
    };
    for rec in get_arr(doc, "decisions").into_iter().flatten() {
        if get_str(rec, "status") == Some("superseded") || crate::split::has_alternative(rec) {
            continue;
        }
        let id = get_str(rec, "id").unwrap_or("D-????");
        findings.warn(
            "decision-without-alternative",
            format!(
                "{} ({}) names no alternative: a decision carries its question and the choices not taken. Re-record it with them through `graphos-factory-core decisions migrate --split --sorted FILE` and `{}: {{as: decision, question: …, choices: [{{id, label}}…], chosen: [id]}}`; if no reasonable engineer could have gone the other way, it is a finding (`findings add --cites …`) or a memory.md line instead",
                id,
                get_str(rec, "title").unwrap_or(""),
                id
            ),
            Some(crate::decisions::file_of(id)),
            None,
        );
    }
}

/// The decision log's own integrity after a merge (ADR 0118), which git
/// cannot see: `decision-id-duplicate` (error) for an id two records of
/// either log share, `decision-link-unresolved` and `decision-link-cycle`
/// (errors) for an `after`/`amends` that names nothing or loops, and
/// `decision-overlap` (warn) for two resolved decisions, at least one with
/// a random id, that name the same span with no `after`/`amends` path
/// between them. Two numbered records are never an overlap: their numbers
/// already order them. A log that does not load is reported elsewhere.
fn lint_decision_log(
    decisions: Option<&Value>,
    logged_findings: Option<&Value>,
    findings: &mut Findings,
) {
    let file_of: [fn(&str) -> &'static str; 2] =
        [crate::decisions::file_of, crate::findings::file_of];
    for ((doc, array), file_of) in [(decisions, "decisions"), (logged_findings, "findings")]
        .into_iter()
        .zip(file_of)
    {
        let Some(doc) = doc else { continue };
        for id in crate::record_log::duplicate_ids(doc, array) {
            findings.error(
                "decision-id-duplicate",
                format!(
                    "{} is the id of more than one record: a merge kept both. Every reference to {} now means whichever comes first; settle it as the merge conflict it is: keep one of the two, or, if both are real, remove the later one and `decisions add` it again so it gets a fresh id, then update what cites it",
                    id, id
                ),
                Some(file_of(&id)),
                None,
            );
        }
    }
    let Some(doc) = decisions else {
        return;
    };
    for (from, to) in crate::decisions::unresolved_links(doc) {
        findings.error(
            "decision-link-unresolved",
            format!(
                "{} names {} in after or amends, and the log has no {}: a merge dropped it, or the id is mistyped. Restore {} or correct the id; to drop the edge, edit {}'s record file under {}/ by hand, since no verb removes an edge",
                from, to, to, to, from, crate::record_log::DECISIONS.dir
            ),
            Some(crate::decisions::file_of(&from)),
            None,
        );
    }
    if let Some(cycle) = crate::decisions::link_cycle(doc) {
        findings.error(
            "decision-link-cycle",
            format!(
                "after and amends form a cycle, so no decision in it comes first: {}. Remove the edge that runs backwards",
                cycle.join(" -> ")
            ),
            Some(crate::decisions::file_of(&cycle[0])),
            None,
        );
    }
    for (old, new, span) in crate::decisions::unordered_overlaps(doc, true) {
        findings.warn(
            "decision-overlap",
            format!(
                "{} and {} both decide {} and neither names the other: added independently, they may disagree. Record how they relate on the newer one: `graphos-factory-core decisions link . --id {} --after {}` (it presumes the other) or `--amends {}` (it changes the other's answer there)",
                old, new, span, new, old, old
            ),
            Some(crate::decisions::file_of(&new)),
            None,
        );
    }
}

/// `stale-omit` (warn, ADR 0103): a resolved decision's `omits` entry that
/// no offered row needs any more — the schema maps every path it covers,
/// the envelope reads them while the entry says `editorial`, the source
/// offers no row at the path, or the inventory has no such operation. It
/// is `source-coverage`'s stale list, one finding per entry, naming the
/// decision, the row and the fix. A log with no resolved `omits` entry
/// costs nothing: the classifier runs only when there is one to check. A
/// workspace the classifier cannot load, or a log that does not load, is
/// reported elsewhere and yields no finding here.
fn lint_stale_omits(dir: &Path, union: Option<&Value>, findings: &mut Findings) {
    let Some(doc) = union else {
        return;
    };
    let any = crate::obligations::omits_from_doc(doc)
        .iter()
        .any(|e| e.status == "resolved");
    if !any {
        return;
    }
    let Ok(prepared) = crate::obligations::prepare(dir) else {
        return;
    };
    for stale in crate::obligations::stale_omits(&prepared) {
        findings.warn(
            "stale-omit",
            format!("{}; {}", stale.describe(), stale.fix()),
            Some(stale.file()),
            None,
        );
    }
}

/// The `affects` entries of every **resolved** decision in
/// `.factory/decisions.json`. An absent file is an empty log; an unreadable
/// or invalid one is treated the same way here — no suppression, and no
/// finding about the file, which lint reports elsewhere. An `open` decision
/// contributes nothing: a pending question is not a recorded reason.
fn resolved_decision_affects(union: Option<&Value>) -> Vec<String> {
    // Resolved decisions and current findings alike (ADR 0113 §2).
    let Some(doc) = union else {
        return vec![];
    };
    get_arr(doc, "decisions")
        .into_iter()
        .flatten()
        .filter(|d| get_str(d, "status") == Some("resolved"))
        .flat_map(|d| get_arr(d, "affects").into_iter().flatten())
        .filter_map(|a| a.as_str().map(|s| s.trim().to_string()))
        .collect()
}

/// A leaf is named as `Type.field`: the GraphQL type name and the exposed
/// (post-alias) field name, as gitea's D-0003 writes `Gitea_Issue.state`.
fn decision_names_leaf(affects: &[String], owner_type: &str, field: &str) -> bool {
    let slot = format!("{}.{}", owner_type, field);
    affects.iter().any(|a| a == &slot)
}

/// An argument is named by the prefixed root field with a parenthesised
/// argument list — `gitea_listIssues(state)`, `Query.gitea_listIssues(state)`,
/// `Mutation.gitea_createIssue(closed)` — and the list may carry several
/// arguments, as D-0003 writes `gitea_listIssues(state, type)`: an entry names
/// the slot when its list contains the argument. A root-type prefix, when
/// present, must be the field's own; a trailing `:` on an argument (the
/// `state:` spelling this repo's ADRs use in prose) is tolerated.
fn decision_names_argument(affects: &[String], root_type: &str, field: &str, arg: &str) -> bool {
    let re = re(
        &AFFECTS_ARGUMENT_RE,
        r"^(?:(Query|Mutation)\.)?([A-Za-z_][A-Za-z0-9_]*)\s*\((.*)\)$",
    );
    affects.iter().any(|a| {
        re.captures(a)
            .filter(|m| m.get(1).is_none_or(|r| r.as_str() == root_type))
            .filter(|m| &m[2] == field)
            .map(|m| {
                m[3].split(',')
                    .map(|x| x.trim().trim_end_matches(':').trim())
                    .any(|x| x == arg)
            })
            .unwrap_or(false)
    })
}
static AFFECTS_ARGUMENT_RE: std::sync::OnceLock<Regex> = std::sync::OnceLock::new();

/// One rule about a vocabulary the schema flattens (ADR 0041):
///
/// - `closed-enum-as-string`: a selected operation's argument or response
///   leaf typed `String` while the source declares an all-string `enum` of
///   two or more values, every one a valid GraphQL enum value name — exactly
///   the case schema-authoring.md § Enums says to map to a GraphQL enum, so
///   the consumer sees the vocabulary in the schema instead of guessing it.
///   A vocabulary with a value that is not a valid name (`10`, `in-progress`)
///   is not a finding: `String` is the right mapping there. Neither is a
///   one-member vocabulary: a constant discriminator (`type: service`) is not
///   a choice, and a one-member enum says nothing the field does not
///   (`closed_enum_values`). A slot the connector maps through the language
///   is the mapping's to spell and is skipped, as `wire-enum-drift` skips it.
///   A response leaf is one GraphQL field (`Type.field`) that several
///   selected operations may reach, each through its own spec property; the
///   leaf fires only when **every** reaching property declares such an enum,
///   and the message carries the union of their values — an enum built from
///   the declaring slots would fail coercion on a slot whose property
///   declares none. A reach the spec does not document — the property, or
///   an ancestor of it, is not in the shape, or the operation has no
///   response `shape_ref` — declares none. An argument is one slot. A
///   **resolved** decision in
///   `.factory/decisions.json` whose `affects` names the slot is the recorded
///   reason and suppresses the finding; an `open` one does not. A warning,
///   not an error: the schema is less descriptive than it could be, not
///   wrong. One finding per argument and one per leaf, however many
///   operations reach it.
#[allow(clippy::too_many_arguments)]
fn lint_closed_enums(
    union: Option<&Value>,
    sdl: &str,
    schema_file: &str,
    workspace: &Value,
    selection: Option<&Value>,
    inventory: Option<&Value>,
    findings: &mut Findings,
) {
    let (selection, inventory) = match (selection, inventory) {
        (Some(s), Some(i)) => (s, i),
        _ => return,
    };
    let shapes = match get(inventory, "shapes").and_then(Value::as_object) {
        Some(s) => s,
        None => return,
    };
    let field_prefix = get_str(workspace, "field_prefix").unwrap_or("");
    let type_prefix = get_str(workspace, "type_prefix").unwrap_or("");
    let operations: HashMap<&str, &Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .filter_map(|o| get_str(o, "key").map(|k| (k, o)))
        .collect();
    let affects = resolved_decision_affects(union);
    let mut index = crate::sdl_index::SdlIndex::new(sdl);
    let query_args = root_field_args(sdl, "Query");
    let mutation_args = root_field_args(sdl, "Mutation");
    let return_re = Regex::new(r"\)\s*:\s*([\[\]!A-Za-z0-9_]+)").unwrap();
    let selection_block = Regex::new(r#"(?s)\bselection\s*:\s*"""(.*?)""""#).unwrap();
    let selection_line = Regex::new(r#"\bselection\s*:\s*"([^"]*)""#).unwrap();
    let http_path_re =
        Regex::new(r#"\b(?:GET|POST|PUT|PATCH|DELETE|HEAD)\s*:\s*"([^"]+)""#).unwrap();
    // One finding per argument slot.
    let mut reported: HashSet<String> = HashSet::new();
    // Every `String` leaf the selections reach, keyed `Type.field` in
    // first-seen order, with what each reaching spec property declares. A
    // shared type is declared once and fixed once, so a leaf is judged once,
    // after every included operation has been walked — and judged on all of
    // its slots, not on the first one that carries an enum.
    let mut leaves: Vec<(String, LeafSlots)> = Vec::new();
    let mut leaf_index: HashMap<String, usize> = HashMap::new();
    // The proposed declaration is the paste target, so it carries every
    // value; only the summary list in the message is capped (`list_capped`).
    let advice = |wire_name: &str, values: &[String], slot: &str| {
        format!(
            "declare `enum {}_{} {{ {} }}` in wire casing (naming.md), or record the reason: a judgement with `graphos-factory-core decisions add . --question … --choice … --resolved … --affects \"{}\"`, a fact a reference settles with `graphos-factory-core findings add . --title … --body … --cites … --affects \"{}\"` (schema-authoring.md § Enums)",
            type_prefix,
            pascal(wire_name),
            values.join(" "),
            slot,
            slot
        )
    };

    for (key, entry) in get_obj(selection, "operations").into_iter().flatten() {
        if !truthy(get(entry, "include")) {
            continue;
        }
        let name = match get(entry, "graphql").and_then(|g| get_str(g, "name")) {
            Some(n) => n,
            None => continue,
        };
        let root = get(entry, "graphql")
            .and_then(|g| get_str(g, "root"))
            .unwrap_or("query");
        let root_type = if root == "query" { "Query" } else { "Mutation" };
        let field = format!("{}_{}", field_prefix, name);
        let field_text = match root_field_text(sdl, root_type, &field) {
            Some(t) => t,
            None => continue,
        };
        let op = match operations.get(key.as_str()) {
            Some(o) => *o,
            None => continue,
        };

        // ── Arguments: the vocabulary the caller has to guess ─────────────
        let wire = wiring(&field_text);
        let body = body_mapping(&field_text);
        let http_path = http_path_re
            .captures(&field_text)
            .map(|m| m[1].to_string())
            .unwrap_or_default();
        let args = if root == "query" {
            &query_args
        } else {
            &mutation_args
        }
        .iter()
        .find(|(f, _)| *f == field)
        .map(|(_, a)| a.clone())
        .unwrap_or_default();

        for arg in &args {
            if crate::sdl_index::base_type(&arg.type_) != "String" {
                continue;
            }
            // (where the enum lives, the wire name, the spec's values)
            let mut source: Option<(String, String, Vec<String>)> = None;
            if let Some((_, k)) = wire.query_keys.iter().find(|(a, _)| a == &arg.name) {
                if slot_expression(&field_text, "queryParams", k)
                    .map(|e| uses_mapping_language(&e))
                    .unwrap_or(false)
                {
                    continue;
                }
                let base_key = k.trim_end_matches("[]");
                if let Some(p) = get_arr(op, "parameters").into_iter().flatten().find(|p| {
                    get_str(p, "in") == Some("query")
                        && (get_str(p, "name") == Some(base_key) || get_str(p, "name") == Some(k))
                }) {
                    source = param_enum(p).map(|v| {
                        (
                            format!("query parameter `{}`", base_key),
                            base_key.to_string(),
                            v,
                        )
                    });
                }
            }
            if source.is_none() && http_path.contains(&format!("{{$args.{}}}", arg.name)) {
                if let Some(p) = get_arr(op, "parameters").into_iter().flatten().find(|p| {
                    get_str(p, "in") == Some("path") && get_str(p, "name") == Some(&arg.name)
                }) {
                    source = param_enum(p).map(|v| {
                        (
                            format!("path parameter `{}`", arg.name),
                            arg.name.clone(),
                            v,
                        )
                    });
                }
            } else if source.is_none() && http_path.contains(&format!("{{$args.{}->", arg.name)) {
                continue;
            }
            if source.is_none() {
                if let BodyMapping::Flat(map) = &body {
                    if let Some((_, k)) = map.iter().find(|(a, _)| a == &arg.name) {
                        if let Some(prop) = get(op, "request_body")
                            .and_then(|r| get_str(r, "shape_ref"))
                            .and_then(|sr| {
                                let reference =
                                    crate::json::object(vec![("$ref", Value::from(sr))]);
                                property_of(&reference, shapes, k, 0).cloned()
                            })
                        {
                            source = spec_enum(&prop, shapes)
                                .map(|v| (format!("body key `{}`", k), k.clone(), v));
                        }
                    }
                }
            }
            let (where_, wire_name, values) = match source {
                Some(x) => x,
                None => continue,
            };
            if !closed_enum_values(&values) {
                continue;
            }
            if decision_names_argument(&affects, root_type, &field, &arg.name) {
                continue;
            }
            if reported.insert(format!("arg:{}.{}", field, arg.name)) {
                let slot = format!("{}({})", field, arg.name);
                findings.warn(
                    "closed-enum-as-string",
                    format!(
                        "{}: argument `{}` of `{}` is String, but the source's {} is a closed enum of {}; {}",
                        key,
                        arg.name,
                        field,
                        where_,
                        list_capped(&values, 8),
                        advice(&wire_name, &values, &slot)
                    ),
                    Some(schema_file),
                    field_line(sdl, root_type, &field),
                );
            }
        }

        // ── Response leaves: the vocabulary the consumer cannot see ───────
        // An operation with no response `shape_ref` still fills the types
        // its selection names, from nothing the spec documents: its leaves
        // are walked with no shape and recorded as reaches that declare
        // nothing, so a shared leaf it reaches stays quiet.
        let response_shape = get(op, "response")
            .and_then(|r| get_str(r, "shape_ref"))
            .map(|sr| crate::json::object(vec![("$ref", Value::from(sr))]));
        let blanked = crate::graphql::blank(&field_text);
        let return_type = return_re
            .captures(&blanked)
            .map(|m| crate::sdl_index::base_type(&m[1]))
            .or_else(|| {
                Regex::new(&format!(
                    r"\b{}\s*:\s*([\[\]!A-Za-z0-9_]+)",
                    regex::escape(&field)
                ))
                .unwrap()
                .captures(&blanked)
                .map(|m| crate::sdl_index::base_type(&m[1]))
            });
        let return_type = match return_type {
            Some(t) => t,
            None => continue,
        };
        let selection_text = selection_block
            .captures(&field_text)
            .map(|m| m[1].to_string())
            .or_else(|| {
                selection_line
                    .captures(&field_text)
                    .map(|m| m[1].to_string())
            })
            .unwrap_or_default();
        let nodes = crate::reconcile::parse_selection(&selection_text);
        let mut hits: Vec<LeafHit> = Vec::new();
        walk_closed_enums(
            &nodes,
            response_shape.as_ref(),
            shapes,
            &return_type,
            "",
            &field,
            &mut index,
            &mut hits,
        );
        for hit in hits {
            let owner_type = if hit.owner.is_empty() {
                root_type.to_string()
            } else {
                hit.owner
            };
            let slot = format!("{}.{}", owner_type, hit.field);
            let i = match leaf_index.get(&slot) {
                Some(i) => *i,
                None => {
                    leaves.push((
                        slot.clone(),
                        LeafSlots {
                            owner_type,
                            field: hit.field,
                            reached_by: Vec::new(),
                            values: Vec::new(),
                            every_slot_declares: true,
                        },
                    ));
                    leaf_index.insert(slot, leaves.len() - 1);
                    leaves.len() - 1
                }
            };
            let agg = &mut leaves[i].1;
            let reach = (key.to_string(), hit.path);
            if !agg.reached_by.contains(&reach) {
                agg.reached_by.push(reach);
            }
            match hit.values {
                Some(values) => {
                    for v in values {
                        if !agg.values.contains(&v) {
                            agg.values.push(v);
                        }
                    }
                }
                None => agg.every_slot_declares = false,
            }
        }
    }

    // ── Leaves, judged on every slot that reaches them ────────────────────
    for (slot, agg) in leaves {
        if !agg.every_slot_declares {
            continue;
        }
        if decision_names_leaf(&affects, &agg.owner_type, &agg.field) {
            continue;
        }
        let (first_key, first_path) = &agg.reached_by[0];
        let wire_name = first_path
            .rsplit('.')
            .next()
            .unwrap_or(&agg.field)
            .to_string();
        let where_ = if agg.reached_by.len() == 1 {
            format!(
                "the source property at `{}` is a closed enum of {}",
                first_path,
                list_capped(&agg.values, 8)
            )
        } else {
            let shown = agg
                .reached_by
                .iter()
                .take(4)
                .map(|(k, p)| format!("`{}` via {}", p, k))
                .collect::<Vec<_>>()
                .join(", ");
            let more = agg.reached_by.len().saturating_sub(4);
            format!(
                "every source property reaching it ({}{}) is a closed enum; together they enumerate {}",
                shown,
                if more > 0 {
                    format!(" (+{} more)", more)
                } else {
                    String::new()
                },
                list_capped(&agg.values, 8)
            )
        };
        findings.warn(
            "closed-enum-as-string",
            format!(
                "{}: `{}` is String, but {}; {}",
                first_key,
                slot,
                where_,
                advice(&wire_name, &agg.values, &slot)
            ),
            Some(schema_file),
            field_line(sdl, &agg.owner_type, &agg.field),
        );
    }
}

/// One `String`-typed response leaf as one selected operation reaches it:
/// the GraphQL owner type (empty for the root field itself), the exposed
/// field, the wire path of the spec property it is selected from, and that
/// property's closed vocabulary — `None` when the property declares no
/// `enum`, declares one `closed_enum_values` rejects, or is not in the shape,
/// which includes the shape itself being unknown: an ancestor property the
/// spec does not document, or an operation with no response `shape_ref`.
/// Every reach is recorded, documented or not; a leaf's silence must count
/// the slots the spec says nothing about.
struct LeafHit {
    owner: String,
    field: String,
    path: String,
    values: Option<Vec<String>>,
}

/// Everything the included operations say about one `Type.field` leaf.
struct LeafSlots {
    owner_type: String,
    field: String,
    /// `(operation key, wire path)` of every reach, first-seen, deduplicated.
    /// The first is the finding's prefix and the property it names.
    reached_by: Vec<(String, String)>,
    /// The union of the reaching vocabularies, first-seen, deduplicated.
    values: Vec<String>,
    /// False once any reaching property declares no closed enum: an enum
    /// built from the others would fail coercion on that slot.
    every_slot_declares: bool,
}

/// Selection nodes beside the response shape and the GraphQL type they land
/// in; **every** `String`-typed leaf is collected as a `LeafHit`, with its
/// spec property's closed, GraphQL-spellable `enum` when it has one and
/// `None` when it does not — the caller needs both, because a leaf several
/// operations reach fires only when every reaching property declares one.
/// `spec_enum` reads an array property's enum off its `items`, so a
/// `[String]` leaf over enumerated items is collected too. A leaf the
/// connector maps is skipped, and `n.opaque` is the whole test — see
/// `walk_enums`. `root_field` names the root field, which owns a `$.x` leaf
/// selected straight into the root type. `shape` is `None` when nothing
/// documents the nodes' parent — an ancestor property absent from its shape,
/// or an operation with no response `shape_ref` — and the walk still
/// descends by SDL type (`SdlIndex::field_type`), so every `String` leaf
/// under it is recorded with `values: None`; returning early there would
/// drop the reach and judge the leaf on its documented slots alone.
#[allow(clippy::too_many_arguments)]
fn walk_closed_enums(
    nodes: &[crate::reconcile::Node],
    shape: Option<&Value>,
    shapes: &Object,
    gql_type: &str,
    prefix: &str,
    root_field: &str,
    index: &mut crate::sdl_index::SdlIndex,
    hits: &mut Vec<LeafHit>,
) {
    for n in nodes {
        if n.opaque {
            continue;
        }
        let key = match &n.key {
            Some(k) => k,
            None => continue,
        };
        let path = if prefix.is_empty() {
            key.join(".")
        } else {
            format!("{}.{}", prefix, key.join("."))
        };
        if n.rooted && n.alias.is_none() {
            match &n.children {
                // `$.data { … }`: the children live in the same GraphQL type.
                Some(children) => {
                    let inner = shape.and_then(|s| shape_at(s, shapes, key, true));
                    walk_closed_enums(
                        children, inner, shapes, gql_type, &path, root_field, index, hits,
                    );
                }
                // `$.status` alone: the root field itself is the leaf.
                None => {
                    if gql_type != "String" {
                        continue;
                    }
                    let values = shape
                        .and_then(|s| shape_at(s, shapes, key, false))
                        .and_then(|prop| spec_enum(prop, shapes))
                        .filter(|v| closed_enum_values(v));
                    hits.push(LeafHit {
                        owner: String::new(),
                        field: root_field.to_string(),
                        path,
                        values,
                    });
                }
            }
            continue;
        }
        let exposed = match n.alias.clone().or_else(|| key.last().cloned()) {
            Some(x) => x,
            None => continue,
        };
        let ftype = match index.field_type(gql_type, &exposed) {
            Some(t) => crate::sdl_index::base_type(&t),
            None => continue,
        };
        match &n.children {
            Some(children) => {
                let inner = shape.and_then(|s| shape_at(s, shapes, key, true));
                walk_closed_enums(
                    children, inner, shapes, &ftype, &path, root_field, index, hits,
                );
            }
            None => {
                if ftype != "String" {
                    continue;
                }
                let values = shape
                    .and_then(|s| shape_at(s, shapes, key, false))
                    .and_then(|prop| spec_enum(prop, shapes))
                    .filter(|v| closed_enum_values(v));
                hits.push(LeafHit {
                    owner: gql_type.to_string(),
                    field: exposed,
                    path,
                    values,
                });
            }
        }
    }
}

fn lint_evidence(
    dir: &Path,
    selection: Option<&Value>,
    findings: &mut Findings,
    schemas_dir: Option<&Path>,
) {
    let rel = Path::new(".factory").join("evidence").join("latest.json");
    // "Something is there" is the question here: a refused (symlinked)
    // evidence file is not a missing one, and `load_json_file` reports it.
    let present = !matches!(
        crate::factory_io::symlink_metadata(dir, &rel.to_string_lossy()),
        Ok(None)
    );
    if !present {
        findings.warn(
            "no-evidence",
            "no .factory/evidence/latest.json — the workspace has not been validated",
            Some("evidence"),
            None,
        );
        return;
    }
    let evidence = match load_json_file(dir, &rel.to_string_lossy(), findings) {
        Some(e) => e,
        None => return,
    };
    contract_check(
        &evidence,
        "evidence.schema.json",
        ".factory/evidence/latest.json",
        findings,
        schemas_dir,
    );
    for (name, layer) in get_obj(&evidence, "layers").into_iter().flatten() {
        if get_str(layer, "status") != Some("pass") && get(layer, "reason").is_none() {
            findings.error(
                "unexplained-status",
                format!(
                    "evidence layer \"{}\" is {} with no reason",
                    name,
                    get_str(layer, "status").unwrap_or("undefined")
                ),
                Some("evidence"),
                None,
            );
        }
    }
    for (key, entry) in selection
        .and_then(|s| get_obj(s, "operations"))
        .into_iter()
        .flatten()
    {
        if !truthy(get(entry, "include")) {
            continue;
        }
        if get(&evidence, "operations")
            .and_then(|o| get(o, key))
            .is_none()
        {
            findings.error(
                "unproven-operation",
                format!(
                    "{} is included but has no entry in evidence/latest.json",
                    key
                ),
                Some("evidence"),
                None,
            );
        }
    }
    // When the live layer ran, every selected operation is either covered by
    // a case, or excluded with a reason. An anonymous n/a is neither.
    let live_ran = matches!(
        get(&evidence, "layers")
            .and_then(|l| get(l, "live"))
            .and_then(|l| get_str(l, "status")),
        Some("pass") | Some("fail")
    );
    if live_ran {
        for (key, entry) in selection
            .and_then(|s| get_obj(s, "operations"))
            .into_iter()
            .flatten()
        {
            if !truthy(get(entry, "include")) {
                continue;
            }
            let live = get(&evidence, "operations")
                .and_then(|o| get(o, key))
                .and_then(|e| get_str(e, "live"));
            if live == Some("n/a") {
                findings.warn(
                    "live-unaccounted",
                    format!(
                        "{} is selected but the live layer neither ran a case for it nor excluded it — add a case to tests/live.yaml, or an `exclusions:` entry with the reason it cannot be tested live",
                        key
                    ),
                    Some("live.yaml"),
                    None,
                );
            }
        }
    }
}

/// tests/live.yaml itself: every case has its document, every exclusion
/// names one selected operation or one relationship field and gives a
/// reason, and every relationship field is a live case or a `field:`
/// exclusion.
fn lint_live(
    dir: &Path,
    sdl: &str,
    schema_file: &str,
    selection: Option<&Value>,
    findings: &mut Findings,
) {
    let file = dir.join("tests").join("live.yaml");
    if !file.exists() {
        return;
    }
    let live = match crate::yaml::parse_file(&file) {
        Ok(l) => l,
        Err(e) => {
            findings.error(
                "unreadable-file",
                format!("tests/live.yaml: {}", e),
                Some("live.yaml"),
                None,
            );
            return;
        }
    };
    for case in get_arr(&live, "cases").into_iter().flatten() {
        if let Some(name) = get_str(case, "name") {
            if !dir
                .join("tests/live")
                .join(format!("{}.graphql", name))
                .exists()
            {
                findings.error(
                    "live-case-missing-doc",
                    format!("live case {} has no tests/live/{}.graphql", name, name),
                    Some("live.yaml"),
                    None,
                );
            }
        }
    }
    let included: Vec<&str> = selection
        .and_then(|s| get_obj(s, "operations"))
        .into_iter()
        .flatten()
        .filter(|(_, e)| truthy(get(e, "include")))
        .map(|(k, _)| k.as_str())
        .collect();
    // A relationship field has no evidence row (ADR 0094), so live
    // accounts for one by name: `field: "<Type>.<field>"` (ADR 0106).
    let links: Vec<String> = crate::reconcile::link_connectors(sdl)
        .into_iter()
        .map(|lc| format!("{}.{}", lc.type_name, lc.field))
        .collect();
    let mut excluded_fields: HashSet<String> = HashSet::new();
    for x in get_arr(&live, "exclusions").into_iter().flatten() {
        let op = get_str(x, "operation").unwrap_or("").trim();
        let field = get_str(x, "field").unwrap_or("").trim();
        if op.is_empty() == field.is_empty() {
            findings.error(
                "live-exclusion-malformed",
                format!(
                    "live exclusion {} must name exactly one of `operation:` (a selected operation) or `field:` (a relationship field, \"<Type>.<field>\") — live.sh fails on it",
                    match (op.is_empty(), field.is_empty()) {
                        (true, true) => "(unnamed)".to_string(),
                        _ => format!("{} / {}", op, field),
                    }
                ),
                Some("live.yaml"),
                None,
            );
        }
        let name = if op.is_empty() { field } else { op };
        if get_str(x, "reason").map(str::trim).unwrap_or("").is_empty() {
            findings.error(
                "live-exclusion-unreasoned",
                format!(
                    "live exclusion {} gives no reason — say why the {} cannot be tested live",
                    if name.is_empty() { "(unnamed)" } else { name },
                    if op.is_empty() && !field.is_empty() {
                        "field"
                    } else {
                        "operation"
                    }
                ),
                Some("live.yaml"),
                None,
            );
        }
        if !op.is_empty() && !included.contains(&op) {
            findings.warn(
                "live-exclusion-unknown",
                format!(
                    "live exclusion {} is not a selected operation (selection.yaml include: true)",
                    op
                ),
                Some("live.yaml"),
                None,
            );
        }
        if op.is_empty() && !field.is_empty() {
            if links.iter().any(|l| l == field) {
                excluded_fields.insert(field.to_string());
            } else {
                findings.warn(
                    "live-exclusion-unknown",
                    format!(
                        "live exclusion field {} is not a relationship field of the schema (a field-level @connect reading {{$this.<fk>}})",
                        field
                    ),
                    Some("live.yaml"),
                    None,
                );
            }
        }
    }
    // `link-live-unaccounted` (ADR 0106): `live-unaccounted` reads evidence
    // rows, and a relationship field has none, so every one is a live case
    // that selects it on its host type or a `field:` exclusion. An
    // operation exclusion whose reason names the field does not count: a
    // reason is prose, and this is what replaces it.
    if links.is_empty() {
        return;
    }
    let schema = assume_valid_schema(sdl, schema_file);
    let mut selected: HashSet<(String, String)> = HashSet::new();
    for case in get_arr(&live, "cases").into_iter().flatten() {
        let Some(name) = get_str(case, "name") else {
            continue;
        };
        let doc = dir.join("tests/live").join(format!("{}.graphql", name));
        if doc.exists() {
            selected.extend(selected_fields(
                &schema,
                &read(&doc),
                &format!("tests/live/{}.graphql", name),
            ));
        }
    }
    for lc in crate::reconcile::link_connectors(sdl) {
        let coordinate = format!("{}.{}", lc.type_name, lc.field);
        if selected.contains(&(lc.type_name.clone(), lc.field.clone()))
            || excluded_fields.contains(&coordinate)
        {
            continue;
        }
        findings.warn(
            "link-live-unaccounted",
            format!(
                "{} is a relationship field no live case selects and no exclusion names — add a tests/live case that selects {} on {}, or an `exclusions:` entry `field: \"{}\"` with the reason it cannot be tested live; until then it has no live evidence",
                coordinate, lc.field, lc.type_name, coordinate
            ),
            Some("live.yaml"),
            None,
        );
    }
}

/// Pinned description documents: the sources lock validates, every document
/// has an untouched upstream copy, the working copy is acknowledged in the
/// applied lock, and the recorded patches reproduce it.
/// The inventory against what its inputs build (ADR 0138). It is a reading
/// of the sources, never edited, so every operation in it should come back
/// from `inventory build`: one no input declares was written by hand, one
/// the inputs declare that it lacks means it is stale, and one that differs
/// carries an edit no input records. Shapes are not compared, because
/// `infer --update-inventory` fills them from samples after the build. A
/// workspace with no sources lock at all is `lint_sources`'s to report.
fn lint_inventory_rebuild(dir: &Path, inventory: &Value, findings: &mut Findings) {
    const RULE: &str = "inventory-not-rebuildable";
    let file = Some(".factory/inventory.json");
    let ops: Vec<&Value> = get_arr(inventory, "operations")
        .into_iter()
        .flatten()
        .collect();
    if ops.is_empty() || !matches!(crate::sources::read_sources_lock(dir), Ok(Some(_))) {
        return;
    }
    let fix = "rebuild it (graphos-factory-core inventory build); move operations no input declares into a supplement (graphos-factory-core inventory supplement), a correction to a pinned document into a patch (codify --source), and a judgement into selection.yaml";
    let inputs = match crate::inputs::workspace_inputs(dir) {
        Ok(i) => i,
        Err(e) => {
            findings.warn(
                RULE,
                format!("the inventory's inputs cannot be read: {}", e),
                file,
                None,
            );
            return;
        }
    };
    if inputs.is_empty() {
        findings.warn(
            RULE,
            format!(
                "{} operation(s), and {} names no document or supplement that builds them: the inventory was written by hand and nothing can rebuild it — {}",
                ops.len(),
                crate::sources::SOURCES_LOCK,
                fix
            ),
            file,
            None,
        );
        return;
    }
    let built = match crate::inputs::build_workspace(dir, None) {
        Ok((b, _)) => b.inventory,
        Err(e) => {
            findings.warn(
                RULE,
                format!("the inventory's inputs do not build: {}", e),
                file,
                None,
            );
            return;
        }
    };
    let index = |v: Vec<&Value>| -> std::collections::BTreeMap<String, Value> {
        v.into_iter()
            .filter_map(|o| Some((get_str(o, "key")?.to_string(), o.clone())))
            .collect()
    };
    let current = index(ops);
    let rebuilt = index(
        get_arr(&built, "operations")
            .into_iter()
            .flatten()
            .collect(),
    );
    let unsourced: Vec<&String> = current
        .keys()
        .filter(|k| !rebuilt.contains_key(*k))
        .collect();
    let missing: Vec<&String> = rebuilt
        .keys()
        .filter(|k| !current.contains_key(*k))
        .collect();
    let mut fields: std::collections::BTreeMap<String, usize> = Default::default();
    let mut differing: Vec<&String> = Vec::new();
    for (k, op) in &current {
        let Some(other) = rebuilt.get(k) else {
            continue;
        };
        if crate::json::deep_equal(op, other) {
            continue;
        }
        differing.push(k);
        let names: std::collections::BTreeSet<&String> = op
            .as_object()
            .into_iter()
            .flat_map(|o| o.keys())
            .chain(other.as_object().into_iter().flat_map(|o| o.keys()))
            .collect();
        for f in names {
            if !crate::json::deep_equal(
                op.get(f.as_str()).unwrap_or(&Value::Null),
                other.get(f.as_str()).unwrap_or(&Value::Null),
            ) {
                *fields.entry(f.clone()).or_default() += 1;
            }
        }
    }
    if unsourced.is_empty() && missing.is_empty() && differing.is_empty() {
        return;
    }
    let first = |v: &[&String]| {
        let shown: Vec<&str> = v.iter().take(5).map(|s| s.as_str()).collect();
        format!(
            "{}{}",
            shown.join(", "),
            if v.len() > 5 { ", …" } else { "" }
        )
    };
    let mut parts: Vec<String> = Vec::new();
    if !unsourced.is_empty() {
        parts.push(format!(
            "{} operation(s) no input declares ({})",
            unsourced.len(),
            first(&unsourced)
        ));
    }
    if !missing.is_empty() {
        parts.push(format!(
            "{} the inputs declare that it lacks ({})",
            missing.len(),
            first(&missing)
        ));
    }
    if !differing.is_empty() {
        let by: Vec<String> = fields.iter().map(|(f, n)| format!("{} {}", f, n)).collect();
        parts.push(format!(
            "{} that differ from what the inputs build ({}; by field: {})",
            differing.len(),
            first(&differing),
            by.join(", ")
        ));
    }
    findings.warn(
        RULE,
        format!(
            "inventory.json is not what its inputs build: {} — {}",
            parts.join("; "),
            fix
        ),
        file,
        None,
    );
}

fn lint_sources(dir: &Path, findings: &mut Findings, schemas_dir: Option<&Path>) {
    // Acknowledged but no longer pinned — including when the whole sources
    // lock is gone — is reported before anything else can return early. An
    // unreadable lock is its own error, not an unpinning.
    let lock_read = crate::sources::read_sources_lock(dir);
    let applied_for_unpinned = if lock_read.is_ok() {
        crate::spans::read_lock(dir).ok().flatten()
    } else {
        None
    };
    for path in crate::sources::unpinned(dir, applied_for_unpinned.as_ref()) {
        findings.error(
            "source-unpinned",
            format!(
                "applied.lock.yaml acknowledges {} but {} no longer pins it — restore the entry, or run `graphos-factory-core lock` to stop watching it on purpose",
                path,
                crate::sources::SOURCES_LOCK
            ),
            Some("sources.lock.yaml"),
            None,
        );
    }
    let lock = match lock_read {
        Ok(Some(l)) => l,
        Ok(None) => return,
        Err(e) => {
            findings.error("unreadable-file", e, Some("sources.lock.yaml"), None);
            return;
        }
    };
    contract_check(
        &lock,
        "sources-lock.schema.json",
        ".factory/sources.lock.yaml",
        findings,
        schemas_dir,
    );
    let applied = crate::spans::read_lock(dir).ok().flatten();
    let file = Some("sources.lock.yaml");
    let entries = crate::sources::document_entries(&lock);
    for entry in &entries {
        let s = crate::sources::status(dir, entry, &entries, applied.as_ref());
        if s.outside_workspace {
            findings.error(
                "source-outside-workspace",
                format!(
                    "{}: {} (nothing is read or written through this entry)",
                    s.path,
                    s.misplaced_reason
                        .as_deref()
                        .unwrap_or("a copy is outside the workspace")
                ),
                file,
                None,
            );
            continue;
        }
        if s.upstream_misplaced {
            let duplicate = matches!(s.fault, Some(crate::sources::NotFollowed::DuplicatePath));
            findings.error(
                if duplicate {
                    "source-duplicate-path"
                } else {
                    "source-upstream-misplaced"
                },
                format!(
                    "{}: {}{}; fix it in {} (nothing is read or written through this entry)",
                    s.path,
                    s.misplaced_reason
                        .as_deref()
                        .unwrap_or("its vendor copy is misplaced"),
                    if s.fault
                        .as_ref()
                        .map(|f| f.is_overwrite_hazard())
                        .unwrap_or(true)
                    {
                        " — so `sources pin --force` can never overwrite a document someone edits"
                    } else {
                        ""
                    },
                    crate::sources::SOURCES_LOCK
                ),
                file,
                None,
            );
            continue;
        }
        if !s.working_present {
            findings.error(
                "source-missing",
                format!(
                    "{} pins {} but the file is missing",
                    crate::sources::SOURCES_LOCK,
                    s.path
                ),
                file,
                None,
            );
            continue;
        }
        if let Some(e) = &s.working_error {
            findings.error(
                "source-unreadable",
                format!(
                    "{} does not parse ({}) — the working copy of a pinned document must be valid JSON or YAML; fix it or restore it from {}",
                    s.path, e, s.upstream
                ),
                file,
                None,
            );
            continue;
        }
        if s.detected_kind != "unknown" && s.detected_kind != s.declared_kind {
            findings.warn(
                "source-kind-mismatch",
                format!(
                    "{} is recorded as kind {} but declares {} {} — openapi is OpenAPI 3.x, swagger is Swagger 2.0 (graphos-factory-core sources pin fixes the record)",
                    s.path, s.declared_kind, s.detected_kind, s.detected_version
                ),
                file,
                None,
            );
        }
        if !s.upstream_present {
            let remedy = if entry.upstream_sha256.is_some() {
                format!(
                    "restore it from git (git checkout -- {}) — it is never re-created from the working copy; to replace the baseline on purpose, `graphos-factory-core sources pin --path {} --force --reason R`",
                    s.upstream, s.path
                )
            } else {
                format!(
                    "run `graphos-factory-core sources pin --path {}` so hand edits to the spec can be detected and codified",
                    s.path
                )
            };
            findings.error(
                "source-upstream-missing",
                format!(
                    "{} has no pristine upstream copy at {} — {}",
                    s.path, s.upstream, remedy
                ),
                file,
                None,
            );
            continue;
        }
        if let Some(e) = &s.upstream_error {
            findings.error(
                "source-upstream-unreadable",
                format!(
                    "{} cannot be read ({}) — nothing about {} can be checked against it; restore it (git checkout -- {}), or replace the baseline on purpose with `graphos-factory-core sources pin --path {} --force --reason R`",
                    s.upstream, e, s.path, s.upstream, s.path
                ),
                file,
                None,
            );
            continue;
        }
        if s.upstream_ok == Some(false) {
            findings.error(
                "source-upstream-modified",
                format!(
                    "{} no longer hashes to upstream_sha256 — the vendor copy is never edited; restore it and edit the working copy {} instead",
                    s.upstream, s.path
                ),
                file,
                None,
            );
        }
        if applied.is_some() {
            if s.unlocked() {
                findings.error(
                    "source-unlocked",
                    format!(
                        "{} is pinned but applied.lock.yaml has no hash for it — run `graphos-factory-core lock`{} to acknowledge the working copy",
                        s.path,
                        if s.upstream_ok == Some(false) {
                            ""
                        } else {
                            " (or sources pin)"
                        }
                    ),
                    file,
                    None,
                );
            } else if s.hand_edit {
                // `lock` is a remedy only while the recorded patches still
                // describe the working copy; otherwise only codify gets green.
                let alternative = if s.patches_ok == Some(false) {
                    ""
                } else {
                    " or, if the agent wrote it and the patches still describe it, acknowledge it (graphos-factory-core lock)"
                };
                let remedy = if s.upstream_ok == Some(false) {
                    format!(
                        "restore {} first — codify refuses while the vendor copy is modified — then codify it (graphos-factory-core codify --source {} --reason …)",
                        s.upstream, s.path
                    )
                } else {
                    format!(
                        "codify it (graphos-factory-core codify --source {} --reason …){}",
                        s.path, alternative
                    )
                };
                findings.error(
                    "unacknowledged-source-edit",
                    format!("{} changed since applied.lock.yaml — {}", s.path, remedy),
                    file,
                    None,
                );
            }
        }
        if s.patches_ok == Some(false) {
            findings.error(
                "source-patches-stale",
                format!(
                    "applying the {} recorded patch{} to {} does not give {}{} — {}",
                    s.patches,
                    if s.patches == 1 { "" } else { "es" },
                    s.upstream,
                    s.path,
                    s.patches_error
                        .as_ref()
                        .map(|e| format!(" ({})", e))
                        .unwrap_or_default(),
                    if s.upstream_ok == Some(false) {
                        format!("restore {} first (codify refuses while the vendor copy is modified), then `graphos-factory-core codify --source {} --reason …` if a difference remains", s.upstream, s.path)
                    } else {
                        format!("run `graphos-factory-core codify --source {} --reason …` to record the difference as it is", s.path)
                    }
                ),
                file,
                None,
            );
        }
    }
}

pub struct LintOptions<'a> {
    pub schemas_dir: Option<&'a Path>,
    pub skip_evidence: bool,
    /// The target whose placeholders, rule overrides and rules apply.
    pub target: &'a crate::target::Target,
}

fn summarize(findings: Findings) -> LintResult {
    let errors = findings
        .items
        .iter()
        .filter(|f| f.severity == "error")
        .count();
    let warnings = findings
        .items
        .iter()
        .filter(|f| f.severity == "warn")
        .count();
    LintResult {
        findings: findings.items,
        errors,
        warnings,
    }
}

/// Field-name suffixes ADR 0078 treats as a credential bare or as any
/// compound's last word, unconditionally: there is no established
/// non-credential meaning for a `*Secret`/`*Password`/`*Passcode`/`*Pin`/
/// `*Credential(s)` field the way there is for `*Token`/`*Key` (a pagination
/// cursor, a lookup key) -- see `TOKEN_PREFIXES` and `SECRET_KEY_PREFIXES`.
/// `pin` (a dial-in PIN) was added after measuring Google Calendar's
/// `EntryPoint.pin`, alongside its `passcode`/`password` siblings on the
/// same type.
const SECRET_FIELD_UNCONDITIONAL_SUFFIXES: [&str; 6] = [
    "secret",
    "password",
    "passcode",
    "pin",
    "credential",
    "credentials",
];

/// The word immediately before a `*Token`/`*_token` compound that makes it a
/// credential rather than an opaque cursor. Measured need: a vendor's
/// `syncToken` (a sync cursor, not a credential) fires on 40 list-response
/// types under a first, unqualified "any compound ending in token" rule --
/// the same shape as `pageToken`/`nextToken`/`continuationToken`/
/// `cursorToken` on other vendors' pagination. A bare `token` still fires
/// (ADR 0078 names it directly); only a compound is gated.
const TOKEN_PREFIXES: [&str; 11] = [
    "access", "refresh", "api", "auth", "bearer", "session", "client", "oauth", "id", "secret",
    "signing",
];

/// The word immediately before a `*Key`/`*_key` compound that makes it a
/// credential, not a business identifier. Measured need: PagerDuty's
/// `Incident.incidentKey` (a deduplication key, ADR 0024's own vocabulary)
/// fired on a first, unqualified "any compound ending in key" rule --
/// `incidentKey`, like a database's `primaryKey`/`sortKey`/`partitionKey`, is
/// exactly the false-positive shape a wide-open suffix invites. `public` is
/// deliberately absent: a public key is not a secret by definition. A bare
/// `key` never fires (ADR 0078 does not name it alone) -- see
/// `looks_like_secret_field`.
const SECRET_KEY_PREFIXES: [&str; 7] = [
    "api",
    "private",
    "client",
    "secret",
    "encryption",
    "signing",
    "access",
];

/// True when a field's name marks it a credential under ADR 0078's
/// vocabulary: `secret`/`password`/`passcode`/`credential(s)` bare or as a
/// compound's last word (`webhookSigningSecret`, `client_secret`)
/// unconditionally; `token` bare, or as a compound gated on a
/// credential-shaped word immediately before it (`accessToken`,
/// `refresh_token`) -- a compound like `syncToken`/`pageToken`/
/// `continuationToken` is a pagination cursor, not a credential; `key`
/// never bare, only as a compound gated the same way (`apiKey`,
/// `private_key`) -- a bare `key`, or a compound like
/// `incidentKey`/`primaryKey`/`sortKey`, is a business identifier. Both
/// gates were added after measuring: an ungated `*_token`/`*_key` suffix
/// false-positived on real pilot and artifacts-main fields (see
/// `TOKEN_PREFIXES`, `SECRET_KEY_PREFIXES`, ADR 0078's Consequences).
fn looks_like_secret_field(name: &str) -> bool {
    let tokens = name_words(name);
    let Some(last) = tokens.last().map(String::as_str) else {
        return false;
    };
    let prefix = tokens
        .len()
        .checked_sub(2)
        .and_then(|i| tokens.get(i))
        .map(String::as_str);
    match last {
        "token" => tokens.len() == 1 || prefix.is_some_and(|p| TOKEN_PREFIXES.contains(&p)),
        "key" => prefix.is_some_and(|p| SECRET_KEY_PREFIXES.contains(&p)),
        _ => SECRET_FIELD_UNCONDITIONAL_SUFFIXES.contains(&last),
    }
}

/// A resolved `secret_fields` record in `.factory/decisions.json` naming this
/// exact `Type.field` (ADR 0078). Either disposition counts as reviewed: the
/// point is that someone looked at it, not which way they went -- a
/// `disposition: exclude` record is expected to be paired with actually
/// excluding the field from the selection, at which point the field is no
/// longer in the rendered SDL for this rule to see in the first place.
fn secret_field_reviewed(decisions: Option<&Value>, type_name: &str, field: &str) -> bool {
    let Some(doc) = decisions else {
        return false;
    };
    get_arr(doc, "decisions")
        .into_iter()
        .flatten()
        .filter(|d| get_str(d, "status") == Some("resolved"))
        .flat_map(|d| get_arr(d, "secret_fields").into_iter().flatten())
        .any(|s| get_str(s, "type") == Some(type_name) && get_str(s, "field") == Some(field))
}

/// `secret-field-exposed` (warning, ADR 0078): a response object field whose
/// name matches the credential vocabulary above is present in the rendered
/// schema. Only `type` declarations, excluding the three root types, are
/// walked: `Query`/`Mutation`'s own "fields" are operation names, not
/// response data, and an `input` declaration's fields are arguments, never
/// response fields -- so a create mutation's own `password` argument never
/// fires (an argument is the point; ADR 0078 flags it only if the same name
/// is echoed back on a response type). A field the selection has genuinely
/// excluded is not in the rendered SDL at all and so never reaches this
/// scan; a resolved `secret_fields` decision for the exact `Type.field`
/// suppresses the rest. `format: password` in the pinned spec is not
/// checked here: measured across the three pilots and 25 released
/// workspaces, zero response properties declare
/// it, so wiring that correlation up is speculative until real data
/// motivates it (see the ADR).
fn lint_secret_fields(
    decisions: Option<&Value>,
    sdl: &str,
    schema_file: &str,
    findings: &mut Findings,
) {
    let mut seen: HashSet<String> = HashSet::new();
    for decl in type_declarations(sdl) {
        if decl.kind != "type"
            || matches!(decl.name.as_str(), "Query" | "Mutation" | "Subscription")
        {
            continue;
        }
        if !seen.insert(decl.name.clone()) {
            continue;
        }
        for f in crate::reconcile::field_spans(sdl, &decl.name) {
            if !looks_like_secret_field(&f.name) {
                continue;
            }
            if secret_field_reviewed(decisions, &decl.name, &f.name) {
                continue;
            }
            findings.warn(
                "secret-field-exposed",
                format!(
                    "{}.{} is named like a credential and is exposed in the schema; exclude it via selection.yaml's fields.exclude, or record `graphos-factory-core decisions add . --resolved … --secret-field '{}.{}|expose|<reason>'` if exposing it is deliberate",
                    decl.name, f.name, decl.name, f.name
                ),
                Some(schema_file),
                Some(f.line),
            );
        }
    }
}

/// Lint a service workspace.
pub fn lint_workspace(dir: &Path, options: &LintOptions) -> LintResult {
    let mut findings = Findings::default();
    let workspace = match load_yaml_file(dir, ".factory/workspace.yaml", &mut findings) {
        Some(w) => w,
        None => {
            findings.error(
                "missing-workspace",
                ".factory/workspace.yaml is missing; this is not a service workspace",
                Some("workspace.yaml"),
                None,
            );
            return summarize(findings);
        }
    };
    contract_check(
        &workspace,
        "workspace.schema.json",
        ".factory/workspace.yaml",
        &mut findings,
        options.schemas_dir,
    );
    if let (Some(service), Some(directory)) = (
        get_str(&workspace, "service"),
        get_str(&workspace, "directory"),
    ) {
        if service != directory.replace('-', "_") {
            findings.error(
                "name-mismatch",
                format!(
                    "service \"{}\" must be the snake_case form of directory \"{}\"",
                    service, directory
                ),
                Some("workspace.yaml"),
                None,
            );
        }
    }
    // Lint enforces the declared context, including with --skip-evidence. An
    // unrecorded context_mode reads as generic, so a workspace with neither
    // marker nor companion file has nothing to report (ADR 0081).
    let context = crate::context::check(dir, options.schemas_dir);
    for error in context.errors {
        findings.error("context-invalid", error, Some(crate::context::FILE), None);
    }
    for blocker in context.blockers {
        findings.add(
            if blocker.phase == "build" {
                "error"
            } else {
                "warn"
            },
            "context-unresolved",
            format!(
                "{} [{}; {}]: {}; next: {}",
                blocker.id,
                blocker.phase,
                blocker.affects.join(", "),
                blocker.reason,
                blocker.resolve_with
            ),
            Some(crate::context::FILE),
            None,
        );
    }
    let inventory = load_json_file(dir, ".factory/inventory.json", &mut findings);
    if let Some(inv) = &inventory {
        contract_check(
            inv,
            "inventory.schema.json",
            ".factory/inventory.json",
            &mut findings,
            options.schemas_dir,
        );
        lint_inventory_rebuild(dir, inv, &mut findings);
    }
    let selection = load_yaml_file(dir, ".factory/selection.yaml", &mut findings);
    if let Some(sel) = &selection {
        contract_check(
            sel,
            "selection.schema.json",
            ".factory/selection.yaml",
            &mut findings,
            options.schemas_dir,
        );
    }
    // `directory` is validated against workspace.schema.json's own pattern,
    // and the read goes through the same custody-aware path as `lock` and
    // `reconcile`, before it is ever joined into a filesystem path (Phase
    // 7az) — a hand-edited `directory: .factory/x` no longer reads whatever
    // `.factory/x.graphql` happens to be a symlink to.
    let (schema_file, sdl) = match crate::reconcile::read_schema_file(dir, &workspace) {
        Ok(pair) => pair,
        Err(e) => {
            findings.error("missing-schema", e, None, None);
            return summarize(findings);
        }
    };

    // The decision log and its union with the findings, read and validated
    // once per run and handed to every rule that reads them (ADR 0113).
    // Either one not loading is reported below as `unreadable-file` and
    // leaves its readers with nothing to count; a findings log that does
    // not load leaves the union with the decisions alone, so one corrupt
    // findings file never blanks a resolved decision. Read before the
    // schema rules, which ask it for the declared foreign types.
    let decisions_loaded = crate::decisions::load(dir, options.schemas_dir);
    let (union_loaded, findings_error) = crate::findings::union_of(
        decisions_loaded.as_ref().map_err(String::clone),
        crate::findings::load(dir, options.schemas_dir),
    );
    // The types a resolved decision declares another subgraph's, for a
    // target that honours them (ADR 0132); none otherwise.
    let foreign = match &decisions_loaded {
        Ok(doc) if options.target.foreign_types => crate::decisions::foreign_types(doc),
        _ => Default::default(),
    };

    lint_schema(
        &sdl,
        &schema_file,
        &workspace,
        selection.as_ref(),
        options.target,
        &foreign,
        &mut findings,
    );
    lint_link_connectors(&sdl, &schema_file, &mut findings);
    lint_link_coverage(dir, &sdl, &schema_file, &mut findings);
    lint_template(dir, &sdl, options.target, &mut findings);
    if let Some(e) = findings_error {
        let file = crate::record_log::error_path(&crate::record_log::FINDINGS, &e).to_string();
        findings.error("unreadable-file", e, Some(&file), None);
    }
    // A decision log that does not load is an error too, never a silent
    // empty log: every rule below would otherwise read "no decision" where a
    // resolved one stands (a stray `.orig` under `.factory/decisions/` was
    // reported only through the vanished decision's consequences).
    if let Err(e) = &decisions_loaded {
        let file = crate::record_log::error_path(&crate::record_log::DECISIONS, e);
        findings.error("unreadable-file", e.clone(), Some(file), None);
    }
    let decisions_doc = decisions_loaded.ok();
    let union_doc = union_loaded.ok();
    // A stale link a resolved `keep` decision names is kept (ADR 0098,
    // ADR 0113 §4); an unreadable log keeps none. Findings never keep one.
    lint_selection(
        selection.as_ref(),
        inventory.as_ref(),
        &sdl,
        &workspace,
        decisions_doc.as_ref(),
        &mut findings,
    );
    lint_batching(&sdl, selection.as_ref(), inventory.as_ref(), &mut findings);
    for (severity, rule, message) in crate::entity::check_with(
        &sdl,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &foreign,
    ) {
        findings.add(severity, rule, message, Some(&schema_file), None);
    }
    lint_tags(&sdl, &schema_file, options.target, &mut findings);
    lint_waivers(dir, selection.as_ref(), inventory.as_ref(), &mut findings);
    lint_lock(
        dir,
        &sdl,
        inventory.as_ref(),
        &workspace,
        selection.as_ref(),
        &mut findings,
        options.schemas_dir,
    );
    lint_sources(dir, &mut findings, options.schemas_dir);
    lint_coverage(
        dir,
        &sdl,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &mut findings,
    );
    lint_test_shape(
        dir,
        &sdl,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &mut findings,
    );
    lint_failure_cases(
        dir,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &mut findings,
    );
    lint_fixture_collisions(dir, &mut findings);
    lint_fixture_overlaps(dir, &mut findings);
    lint_casing(
        &sdl,
        &schema_file,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &mut findings,
    );
    lint_closed_enums(
        union_doc.as_ref(),
        &sdl,
        &schema_file,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &mut findings,
    );
    lint_pagination(
        &sdl,
        &schema_file,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &mut findings,
    );
    lint_returns_line(
        &sdl,
        &schema_file,
        &workspace,
        selection.as_ref(),
        &mut findings,
    );
    lint_credential_source(
        &sdl,
        &schema_file,
        &workspace,
        selection.as_ref(),
        &mut findings,
    );
    lint_int_overflow(
        &sdl,
        &schema_file,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &mut findings,
    );
    // A behaviour waiver on a resolved decision or a current finding (ADR 0113 §2).
    let waived: HashSet<(String, String)> = union_doc
        .as_ref()
        .map(crate::obligations::behaviour_waivers)
        .unwrap_or_default()
        .into_iter()
        .filter(|w| w.status == "resolved")
        .map(|w| (w.operation, w.path))
        .collect();
    lint_descriptions(
        &sdl,
        &schema_file,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &waived,
        &mut findings,
    );
    if inventory.is_some() {
        lint_error_paths(
            &sdl,
            &schema_file,
            &workspace,
            selection.as_ref(),
            inventory.as_ref(),
            &mut findings,
        );
        lint_success_paths(
            &sdl,
            &schema_file,
            &workspace,
            selection.as_ref(),
            inventory.as_ref(),
            &mut findings,
        );
        lint_wire_keys(
            &sdl,
            &schema_file,
            &workspace,
            selection.as_ref(),
            inventory.as_ref(),
            &mut findings,
        );
        lint_auth_scheme(&sdl, &schema_file, inventory.as_ref(), &mut findings);
        lint_connector_paths(
            &sdl,
            &schema_file,
            &workspace,
            selection.as_ref(),
            inventory.as_ref(),
            &mut findings,
        );
    }
    lint_sparse_fieldsets(
        &sdl,
        &schema_file,
        &workspace,
        selection.as_ref(),
        inventory.as_ref(),
        &mut findings,
        union_doc.as_ref(),
    );
    lint_secret_fields(decisions_doc.as_ref(), &sdl, &schema_file, &mut findings);
    lint_stale_omits(dir, union_doc.as_ref(), &mut findings);
    lint_decision_alternatives(decisions_doc.as_ref(), &mut findings);
    let logged_findings = crate::findings::load(dir, options.schemas_dir).ok();
    lint_decision_log(
        decisions_doc.as_ref(),
        logged_findings.as_ref(),
        &mut findings,
    );
    lint_live(dir, &sdl, &schema_file, selection.as_ref(), &mut findings);
    if !options.skip_evidence {
        lint_evidence(dir, selection.as_ref(), &mut findings, options.schemas_dir);
    }
    apply_overrides(options.target, &mut findings);
    // The target's rules last, with the inputs the core rules read.
    findings.origin = Some(options.target.name);
    (options.target.lint)(
        &crate::target::LintInput {
            dir,
            target: options.target,
            schemas_dir: options.schemas_dir,
            workspace: &workspace,
            selection: selection.as_ref(),
            inventory: inventory.as_ref(),
            schema_file: &schema_file,
            sdl: &sdl,
            decisions: decisions_doc.as_ref(),
            decisions_and_findings: union_doc.as_ref(),
            foreign_types: &foreign,
        },
        &mut findings,
    );
    findings.origin = None;
    summarize(findings)
}

/// A target's `rule_overrides` over the core's findings: a rule turned
/// off is dropped, a severity or a message replaced, every override the
/// target lists for the rule applied in order. Run before the target's own
/// rules, so an override never touches a target finding.
fn apply_overrides(target: &crate::target::Target, findings: &mut Findings) {
    use crate::target::Override;
    findings.items.retain_mut(|f| {
        let rule = f.rule.clone();
        for o in target.rule_overrides_for(&rule) {
            match o {
                Override::Off => return false,
                Override::Severity(s) => f.severity = s.to_string(),
                Override::Message(rewrite) => f.message = rewrite(&f.message),
            }
        }
        true
    });
}

#[cfg(test)]
mod flat_arg_tests {
    use super::flat_arg;

    #[test]
    fn a_trailing_question_mark_is_part_of_a_flat_argument() {
        assert_eq!(flat_arg("$args.body"), Some("body".to_string()));
        assert_eq!(flat_arg("$args.body?"), Some("body".to_string()));
        assert_eq!(flat_arg("$args.due_date?"), Some("due_date".to_string()));
    }

    #[test]
    fn nothing_else_after_the_argument_is_flat() {
        assert_eq!(flat_arg("$args.body??"), None);
        assert_eq!(flat_arg("$args.body?->jsonStringify"), None);
        assert_eq!(flat_arg("$args.body ?? \"x\""), None);
        assert_eq!(flat_arg("$args.a.b?"), None);
        assert_eq!(flat_arg("?$args.body"), None);
    }
}
