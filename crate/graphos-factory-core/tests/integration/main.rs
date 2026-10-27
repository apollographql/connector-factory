//! Every integration test, compiled as one test binary.
//!
//! Cargo builds each `tests/*.rs` file as its own executable, and each one
//! statically links the whole `service_factory` library. With 41 files that
//! was 41 links per build and ~424 MB of test executables per checkout (ADR
//! 0061). Each file is now a module here: the files and their namespaces are
//! unchanged, only the compilation unit is shared.
//!
//! Run one file's tests with `cargo test --test integration lint::`, or one
//! test with `cargo test --test integration lint::some_case`.
//!
//! A new test file goes in this directory with a `mod` line below. A file
//! placed directly in `tests/` would become a separate binary again.
//! Modules share one process, so a test must not mutate process-global state
//! (environment variables, the working directory).

mod abstract_types;
mod batch;
mod batch_emit;
mod behaviour_facts;
mod broken_pipe;
mod codify;
mod conformance;
mod context;
mod decisions;
mod decisions_directory;
mod decisions_split;
mod elv2;
mod entity;
mod envelope;
mod evidence;
mod expansion;
mod findings;
mod fixtures;
mod flags;
mod foreign_types;
mod help;
mod infer;
mod inference;
mod init;
mod inventory;
mod inventory_links;
mod inventory_supplement;
mod jsonschema;
mod links;
mod lint;
mod live_script;
mod obligations_grammar;
mod op_match;
mod openapi;
mod patch;
mod provenance;
mod reconcile;
mod reconcile_zero_match;
mod refresh;
mod render;
mod request_serialization_fixtures;
mod resolve_bin;
mod scaffold;
mod schema_read_custody;
mod scrub;
mod selection;
mod selection_review;
mod selection_set;
mod skill_doc;
mod source_coverage_check;
mod sources;
mod spans;
mod sparse;
mod stale_omit;
mod swagger;
mod target;
mod unit_layer;
mod validate;
mod yaml;
