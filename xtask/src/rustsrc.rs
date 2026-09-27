//! A syntax-aware scan of Rust sources for `check-sans-io` and `check-codes`: finds paths, identifiers and string
//! literals with their line numbers and whether they are in test-only code. Comments (including doc comments) are
//! never seen, and macro bodies are scanned token by token.

use proc_macro2::{TokenStream, TokenTree};
use syn::punctuated::Punctuated;
use syn::visit::{self, Visit};

/// One path as written (for example `std::fs::read`), with every `use` alias expanded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathUse {
    /// Its segments.
    pub segments: Vec<String>,
    /// 1-based line.
    pub line: usize,
    /// Whether it is a glob import (`use a::b::*`).
    pub glob: bool,
}

/// A string literal or identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Token {
    /// The literal's value (for strings) or the identifier.
    pub text: String,
    /// 1-based line.
    pub line: usize,
    /// Whether it is inside `#[cfg(test)]` or `#[test]` code.
    pub in_test: bool,
}

/// Everything `scan` collects from one file.
#[derive(Debug, Default)]
pub struct Scanned {
    /// Paths (from `use` trees, expressions, types, patterns and macro token streams).
    pub paths: Vec<PathUse>,
    /// String literals and identifiers.
    pub tokens: Vec<Token>,
}

/// Parses and scans one file.
pub fn scan(source: &str) -> syn::Result<Scanned> {
    let file = syn::parse_file(source)?;
    let mut v = Scanner {
        out: Scanned::default(),
        test_depth: 0,
        aliases: Vec::new(),
    };
    // Collect every `use` first — at file level, in nested modules and inside function bodies — so each is
    // recorded and its aliases are known everywhere. Aliases are applied file-wide: an alias declared in one scope
    // may expand a same-named path in another, which can only add findings, never hide one.
    let mut uses = UseItems(Vec::new());
    uses.visit_file(&file);
    for u in uses.0 {
        let mut prefix = Vec::new();
        v.collect_use(&u.tree, &mut prefix, u.use_token.span.start().line);
    }
    v.visit_file(&file);
    Ok(v.out)
}

/// Every `use` item of a file, at any depth.
struct UseItems<'ast>(Vec<&'ast syn::ItemUse>);

impl<'ast> Visit<'ast> for UseItems<'ast> {
    fn visit_item_use(&mut self, u: &'ast syn::ItemUse) {
        self.0.push(u);
    }
}

struct Scanner {
    out: Scanned,
    test_depth: usize,
    /// `use` aliases: the last segment (or `as` name) and the full path it stands for.
    aliases: Vec<(String, Vec<String>)>,
}

/// Whether the attributes mark test-only code: `#[test]`, `#[…::test]`, or a `#[cfg(P)]` whose predicate `P` can
/// only hold in a test build (see [`cfg_requires_test`]).
fn is_test_code(attrs: &[syn::Attribute]) -> bool {
    attrs.iter().any(|a| {
        let path = a.path();
        if path.segments.last().is_some_and(|s| s.ident == "test") {
            return true;
        }
        path.is_ident("cfg") && a.parse_args::<syn::Meta>().is_ok_and(|pred| cfg_requires_test(&pred))
    })
}

/// Whether the `cfg` predicate implies `test`: `test` itself, an `all(…)` with such a member, or a non-empty
/// `any(…)` whose members all are. Anything else — `not(…)`, `feature = "…"`, other options, a predicate that does
/// not parse — may hold outside tests, so the code is checked as production code.
fn cfg_requires_test(pred: &syn::Meta) -> bool {
    let members = |list: &syn::MetaList| {
        list.parse_args_with(Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated)
            .map(|p| p.into_iter().collect::<Vec<_>>())
            .unwrap_or_default()
    };
    match pred {
        syn::Meta::Path(p) => p.is_ident("test"),
        syn::Meta::List(list) if list.path.is_ident("all") => members(list).iter().any(cfg_requires_test),
        syn::Meta::List(list) if list.path.is_ident("any") => {
            let members = members(list);
            !members.is_empty() && members.iter().all(cfg_requires_test)
        }
        syn::Meta::List(_) | syn::Meta::NameValue(_) => false,
    }
}

impl Scanner {
    fn collect_use(&mut self, tree: &syn::UseTree, prefix: &mut Vec<String>, line: usize) {
        match tree {
            syn::UseTree::Path(p) => {
                prefix.push(p.ident.to_string());
                self.collect_use(&p.tree, prefix, line);
                prefix.pop();
            }
            syn::UseTree::Name(n) => {
                let mut full = prefix.clone();
                let name = n.ident.to_string();
                if name != "self" {
                    full.push(name.clone());
                }
                let alias = if name == "self" {
                    prefix.last().cloned().unwrap_or_default()
                } else {
                    name
                };
                self.aliases.push((alias, full.clone()));
                self.out.paths.push(PathUse {
                    segments: full,
                    line,
                    glob: false,
                });
            }
            syn::UseTree::Rename(r) => {
                let mut full = prefix.clone();
                if r.ident != "self" {
                    full.push(r.ident.to_string());
                }
                self.aliases.push((r.rename.to_string(), full.clone()));
                self.out.paths.push(PathUse {
                    segments: full,
                    line,
                    glob: false,
                });
            }
            syn::UseTree::Glob(_) => {
                self.out.paths.push(PathUse {
                    segments: prefix.clone(),
                    line,
                    glob: true,
                });
            }
            syn::UseTree::Group(g) => {
                for t in &g.items {
                    self.collect_use(t, prefix, line);
                }
            }
        }
    }

