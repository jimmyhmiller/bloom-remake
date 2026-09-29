//! Loading `.ded` roots and their `include`s into one program.

use std::collections::BTreeSet;
use std::sync::Arc;

use blossom_base::{Diagnostic, Diagnostics, SourceDb, Span, code};
use blossom_syntax::ded::{self, Clause, Fact, Rule};

use super::DedLoader;

/// Every clause of a program, in load order: each file's clauses in source order, an included file's clauses at
/// its `include` (Molly concatenates files).
#[derive(Debug, Default)]
pub(crate) struct Program {
    pub rules: Vec<Rule>,
    pub facts: Vec<Fact>,
}

pub(crate) fn load(
    roots: &[&str],
    loader: &mut dyn DedLoader,
    sources: &mut SourceDb,
    diags: &mut Diagnostics,
) -> Program {
    let mut cx = Loader {
        loader,
        sources,
        diags,
        loaded: BTreeSet::new(),
        stack: Vec::new(),
        program: Program::default(),
    };
    for root in roots {
        cx.file(None, root, None);
    }
    cx.program
}

struct Loader<'a> {
    loader: &'a mut dyn DedLoader,
    sources: &'a mut SourceDb,
    diags: &'a mut Diagnostics,
    loaded: BTreeSet<Arc<str>>,
    stack: Vec<Arc<str>>,
    program: Program,
}

impl Loader<'_> {
    fn file(&mut self, from: Option<Arc<str>>, path: &str, at: Option<Span>) {
        let file = match self.loader.load(from.as_deref(), path) {
            Ok(f) => f,
            Err(why) => {
                let mut d = Diagnostic::new(code!("BLS0204"), format!("cannot load `{path}`: {why}"));
                if let Some(span) = at {
                    d = d.with_primary(span);
                }
                self.diags.push(d);
                return;
            }
        };
        if self.stack.contains(&file.key) {
            let mut d = Diagnostic::new(code!("BLS0204"), format!("`{}` includes itself", file.key));
            if let Some(span) = at {
                d = d.with_primary(span);
            }
            self.diags.push(d.with_note(format!(
                "include chain: {}",
                self.stack
                    .iter()
                    .map(|k| &**k)
                    .chain([&*file.key])
                    .collect::<Vec<_>>()
                    .join(" -> ")
            )));
            return;
        }
        if !self.loaded.insert(file.key.clone()) {
            return;
        }
        let id = match self.sources.add_text(file.key.clone(), file.text) {
            Ok(id) => id,
            Err(e) => {
                self.diags.push(Diagnostic::new(
                    code!("BLS0204"),
                    format!("cannot add `{}`: {e}", file.key),
                ));
                return;
            }
        };
        let Ok(text) = self.sources.text(id).cloned() else {
            self.diags.push(Diagnostic::new(
                code!("BLS0204"),
                format!("cannot read back `{}`", file.key),
            ));
            return;
        };
        let (parsed, parse_diags) = ded::parse(id, &text);
        for d in parse_diags.into_vec() {
            self.diags.push(d);
        }
        self.stack.push(file.key.clone());
        for clause in parsed.clauses {
            match clause {
                Clause::Include(inc) => self.file(Some(file.key.clone()), &inc.path, Some(inc.span)),
                Clause::Rule(r) => self.program.rules.push(r),
                Clause::Fact(f) => self.program.facts.push(f),
            }
        }
        self.stack.pop();
    }
}
