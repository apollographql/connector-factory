//! inventory — build, page through, and diff a connector's API inventory.
//!
//!   inventory build [<document>... | <workspace>] [--out .factory/inventory.json] [--force]   (OpenAPI 3.x or
//!     Swagger 2.0; a workspace directory, or nothing for `.`: every input its sources lock names, supplements
//!     included, ADR 0138)
//!   inventory list [--inventory F] [--tag T] [--support S] [--grep RE] [--offset N] [--limit N] [--json]
//!     one page (default 50); `--json` names `limit` and `next_offset`, null on the last page (ADR 0091)
//!   inventory describe <key>... [--inventory F] [--no-shape] [--json]
//!   inventory links [workspace] [--inventory F] [--json]   (every candidate_entity_link fact, flat, ADR 0069)
//!   inventory diff <before.json> <after.json> [--json]
//!   inventory validate [--inventory F]

use crate::args::{Args, Flags};
use crate::inventory::{api_change_line, diff_inventories, expand_shape, shape_name};
use crate::json::{compact, get, get_arr, get_str, pretty};
use crate::openapi::build_inventory;
use serde_json::Value;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub const DEFAULT_INVENTORY_REL: &str = ".factory/inventory.json";
const DEFAULT_INVENTORY: &str = DEFAULT_INVENTORY_REL;

fn fail(message: &str) -> i32 {
    eprintln!("inventory: {}", message);
    1
}

fn read_json(file: &Path) -> Result<Value, String> {
    // `--inventory` / `--out` name a path: custody claims it when it points
    // inside a workspace's `.factory/` (ADR 0025).
    let bytes = crate::factory_io::read_named_path(file)?;
    let text = String::from_utf8(bytes).map_err(|_| format!("{}: not UTF-8", file.display()))?;
    crate::json::parse(&text)
}

fn load_inventory(args: &Args) -> Result<(PathBuf, Value), String> {
    let file = PathBuf::from(args.get("inventory").unwrap_or(DEFAULT_INVENTORY));
    if !file.exists() {
        return Err(format!(
            "no inventory at {} — run `inventory build <document>` first",
            file.display()
        ));
    }
    let inv = read_json(&file)?;
    Ok((file, inv))
}

/// The built inventory's violations of `schemas/inventory.schema.json`.
pub fn schema_errors(inventory: &Value, schemas_dir: Option<&Path>) -> Vec<String> {
    match crate::schemas::load("inventory.schema.json", schemas_dir) {
        Some(schema) => crate::jsonschema::validate(inventory, &schema),
        None => vec![],
    }
}

/// The report `inventory build` prints instead of overwriting a hand-edited
/// inventory: what the lock acknowledged, what is on disk now, and what the
/// fresh build would do to it. `None` when there is nothing to protect (no
/// lock, no entry, the file is in sync, or it is not a workspace inventory).
fn unacknowledged_edit(out: &Path, built: &Value) -> Option<String> {
    let factory = out.parent()?;
    let lock_file = factory.join("applied.lock.yaml");
    let lock = crate::yaml::parse(
        &String::from_utf8(crate::factory_io::read_named_path(&lock_file).ok()?).ok()?,
    )
    .ok()?;
    let recorded = get_str(&lock, "inventory")?;
    let text = String::from_utf8(crate::factory_io::read_named_path(out).ok()?).ok()?;
    let current = crate::json::parse(&text).ok()?;
    if crate::spans::sha256_hex(&compact(&current)) == recorded {
        return None;
    }
    let d = diff_inventories(&current, built);
    let count = |k: &str| get_arr(&d, k).map(|a| a.len()).unwrap_or(0);
    let mut out_lines = vec![format!(
        "inventory: refused — {} changed since {} and this build would revert it.",
        out.display(),
        lock_file.display()
    )];
    out_lines.push(
        "  The inventory is built, never edited: it is a straight reading of the description document, and every judgement it might carry belongs in selection.yaml."
            .to_string(),
    );
    out_lines.push(format!(
        "  What the build would change against the file on disk: {} added, {} removed, {} changed.",
        count("added"),
        count("removed"),
        count("changed")
    ));
    for c in get_arr(&d, "changed").into_iter().flatten().take(10) {
        out_lines.push(format!(
            "    ~ {}  ({})",
            get_str(c, "key").unwrap_or(""),
            get_arr(c, "fields")
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(", ")
        ));
    }
    for e in get_arr(&d, "api").into_iter().flatten() {
        out_lines.push(format!("    ~ {}", api_change_line(e)));
    }
    out_lines.push(
        "  Fix the cause, not the file: a wrong shape or response is a spec defect — edit the pinned working copy and `graphos-factory-core codify --source`; a judgement (the envelope, the root, a name) goes in selection.yaml. Then rebuild. `--force` overwrites and loses the edit."
            .to_string(),
    );
    Some(format!("{}\n", out_lines.join("\n")))
}

