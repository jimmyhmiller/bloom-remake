//! The registry of every diagnostic code (ARCHITECTURE §1.6, §12.1; LANGUAGE §20).
//!
//! LANGUAGE §20 allocates every `BLSnnnn` (compile time) and `BLSRnnn` (runtime) code; ARCHITECTURE §0.3 adds
//! BLSR011 (L1), BLS0908 (L2), BLS1009 (L3) and BLS1010 (L4) and extends BLS1003 (L7). [`REGISTRY`] records each
//! code's severity, its owning crate, the other crates allowed to construct it, and its one-line meaning, so
//! parallel work never has to invent or share a number.
//!
//! Rules, checked by `cargo run -p xtask -- check-codes`:
//! - the registry equals LANGUAGE §20 plus the §0.3 amendments (codes, severities and meanings);
//! - every code written in `crates/*/src` is registered;
//! - outside `#[cfg(test)]` code, a code is constructed only in its [`owner_crate`](CodeInfo::owner_crate) or in
//!   one of the crates listed in [`also`](CodeInfo::also).
//!
//! Construct codes with [`code!`](crate::code), which rejects an unregistered code at compile time:
//! `Diagnostic::new(code!("BLS0502"), "…")`. BLS0908 is built only through
//! [`Diagnostic::not_implemented`](crate::Diagnostic::not_implemented).
//!
//! Owners follow ARCHITECTURE §13.1: lexer and parser → `blossom-syntax`; resolution, type checking,
//! classification, specs and the schema lock → `blossom-front`; stratification, CALM, determinism and ACL analyses
//! → `blossom-analysis`; runtime errors → `blossom-engine`, which the independent reference evaluator
//! `blossom-oracle` must reproduce code for code (ARCHITECTURE §11.2). Changing an entry follows the frozen-crate
//! procedure of ARCHITECTURE §1.6.

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::diag::Severity;
use crate::diag::Severity::{Error, Runtime, Warning};

/// Where a code was allocated.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub enum CodeOrigin {
    /// Allocated by LANGUAGE §20 and used as written there.
    Language,
    /// Added by the named ARCHITECTURE §0.3 amendment (`"L1"` …).
    Amendment(&'static str),
    /// Allocated by LANGUAGE §20 and extended by the named ARCHITECTURE §0.3 amendment.
    Extended(&'static str),
}

/// One registered code.
#[derive(Copy, Clone, PartialEq, Eq, Hash, Debug)]
pub struct CodeInfo {
    /// The code, e.g. `BLS0502`.
    pub code: &'static str,
    /// Its default severity: **E** error, **W** warning (an error under `--strict`), **R** runtime hard error.
    pub severity: Severity,
    /// The crate that owns the code and normally constructs it.
    pub owner_crate: &'static str,
    /// Other crates allowed to construct it (for example the oracle for runtime codes).
    pub also: &'static [&'static str],
    /// Where the code was allocated.
    pub origin: CodeOrigin,
    /// The one-line meaning, as LANGUAGE §20 (or the ARCHITECTURE §0.3 amendment) states it.
    pub meaning: &'static str,
}

impl CodeInfo {
    /// Whether `crate_name` may construct this code.
    pub fn may_be_constructed_in(&self, crate_name: &str) -> bool {
        self.owner_crate == crate_name || self.also.contains(&crate_name)
    }
}

const fn info(
    code: &'static str,
    severity: Severity,
    owner_crate: &'static str,
    also: &'static [&'static str],
    origin: CodeOrigin,
    meaning: &'static str,
) -> CodeInfo {
    CodeInfo {
        code,
        severity,
        owner_crate,
        also,
        origin,
        meaning,
    }
}

