//! ANA-011 (LANGUAGE §10.5, §13.6): numbering over persistent input. `index!()` ranks a view's head tuples afresh at
//! every tick; when those tuples come from persistent state they are the same tick after tick and are re-ranked every
//! tick, which is almost never what the program means (a number that should stay is `seq!`'s job). BLS0601 warns.
//!
//! A numbering's input is tick-local when every alternative of it reads, positively, a tick-local relation: an event
//! (an input, a timer, `boot`), a channel's received tuples, or a tick-local derived relation whose every rule does the
//! same. Otherwise its tuples can persist, and it is linted.

use std::collections::{BTreeMap, BTreeSet};

use blossom_base::{Diagnostic, Diagnostics, RelId, code};
use blossom_ir::core::{ConstructKind, IndexMode, Literal, Persistence, Program, RelClass, RuleKind};

/// BLS0601 for every `index!` whose input is not tick-local.
pub fn check(p: &Program) -> Diagnostics {
    let mut diags = Diagnostics::new();
    let mut memo: BTreeMap<RelId, bool> = BTreeMap::new();
    for c in p.constructs.iter() {
        let ConstructKind::Index(spec) = &c.kind else { continue };
        if spec.mode != IndexMode::Index {
            continue;
        }
        if tick_local(p, spec.input, &mut memo, &mut BTreeSet::new()) {
            continue;
        }
        let Some(out) = p.rels.get(spec.output) else { continue };
        diags.push(
            Diagnostic::new(
                code!("BLS0601"),
                format!(
                    "`index!` in `{}` numbers tuples that come from persistent state: it re-ranks them at every tick",
                    out.name
                ),
            )
            .with_primary(out.span),
        );
    }
    diags
}

/// Whether `rel`'s rows exist only in the tick that produced them (see the module comment).
fn tick_local(p: &Program, rel: RelId, memo: &mut BTreeMap<RelId, bool>, visiting: &mut BTreeSet<RelId>) -> bool {
    if let Some(v) = memo.get(&rel) {
        return *v;
    }
    let Some(decl) = p.rels.get(rel) else { return false };
    let v = match &decl.class {
        RelClass::Event(_) | RelClass::Channel(_) => true,
        RelClass::Idb if decl.persistence == Persistence::None => {
            if !visiting.insert(rel) {
                // A cycle through tick-local relations adds no persistence.
                return true;
            }
            let mut rules = p
                .rules
                .iter()
                .filter(|r| r.head.rel == rel && r.kind == RuleKind::Deductive)
                .peekable();
            let any = rules.peek().is_some();
            let all = rules.all(|r| {
                r.body.lits.iter().any(|l| match l {
                    Literal::Pos(a) => tick_local(p, a.rel, memo, visiting),
                    _ => false,
                })
            });
            visiting.remove(&rel);
            any && all
        }
        _ => false,
    };
    memo.insert(rel, v);
    v
}
