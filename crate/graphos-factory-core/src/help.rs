//! `--help` and `-h` for every subcommand and sub-verb (ADR 0086).
//!
//! The flag is answered here, in [`crate::cmd::dispatch`], before any
//! subcommand parses its arguments: the command's usage goes to stdout, the
//! exit code is 0, and nothing is read or written. Answering it per
//! subcommand is what let `selection draft --help` run the draft and write
//! `.factory/selection.yaml`: the shared [`crate::args::Args`] grammar
//! records `--help` as one more bare flag, and a subcommand that never asks
//! for it simply ignores it.
//!
//! A `-h` or `--help` token anywhere after the command asks for help, even
//! where a valued flag would have taken it as its value (`--reason -h`):
//! the cost of the rule is a value no-one writes, the gain is that no
//! spelling of the flag can reach a writer.

use crate::cmd;

/// The top-level usage: the binary alone, or `--help`. Written with the
/// shared name; [`usage_text`] prints the binary's own ([`named`]).
pub const USAGE: &str = "usage: graphos-factory-core <command> [args]

  init       [workspace] --name N --spec PATH [--target NAME] [--url U] [--retrieved-at T] [--created-at T] [--context-mode generic|specialized|undecided] [--dry-run] [--json]
             a new spec-backed workspace: workspace.yaml, the pinned document, sources.lock.yaml, inventory.json; never runs git
  inventory <build|list|describe|links|diff|validate> …     build (from an OpenAPI 3.x or Swagger 2.0 document), page, diff an API inventory; links lists every candidate_entity_link fact flat
             build <document> [--out F] [--force]      --force overwrites an inventory that changed since applied.lock.yaml
  context    check [workspace] [--phase build|live] [--json]   assess declared customer-context requirements before authoring
             capture [workspace] (--requirement ID|--input) --id ID --from FILE --representation raw|derived|transcribed …   preserve an exact local artifact; no network or credentials
  decisions  list [workspace] [--open] [--causal] [--json]     the decision log: judgement calls recorded and open questions awaiting the user
             add [workspace] --title T (--question Q | --choice LABEL --choice LABEL…) [--multiple] [--resolved --chosen N --note TEXT --decision TEXT [--by user|agent]] [--omit operation|direction|path|reason]…   choices numbered 1, 2, 3… (ids kept only when every --choice is `id:label`); a record with no question and fewer than two choices is refused (no-alternative)
             resolve [workspace] --id D-id [--chosen N|LABEL]… [--note TEXT] [--decision TEXT] [--by user|agent] [--force]   --by defaults to agent
             reopen [workspace] --id D-id [--json]           clear a resolved or superseded decision's answer so it can be resolved again
             supersede [workspace] --id D-id [--json]        mark a resolved decision replaced: its answer is kept, its omits stop counting
             link [workspace] --id D-id (--after D-id | --amends D-id)… [--json]   edges on a decision recorded as its own file
             migrate [workspace] [--keep-md] [--force] [--dry-run] [--json]   import a legacy .factory/decisions.md into decisions.json
             migrate [workspace] --split [--sorted FILE] [--dry-run] [--json]   split an older-format log: provenance onto its entries, facts to findings.json, the hand-sorted rest per FILE
  findings   list [workspace] [--json]                 facts a reference or the wire settled, read by the instruments that read omits and affects
             add [workspace] --title T --body TEXT [--cites C] [--source agent|codify|sources] [--affects P]… [--omit operation|direction|path|reason]… [--evidence E]… [--related D-id|F-id]… [--json]   --omit refuses editorial
             supersede [workspace] --id F-id [--json]        a later finding replaced it: its omits and affects stop counting
  lint       [workspace] [--json] [--warnings-as-errors] [--skip-evidence]
  reconcile  [workspace] [--json] [--baseline <git-rev>]  schema <-> selection <-> inventory delta; overrides; hand edits
  lock       [workspace] [--check [--provenance]] [--json] [--skill-dir DIR] [--model MODEL]   record applied state plus authoring provenance
  codify     [workspace] --key K --reason R [--assert kind=value]… [--decision D-id] [--context TEXT] [--expressed] [--model MODEL]   --context lands on the override; with --expressed, a finding
             [workspace] --source PATH --reason R [--decision D-id] [--context TEXT] [--model MODEL]   a hand edit to a pinned spec -> patches[]
             [workspace] --waive TARGET --status unchecked|unmatched --reason R [--decision D-id] [--context TEXT]   a conformance gap -> waivers[]
  sources    pin [workspace] --path SPEC [--url U] [--retrieved-at T] [--force --reason R [--decision D-id]] [--json] [--model MODEL] · status [workspace] [--json]
             refresh [workspace] --path SPEC --from FILE --reason R [--decision D-id] [--url U] [--retrieved-at T] [--dry-run] [--json] [--model MODEL]   a new vendor document: re-apply patches[], rebuild + diff the inventory; both record a finding
  source-coverage [workspace] [OP-KEY] [--json] [--check]   every request-body and response path the source offers, classified: covered by the schema, or a decision says why not; --check fails on unaccounted, unresolved, unverified-default, transport-expansion-missing or an unaccounted behaviour fact; no OP-KEY: every selected operation, one counts line each
  spans      obligations …                             the old spelling of source-coverage; still runs
             json-accounting [workspace] [--json] [--check]   every response field still typed as the JSON scalar, matched against a resolved json_reasons decision
  serialization [workspace] [--json]                   read-only: per body-sending write, whether each argument's value is proven at its own body pointer, query key, path or header placeholder by an executed e2e case, omission and explicit null included; the standalone form of the write_body_proof evidence layer; exit 1 on any gap
  render     [workspace] --out DIR [--unit]
  scaffold   [workspace] [--op KEY]… [--force] [--dry-run] [--json]   a case, a stub and a unit entry per selected operation, from the shapes; --op batch:<Shape> a $batch connector's proving case
             --op KEY --status CODE|all-missing        error cases, each with `# expect-upstream-status`
  selection  draft [workspace] [--op KEY]… [--links | --envelopes] [--force] [--dry-run] [--json]   propose a response.envelope per included operation and a links: entry per candidate_entity_link fact, as hints to confirm
             set [workspace] --op KEY… --include true|false [--dry-run] [--json]   toggle an operation's include flag in place
             review [workspace] [--candidate FILE --expect-input TOKEN] [--expect-review TOKEN]   read-only JSON selection review
  links      apply [workspace] --dry-run [--link \"<shape> > <path>\"] [--json]   print the field-level @connect for each confirmed links: entry; never writes the schema
  batch      find [workspace] [--json] [--all-types] [--check]   per keyed type, the operations that return an array of it and whether they take a list of its key; --check fails when a batchable keyed type has no type-level $batch connector; exit 2 when the schema does not parse
  validate   [workspace] [--json]                      conformance of bodies against the oracle
  probe      [workspace] --key K --url URL …           one recorded, scrubbed live request
  infer      [workspace] [--update-inventory] [--json]
  fixtures   [workspace] [--check]
  evidence   [workspace] --scripts DIR [--skip a,b] [--only a,b]
  auth-env   [workspace]                               env var named by AUTH_EXPR's test_default
  yaml2json  <file>                                    a YAML document as JSON, for shell scripts
  error-statuses [workspace]                           root field and documented non-2xx statuses, for e2e.sh
  version
";

/// The top-level usage: the core's commands, then every registered
/// target's, grouped under the target's name, then `version`. A workspace
/// command runs against the target its `skill.name` names; a target's
/// commands run only on that target's workspaces.
pub fn usage_text() -> String {
    named(&usage_rows())
}

/// `text` with the shared name (`graphos-factory-core`, as every usage
/// string in the core is written) replaced by this binary's own, so
/// a product's `--help` says `usage: <its own name> …`.
pub fn named(text: &str) -> String {
    let bin = crate::bin_name();
    if bin == crate::CORE_NAME {
        return text.to_string();
    }
    text.replace(&format!("{} ", crate::CORE_NAME), &format!("{} ", bin))
}

fn usage_rows() -> String {
    let (head, tail) = USAGE
        .split_once("\n  version\n")
        .unwrap_or((USAGE.trim_end_matches('\n'), ""));
    let mut out = format!("{}\n", head);
    for t in crate::target::registered() {
        if t.commands.is_empty() {
            out.push_str(&format!("  [target {}]  no commands of its own\n", t.name));
            continue;
        }
        out.push_str(&format!("  [target {}]\n", t.name));
        for c in t.commands {
            for (i, row) in c.summary.iter().enumerate() {
                if i == 0 {
                    out.push_str(&format!("  {:<10} {}\n", c.name, row));
                } else {
                    out.push_str(&format!("             {}\n", row));
                }
            }
        }
    }
    out.push_str("  version\n");
    out.push_str(tail);
    out
}

/// The sub-verbs of each command that has them, in the order `USAGE` lists
/// them. A first argument that names one selects that verb's usage.
pub const VERBS: &[(&str, &[&str])] = &[
    (
        "inventory",
        &["build", "list", "describe", "links", "diff", "validate"],
    ),
    ("context", &["check", "capture"]),
    (
        "decisions",
        &[
            "list",
            "add",
            "resolve",
            "reopen",
            "supersede",
            "link",
            "migrate",
        ],
    ),
    ("findings", &["list", "add", "supersede"]),
    ("sources", &["pin", "status", "refresh"]),
    ("spans", &["obligations", "json-accounting"]),
    ("selection", &["draft", "set", "review"]),
    ("links", &["apply"]),
    ("batch", &["find"]),
];

/// True when the arguments after the command ask for help.
pub fn requested(argv: &[String]) -> bool {
    argv.iter().any(|a| a == "--help" || a == "-h")
}

/// True when `verb` is one of `command`'s sub-verbs.
pub fn is_verb(command: &str, verb: &str) -> bool {
    verbs(command).contains(&verb)
}

/// `command`'s sub-verbs; empty for a command without any.
pub fn verbs(command: &str) -> &'static [&'static str] {
    VERBS
        .iter()
        .find(|(c, _)| *c == command)
        .map(|(_, v)| *v)
        .or_else(|| crate::target::command(command).map(|(_, c)| c.verbs))
        .unwrap_or(&[])
}