/// Every registered code, sorted by code.
pub static REGISTRY: &[CodeInfo] = &[
    info(
        "BLS0001",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "unexpected character",
    ),
    info(
        "BLS0002",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "unterminated string, byte string or block comment",
    ),
    info(
        "BLS0003",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "unknown numeric suffix (`10kb`)",
    ),
    info(
        "BLS0004",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "a hard keyword followed by `!(` (`not!(…)`): bangs mark operators, not keywords",
    ),
    info(
        "BLS0005",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "unknown escape in a string",
    ),
    info(
        "BLS0100",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "unexpected token, with the expected-token set",
    ),
    info(
        "BLS0101",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "missing `;` (inserted during recovery)",
    ),
    info(
        "BLS0102",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "`let` as a statement: bind it in the header or an `if`/`for` condition",
    ),
    info(
        "BLS0103",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "chained comparison or range (`a < b < c`)",
    ),
    info(
        "BLS0104",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "struct literal in a no-struct context: parenthesize it",
    ),
    info(
        "BLS0105",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "a label not followed by `on`, `while` or `monotone`",
    ),
    info(
        "BLS0106",
        Error,
        "blossom-syntax",
        // The parser checks clause repetition; the frontend checks a clause against the relation's kind (§12).
        &["blossom-front"],
        CodeOrigin::Language,
        "a declaration clause given twice, or on a kind that does not take it",
    ),
    info(
        "BLS0107",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "an aggregate clause (`default`, `per`, `by`, …) outside the call's parentheses",
    ),
    info(
        "BLS0108",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "`after` on a non-`stable` fn, or a `stable fn` without `after`",
    ),
    info(
        "BLS0109",
        Error,
        "blossom-syntax",
        &[],
        CodeOrigin::Language,
        "an `if` expression without `else`",
    ),
    info(
        "BLS0110",
        Error,
        "blossom-front",
        &["blossom-syntax"],
        CodeOrigin::Language,
        "an item where it may not appear (§6.2), or `pub` on a relation",
    ),
    info(
        "BLS0200",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "unknown name",
    ),
    info(
        "BLS0201",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "duplicate declaration, alias or label; a reserved name (`lset`, `set`, `map`, …) declared",
    ),
    info(
        "BLS0202",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a relation used as a function or a function used as a relation",
    ),
    info(
        "BLS0203",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "not an interface of the instance (`data.outbox`)",
    ),
    info(
        "BLS0204",
        Error,
        "blossom-front",
        &["blossom-driver"],
        CodeOrigin::Language,
        "unknown module, protocol or file path",
    ),
    info(
        "BLS0205",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "import arguments: unknown parameter, unbound relation parameter, column types that do not match",
    ),
    info(
        "BLS0206",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "protocol conformance (a redeclared interface differs, an extra interface) or role binding (unbound role, kinds differ)",
    ),
    info(
        "BLS0207",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`override` with nothing to override, or a same-name block without `override`",
    ),
    info(
        "BLS0208",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "interposition on a non-interface, or on one interface twice",
    ),
    info(
        "BLS0209",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a hard keyword used as a binding: write `r#kw`",
    ),
    info(
        "BLS0210",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "unknown attribute, or an attribute on an item that does not take it",
    ),
    info(
        "BLS0211",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a `const`, `param`, module value parameter or spec node name that is not SCREAMING_CASE",
    ),
    info(
        "BLS0212",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`from`/`principal` on an atom that is not a channel or loopback",
    ),
    info(
        "BLS0213",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a recursive function (functions are total, §16.1)",
    ),
    info(
        "BLS0214",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a `let` block outside a function body, or a closure that is not a combinator's argument in one (§16.1)",
    ),
    info(
        "BLS0215",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a function body that reads a relation, `now()`, `tick()`, `self`, randomness or a role's members (§16.1)",
    ),
    info(
        "BLS0216",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "an `extern fn` that names no host function of the standard library, or declares a different signature (§16.2)",
    ),
    info(
        "BLS0217",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "an evaluation deeper than the bound the evaluators' stacks are sized for (§16.1)",
    ),
    info(
        "BLS0218",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`?` where it cannot return early: outside a function returning `Option`, under a branch, the right of `&&`/`||`, a nested block or a closure (§16.1)",
    ),
    info(
        "BLS0219",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a function type outside a function's parameter list, a function parameter neither called nor passed on, or a function argument that is not a named function (§16.1)",
    ),
    info(
        "BLS0220",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "generic functions instantiated more than the bound allows (each call is an instance; nested generic calls multiply them) (§16.1)",
    ),
    info(
        "BLS0300",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "type mismatch, listing every piece of evidence (LANG-021)",
    ),
    info(
        "BLS0301",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "arity mismatch",
    ),
    info(
        "BLS0302",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "unknown field in a named atom, or omitted non-`since` columns without `..`",
    ),
    info(
        "BLS0303",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`..` in a head, or a head missing a column that has no default",
    ),
    info(
        "BLS0304",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a lattice column as a key, join key or group key",
    ),
    info(
        "BLS0305",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`==`/`!=` on lattice values",
    ),
    info(
        "BLS0306",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a lattice comparison in the non-threshold direction: use `reveal!` or `not (x > c)`",
    ),
    info(
        "BLS0307",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a group-typed payload on a channel without `exactly_once` (ANA-015)",
    ),
    info(
        "BLS0308",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "an enum that reaches a channel, durable relation or interface without an `#[unknown]` variant",
    ),
    info(
        "BLS0309",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a constant expression overflows",
    ),
    info(
        "BLS0310",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a view column's type cannot be inferred",
    ),
    info(
        "BLS0311",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a lattice lift with no expected lattice type",
    ),
    info(
        "BLS0312",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`f64` as the element of `LMax`/`LMin`",
    ),
    info(
        "BLS0313",
        Error,
        "blossom-front",
        &["blossom-analysis"],
        CodeOrigin::Language,
        "a possibly negative contribution into a `bag`",
    ),
    info(
        "BLS0314",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a `match` that does not cover every value of its scrutinee",
    ),
    info(
        "BLS0315",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a `Conn` in a channel or a durable relation",
    ),
    info(
        "BLS0400",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a verb that cannot target this kind (§12)",
    ),
    info(
        "BLS0401",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a write into a `sealed table` outside bootstrap",
    ),
    info(
        "BLS0402",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a durable relation written in a plain `bootstrap` (use `bootstrap fresh`)",
    ),
    info(
        "BLS0403",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`send` destination: missing `to`, `to` on a loopback, on a column-form channel or on a partitioned channel",
    ),
    info(
        "BLS0404",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a send, receive, read or write placed at a role the relation does not live at",
    ),
    info(
        "BLS0405",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`fact` into a non-`static` relation (or, in a spec, into an input without `at tick`)",
    ),
    info(
        "BLS0406",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a write into an own `input`, an instance `output`, a relation parameter or a `view`",
    ),
    info(
        "BLS0407",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`seal` of a relation without `sealed by`, or naming other than exactly its seal key",
    ),
    info(
        "BLS0408",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a rule outside every `at` section in a multi-role module, or placed at an external role",
    ),
    info(
        "BLS0409",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`else` after a condition that is not a single scalar guard",
    ),
    info(
        "BLS0410",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`delete`/`upsert` on a lattice-valued relation (LANG-284)",
    ),
    info(
        "BLS0411",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`resolve prefer(…)` naming no handler, one twice, or one that does not write the table with `next` or `upsert` (§10.7)",
    ),
    info(
        "BLS0412",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a timer's `while` guard that is not a view or table placed where the timer is (§15.2)",
    ),
    info(
        "BLS0500",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "range restriction: a variable of a head, negation, guard, `to` or `weight` is not bound (ANA-001)",
    ),
    info(
        "BLS0501",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`let` re-binds a variable: write `x == e`; a match arm in a rule re-binds a rule variable",
    ),
    info(
        "BLS0502",
        Error,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "a negative edge on a same-tick cycle, with the cycle as a path of surface constructs (ANA-002)",
    ),
    info(
        "BLS0503",
        Error,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "a choice or order-sensitive site on a same-tick recursive cycle (SEM-086)",
    ),
    info(
        "BLS0504",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`on` without a positive event literal, with the chain that makes the header standing",
    ),
    info(
        "BLS0505",
        Warning,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`while` with an event literal: write `on`",
    ),
    info(
        "BLS0506",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a handler emits a relation it tests negatively (fix-it: `next`; override: `#[allow(self_negation)]`)",
    ),
    info(
        "BLS0507",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a relation atom after `where`",
    ),
    info(
        "BLS0508",
        Error,
        "blossom-front",
        &["blossom-analysis"],
        CodeOrigin::Language,
        "a body atom at another location without `#[localize]` (ANA-004)",
    ),
    info(
        "BLS0509",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a spec-only oracle or trace relation in a program rule (ANA-010)",
    ),
    info(
        "BLS0511",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a `default` with grouping columns but no `per` driver, a driver that does not determine the group, or, with a driver, an aggregate with no identity (`min!`, `max!`, `index!`) and no `default`",
    ),
    info(
        "BLS0600",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a seeded site (`choose*!`, `seq!`) in an unlabelled handler or a multi-alternative view",
    ),
    info(
        "BLS0601",
        Warning,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "ANA-011: `index!`/`top!`/carried `fold!` over persistent input, or an uncaptured `rand` over persistent input",
    ),
    info(
        "BLS0602",
        Warning,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "`seq!` numbers that reach a `send` or an output without `durable`",
    ),
    info(
        "BLS0603",
        Error,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "a `#[deterministic]` output is schedule-dependent",
    ),
    info(
        "BLS0604",
        Error,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "a lattice-typed variable among a choice's chosen columns",
    ),
    info(
        "BLS0605",
        Warning,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "a `choose` into a keyed relation whose determinant is not within the key",
    ),
    info(
        "BLS0700",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a non-monotone operation without its bang (fix-it inserts it)",
    ),
    info(
        "BLS0701",
        Warning,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a superfluous bang on a monotone operation",
    ),
    info(
        "BLS0702",
        Error,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "a `monotone` region contains points of order (each is listed)",
    ),
    info(
        "BLS0703",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a `stable` method read without its threshold guard in the same body, and without a bang",
    ),
    info(
        "BLS0704",
        Error,
        "blossom-verify",
        &["blossom-lattice"],
        CodeOrigin::Language,
        "a law or class claim refuted by the harness or the SMT backend",
    ),
    info(
        "BLS0705",
        Error,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "a `final output` that ANA-120 cannot classify, or that is NEVER-FINAL",
    ),
    info(
        "BLS0706",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`DomPair` outside `unsafe`",
    ),
    info(
        "BLS0707",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`threshold(…)` values that are not pairwise incompatible",
    ),
    info(
        "BLS0800",
        Error,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "an explicit ACL excludes a role the program sends from (ANA-105)",
    ),
    info(
        "BLS0802",
        Warning,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "a payload column used as an identity without being equated with `from`/`principal` (ANA-106)",
    ),
    info(
        "BLS0803",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "an unanimous `sealed c(…)` read with no producer set",
    ),
    info(
        "BLS0804",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`partition by` on a `Node -> Node` channel without `over`",
    ),
    info(
        "BLS0805",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a localized body that is not well-connected, or mirrors a relation with deletions",
    ),
    info(
        "BLS0900",
        Error,
        "blossom-front",
        &["blossom-ldfi"],
        CodeOrigin::Language,
        "`check ldfi` without `pre`, `post` or `faults` (CR-30)",
    ),
    info(
        "BLS0901",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a spec rule that would feed a protocol relation",
    ),
    info(
        "BLS0902",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`prove … using …` names an unknown invariant",
    ),
    info(
        "BLS0903",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a schema change without a version bump or lock update (TEST-108)",
    ),
    info(
        "BLS0904",
        Error,
        "blossom-front",
        &["blossom-analysis", "blossom-schema"],
        CodeOrigin::Language,
        "ANA-100 compatibility: a reused field number, a changed type, a field added without a default, a `semantics_changed` field without a new number",
    ),
    info(
        "BLS0905",
        Error,
        "blossom-analysis",
        &["blossom-front"],
        CodeOrigin::Language,
        "a `since` feature written or sent without a `cluster_version()` gate (ANA-102)",
    ),
    info(
        "BLS0906",
        Error,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "a forbidden construct in a `migrate` or `translate` block, or a translation that is not tuple-local",
    ),
    info(
        "BLS0907",
        Error,
        "blossom-front",
        &["blossom-syntax"],
        CodeOrigin::Language,
        "a P2 feature reserved for a later edition (entanglement, `Signed<T>`, `migrate … down`, `#[blazes]`)",
    ),
    info(
        "BLS0908",
        Error,
        "blossom-base",
        &[],
        CodeOrigin::Amendment("L2"),
        "not implemented in this build: FEATURE-ID (what), needed by LABEL",
    ),
    info(
        "BLS1001",
        Warning,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "naming convention",
    ),
    info(
        "BLS1002",
        Warning,
        "blossom-analysis",
        &[],
        CodeOrigin::Language,
        "unused variable, relation or interface (ANA-008)",
    ),
    info(
        "BLS1003",
        Warning,
        "blossom-front",
        &["blossom-analysis"],
        CodeOrigin::Extended("L7"),
        "possible same-tick key conflict, with a two-message example (ANA-007); extended (ARCHITECTURE §0.3 L7) to channels whose key excludes the sender while more than one node may send into them",
    ),
    info(
        "BLS1004",
        Warning,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "wildcard under an aggregate",
    ),
    info(
        "BLS1005",
        Warning,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "localized cross-node body",
    ),
    info(
        "BLS1006",
        Warning,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "logical timer in a deployed build",
    ),
    info(
        "BLS1007",
        Warning,
        "blossom-front",
        &["blossom-analysis"],
        CodeOrigin::Language,
        "soft-state TTL shorter than a body's (ANA-006)",
    ),
    info(
        "BLS1008",
        Warning,
        "blossom-front",
        &[],
        CodeOrigin::Language,
        "`if` that binds variables or `for` that binds none",
    ),
    info(
        "BLS1009",
        Warning,
        "blossom-front",
        &[],
        CodeOrigin::Amendment("L3"),
        "a level-triggered statement writes a `zset`/`bag` table: its weight is added at every tick the header holds",
    ),
    info(
        "BLS1010",
        Warning,
        "blossom-analysis",
        &["blossom-driver"],
        CodeOrigin::Amendment("L4"),
        "a level-triggered `send` in a deployed program whose node has neither a timer nor a heartbeat that would make it resend while idle",
    ),
    info(
        "BLSR001",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "key violation (SEM-050), naming both derivations",
    ),
    info(
        "BLSR002",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "conflicting upserts (SEM-051), naming both statements",
    ),
    info(
        "BLSR003",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "an invariant with `abort`",
    ),
    info(
        "BLSR004",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "arithmetic overflow, division by zero, out-of-range cast, weight overflow",
    ),
    info(
        "BLSR005",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "`collect_map!` duplicate key",
    ),
    info(
        "BLSR006",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "`LPoint` conflict",
    ),
    info(
        "BLSR007",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "an in-tick fixpoint that does not converge (CR-53)",
    ),
    info(
        "BLSR008",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "a write at a non-owner of a partitioned table",
    ),
    info(
        "BLSR009",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "a host insert into a sealed input key",
    ),
    info(
        "BLSR010",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "`error(\"…\")` in a function",
    ),
    info(
        "BLSR011",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Amendment("L1"),
        "an exactly-once dot reused with a different payload",
    ),
    info(
        "BLSR012",
        Runtime,
        "blossom-engine",
        &["blossom-oracle"],
        CodeOrigin::Language,
        "a pure function's evaluation exceeds its step budget",
    ),
];