    /// Records a path, expanding a leading `use` alias.
    fn record_path(&mut self, segments: Vec<String>, line: usize) {
        let expanded = match segments.split_first() {
            Some((first, rest)) => match self.aliases.iter().find(|(alias, _)| alias == first) {
                Some((_, full)) => full.iter().cloned().chain(rest.iter().cloned()).collect(),
                None => segments,
            },
            None => segments,
        };
        self.out.paths.push(PathUse {
            segments: expanded,
            line,
            glob: false,
        });
    }

    fn record_token(&mut self, text: String, line: usize) {
        self.out.tokens.push(Token {
            text,
            line,
            in_test: self.test_depth > 0,
        });
    }

    /// Scans a macro's or attribute's tokens: string literals, identifiers and `a::b` paths.
    fn scan_tokens(&mut self, tokens: TokenStream) {
        let mut path: Vec<String> = Vec::new();
        let mut path_line = 0;
        let mut pending_colons = 0;
        let flush = |this: &mut Scanner, path: &mut Vec<String>, line: usize| {
            if path.len() > 1 {
                this.record_path(std::mem::take(path), line);
            } else {
                path.clear();
            }
        };
        for tt in tokens {
            match tt {
                TokenTree::Ident(i) => {
                    let line = i.span().start().line;
                    self.record_token(i.to_string(), line);
                    if pending_colons == 2 && !path.is_empty() {
                        path.push(i.to_string());
                    } else {
                        flush(self, &mut path, path_line);
                        path.push(i.to_string());
                        path_line = line;
                    }
                    pending_colons = 0;
                }
                TokenTree::Punct(p) if p.as_char() == ':' => pending_colons += 1,
                TokenTree::Punct(_) => {
                    flush(self, &mut path, path_line);
                    pending_colons = 0;
                }
                TokenTree::Literal(lit) => {
                    flush(self, &mut path, path_line);
                    pending_colons = 0;
                    let line = lit.span().start().line;
                    // The literal as written, quotes and escapes included: codes never need escaping.
                    self.record_token(lit.to_string(), line);
                }
                TokenTree::Group(g) => {
                    flush(self, &mut path, path_line);
                    pending_colons = 0;
                    self.scan_tokens(g.stream());
                }
            }
        }
        flush(self, &mut path, path_line);
    }

    fn with_test_scope(&mut self, test: bool, f: impl FnOnce(&mut Self)) {
        if test {
            self.test_depth += 1;
        }
        f(self);
        if test {
            self.test_depth -= 1;
        }
    }
}

impl<'ast> Visit<'ast> for Scanner {
    fn visit_item(&mut self, item: &'ast syn::Item) {
        let attrs: &[syn::Attribute] = match item {
            syn::Item::Const(i) => &i.attrs,
            syn::Item::Enum(i) => &i.attrs,
            syn::Item::ExternCrate(i) => &i.attrs,
            syn::Item::Fn(i) => &i.attrs,
            syn::Item::ForeignMod(i) => &i.attrs,
            syn::Item::Impl(i) => &i.attrs,
            syn::Item::Macro(i) => &i.attrs,
            syn::Item::Mod(i) => &i.attrs,
            syn::Item::Static(i) => &i.attrs,
            syn::Item::Struct(i) => &i.attrs,
            syn::Item::Trait(i) => &i.attrs,
            syn::Item::TraitAlias(i) => &i.attrs,
            syn::Item::Type(i) => &i.attrs,
            syn::Item::Union(i) => &i.attrs,
            syn::Item::Use(i) => &i.attrs,
            _ => &[],
        };
        let test = is_test_code(attrs);
        self.with_test_scope(test, |this| visit::visit_item(this, item));
    }

    fn visit_impl_item_fn(&mut self, f: &'ast syn::ImplItemFn) {
        let test = is_test_code(&f.attrs);
        self.with_test_scope(test, |this| visit::visit_impl_item_fn(this, f));
    }

    fn visit_item_use(&mut self, _u: &'ast syn::ItemUse) {
        // Recorded (with aliases) before the walk.
    }

    fn visit_item_extern_crate(&mut self, e: &'ast syn::ItemExternCrate) {
        self.record_path(vec![e.ident.to_string()], e.ident.span().start().line);
    }

    fn visit_path(&mut self, p: &'ast syn::Path) {
        let segments: Vec<String> = p.segments.iter().map(|s| s.ident.to_string()).collect();
        let line = p.segments.first().map_or(0, |s| s.ident.span().start().line);
        self.record_path(segments, line);
        visit::visit_path(self, p);
    }

