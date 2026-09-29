//! Channel ACL consistency (ANA-105, LANGUAGE §18.3): the senders of a channel are the roles of the rules that send
//! into it. An explicit ACL (`#[accept(…)]`) that excludes one of them is BLS0800: the program would drop its own
//! messages at ingress.

use std::collections::BTreeSet;

use blossom_base::{Diagnostic, Diagnostics, RoleId, code};
use blossom_ir::core::{AclSpec, Program, RelClass, RuleKind};

/// BLS0800 for every role that sends on a channel whose explicit ACL does not admit it (one diagnostic per channel
/// and role, at its first sending rule).
pub fn check(p: &Program) -> Diagnostics {
    let mut diags = Diagnostics::new();
    for (id, rel) in p.rels.iter_enumerated() {
        let RelClass::Channel(ch) = &rel.class else {
            continue;
        };
        let AclSpec::Explicit(acl) = &ch.acl else {
            continue;
        };
        let mut reported: BTreeSet<Option<RoleId>> = BTreeSet::new();
        for rule in p.rules.iter() {
            if rule.kind != RuleKind::Async || rule.head.rel != id {
                continue;
            }
            // A rule of a role-free program runs on every node, which no role list admits.
            let admitted = rule.role.is_some_and(|r| acl.roles.contains(&r));
            if admitted || !reported.insert(rule.role) {
                continue;
            }
            let sender = match rule.role.and_then(|r| p.roles.get(r)) {
                Some(role) => format!("role `{}`", role.name),
                None => "every node".to_owned(),
            };
            diags.push(
                Diagnostic::new(
                    code!("BLS0800"),
                    format!(
                        "the explicit ACL of `{}` excludes {sender}, which sends on it: those messages would be dropped at ingress",
                        rel.name
                    ),
                )
                .with_primary(rule.span)
                .with_label(rel.span, "the channel whose `#[accept(…)]` does not admit the sender"),
            );
        }
    }
    diags
}
