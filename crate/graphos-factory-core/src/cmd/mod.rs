pub mod batch;
pub mod codify;
pub mod context;
pub mod decisions;
pub mod evidence;
pub mod findings;
pub mod fixtures;
pub mod infer;
pub mod init;
pub mod inventory;
pub mod inventory_links;
pub mod inventory_supplement;
pub mod links;
pub mod lint;
pub mod lock;
pub mod probe;
pub mod reconcile;
pub mod render;
pub mod scaffold;
pub mod selection;
pub mod selection_review;
pub mod selection_set;
pub mod serialization;
pub mod source_coverage;
pub mod sources;
pub mod spans;
pub mod util;

use crate::args::Flags;
use std::path::PathBuf;
pub mod validate;

/// A subcommand's entry point: the arguments after its name, an exit code.
pub type Entry = fn(&[String]) -> i32;

fn version(_: &[String]) -> i32 {
    println!("{}", crate::written_by());
    0
}

/// Every subcommand the binary dispatches, by name. [`dispatch`] answers
/// `--help` for each one before calling it, and a test holds every name
/// here to a usage in [`crate::help::usage`] — a command added here without
/// one is a failing test, not a writer that `--help` can reach (ADR 0086).
pub const COMMANDS: &[(&str, Entry)] = &[
    ("init", init::main),
    ("inventory", inventory::main),
    ("lint", lint::main),
    ("context", context::main),
    ("decisions", decisions::main),
    ("findings", findings::main),
    ("reconcile", reconcile::main),
    ("lock", lock::main),
    ("codify", codify::main),
    ("render", render::main),
    ("scaffold", scaffold::main),
    ("selection", selection::main),
    ("links", links::main),
    ("batch", batch::main),
    ("sources", sources::main),
    ("source-coverage", source_coverage::main),
    ("spans", spans::main),
    ("serialization", serialization::main),
    ("validate", validate::main),
    ("probe", probe::main),
    ("infer", infer::main),
    ("fixtures", fixtures::main),
    ("evidence", evidence::main),
    ("auth-env", util::auth_env),
    ("yaml2json", util::yaml2json),
    ("error-statuses", util::error_statuses),
    ("version", version),
];

/// The flags `command` (with `verb`, for a command that has verbs)
/// accepts: the one set its parser reads, so what dispatch checks and what
/// the verb parses cannot drift apart (ADR 0097). `None` for an unknown
/// command, and for a command with verbs when `verb` is not one of them —
/// the command itself refuses that.
pub fn flags(command: &str, verb: Option<&str>) -> Option<&'static Flags> {
    Some(match (command, verb) {
        ("init", _) => &init::FLAGS,
        ("inventory", Some("build")) => &inventory::BUILD_FLAGS,
        ("inventory", Some("list")) => &inventory::LIST_FLAGS,
        ("inventory", Some("describe")) => &inventory::DESCRIBE_FLAGS,
        ("inventory", Some("links")) => &inventory_links::FLAGS,
        ("inventory", Some("supplement")) => &inventory_supplement::FLAGS,
        ("inventory", Some("diff")) => &inventory::DIFF_FLAGS,
        ("inventory", Some("validate")) => &inventory::VALIDATE_FLAGS,
        ("lint", _) => &lint::FLAGS,
        ("context", Some("check")) => &context::CHECK_FLAGS,
        ("context", Some("capture")) => &context::CAPTURE_FLAGS,
        ("decisions", Some("list")) => &decisions::LIST_FLAGS,
        ("decisions", Some("add")) => &decisions::ADD_FLAGS,
        ("decisions", Some("resolve")) => &decisions::RESOLVE_FLAGS,
        ("decisions", Some("reopen")) => &decisions::REOPEN_FLAGS,
        ("decisions", Some("supersede")) => &decisions::SUPERSEDE_FLAGS,
        ("decisions", Some("link")) => &decisions::LINK_FLAGS,
        ("decisions", Some("migrate")) => &decisions::MIGRATE_FLAGS,
        ("findings", Some("list")) => &findings::LIST_FLAGS,
        ("findings", Some("add")) => &findings::ADD_FLAGS,
        ("findings", Some("supersede")) => &findings::SUPERSEDE_FLAGS,
        ("reconcile", _) => &reconcile::FLAGS,
        ("lock", _) => &lock::FLAGS,
        ("codify", _) => &codify::FLAGS,
        ("render", _) => &render::FLAGS,
        ("scaffold", _) => &scaffold::FLAGS,
        ("selection", Some("draft")) => &selection::DRAFT_FLAGS,
        ("selection", Some("set")) => &selection_set::FLAGS,
        ("selection", Some("review")) => &selection_review::FLAGS,
        ("links", Some("apply")) => &links::APPLY_FLAGS,
        ("batch", Some("find")) => &batch::FIND_FLAGS,
        ("sources", Some("pin")) => &sources::PIN_FLAGS,
        ("sources", Some("status")) => &sources::STATUS_FLAGS,
        ("sources", Some("refresh")) => &sources::REFRESH_FLAGS,
        ("source-coverage", _) => &source_coverage::FLAGS,
        ("spans", Some("obligations")) => &source_coverage::FLAGS,
        ("spans", Some("json-accounting")) => &spans::JSON_ACCOUNTING_FLAGS,
        ("serialization", _) => &serialization::FLAGS,
        ("validate", _) => &validate::FLAGS,
        ("probe", _) => &probe::FLAGS,
        ("infer", _) => &infer::FLAGS,
        ("fixtures", _) => &fixtures::FLAGS,
        ("evidence", _) => &evidence::FLAGS,
        ("auth-env", _) | ("error-statuses", _) | ("yaml2json", _) | ("version", _) => &Flags::NONE,
        _ => return crate::target::command(command).and_then(|(_, c)| (c.flags)(verb)),
    })
}

