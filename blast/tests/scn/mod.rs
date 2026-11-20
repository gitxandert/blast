//! Support for rendered scenario checks (SCN): synthetic assets, signal
//! measurements and the `# scn:` / `# expect:` language. The runner is
//! `tests/scenarios.rs`.

pub mod assets;
pub mod expect;
pub mod measure;
pub mod sha256;
