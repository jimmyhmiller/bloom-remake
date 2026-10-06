//! Attributes (LANGUAGE §2.5, Appendix C): [`check`] visits every attribute of a file once, where it is written.
//!
//! An attribute this build implements passes, and the phase that gives it meaning consumes it: `#[accept(…)]` on a
//! channel (the resolver checks its arguments, LANGUAGE §18.3), `#[fault(lossy)]` on a channel (the default fault
//! model, LANGUAGE §14.2), `#[unknown]` on an enum variant (LANGUAGE §19.2) and `#[allow(self_negation)]` on a verb
//! statement (LANGUAGE §8.6). Every other attribute is rejected, never dropped: an unknown name, or a built-in
//! attribute written where it does not apply, is BLS0210; `#[blazes(…)]` is reserved for a later edition (BLS0907);
//! a built-in attribute this build does not implement is BLS0908 naming its feature.

use blossom_base::{Diagnostic, Diagnostics, FeatureId, code};

use super::{Arg, Attr, Else, Expr, ExprKind, File, Item, ItemKind, RelKind, Stmt};

/// Where an attribute is written.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
enum Site {
    /// `#![…]` at the top of a file.
    File,
    Rel(RelKind),
    View,
    Handler,
    Invariant,
    /// A module or a choreography.
    Module,
    Struct,
    Enum,
    /// Any other item, by its keyword.
    Item(&'static str),
    /// `emit`, `next`, `send`, `delete`, `upsert` or `seal`.
    VerbStatement,
    /// `if` or `for`.
    BlockStatement,
    Column,
    Field,
    Variant,
    SpecMember,
}

impl Site {
    fn describe(self) -> String {
        match self {
            Site::File => "a file".to_owned(),
            Site::Rel(k) => format!("a {}", rel_word(k)),
            Site::View => "a view".to_owned(),
            Site::Handler => "a handler".to_owned(),
            Site::Invariant => "an invariant".to_owned(),
            Site::Module => "a module".to_owned(),
            Site::Struct => "a struct".to_owned(),
            Site::Enum => "an enum".to_owned(),
            Site::Item(kw) => format!("a `{kw}` item"),
            Site::VerbStatement | Site::BlockStatement => "a statement".to_owned(),
            Site::Column => "a column".to_owned(),
            Site::Field => "a field".to_owned(),
            Site::Variant => "an enum variant".to_owned(),
            Site::SpecMember => "a spec member".to_owned(),
        }
    }

    fn is_item(self) -> bool {
        !matches!(
            self,
            Site::File
                | Site::VerbStatement
                | Site::BlockStatement
                | Site::Column
                | Site::Field
                | Site::Variant
                | Site::SpecMember
        )
    }
}

fn rel_word(k: RelKind) -> &'static str {
    match k {
        RelKind::Table => "table",
        RelKind::Scratch => "scratch",
        RelKind::Channel => "channel",
        RelKind::Input => "input",
        RelKind::Output => "output",
        RelKind::Static => "static relation",
        RelKind::Loopback => "loopback",
    }
}

/// A built-in attribute of Appendix C: its feature, where it applies, and those places in words.
struct Builtin {
    name: &'static str,
    feature: &'static str,
    on: fn(Site) -> bool,
    on_words: &'static str,
}

fn channel(s: Site) -> bool {
    s == Site::Rel(RelKind::Channel)
}

fn output(s: Site) -> bool {
    s == Site::Rel(RelKind::Output)
}

fn module(s: Site) -> bool {
    s == Site::Module
}

fn evolution(s: Site) -> bool {
    matches!(s, Site::Column | Site::Field | Site::Variant | Site::Rel(_))
}

fn algebraic(s: Site) -> bool {
    // Functions and aggregates. Their claims are checked by TEST-087's harness, not built yet: the attributes are
    // accepted by the grammar and reported as not implemented.
    matches!(s, Site::Item("fn") | Site::Item("aggregate"))
}

