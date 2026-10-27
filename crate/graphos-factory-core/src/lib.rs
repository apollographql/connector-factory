//! graphos-factory-core: the instruments every factory skill shares.
//!
//! Everything here is mechanical work an agent should not do by hand —
//! reading a spec into an inventory, checking invariants, diffing schema
//! against selection, scrubbing recorded traffic, running the validation
//! layers and writing evidence. None of it makes a design decision.

pub mod args;
pub mod batch;
pub mod closure;
pub mod conformance;
pub mod context;
pub mod decisions;
pub mod entity;
pub mod env;
pub mod envelope;
pub mod factory_io;
pub mod findings;
pub mod graphql;
pub mod help;
pub mod infer;
pub mod inputs;
pub mod inventory;
pub mod json;
pub mod json_accounting;
pub mod jsonschema;
pub mod lint;
pub mod obligations;
pub mod op_match;
pub mod openapi;
pub mod patch;
pub mod provenance;
pub mod reconcile;
pub mod record_log;
pub mod refresh;
pub mod render;
pub mod request_serialization;
pub mod schemas;
pub mod scrub;
pub mod sdl_index;
pub mod sources;
pub mod spans;
pub mod sparse;
pub mod spec;
pub mod split;
pub mod success_shape;
pub mod swagger;
pub mod target;
pub mod waivers;
pub mod yaml;

/// The HTTP methods a path item can carry (OpenAPI 3 and Swagger 2.0 alike).
pub fn http_methods() -> [&'static str; 8] {
    [
        "get", "put", "post", "delete", "patch", "head", "options", "trace",
    ]
}

/// Today's UTC date as `YYYY-MM-DD`.
pub fn today() -> String {
    let now = time::OffsetDateTime::now_utc();
    let format = time::macros::format_description!("[year]-[month]-[day]");
    now.format(&format)
        .unwrap_or_else(|_| "1970-01-01".to_string())
}

pub mod cmd;

/// Run one command line (everything after the binary name) as the binary
/// `name`, against the targets its composition root registers; the exit
/// code to return.
pub fn run(name: &'static str, targets: &'static [target::Target], argv: &[String]) -> i32 {
    default_sigpipe();
    let _ = BIN.set(name);
    target::register(targets);
    cmd::dispatch(argv)
}

/// Give SIGPIPE back its default action, as every Unix filter has it, so a
/// reader that closes the pipe early (`| head -1`) ends this process quietly
/// instead of `print!` panicking on EPIPE. The Rust runtime ignores the
/// signal by default; output goes through `print!`/`println!` at hundreds of
/// sites, so the one place to decide this is the process start, before any
/// thread exists. Nothing here writes to a child's stdin or a socket, and
/// `std::process::Command` already resets the signal in every child, so no
/// command relied on surviving EPIPE. A non-Unix build keeps the runtime's
/// behaviour.
fn default_sigpipe() {
    #[cfg(unix)]
    // SAFETY: called once at process start, single-threaded, with a valid
    // signal number and the default disposition; no handler is installed.
    unsafe {
        libc::signal(libc::SIGPIPE, libc::SIG_DFL);
    }
}

/// The name every product installs beside its own binary, as a link to it,
/// and the one the shared text spells: a command copied from a core
/// reference or message runs in either product (ADR 0114, Phase 8f).
pub const CORE_NAME: &str = "graphos-factory-core";

static BIN: std::sync::OnceLock<&'static str> = std::sync::OnceLock::new();

/// The binary's name as files record it and its usage prints it: the
/// constant its composition root hands [`run`] (a product's own name, or
/// `graphos-factory-bare` for the core alone), and
/// [`CORE_NAME`] when nothing ran through [`run`] (the core called as a
/// library). Never argv[0], so a lock written through the
/// `graphos-factory-core` link (or any copy under another file name)
/// records the same writer as one written by the binary itself, and a
/// relock does not churn on how the binary happened to be invoked.
pub fn bin_name() -> &'static str {
    BIN.get().copied().unwrap_or(CORE_NAME)
}

/// What a file this binary writes records as its writer: the binary's name
/// and version.
pub fn written_by() -> String {
    format!("{} {}", bin_name(), env!("CARGO_PKG_VERSION"))
}

/// ISO 8601 with milliseconds and a trailing Z, like `Date.toISOString()`.
pub fn now_iso() -> String {
    let now = time::OffsetDateTime::now_utc();
    let format = time::macros::format_description!(
        "[year]-[month]-[day]T[hour]:[minute]:[second].[subsecond digits:3]Z"
    );
    now.format(&format)
        .unwrap_or_else(|_| "1970-01-01T00:00:00.000Z".to_string())
}