/// A registered diagnostic code. Only codes in [`REGISTRY`] can be represented.
#[derive(Copy, Clone)]
pub struct Code(&'static CodeInfo);

impl Code {
    /// The registered code with this text.
    pub fn lookup(code: &str) -> Option<Code> {
        REGISTRY
            .binary_search_by(|info| info.code.cmp(code))
            .ok()
            .and_then(|i| REGISTRY.get(i))
            .map(Code)
    }

    /// Compile-time lookup, used by [`code!`](crate::code).
    pub const fn lookup_const(code: &str) -> Option<Code> {
        let mut rest: &'static [CodeInfo] = REGISTRY;
        while let [first, tail @ ..] = rest {
            if str_eq(first.code, code) {
                return Some(Code(first));
            }
            rest = tail;
        }
        None
    }

    /// Used by [`code!`](crate::code); evaluated at compile time, where an unregistered code is a compile error.
    #[doc(hidden)]
    #[allow(clippy::panic)] // Only evaluated in a `const` item: the panic is a compile-time error, never a runtime one.
    pub const fn __registered(code: &str) -> Code {
        match Code::lookup_const(code) {
            Some(c) => c,
            None => panic!("unregistered diagnostic code: add it to LANGUAGE §20 and blossom_base::codes::REGISTRY"),
        }
    }

    /// The code text, e.g. `BLS0502`.
    pub const fn as_str(self) -> &'static str {
        self.0.code
    }

    /// The registry entry.
    pub const fn info(self) -> &'static CodeInfo {
        self.0
    }

    /// The default severity.
    pub const fn severity(self) -> Severity {
        self.0.severity
    }
}