const BUILTINS: &[Builtin] = &[
    Builtin {
        name: "fault",
        feature: "LANG-155",
        on: channel,
        on_words: "channels",
    },
    Builtin {
        name: "accept",
        feature: "LANG-242",
        on: channel,
        on_words: "channels",
    },
    Builtin {
        name: "replicated",
        feature: "ANA-041",
        on: |s| matches!(s, Site::Rel(RelKind::Channel | RelKind::Input)),
        on_words: "channels and inputs",
    },
    Builtin {
        name: "atomic",
        feature: "LANG-206",
        on: output,
        on_words: "outputs",
    },
    Builtin {
        name: "handler",
        feature: "LANG-186",
        on: output,
        on_words: "outputs",
    },
    Builtin {
        name: "nondet",
        feature: "LANG-204",
        on: |s| {
            matches!(
                s,
                Site::Rel(_) | Site::View | Site::Handler | Site::VerbStatement | Site::BlockStatement
            )
        },
        on_words: "handlers, views, statements, relations and outputs",
    },
    Builtin {
        name: "deterministic",
        feature: "ANA-039",
        on: output,
        on_words: "outputs",
    },
    Builtin {
        name: "trusted",
        feature: "LANG-205",
        on: module,
        on_words: "modules and choreographies",
    },
    Builtin {
        name: "finite",
        feature: "ANA-122",
        on: module,
        on_words: "modules and choreographies",
    },
    Builtin {
        name: "readonly",
        feature: "LANG-051",
        on: |s| s == Site::Rel(RelKind::Table),
        on_words: "tables",
    },
    Builtin {
        name: "materialize",
        feature: "LANG-053",
        on: |s| matches!(s, Site::View | Site::Rel(RelKind::Scratch)),
        on_words: "views and scratches",
    },
    Builtin {
        name: "recompute",
        feature: "LANG-053",
        on: |s| matches!(s, Site::View | Site::Rel(RelKind::Scratch)),
        on_words: "views and scratches",
    },
    Builtin {
        name: "localize",
        feature: "LANG-095",
        on: |s| s == Site::Handler,
        on_words: "handlers",
    },
    Builtin {
        name: "on_violation",
        feature: "LANG-200",
        on: |s| s == Site::Invariant,
        on_words: "invariants",
    },
    Builtin {
        name: "allow",
        feature: "TEST-091",
        on: |s| s.is_item() || matches!(s, Site::VerbStatement | Site::BlockStatement),
        on_words: "items and statements",
    },
    Builtin {
        name: "warn",
        feature: "TEST-091",
        on: |s| s.is_item() || matches!(s, Site::VerbStatement | Site::BlockStatement),
        on_words: "items and statements",
    },
    Builtin {
        name: "deny",
        feature: "TEST-091",
        on: |s| s.is_item() || matches!(s, Site::VerbStatement | Site::BlockStatement),
        on_words: "items and statements",
    },
    Builtin {
        name: "unsafe_ungated",
        feature: "LANG-264",
        on: |s| matches!(s, Site::Handler | Site::VerbStatement | Site::BlockStatement),
        on_words: "handlers and statements",
    },
    Builtin {
        name: "since",
        feature: "LANG-261",
        on: evolution,
        on_words: "columns, fields, variants and relations",
    },
    Builtin {
        name: "deprecated",
        feature: "LANG-265",
        on: evolution,
        on_words: "columns, fields, variants and relations",
    },
    Builtin {
        name: "semantics_changed",
        feature: "LANG-265",
        on: evolution,
        on_words: "columns, fields, variants and relations",
    },
    Builtin {
        name: "renamed_from",
        feature: "LANG-262",
        on: evolution,
        on_words: "columns, fields, variants and relations",
    },
    Builtin {
        name: "reserved",
        feature: "LANG-261",
        on: |s| matches!(s, Site::Rel(_) | Site::Struct | Site::Enum),
        on_words: "relations, structs and enums",
    },
    Builtin {
        name: "unknown",
        feature: "LANG-261",
        on: |s| s == Site::Variant,
        on_words: "enum variants",
    },
    Builtin {
        name: "injective",
        feature: "TEST-087",
        on: algebraic,
        on_words: "functions and aggregates",
    },
    Builtin {
        name: "commutative",
        feature: "TEST-087",
        on: algebraic,
        on_words: "functions and aggregates",
    },
    Builtin {
        name: "associative",
        feature: "TEST-087",
        on: algebraic,
        on_words: "functions and aggregates",
    },
    Builtin {
        name: "idempotent",
        feature: "TEST-087",
        on: algebraic,
        on_words: "functions and aggregates",
    },
];

