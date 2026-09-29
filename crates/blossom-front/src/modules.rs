//! Source loading and the module tree (ARCHITECTURE §13.4, LANGUAGE §6.1).
//!
//! Every file is a module. A module path `a::b` names `a/b.bls` (or `a/b/mod.bls`) relative to the directory of the
//! program root. [`ModuleTree::load`] parses the root and, transitively, every file named by a `use` or `import`
//! path, converting each to the owned AST. Loading goes through the [`Loader`] trait, so the frontend does no I/O.

use std::collections::BTreeMap;
use std::sync::Arc;

use blossom_base::{Diagnostic, Diagnostics, SourceDb, Span, code};

use crate::ast::{self, ItemKind};
use crate::ded::LoadedFile;

/// Where `.bls` sources come from.
pub trait Loader {
    /// Loads `path`. `from` is the key of the file it is relative to, or `None` for the root, which is resolved as
    /// given.
    fn load(&mut self, from: Option<&str>, path: &str) -> Result<LoadedFile, String>;
}

/// The loaded modules: the root and every file reachable from it through `use` and `import` paths.
pub struct ModuleTree {
    /// The root file's key.
    pub root_key: Arc<str>,
    pub root: ast::File,
    /// Other files by module name (the first path segment; nested module directories are not used by any program
    /// yet and are reported when met).
    pub modules: BTreeMap<String, ast::File>,
}

impl ModuleTree {
    /// Loads and parses the root and the modules it uses. Returns `None` when a file cannot be loaded or parsed;
    /// the diagnostics say why.
    pub fn load(
        root: &str,
        loader: &mut dyn Loader,
        sources: &mut SourceDb,
        diags: &mut Diagnostics,
    ) -> Option<ModuleTree> {
        let file = match loader.load(None, root) {
            Ok(f) => f,
            Err(e) => {
                diags.push(Diagnostic::new(code!("BLS0204"), format!("cannot read `{root}`: {e}")));
                return None;
            }
        };
        let root_key = file.key.clone();
        let root_ast = parse(&file, sources, diags)?;
        let mut tree = ModuleTree {
            root_key,
            root: root_ast,
            modules: BTreeMap::new(),
        };
        let mut pending: Vec<(String, Span)> = Vec::new();
        referenced(&tree.root.items, &mut pending);
        while let Some((name, span)) = pending.pop() {
            if tree.modules.contains_key(&name) || is_local(&tree, &name) {
                continue;
            }
            if name == "std" {
                diags.push(
                    Diagnostic::not_implemented(
                        blossom_base::FeatureId("LIB-001"),
                        "the standard library",
                        "the Blossom frontend (slice 2)",
                    )
                    .with_primary(span),
                );
                continue;
            }
            let candidates = [format!("{name}.bls"), format!("{name}/mod.bls")];
            let mut loaded = None;
            let mut errors = Vec::new();
            for c in &candidates {
                match loader.load(Some(&tree.root_key), c) {
                    Ok(f) => {
                        loaded = Some(f);
                        break;
                    }
                    Err(e) => errors.push(format!("{c}: {e}")),
                }
            }
            let Some(f) = loaded else {
                diags.push(
                    Diagnostic::new(code!("BLS0204"), format!("cannot find module `{name}`"))
                        .with_primary(span)
                        .with_note(errors.join("; ")),
                );
                continue;
            };
            let Some(parsed) = parse(&f, sources, diags) else {
                continue;
            };
            referenced(&parsed.items, &mut pending);
            tree.modules.insert(name, parsed);
        }
        Some(tree)
    }
}

/// Whether `name` is an item of the root file (a local module, protocol or type), so no file is needed.
fn is_local(tree: &ModuleTree, name: &str) -> bool {
    fn declares(items: &[ast::Item], name: &str) -> bool {
        items.iter().any(|i| match &i.kind {
            ItemKind::Module(m) => m.name.as_str() == name,
            ItemKind::Protocol(p) => p.name.as_str() == name,
            ItemKind::Struct(s) => s.name.as_str() == name,
            ItemKind::Enum(e) => e.name.as_str() == name,
            ItemKind::TypeAlias { name: n, .. } => n.as_str() == name,
            ItemKind::At { items, .. } => declares(items, name),
            _ => false,
        })
    }
    declares(&tree.root.items, name) || tree.modules.values().any(|f| declares(&f.items, name))
}

/// The first segments of every multi-segment `use` path and `import` path in `items`.
fn referenced(items: &[ast::Item], out: &mut Vec<(String, Span)>) {
    for item in items {
        match &item.kind {
            ItemKind::Use(tree) => {
                for p in &tree.paths {
                    if p.len() > 1
                        && let Some(first) = p.first()
                    {
                        out.push((first.as_str().to_owned(), first.span));
                    }
                }
            }
            ItemKind::Import(i) => {
                if i.module.len() > 1
                    && let Some(first) = i.module.first()
                {
                    out.push((first.as_str().to_owned(), first.span));
                }
            }
            ItemKind::Module(m) => referenced(&m.items, out),
            ItemKind::At { items, .. } => referenced(items, out),
            _ => {}
        }
    }
}

fn parse(file: &LoadedFile, sources: &mut SourceDb, diags: &mut Diagnostics) -> Option<ast::File> {
    let id = match sources.add_text(file.key.clone(), file.text.as_str()) {
        Ok(id) => id,
        Err(e) => {
            diags.push(Diagnostic::new(
                code!("BLS0204"),
                format!("cannot load `{}`: {e}", file.key),
            ));
            return None;
        }
    };
    let parse = blossom_syntax::parser::parse(id, &file.text);
    if !parse.errors.is_empty() {
        for e in parse.errors {
            diags.push(e.diagnostic);
        }
        return None;
    }
    let before = diags.error_count();
    let converted = ast::convert(id, &parse.syntax(), diags);
    (diags.error_count() == before).then_some(converted)
}