/// The sub-verb `argv` names for `command`, when it names a known one.
fn verb<'a>(command: &str, argv: &'a [String]) -> Option<&'a str> {
    let first = argv.first()?.as_str();
    verbs(command).contains(&first).then_some(first)
}

/// The usage `command --help` prints (with `argv` the arguments after the
/// command, so a sub-verb selects its own), or `None` for an unknown
/// command. The text a subcommand prints with its own usage errors wins
/// where one exists; the rest is the command's block of [`USAGE`].
pub fn usage(command: &str, argv: &[String]) -> Option<String> {
    let own: Option<String> = match (command, verb(command, argv)) {
        ("init", _) => Some(cmd::init::USAGE.to_string()),
        ("inventory", Some("links")) => Some(cmd::inventory_links::USAGE.to_string()),
        ("inventory", Some("supplement")) => Some(cmd::inventory_supplement::USAGE.to_string()),
        ("inventory", _) => Some(cmd::inventory::usage()),
        ("context", _) => Some(cmd::context::USAGE.to_string()),
        ("decisions", _) => Some(cmd::decisions::USAGE.to_string()),
        ("findings", _) => Some(cmd::findings::USAGE.to_string()),
        ("codify", _) => Some(cmd::codify::USAGE.to_string()),
        ("sources", _) => Some(cmd::sources::USAGE.to_string()),
        ("spans", Some("json-accounting")) => Some(cmd::spans::JSON_ACCOUNTING_USAGE.to_string()),
        ("source-coverage", _) | ("spans", _) => Some(cmd::source_coverage::USAGE.to_string()),
        ("render", _) => Some(cmd::render::USAGE.to_string()),
        ("scaffold", _) => Some(cmd::scaffold::USAGE.to_string()),
        ("selection", Some("draft")) => Some(cmd::selection::USAGE.to_string()),
        ("selection", Some("set")) => Some(cmd::selection_set::USAGE.to_string()),
        ("links", _) => Some(cmd::links::USAGE.to_string()),
        _ => crate::target::command(command)
            .and_then(|(_, c)| c.usage)
            .map(str::to_string),
    };
    if let Some(text) = own {
        return Some(with_newline(named(&text)));
    }
    block(command, verb(command, argv)).map(with_newline)
}