const fn str_eq(a: &str, b: &str) -> bool {
    let (mut a, mut b) = (a.as_bytes(), b.as_bytes());
    loop {
        match (a, b) {
            ([], []) => return true,
            ([x, rest_a @ ..], [y, rest_b @ ..]) if *x == *y => {
                a = rest_a;
                b = rest_b;
            }
            _ => return false,
        }
    }
}

/// A registered [`Code`], checked at compile time: `code!("BLS0502")`. An unregistered code does not compile.
#[macro_export]
macro_rules! code {
    ($code:literal) => {{
        const CODE: $crate::codes::Code = $crate::codes::Code::__registered($code);
        CODE
    }};
}

impl PartialEq for Code {
    fn eq(&self, other: &Self) -> bool {
        self.0.code == other.0.code
    }
}

impl Eq for Code {}

impl std::hash::Hash for Code {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.0.code.hash(state);
    }
}

impl PartialOrd for Code {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Code {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.0.code.cmp(other.0.code)
    }
}

impl fmt::Debug for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.code)
    }
}

impl fmt::Display for Code {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.0.code)
    }
}

impl Serialize for Code {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.0.code)
    }
}

impl<'de> Deserialize<'de> for Code {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let text = <std::borrow::Cow<'de, str>>::deserialize(deserializer)?;
        Code::lookup(&text).ok_or_else(|| serde::de::Error::custom(format!("unregistered diagnostic code `{text}`")))
    }
}