/// Hold `rest` (the arguments after `command`) to the flags the command or
/// verb declares. `Err` is the exit code of a usage error already printed
/// on stderr: 2, and nothing has run (ADR 0097).
fn check_flags(command: &str, rest: &[String]) -> Result<(), i32> {
    let has_verbs = !crate::help::verbs(command).is_empty();
    let verb = if has_verbs {
        rest.first()
            .map(String::as_str)
            .filter(|v| crate::help::is_verb(command, v))
    } else {
        None
    };
    let (label, argv) = match verb {
        Some(v) => (format!("{} {}", command, v), &rest[1..]),
        None => (command.to_string(), rest),
    };
    let flags = match flags(command, verb) {
        Some(f) => f,
        None => {
            // A command with verbs given a flag where its verb belongs.
            if let Some(first) = rest.first().filter(|a| a.starts_with('-')) {
                eprintln!(
                    "{} {}: {:?} where a verb belongs; expected one of: {}",
                    crate::bin_name(),
                    label,
                    first,
                    crate::help::verbs(command).join(", ")
                );
                return Err(2);
            }
            return Ok(());
        }
    };
    let Err(bad) = flags.check(argv) else {
        return Ok(());
    };
    let name = bad.split_once('=').map_or(bad.as_str(), |(n, _)| n);
    let mut message = format!("{} {}: unknown flag {:?}", crate::bin_name(), label, name);
    if bad.contains(char::is_whitespace) {
        message.push_str(" (one argument holding a space: pass each flag as its own word)");
    }
    eprintln!("{}", message);
    eprintln!("  {} accepts: {}", label, flags.describe());
    if let Some(usage) = crate::help::usage(command, rest) {
        eprint!("{}", usage);
    }
    // A verb with a JSON report keeps its contract for a caller that asked
    // for one: the failure as `{error, code, exit}` on stdout, init's shape.
    if flags.accepts("json") && argv.iter().any(|a| a == "--json") {
        print!(
            "{}",
            crate::json::pretty(&crate::json::object(vec![
                ("error", serde_json::Value::from(message.as_str())),
                ("code", serde_json::Value::from("unknown-flag")),
                ("exit", serde_json::Value::from(2)),
            ]))
        );
    }
    Err(2)
}

/// Run one subcommand: `argv` is everything after the binary name. A
/// `--help` or `-h` after a known command prints that command's usage on
/// stdout and returns 0 before the subcommand sees its arguments, so no
/// spelling of the flag reads or writes anything (ADR 0086). Then a flag
/// the command or verb does not declare ([`flags`]) is a usage error, exit
/// 2, before the subcommand runs (ADR 0097).
pub fn dispatch(argv: &[String]) -> i32 {
    let Some((command, rest)) = argv.split_first() else {
        print!("{}", crate::help::usage_text());
        return 0;
    };
    match command.as_str() {
        "--version" | "-V" => return version(rest),
        "--help" | "-h" | "help" => {
            print!("{}", crate::help::usage_text());
            return 0;
        }
        _ => {}
    }
    let found = COMMANDS
        .iter()
        .find(|(name, _)| name == command)
        .copied()
        .or_else(|| crate::target::command(command).map(|(_, c)| (c.name, c.entry)));
    let Some((name, entry)) = found else {
        eprintln!(
            "{}: unknown command \"{}\" (try --help)",
            crate::bin_name(),
            command
        );
        return 1;
    };
    if crate::help::requested(rest) {
        // Every name in COMMANDS has a usage (tests/integration/help.rs);
        // the fallback keeps the promise even if one is ever missing.
        let text = crate::help::usage(name, rest).unwrap_or_else(|| crate::help::usage_text());
        print!("{}", text);
        return 0;
    }
    if let Err(code) = check_flags(name, rest) {
        return code;
    }
    match select_target(name, rest) {
        Ok(Some(target_entry)) => target_entry(rest),
        Ok(None) => entry(rest),
        Err(code) => code,
    }
}