/// (supported, needs_review, unsupported) operations.
pub fn tally(operations: &[Value]) -> (usize, usize, usize) {
    let mut t = (0, 0, 0);
    for op in operations {
        match get_str(op, "support") {
            Some("supported") => t.0 += 1,
            Some("needs_review") => t.1 += 1,
            Some("unsupported") => t.2 += 1,
            _ => {}
        }
    }
    t
}

fn cmd_build(args: &Args) -> i32 {
    // No document named: the workspace's own inputs, every pinned document
    // and supplement its sources lock names, so a rebuild never drops what
    // one of them declares (ADR 0138).
    let workspace = match args.positional.as_slice() {
        [] => Some(PathBuf::from(".")),
        [one] if Path::new(one).is_dir() => Some(PathBuf::from(one)),
        _ => None,
    };
    let result = if let Some(dir) = &workspace {
        crate::inputs::build_workspace(dir, None)
    } else {
        let files: Vec<PathBuf> = args.positional.iter().map(PathBuf::from).collect();
        crate::inputs::build_files(&files)
    };
    let (built, formats) = match result {
        Ok(b) => b,
        Err(e) => return fail(&e),
    };
    let format = formats
        .iter()
        .map(|f| f.to_string())
        .collect::<Vec<_>>()
        .join(" + ");
    let schemas_dir = args.get("schemas").map(PathBuf::from);
    let errors = schema_errors(&built.inventory, schemas_dir.as_deref());
    if !errors.is_empty() {
        for e in errors.iter().take(20) {
            eprintln!("  contract: {}", e);
        }
        return fail(&format!(
            "the built inventory does not satisfy schemas/inventory.schema.json ({} errors)",
            errors.len()
        ));
    }
    let default_out = match &workspace {
        Some(dir) if dir != Path::new(".") => {
            dir.join(DEFAULT_INVENTORY).to_string_lossy().to_string()
        }
        _ => DEFAULT_INVENTORY.to_string(),
    };
    let out = args.get("out").unwrap_or(&default_out);
    if out != "-" {
        // Never revert a hand edit in silence. When the file being replaced
        // is a workspace inventory the lock has acknowledged, and it no
        // longer hashes to what the lock recorded, someone edited it; the
        // build says what would change and stops (ADR 0018).
        if !args.has("force") {
            if let Some(diff) = unacknowledged_edit(Path::new(out), &built.inventory) {
                eprint!("{}", diff);
                return 3;
            }
        }
        if let Err(e) =
            crate::factory_io::write_named_path(Path::new(out), pretty(&built.inventory).as_bytes())
        {
            return fail(&e);
        }
    } else {
        print!("{}", pretty(&built.inventory));
    }
    let ops = get_arr(&built.inventory, "operations")
        .cloned()
        .unwrap_or_default();
    let (s, n, u) = tally(&ops);
    let shapes = get(&built.inventory, "shapes")
        .and_then(Value::as_object)
        .map(|o| o.len())
        .unwrap_or(0);
    let unresolved = get_arr(&built.inventory, "unresolved")
        .map(|a| a.len())
        .unwrap_or(0);
    eprintln!(
        "{} ({}): {} operations ({} supported, {} needs_review, {} unsupported), {} shapes, {} unresolved{}",
        get(&built.inventory, "api").and_then(|a| get_str(a, "title")).unwrap_or(""),
        format,
        ops.len(),
        s,
        n,
        u,
        shapes,
        unresolved,
        if out == "-" { String::new() } else { format!(" -> {}", out) }
    );
    for w in &built.warnings {
        eprintln!("  warning: {}", w);
    }
    0
}