/// The fault models of LANGUAGE §14.2; this build implements only the default, `lossy`.
const FAULT_MODELS: &[&str] = &["lossy", "lossy_delayed", "reliable", "reliable_ordered"];

/// Reports every attribute of `file` that no phase of this build consumes.
pub fn check(file: &File, diags: &mut Diagnostics) {
    let mut cx = Cx { diags };
    cx.attrs(&file.inner_attrs, Site::File);
    cx.items(&file.items);
}

struct Cx<'d> {
    diags: &'d mut Diagnostics,
}

/// The single path segment `e` is, if it is one (`lossy`, `self_negation`).
pub(crate) fn word(e: &Expr) -> Option<&'static str> {
    match &e.kind {
        ExprKind::Path(p, t) if t.is_empty() && p.len() == 1 => p.first().map(|i| i.as_str()),
        _ => None,
    }
}

/// The attribute's only argument, when it has exactly one positional word and no value.
fn only_word(a: &Attr) -> Option<&'static str> {
    match (a.args.as_slice(), &a.value) {
        ([Arg::Pos(e)], None) => word(e),
        _ => None,
    }
}

impl Cx<'_> {
    fn items(&mut self, items: &[Item]) {
        for item in items {
            self.item(item);
        }
    }

    fn item(&mut self, item: &Item) {
        let site = match &item.kind {
            ItemKind::Use(_) => Site::Item("use"),
            ItemKind::Format(_) => Site::Item("format"),
            ItemKind::Tree(_) => Site::Item("tree"),
            ItemKind::Fragment(_) => Site::Item("fragment"),
            ItemKind::Import(_) => Site::Item("import"),
            ItemKind::Include(_) => Site::Item("include"),
            ItemKind::Const { .. } => Site::Item("const"),
            ItemKind::Param { .. } => Site::Item("param"),
            ItemKind::TypeAlias { .. } => Site::Item("type"),
            ItemKind::Struct(s) => {
                for f in &s.fields {
                    self.attrs(&f.attrs, Site::Field);
                }
                Site::Struct
            }
            ItemKind::Enum(e) => {
                for v in &e.variants {
                    self.attrs(&v.attrs, Site::Variant);
                    for f in &v.fields {
                        self.attrs(&f.attrs, Site::Field);
                    }
                }
                Site::Enum
            }
            ItemKind::Module(m) => {
                self.items(&m.items);
                Site::Module
            }
            ItemKind::Protocol(p) => {
                self.items(&p.items);
                Site::Item("protocol")
            }
            ItemKind::Role { .. } => Site::Item("role"),
            ItemKind::At { items, .. } => {
                self.items(items);
                Site::Item("at")
            }
            ItemKind::Rel(d) => {
                for c in &d.cols {
                    self.attrs(&c.attrs, Site::Column);
                }
                Site::Rel(d.kind)
            }
            ItemKind::Timer(_) => Site::Item("timer"),
            ItemKind::View(_) => Site::View,
            ItemKind::Handler(h) => {
                self.stmts(&h.block.stmts);
                Site::Handler
            }
            ItemKind::Bootstrap { block, .. } => {
                self.stmts(&block.stmts);
                Site::Item("bootstrap")
            }
            ItemKind::Fact(_) => Site::Item("fact"),
            ItemKind::Fn(_) | ItemKind::ExternFn(_) => Site::Item("fn"),
            ItemKind::Stream { .. } => Site::Item("stream"),
            ItemKind::Invariant(_) => Site::Invariant,
            ItemKind::Interpose(_) => Site::Item("interpose"),
            ItemKind::Spec(s) => {
                self.attrs(&s.member_attrs, Site::SpecMember);
                Site::Item("spec")
            }
            // Already rejected by the converter (BLS0908), attributes and all.
            ItemKind::Unsupported { .. } => return,
        };
        self.attrs(&item.attrs, site);
    }

    fn stmts(&mut self, stmts: &[Stmt]) {
        for st in stmts {
            match st {
                Stmt::Verb(v) => self.attrs(&v.attrs, Site::VerbStatement),
                Stmt::If { attrs, then, els, .. } => {
                    self.attrs(attrs, Site::BlockStatement);
                    self.stmts(&then.stmts);
                    match els.as_deref() {
                        Some(Else::Block(b)) => self.stmts(&b.stmts),
                        Some(Else::If(s)) => self.stmts(std::slice::from_ref(s.as_ref())),
                        None => {}
                    }
                }
                Stmt::For { attrs, block, .. } => {
                    self.attrs(attrs, Site::BlockStatement);
                    self.stmts(&block.stmts);
                }
                Stmt::Fragment { body, .. } => self.stmts(&body.stmts),
                Stmt::Call(_) => {}
            }
        }
    }

    fn attrs(&mut self, attrs: &[Attr], site: Site) {
        for a in attrs {
            self.attr(a, site);
        }
    }

    fn attr(&mut self, a: &Attr, site: Site) {
        let name = a.name.as_str();
        if name == "blazes" {
            self.diags.push(
                Diagnostic::new(
                    code!("BLS0907"),
                    "`#[blazes(…)]` grey-box annotations (ANA-044) are reserved for a later edition",
                )
                .with_primary(a.span),
            );
            return;
        }
        let Some(b) = BUILTINS.iter().find(|b| b.name == name) else {
            self.diags
                .push(Diagnostic::new(code!("BLS0210"), format!("unknown attribute `#[{name}]`")).with_primary(a.span));
            return;
        };
        if !(b.on)(site) {
            self.diags.push(
                Diagnostic::new(
                    code!("BLS0210"),
                    format!(
                        "`#[{name}]` does not apply to {}: it applies to {}",
                        site.describe(),
                        b.on_words
                    ),
                )
                .with_primary(a.span),
            );
            return;
        }
        match (name, site) {
            // The resolver checks the arguments (LANGUAGE §18.3).
            ("accept", _) => {}
            ("fault", _) => match only_word(a) {
                Some("lossy") => {}
                Some(m) if FAULT_MODELS.contains(&m) => self.unsupported(b.feature, &format!("`#[fault({m})]`"), a),
                _ => self.diags.push(
                    Diagnostic::new(
                        code!("BLS0210"),
                        format!("`#[fault(…)]` takes one fault model: {}", FAULT_MODELS.join(", ")),
                    )
                    .with_primary(a.span),
                ),
            },
            ("unknown", _) if a.args.is_empty() && a.value.is_none() => {}
            ("unknown", _) => self
                .diags
                .push(Diagnostic::new(code!("BLS0210"), "`#[unknown]` takes no arguments").with_primary(a.span)),
            ("allow", Site::VerbStatement)
                if a.value.is_none()
                    && !a.args.is_empty()
                    && a.args
                        .iter()
                        .all(|x| matches!(x, Arg::Pos(e) if word(e) == Some("self_negation"))) => {}
            _ => self.unsupported(
                b.feature,
                &format!("the attribute `#[{name}]` on {}", site.describe()),
                a,
            ),
        }
    }

    fn unsupported(&mut self, feature: &'static str, what: &str, a: &Attr) {
        self.diags.push(
            Diagnostic::not_implemented(FeatureId(feature), what, "the Blossom frontend (slice 2)")
                .with_primary(a.span),
        );
    }
}