    fn visit_ident(&mut self, i: &'ast proc_macro2::Ident) {
        self.record_token(i.to_string(), i.span().start().line);
    }

    fn visit_lit_str(&mut self, s: &'ast syn::LitStr) {
        self.record_token(s.value(), s.span().start().line);
    }

    fn visit_macro(&mut self, m: &'ast syn::Macro) {
        visit::visit_path(self, &m.path);
        self.scan_tokens(m.tokens.clone());
    }

    fn visit_attribute(&mut self, a: &'ast syn::Attribute) {
        // Doc comments are comments.
        if a.path().is_ident("doc") {
            return;
        }
        match &a.meta {
            syn::Meta::Path(_) => {}
            syn::Meta::List(list) => self.scan_tokens(list.tokens.clone()),
            syn::Meta::NameValue(nv) => self.visit_expr(&nv.value),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn paths(src: &str) -> Vec<String> {
        scan(src)
            .unwrap()
            .paths
            .into_iter()
            .map(|p| p.segments.join("::") + if p.glob { "::*" } else { "" })
            .collect()
    }

    #[test]
    fn rustsrc_paths_expand_use_aliases() {
        let p = paths(
            "use std::time::{self as t, Duration};\nuse std::net::TcpStream as Tcp;\nuse std::fs::*;\n\
             fn f() { let _ = t::Instant::now(); let _ = Tcp::connect(\"x\"); std::thread::spawn(|| ()); }",
        );
        assert!(p.contains(&"std::time::Instant::now".to_string()), "{p:?}");
        assert!(p.contains(&"std::net::TcpStream::connect".to_string()), "{p:?}");
        assert!(p.contains(&"std::fs::*".to_string()), "{p:?}");
        assert!(p.contains(&"std::thread::spawn".to_string()), "{p:?}");
        assert!(p.contains(&"std::time::Duration".to_string()), "{p:?}");
    }

    #[test]
    fn rustsrc_scans_macros_and_skips_comments() {
        let s = scan(
            "/// BLS0001 in a doc comment\n// BLS0002 in a comment\n#[error(\"BLSR001 key\")]\nstruct E;\n\
             fn f() { let _ = format!(\"{}\", \"BLS0003\"); tokio::spawn(x); }\n\
             #[cfg(test)]\nmod tests { fn g() { let _ = \"BLS0004\"; } }",
        )
        .unwrap();
        let texts: Vec<(&str, bool)> = s.tokens.iter().map(|t| (t.text.as_str(), t.in_test)).collect();
        assert!(texts.iter().any(|(t, test)| t.contains("BLSR001") && !test));
        assert!(texts.iter().any(|(t, test)| t.contains("BLS0003") && !test));
        assert!(texts.iter().any(|(t, test)| t.contains("BLS0004") && *test));
        assert!(
            !texts
                .iter()
                .any(|(t, _)| t.contains("BLS0001") || t.contains("BLS0002"))
        );
        assert!(s.paths.iter().any(|p| p.segments == ["tokio", "spawn"]));
    }

    #[test]
    fn rustsrc_test_attributes() {
        let s = scan(
            "#[cfg(not(test))] fn a() { let _ = \"x\"; }\n#[test] fn b() { let _ = \"y\"; }\n\
             #[cfg(any(test, feature = \"f\"))] fn c() { let _ = \"z\"; }\n\
             #[cfg(all(test, feature = \"f\"))] fn d() { let _ = \"w\"; }\n\
             #[cfg(any(test, all(test, unix)))] fn e() { let _ = \"v\"; }\n\
             #[cfg(not(not(test)))] fn f() { let _ = \"u\"; }\n\
             #[tokio::test] async fn g() { let _ = \"t\"; }",
        )
        .unwrap();
        let get = |v: &str| s.tokens.iter().find(|t| t.text == v).map(|t| t.in_test);
        assert_eq!(get("x"), Some(false));
        assert_eq!(get("y"), Some(true));
        // `any(test, feature = "f")` also holds in a non-test build with the feature: production code.
        assert_eq!(get("z"), Some(false));
        assert_eq!(get("w"), Some(true));
        assert_eq!(get("v"), Some(true));
        // Double negation is not simplified: checked as production code (the strict side).
        assert_eq!(get("u"), Some(false));
        assert_eq!(get("t"), Some(true));
    }

    #[test]
    fn rustsrc_uses_at_every_depth() {
        let p = paths(
            "mod inner { use std::fs as f; pub fn x() { let _ = f::read(\"p\"); } }\n\
             fn body() { use std::net::TcpStream; let _ = TcpStream::connect(\"x\"); }\n\
             impl S { fn m() { use std::thread; thread::yield_now(); } }",
        );
        for needle in [
            "std::fs",
            "std::fs::read",
            "std::net::TcpStream",
            "std::net::TcpStream::connect",
            "std::thread",
            "std::thread::yield_now",
        ] {
            assert!(p.contains(&needle.to_string()), "{needle} not in {p:?}");
        }
    }
}