fn cmd_list(args: &Args) -> i32 {
    let (file, inventory) = match load_inventory(args) {
        Ok(x) => x,
        Err(e) => return fail(&e),
    };
    // The relationship facts (ADR 0069), flat, with the by-id target's
    // selection state when a selection sits beside this inventory. This is
    // a pager: a selection it cannot read (a `..` in `--inventory`, a
    // selection.yaml that does not parse) costs the notes their
    // annotation, never the listing — `target_selected` stays unset.
    // `inventory links --inventory` is the reader that refuses instead.
    let mut links = super::inventory_links::candidate_links(&inventory);
    match super::inventory_links::selection_beside(&file) {
        Ok(Some(selection)) => super::inventory_links::mark_selected(&mut links, &selection),
        Ok(None) => {}
        Err(e) => eprintln!("inventory list: {}; link targets unannotated", e),
    }
    let mut ops: Vec<Value> = get_arr(&inventory, "operations")
        .cloned()
        .unwrap_or_default();
    if let Some(tag) = args.get("tag") {
        ops.retain(|o| {
            get_arr(o, "tags")
                .map(|t| t.iter().any(|x| x.as_str() == Some(tag)))
                .unwrap_or(false)
        });
    }
    if let Some(support) = args.get("support") {
        ops.retain(|o| get_str(o, "support") == Some(support));
    }
    if let Some(pattern) = args.get("grep") {
        match regex::RegexBuilder::new(pattern)
            .case_insensitive(true)
            .build()
        {
            Ok(re) => ops.retain(|o| {
                re.is_match(get_str(o, "key").unwrap_or(""))
                    || re.is_match(get_str(o, "operation_id").unwrap_or(""))
                    || re.is_match(get_str(o, "summary").unwrap_or(""))
            }),
            Err(e) => return fail(&format!("bad --grep: {}", e)),
        }
    }
    // A value that does not parse used to fall back to the default, and
    // `--limit 0` makes a page whose next page starts where it did: both
    // are refused, so a loop following `next_offset` always ends (ADR 0091).
    // A bare `--offset` or `--limit` (a loop variable that expanded to
    // nothing) is present with no value, and is refused the same way.
    if args.has("offset") && args.get("offset").is_none() {
        return fail("bad --offset: expected a whole number (0 or more)");
    }
    if args.has("limit") && args.get("limit").is_none() {
        return fail("bad --limit: expected a whole number (1 or more)");
    }
    let offset: usize = match args.get("offset").map(str::parse::<usize>) {
        None => 0,
        Some(Ok(n)) => n,
        Some(Err(_)) => return fail("bad --offset: expected a whole number (0 or more)"),
    };
    let limit: usize = match args.get("limit").map(str::parse::<usize>) {
        None => 50,
        Some(Ok(n)) if n > 0 => n,
        Some(_) => return fail("bad --limit: expected a whole number (1 or more)"),
    };
    let page: Vec<Value> = ops.iter().skip(offset).take(limit).cloned().collect();
    // Where the next page starts; `None` on the last page (ADR 0091).
    let next_offset = (offset + page.len() < ops.len()).then_some(offset + page.len());

    if args.has("json") {
        let page_shapes: Vec<String> = page
            .iter()
            .filter_map(|op| {
                get(op, "response")
                    .and_then(|r| get_str(r, "shape_ref"))
                    .map(|r| shape_name(r).to_string())
            })
            .collect();
        let page_links: Vec<Value> = links
            .iter()
            .filter(|l| page_shapes.iter().any(|s| s == &l.shape))
            .map(super::inventory_links::CandidateLink::to_json)
            .collect();
        print!(
            "{}",
            pretty(&crate::json::object(vec![
                ("total", Value::from(ops.len())),
                ("offset", Value::from(offset)),
                ("limit", Value::from(limit)),
                (
                    "next_offset",
                    next_offset.map(Value::from).unwrap_or(Value::Null),
                ),
                ("operations", Value::Array(page)),
                ("candidate_links", Value::Array(page_links)),
            ]))
        );
        // stdout is the JSON; a reader piping it into jq still sees, on the
        // terminal, that this page is not the whole inventory.
        if let Some(next) = next_offset {
            eprintln!(
                "inventory list: operations {}-{} of {}; next page: --offset {} --limit {}",
                offset + 1,
                next,
                ops.len(),
                next,
                limit
            );
        }
        return 0;
    }
    let mut groups: Vec<(String, Vec<&Value>)> = Vec::new();
    for op in &page {
        let tag = get_arr(op, "tags")
            .and_then(|t| t.first())
            .and_then(Value::as_str)
            .unwrap_or("(untagged)")
            .to_string();
        match groups.iter_mut().find(|(t, _)| *t == tag) {
            Some((_, list)) => list.push(op),
            None => groups.push((tag, vec![op])),
        }
    }
    for (tag, entries) in &groups {
        println!("\n{}", tag);
        for op in entries {
            let mark = match get_str(op, "support") {
                Some("supported") => " ",
                Some("needs_review") => "?",
                _ => "x",
            };
            // List-ness follows the envelope, which is a judgement in
            // selection.yaml (ADR 0018); this listing reads the selection
            // only to annotate link targets, never for the envelope, so it
            // shows the tool's own suggestion from the response facts.
            let response = get(op, "response");
            let list_mark = if crate::envelope::is_list(
                response,
                crate::envelope::suggest_envelope(response).as_deref(),
            ) {
                " [list]"
            } else {
                ""
            };
            // A POST the name suggests may be a read: recorded as a write,
            // flagged here for the user to confirm (ADR 0015).
            let hint_mark = if get_str(op, "read_hint").is_some() {
                " [read?]"
            } else {
                ""
            };
            println!(
                "  {} {}  {}{}{}",
                mark,
                get_str(op, "key").unwrap_or(""),
                get_str(op, "operation_id").unwrap_or(""),
                list_mark,
                hint_mark
            );
            if let Some(s) = get_str(op, "summary") {
                println!("      {}", s);
            }
            if let Some(h) = get_str(op, "read_hint") {
                println!("      read? {}", h);
            }
            if let Some(r) = get_str(op, "support_reason") {
                println!("      {}: {}", get_str(op, "support").unwrap_or(""), r);
            }
            // The relationship facts on this operation's response shape
            // (ADR 0069), five at most; `inventory links` has them all.
            let shape = get(op, "response")
                .and_then(|r| get_str(r, "shape_ref"))
                .map(shape_name);
            let notes: Vec<&super::inventory_links::CandidateLink> = links
                .iter()
                .filter(|l| Some(l.shape.as_str()) == shape)
                .collect();
            for l in notes.iter().take(5) {
                println!("      links: {}", l.note());
            }
            if notes.len() > 5 {
                println!("      … and {} more (inventory links)", notes.len() - 5);
            }
        }
    }
    let shown = format!("{}-{}", offset + 1, offset + page.len());
    println!(
        "\n{} of {} operations (x unsupported, ? needs_review)",
        if page.is_empty() {
            "0".to_string()
        } else {
            shown
        },
        ops.len()
    );
    if let Some(next) = next_offset {
        println!("next page: --offset {} --limit {}", next, limit);
    }
    0
}