fn with_newline(mut text: String) -> String {
    if !text.ends_with('\n') {
        text.push('\n');
    }
    text
}

/// `command`'s lines of [`USAGE`]: the line that names it and its indented
/// continuation lines; with `verb`, only the lines for that verb when any
/// line starts with it.
fn block(command: &str, verb: Option<&str>) -> Option<String> {
    let text = usage_text();
    let mut lines = text.lines().skip_while(|l| {
        l.strip_prefix("  ")
            .and_then(|r| r.strip_prefix(command))
            .is_none_or(|r| !(r.is_empty() || r.starts_with(' ')))
    });
    let head = lines.next()?;
    let body = head.trim_start()[command.len()..].trim_start();
    let mut rows = vec![body.to_string()];
    rows.extend(
        lines
            .take_while(|l| l.starts_with("             "))
            .map(|l| l.trim_start().to_string()),
    );
    if let Some(v) = verb {
        let prefix = format!("{} ", v);
        let only: Vec<String> = rows
            .iter()
            .filter(|r| r.starts_with(&prefix))
            .cloned()
            .collect();
        if !only.is_empty() {
            rows = only;
        }
    }
    let indent = " ".repeat("usage: ".len() + crate::bin_name().len() + command.len() + 2);
    let mut out = String::new();
    for (i, row) in rows.iter().enumerate() {
        if i == 0 {
            out.push_str(
                &format!("usage: {} {} {}", crate::bin_name(), command, row)
                    .trim_end()
                    .to_string(),
            );
        } else {
            out.push_str(&format!("\n{}{}", indent, row));
        }
    }
    Some(out)
}
