//! The Molly `.ded` lexer and parser (LANG-220, LANGUAGE §21.1, ARCHITECTURE §13.12).
//!
//! Molly's dialect is small: `include` lines, facts `p(…)@k;` and rules whose heads may carry `@next` or `@async`,
//! with `notin`, head aggregates `count<X>`/`min<X>`/`max<X>`/`sum<X>`, absolute-time body atoms `p(…)@k`, and
//! right-nested, precedence-free expressions. This module turns one file's text into a [`DedFile`]; resolving
//! `include`s, type inference and lowering to the IR are `blossom-front::ded`'s.
//!
//! The parser recovers at the next `;` after an error, so one pass reports every malformed clause.

mod ast;
mod lexer;
mod parser;
#[cfg(test)]
mod tests;

pub use ast::*;
pub use lexer::{Token, TokenKind, lex};
pub use parser::parse;