/// The crates of ARCHITECTURE §1.2 that may own or construct codes.
pub const CRATES: &[&str] = &[
    "blossom-analysis",
    "blossom-base",
    "blossom-driver",
    "blossom-engine",
    "blossom-front",
    "blossom-lattice",
    "blossom-ldfi",
    "blossom-oracle",
    "blossom-plan",
    "blossom-runtime",
    "blossom-schema",
    "blossom-syntax",
    "blossom-verify",
];

#[cfg(test)]
mod tests {
    use super::*;

    fn is_code_text(code: &str) -> bool {
        let digits = |s: &str, n: usize| s.len() == n && s.bytes().all(|b| b.is_ascii_digit());
        match code.strip_prefix("BLS") {
            Some(rest) => match rest.strip_prefix('R') {
                Some(runtime) => digits(runtime, 3),
                None => digits(rest, 4),
            },
            None => false,
        }
    }

    #[test]
    fn codes_registry_unique() {
        assert_eq!(
            REGISTRY.len(),
            126,
            "LANGUAGE §20 has 122 codes; ARCHITECTURE §0.3 adds 4"
        );
        for (a, b) in REGISTRY.iter().zip(REGISTRY.iter().skip(1)) {
            assert!(
                a.code < b.code,
                "REGISTRY must be sorted and unique: {} then {}",
                a.code,
                b.code
            );
        }
    }