/// Whether `command` (with `verb`) reads a workspace, so that the
/// workspace's `skill.name` picks the target it runs against. `init` picks
/// its own with `--target`; the rest read files, or nothing.
fn takes_workspace(command: &str, verb: Option<&str>) -> bool {
    !matches!(
        (command, verb),
        ("init", _)
            | ("version", _)
            | ("yaml2json", _)
            | (
                "inventory",
                Some("build" | "list" | "describe" | "diff" | "validate")
            )
            | ("inventory", None)
    )
}

/// The workspace directory a command line names: its first positional
/// after the verb when that is a directory, else the working directory
/// (every workspace command defaults to `.`; a positional that is not a
/// directory, such as `source-coverage`'s OP-KEY given alone, is not one).
fn workspace_arg(command: &str, verb: Option<&str>, rest: &[String]) -> PathBuf {
    let argv = if verb.is_some() { &rest[1..] } else { rest };
    let flags = flags(command, verb).unwrap_or(&Flags::NONE);
    crate::args::Args::parse(argv, flags)
        .positional
        .first()
        .map(PathBuf::from)
        .filter(|p| p.is_dir())
        .unwrap_or_else(|| PathBuf::from("."))
}

/// Fix the target this command runs against (ADR 0114, Phase 8c): the
/// registered one the workspace's `skill.name` names, when the command
/// reads a workspace that records one; otherwise the target that registers
/// the command, or the first registered (`init` picks its own, from
/// `--target`). `Ok(Some(entry))` is a target
/// command's entry point, taken from the selected target; `Ok(None)` runs
/// the core command. `Err` is the exit code of a refusal already printed:
/// 1 for a workspace naming no registered target (`target-unknown`), or a
/// target command the workspace's target does not register.
fn select_target(command: &str, rest: &[String]) -> Result<Option<Entry>, i32> {
    use crate::target;
    let verb = rest
        .first()
        .map(String::as_str)
        .filter(|v| crate::help::is_verb(command, v));
    let label = match verb {
        Some(v) => format!("{} {}", command, v),
        None => command.to_string(),
    };
    let core = COMMANDS.iter().any(|(n, _)| *n == command);
    let owner = if core {
        None
    } else {
        target::command(command).map(|(t, _)| t)
    };
    let registered = target::registered();
    let mut dir = None;
    let mut chosen = None;
    if takes_workspace(command, verb) {
        let at = workspace_arg(command, verb, rest);
        match target::for_workspace(registered, &at) {
            Ok(t) => chosen = t,
            Err(unknown) => {
                refuse(
                    command,
                    verb,
                    rest,
                    // A product binary is named for its one target, so the
                    // recorded name is the binary that opens the workspace.
                    &format!(
                        "{} {}: {}: {}; this workspace is a {} workspace: run `{} {} …`",
                        crate::bin_name(),
                        label,
                        at.display(),
                        unknown,
                        unknown.recorded,
                        unknown.recorded,
                        label
                    ),
                    "target-unknown",
                );
                return Err(1);
            }
        }
        dir = Some(at);
    }
    // With neither, `active()` stays the first registered target, and
    // `init` is still free to select the one `--target` names.
    if let Some(t) = chosen.or(owner) {
        target::select(t);
    }
    if core {
        return Ok(None);
    }
    let active = target::active();
    match active.commands.iter().find(|c| c.name == command) {
        Some(c) => Ok(Some(c.entry)),
        None => {
            refuse(
                command,
                verb,
                rest,
                &format!(
                    "{} {}: {} is a {} workspace, and {} registers no {:?} command (it is {}'s)",
                    crate::bin_name(),
                    label,
                    dir.as_deref()
                        .unwrap_or(std::path::Path::new("."))
                        .display(),
                    active.name,
                    active.name,
                    command,
                    owner.map_or("another target", |t| t.name)
                ),
                "target-command",
            );
            Err(1)
        }
    }
}

/// Print a selection refusal: on stderr, and for a verb that has a JSON
/// report and was asked for one, as `{error, code, exit: 1}` on stdout too.
fn refuse(command: &str, verb: Option<&str>, rest: &[String], message: &str, code: &str) {
    eprintln!("{}", message);
    let json_asked = rest.iter().any(|a| a == "--json")
        && flags(command, verb).is_some_and(|f| f.accepts("json"));
    if json_asked {
        print!(
            "{}",
            crate::json::pretty(&crate::json::object(vec![
                ("error", serde_json::Value::from(message)),
                ("code", serde_json::Value::from(code)),
                ("exit", serde_json::Value::from(1)),
            ]))
        );
    }
}