fn cmd_describe(args: &Args) -> i32 {
    let (_, inventory) = match load_inventory(args) {
        Ok(x) => x,
        Err(e) => return fail(&e),
    };
    if args.positional.is_empty() {
        return fail("usage: inventory describe <key>...");
    }
    let shapes = get(&inventory, "shapes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut out = Vec::new();
    for key in &args.positional {
        let op = get_arr(&inventory, "operations").and_then(|ops| {
            ops.iter()
                .find(|o| get_str(o, "key") == Some(key) || get_str(o, "operation_id") == Some(key))
        });
        let op = match op {
            Some(o) => o.clone(),
            None => return fail(&format!("no operation {} in the inventory", key)),
        };
        let mut detail = op.clone();
        if !args.has("no-shape") {
            if let Some(r) = get(&op, "response").and_then(|r| get_str(r, "shape_ref")) {
                let mut response = get(&op, "response").cloned().unwrap();
                let expanded = expand_shape(
                    &crate::json::object(vec![("$ref", Value::from(r))]),
                    &shapes,
                    0,
                    &HashSet::new(),
                );
                crate::json::set(&mut response, "shape", expanded);
                crate::json::set(&mut detail, "response", response);
            }
            if let Some(r) = get(&op, "request_body").and_then(|r| get_str(r, "shape_ref")) {
                let mut rb = get(&op, "request_body").cloned().unwrap();
                let expanded = expand_shape(
                    &crate::json::object(vec![("$ref", Value::from(r))]),
                    &shapes,
                    0,
                    &HashSet::new(),
                );
                crate::json::set(&mut rb, "shape", expanded);
                crate::json::set(&mut detail, "request_body", rb);
            }
        }
        out.push(detail);
    }
    if out.len() == 1 {
        print!("{}", pretty(&out[0]));
    } else {
        print!("{}", pretty(&Value::Array(out)));
    }
    0
}

fn cmd_diff(args: &Args) -> i32 {
    let (before, after) = match (args.positional.first(), args.positional.get(1)) {
        (Some(b), Some(a)) => (b, a),
        _ => return fail("usage: inventory diff <before.json> <after.json>"),
    };
    let (b, a) = match (read_json(Path::new(before)), read_json(Path::new(after))) {
        (Ok(b), Ok(a)) => (b, a),
        (Err(e), _) | (_, Err(e)) => return fail(&e),
    };
    let result = diff_inventories(&b, &a);
    if args.has("json") {
        print!("{}", pretty(&result));
        return 0;
    }
    let section = |title: &str, items: &[Value], render: &dyn Fn(&Value) -> String| {
        println!("{}: {}", title, items.len());
        for item in items {
            println!("  {}", render(item));
        }
    };
    let plain = |v: &Value| v.as_str().unwrap_or("").to_string();
    section("added", get_arr(&result, "added").unwrap(), &plain);
    section("removed", get_arr(&result, "removed").unwrap(), &plain);
    section("changed", get_arr(&result, "changed").unwrap(), &|c| {
        let fields: Vec<&str> = get_arr(c, "fields")
            .map(|f| f.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default();
        format!(
            "{}  ({})",
            get_str(c, "key").unwrap_or(""),
            fields.join(", ")
        )
    });
    section("api", get_arr(&result, "api").unwrap(), &api_change_line);
    println!("\nA discover run never edits the schema. Take this diff to the user, change selection.yaml, then apply.");
    0
}

fn cmd_validate(args: &Args) -> i32 {
    let (file, inventory) = match load_inventory(args) {
        Ok(x) => x,
        Err(e) => return fail(&e),
    };
    let schemas_dir = args.get("schemas").map(PathBuf::from);
    let errors = schema_errors(&inventory, schemas_dir.as_deref());
    if !errors.is_empty() {
        for e in &errors {
            println!("  {}", e);
        }
        return fail(&format!(
            "{} does not satisfy schemas/inventory.schema.json ({} errors)",
            file.display(),
            errors.len()
        ));
    }
    let ops = get_arr(&inventory, "operations")
        .cloned()
        .unwrap_or_default();
    let (s, n, u) = tally(&ops);
    println!(
        "{} is valid: {} operations ({} supported, {} needs_review, {} unsupported), {} unresolved",
        file.display(),
        ops.len(),
        s,
        n,
        u,
        get_arr(&inventory, "unresolved")
            .map(|a| a.len())
            .unwrap_or(0)
    );
    let _ = compact; // keep helper imports honest
    0
}

/// The usage text: `inventory --help` prints it on stdout (ADR 0086).
pub fn usage() -> String {
    format!(
        "usage: inventory <command> [options]\n\n  build [<document>... | <workspace>] [--out FILE] [--force]\n                                     build an inventory from description documents (OpenAPI 3.x or Swagger 2.0);\n                                     given a workspace (or nothing, for .), from every pinned document and\n                                     supplement its sources.lock.yaml names\n  list [--tag T] [--support S] [--grep RE] [--offset N] [--limit N] [--json]\n                                     one page, 50 operations by default; --json carries total, offset, limit and\n                                     next_offset (null on the last page) -- follow next_offset to see them all\n  describe <key>... [--no-shape]     one operation with its fully expanded shape\n  links [workspace] [--json]         every candidate_entity_link fact, flat: shape > path -> by-id operation\n  supplement [workspace] --provenance docs|probe [--out FILE] [--cite TEMPLATE] [--note TEXT] [--dry-run] [--force] [--json]\n                                     move the operations no pinned document declares into a supplement (an OpenAPI 3\n                                     document the inventory is built from) and show what a rebuild would change\n  diff <before.json> <after.json>    what a spec refresh changed\n  validate                           check an inventory against the contract schema\n\n  --inventory FILE                   default {}\n\n",
        DEFAULT_INVENTORY
    )
}

/// The flags each verb accepts; dispatch refuses any other (ADR 0097).
/// `links` is [`super::inventory_links::FLAGS`].
pub const BUILD_FLAGS: Flags = Flags {
    boolean: &["force"],
    valued: &["out", "schemas"],
};
pub const LIST_FLAGS: Flags = Flags {
    boolean: &["json"],
    valued: &["inventory", "tag", "support", "grep", "offset", "limit"],
};
pub const DESCRIBE_FLAGS: Flags = Flags {
    boolean: &["no-shape"],
    valued: &["inventory"],
};
pub const DIFF_FLAGS: Flags = Flags {
    boolean: &["json"],
    valued: &[],
};
pub const VALIDATE_FLAGS: Flags = Flags {
    boolean: &[],
    valued: &["inventory", "schemas"],
};

pub fn main(argv: &[String]) -> i32 {
    let (command, rest) = match argv.split_first() {
        Some((c, r)) => (c.as_str(), r.to_vec()),
        None => ("--help", vec![]),
    };
    if matches!(command, "--help" | "-h") {
        print!("{}", usage());
        return 0;
    }
    match command {
        "build" => cmd_build(&Args::parse(&rest, &BUILD_FLAGS)),
        "list" => cmd_list(&Args::parse(&rest, &LIST_FLAGS)),
        "describe" => cmd_describe(&Args::parse(&rest, &DESCRIBE_FLAGS)),
        "links" => super::inventory_links::main(&rest),
        "supplement" => super::inventory_supplement::main(&rest),
        "diff" => cmd_diff(&Args::parse(&rest, &DIFF_FLAGS)),
        "validate" => cmd_validate(&Args::parse(&rest, &VALIDATE_FLAGS)),
        other => fail(&format!("unknown command \"{}\" (try --help)", other)),
    }
}

/// The inventory a description document on disk describes, exactly as
/// `inventory build` writes it: read (Swagger normalised), built, the
/// reader's warnings first, and the patch marks of the enclosing
/// workspace's sources.lock.yaml applied.
pub fn build_from_file(
    spec_file: &Path,
) -> Result<(crate::openapi::Built, crate::spec::Format), String> {
    let (built, mut formats) = crate::inputs::build_files(&[spec_file.to_path_buf()])?;
    Ok((built, formats.remove(0)))
}

/// The inventory a document's text describes, before any patch marks: what
/// `build_from_file` reads for a document no sources.lock.yaml entry patches
/// (a freshly pinned one, for `init`). `filename` names the document in
/// messages.
pub fn build_from_text(
    text: &str,
    filename: &str,
) -> Result<(crate::openapi::Built, crate::spec::Format), String> {
    let loaded = crate::spec::read(text, filename)?;
    let mut built = build_inventory(&loaded.document)?;
    let mut warnings = loaded.warnings.clone();
    warnings.append(&mut built.warnings);
    built.warnings = warnings;
    Ok((built, loaded.format))
}

/// The builder's shape-name spelling of a component name (`ShapeSet::name_for`
/// without the de-duplication suffix), for a component the map does not know.
fn sanitised(name: &str) -> String {
    name.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

/// Mark every operation a pinned document's patches touch: its own path
/// item (or method), or a schema its request, response or error responses
/// reach — directly or through the `$ref`s nested in it. `shape_names` maps
/// component references to inventory shape names, so a component called
/// `Widget List` (shape `Widget_List`) is still found; for a Swagger
/// document (`kind: swagger`) a `/definitions/X` pointer is looked up under
/// the `#/components/schemas/X` the reader rewrote it to. With no patches
/// the inventory is unchanged byte for byte.
pub fn annotate_with(
    inventory: &mut Value,
    kind: &str,
    patches: &[Value],
    shape_names: &std::collections::HashMap<String, String>,
) {
    if patches.is_empty() {
        return;
    }
    let is_swagger = kind == "swagger";
    let shape_name = |v: Option<&Value>| -> Option<String> {
        v.and_then(|x| get_str(x, "shape_ref"))
            .and_then(|r| r.strip_prefix("#/shapes/"))
            .map(str::to_string)
    };
    let all_shapes = get(inventory, "shapes")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    if let Some(Value::Array(ops)) = inventory.get_mut("operations") {
        for op in ops.iter_mut() {
            let path = get_str(op, "path").unwrap_or("").to_string();
            let method = get_str(op, "method").unwrap_or("").to_lowercase();
            let item = format!("/paths/{}", crate::patch::escape_token(&path));
            let own = format!("{}/{}", item, method);
            // Every shape the operation reaches: its request and response
            // shapes and, through their `$ref`s, the ones nested inside
            // (an envelope's payload, a list's items, a nested object).
            let error_shapes: Vec<Option<String>> = get_arr(op, "errors")
                .into_iter()
                .flatten()
                .map(|e| shape_name(Some(e)))
                .collect();
            let shapes = crate::inventory::shape_closure(
                [
                    shape_name(get(op, "response")),
                    shape_name(get(op, "request_body")),
                ]
                .into_iter()
                .chain(error_shapes)
                .flatten(),
                &all_shapes,
            );
            let mut hits: Vec<Value> = Vec::new();
            for p in patches {
                let pointer = get_str(p, "path").unwrap_or("");
                let touches_item = crate::patch::under(pointer, &own)
                    || (crate::patch::under(pointer, &item)
                        && !crate::http_methods()
                            .iter()
                            .any(|m| crate::patch::under(pointer, &format!("{}/{}", item, m))));
                // The pointer's own component, mapped to the shape name the
                // builder gave it (sanitised, de-duplicated).
                let touched_component = ["/components/schemas/", "/definitions/"]
                    .iter()
                    .find_map(|prefix| pointer.strip_prefix(prefix).map(|rest| (prefix, rest)))
                    .map(|(prefix, rest)| {
                        let token = rest.split('/').next().unwrap_or("");
                        // A Swagger document's `#/definitions/X` reaches the
                        // builder as `#/components/schemas/X`; an OpenAPI 3
                        // document's stray `definitions` section is not read
                        // at all, so only its own spelling counts there.
                        let spellings: &[&str] = if is_swagger {
                            &["#/components/schemas/", "#/definitions/"]
                        } else if *prefix == "/definitions/" {
                            &["#/definitions/"]
                        } else {
                            &["#/components/schemas/"]
                        };
                        spellings
                            .iter()
                            .find_map(|p| shape_names.get(&format!("{}{}", p, token)))
                            .cloned()
                            .unwrap_or_else(|| {
                                if !is_swagger && *prefix == "/definitions/" {
                                    String::new() // never a shape in an OpenAPI 3 document
                                } else {
                                    sanitised(&crate::patch::unescape_token(token))
                                }
                            })
                    });
                let touches_shape = touched_component
                    .as_ref()
                    .filter(|c| !c.is_empty())
                    .map(|c| shapes.iter().any(|n| n == c))
                    .unwrap_or(false);
                if touches_item || touches_shape {
                    hits.push(crate::json::object(vec![
                        ("path", Value::from(pointer)),
                        ("op", Value::from(get_str(p, "op").unwrap_or(""))),
                        ("reason", Value::from(get_str(p, "reason").unwrap_or(""))),
                    ]));
                }
            }
            if !hits.is_empty() {
                // A second pinned document's marks follow the first's.
                if let Some(Value::Array(earlier)) = op.get_mut("patches") {
                    earlier.extend(hits);
                } else {
                    crate::json::set(op, "patches", Value::Array(hits));
                }
            }
        }
    }
}