    #[test]
    fn codes_registry_well_formed() {
        for info in REGISTRY {
            assert!(is_code_text(info.code), "{}", info.code);
            assert!(!info.meaning.is_empty(), "{}", info.code);
            assert!(
                CRATES.contains(&info.owner_crate),
                "{}: unknown owner {}",
                info.code,
                info.owner_crate
            );
            for other in info.also {
                assert!(CRATES.contains(other), "{}: unknown crate {other}", info.code);
                assert_ne!(*other, info.owner_crate, "{}: owner repeated in `also`", info.code);
            }
            // Runtime codes are exactly the BLSR codes; the BLS1xxx lints are warnings.
            assert_eq!(info.code.starts_with("BLSR"), info.severity == Runtime, "{}", info.code);
            if info.code.starts_with("BLS1") {
                assert_eq!(info.severity, Warning, "{}", info.code);
            }
            if info.severity == Runtime {
                assert_eq!(info.owner_crate, "blossom-engine", "{}", info.code);
                assert!(
                    info.also.contains(&"blossom-oracle"),
                    "{}: the oracle reports every runtime error",
                    info.code
                );
            }
        }
        let amended: Vec<_> = REGISTRY
            .iter()
            .filter(|i| i.origin != CodeOrigin::Language)
            .map(|i| (i.code, i.origin))
            .collect();
        assert_eq!(
            amended,
            vec![
                ("BLS0908", CodeOrigin::Amendment("L2")),
                ("BLS1003", CodeOrigin::Extended("L7")),
                ("BLS1009", CodeOrigin::Amendment("L3")),
                ("BLS1010", CodeOrigin::Amendment("L4")),
                ("BLSR011", CodeOrigin::Amendment("L1")),
            ]
        );
    }

