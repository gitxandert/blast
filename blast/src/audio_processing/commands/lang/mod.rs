//! The BLAST language front end: source text to syntax tree (LANGa).
//!
//! Implements the grammar approved in `plans/design/REQ-056.md` (rev 2):
//! tokens in §1, the EBNF in §2, errors in §7. This module only parses;
//! nothing here touches the engine or the session. Counting in steps is
//! 1-based as written; conversion happens when the tree is lowered.

pub mod ast;
pub mod errors;
pub mod lexer;
pub mod parser;

pub mod card;
pub mod help;
pub mod lower;
pub mod names;
pub mod runner;
pub mod schema;
pub mod script;
mod session;
pub mod state;
pub mod vars;

#[cfg(test)]
mod buffer_save_tests;
#[cfg(test)]
mod buffer_tests;
#[cfg(test)]
mod env_tests;
#[cfg(test)]
mod lfo_tests;
#[cfg(test)]
mod lower_tests;
#[cfg(test)]
mod runner_tests;
#[cfg(test)]
mod script_tests;
#[cfg(test)]
mod wave_tests;

pub use card::card;
pub use errors::{Code, LangErr, Source, Span, suggest};
pub use parser::{Parse, parse};