    #[test]
    fn codes_lookup() {
        let c = Code::lookup("BLS0502").unwrap();
        assert_eq!(c.as_str(), "BLS0502");
        assert_eq!(c.severity(), Error);
        assert_eq!(c.info().owner_crate, "blossom-analysis");
        assert!(Code::lookup("BLS9999").is_none());
        assert!(Code::lookup("").is_none());
        for info in REGISTRY {
            assert_eq!(Code::lookup(info.code).map(Code::as_str), Some(info.code));
            assert_eq!(Code::lookup_const(info.code).map(Code::as_str), Some(info.code));
        }
        assert_eq!(crate::code!("BLSR001").info().severity, Runtime);
        assert_eq!(crate::code!("BLS0505").severity(), Warning);
        assert!(
            Code::lookup("BLSR011")
                .unwrap()
                .info()
                .may_be_constructed_in("blossom-oracle")
        );
        assert!(
            !Code::lookup("BLS0502")
                .unwrap()
                .info()
                .may_be_constructed_in("blossom-front")
        );
    }

    #[test]
    fn codes_serde_roundtrip() {
        let c = crate::code!("BLS0908");
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, "\"BLS0908\"");
        assert_eq!(serde_json::from_str::<Code>(&json).unwrap(), c);
        assert!(serde_json::from_str::<Code>("\"BLS0510\"").is_err());
    }
}
