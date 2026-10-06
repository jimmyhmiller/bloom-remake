use blossom_base::FileId;
use blossom_syntax::{
    ast::{AstNode, SourceFile},
    lexer, parser,
};
use std::{
    fs,
    path::{Path, PathBuf},
};
#[cfg(test)]
fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}
#[cfg(test)]
fn files(dir: &Path, filename: &str, out: &mut Vec<PathBuf>) {
    for entry in fs::read_dir(dir).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            files(&path, filename, out)
        } else if path.file_name().unwrap().to_string_lossy() == filename
            || filename == "*.bls" && path.extension().is_some_and(|e| e == "bls")
        {
            out.push(path)
        }
    }
}
#[cfg(test)]
fn check_file(path: &Path) {
    let text = fs::read_to_string(path).unwrap();
    let p = parser::parse(FileId::from_raw(0), &text);
    assert_eq!(p.syntax().to_string(), text, "lossless {}", path.display());
    assert!(
        p.errors.is_empty(),
        "{}: {:?}",
        path.display(),
        p.errors
            .iter()
            .take(12)
            .map(|e| format!("{} {:?}", e.diagnostic, e.primary))
            .collect::<Vec<_>>()
    );
    assert!(SourceFile::cast(p.syntax()).is_some());
}
#[test]
fn examples_parse() {
    let mut paths = Vec::new();
    files(&root().join("examples"), "*.bls", &mut paths);
    for path in paths {
        check_file(&path)
    }
}
#[test]
fn corpus_parse() {
    let mut paths = Vec::new();
    files(&root().join("tests/corpus"), "program.bls", &mut paths);
    for path in paths {
        check_file(&path)
    }
}
#[test]
fn lexer_basic() {
    let l = lexer::lex(FileId::from_raw(0), "# comment\n#1 r#match foo!(a) 10ms");
    assert!(l.errors.is_empty());
    assert_eq!(
        l.tokens
            .iter()
            .filter(|t| t.kind == blossom_syntax::SyntaxKind::FIELD_NUM)
            .count(),
        1
    )
}
#[test]
fn parser_items_basic() {
    check_file(&root().join("examples/e01_kvs.bls"))
}
#[test]
fn parser_exprs_precedence() {
    let p = parser::parse(FileId::from_raw(0), "const X: bool = 1 + 2 * 3 == 7;");
    assert!(p.errors.is_empty(), "{:?}", p.errors)
}
#[test]
fn parser_recovery_basic() {
    let p = parser::parse(FileId::from_raw(0), "const X: u64 = ;\nconst Y: u64 = 1;");
    assert!(!p.errors.is_empty());
    assert_eq!(p.syntax().to_string(), "const X: u64 = ;\nconst Y: u64 = 1;")
}
#[test]
fn language_md_blocks_parse() {
    let md = fs::read_to_string(root().join("docs/design/LANGUAGE.md")).unwrap();
    let mut in_block = false;
    let mut current = String::new();
    let mut line = 0;
    for (i, l) in md.lines().enumerate() {
        if l == "```blossom" {
            in_block = true;
            current.clear();
            line = i + 1;
            continue;
        }
        if l == "```" && in_block {
            in_block = false;
            let s = current.trim_start();
            if s.contains('…')
                || s.contains("...")
                || current.lines().any(|l| {
                    ["emit ", "next ", "send ", "delete ", "upsert ", "seal ", "let "]
                        .iter()
                        .any(|v| l.starts_with(v))
                })
            {
                continue;
            }
            if s.starts_with('[') || s.starts_with("store(") || s.starts_with("not ") || s.starts_with("choose!(") {
                continue;
            }
            let p = parser::parse(FileId::from_raw(0), &current);
            assert_eq!(p.syntax().to_string(), current);
            assert!(
                p.errors.is_empty(),
                "LANGUAGE.md line {line}: {:?}\n{current}",
                p.errors
                    .iter()
                    .map(|e| format!("{} {:?}", e.diagnostic, e.primary))
                    .collect::<Vec<_>>()
            );
            continue;
        }
        if in_block {
            current.push_str(l);
            current.push('\n');
        }
    }
}
#[test]
fn gen_ast_up_to_date() {
    let status = std::process::Command::new("cargo")
        .args(["run", "-q", "-p", "xtask", "--", "gen-ast", "--check"])
        .current_dir(root())
        .status()
        .unwrap();
    assert!(status.success());
}
#[test]
fn diag_bls0001_unexpected() {
    assert_code("$", "BLS0001")
}
#[test]
fn diag_bls0002_unterminated() {
    assert_code("\"x", "BLS0002")
}
#[test]
fn diag_bls0003_suffix() {
    assert_code("1kb", "BLS0003")
}
#[test]
fn diag_bls0004_keyword_bang() {
    assert_code("not!(x)", "BLS0004")
}
#[test]
fn diag_bls0005_escape() {
    assert_code("\"\\q\"", "BLS0005")
}
#[test]
fn diag_bls0100_unexpected() {
    assert_code("$", "BLS0001");
    assert_code("?", "BLS0100");
    assert_code("!", "BLS0100")
}
#[test]
fn diag_bls0101_missing_semicolon() {
    assert_code("const X: u64 = 1\nconst Y: u64 = 2;", "BLS0101")
}
#[test]
fn diag_bls0102_let_statement() {
    assert_code("on a(x) { let y = x; }", "BLS0102")
}
#[test]
fn diag_bls0103_chained_comparison() {
    assert_code("const X: bool = 1 < 2 < 3;", "BLS0103")
}
#[test]
fn diag_bls0104_no_struct() {
    assert_code("on Foo { x: 1 } {}", "BLS0104")
}
#[test]
fn diag_bls0105_bad_label() {
    assert_code("label: table r(x: u64);", "BLS0105")
}
#[test]
fn diag_bls0106_duplicate_clause() {
    assert_code("table r(x: u64) key(x) key(x);", "BLS0106")
}
#[test]
fn diag_bls0107_clause_outside_bang() {
    assert_code("on r(x), choose!(x) per x {}", "BLS0107")
}
#[test]
fn diag_bls0108_stable_after() {
    assert_code("stable fn f(x: u64) -> u64 { x }", "BLS0108")
}
#[test]
fn diag_bls0109_missing_else() {
    assert_code("const X: u64 = if true { 1 };", "BLS0109")
}
#[cfg(test)]
fn assert_code(s: &str, code: &str) {
    let p = parser::parse(FileId::from_raw(0), s);
    assert!(
        p.errors.iter().any(|e| e.code.as_str() == code),
        "expected {code}: {:?}",
        p.errors
    );
    assert!(p.errors.iter().all(|e| e.primary.is_some()));
    assert_eq!(p.syntax().to_string(), s);
}
#[test]
fn lexer_trivia_and_literals() {
    use blossom_syntax::SyntaxKind as K;
    let source = "/* a /* b */ c */ # comment\n#12 r#while r##\"raw\"## br\"bytes\" b\"escaped\\n\" 0x1fI 3.5s a<..=b";
    let l = lexer::lex(FileId::from_raw(0), source);
    assert!(l.errors.is_empty(), "{:?}", l.errors);
    let kinds: Vec<_> = l.tokens.iter().map(|t| t.kind).filter(|k| !k.is_trivia()).collect();
    assert_eq!(
        kinds,
        vec![
            K::FIELD_NUM,
            K::IDENT,
            K::RAW_STRING_LIT,
            K::BYTES_LIT,
            K::BYTES_LIT,
            K::MOD_LIT,
            K::DURATION_LIT,
            K::IDENT,
            K::OPEN_RANGE_EQ,
            K::IDENT,
            K::EOF
        ]
    );
    assert_eq!(
        l.tokens.iter().filter_map(|t| t.text(source)).collect::<String>(),
        source
    );
}
#[test]
fn parser_items_ast_views() {
    use blossom_syntax::ast::{Item, RelKind, Trigger, Verb};
    let text = fs::read_to_string(root().join("examples/e01_kvs.bls")).unwrap();
    let p = parser::parse(FileId::from_raw(0), &text);
    assert!(p.errors.is_empty());
    let file = SourceFile::cast(p.syntax()).unwrap();
    assert_eq!(file.header().unwrap().name().unwrap().text(), "kvs");
    let server = file
        .items()
        .find_map(|item| match item {
            Item::At(at) => Some(at),
            _ => None,
        })
        .unwrap();
    let table = server
        .syntax()
        .children()
        .find_map(blossom_syntax::ast::RelDecl::cast)
        .unwrap();
    assert_eq!(table.kind(), Some(RelKind::Table));
    assert_eq!(table.columns().count(), 2);
    let handler = server
        .syntax()
        .children()
        .find_map(blossom_syntax::ast::HandlerItem::cast)
        .unwrap();
    assert_eq!(handler.trigger(), Some(Trigger::On));
    assert_eq!(handler.label().unwrap().text(), "apply_put");
    let stmt = handler.block().unwrap().stmts().next().unwrap();
    assert!(matches!(stmt,blossom_syntax::ast::Stmt::Verb(v) if v.verb()==Some(Verb::Upsert)));
}
#[test]
fn parser_exprs_precedence_tree() {
    use blossom_syntax::SyntaxKind as K;
    let p = parser::parse(FileId::from_raw(0), "const X: bool = 1 + 2 * 3 == 7;");
    assert!(p.errors.is_empty());
    let binary: Vec<_> = p
        .syntax()
        .descendants()
        .filter(|n| n.kind() == K::BINARYEXPR)
        .map(|n| n.to_string().trim().to_owned())
        .collect();
    assert_eq!(binary, vec!["1 + 2 * 3 == 7", "1 + 2 * 3", "2 * 3"]);
}
#[test]
fn parser_type_shift_split_roundtrip() {
    let source = "type T = Map<K, LMax<u64>>;";
    let p = parser::parse(FileId::from_raw(0), source);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.syntax().to_string(), source);
}
#[test]
fn parser_fold_parenthesized_pipe() {
    let s = "view r(x) = lset{ (a | b) | source(a, b) };";
    let p = parser::parse(FileId::from_raw(0), s);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.syntax().to_string(), s);
}
#[test]
fn interpolated_strings_lex_into_text_and_holes() {
    use blossom_syntax::SyntaxKind as K;
    // A nested interpolated string, a hole with brackets and a struct-free `{`, escaped braces, a spec, an escape.
    let source = r#"const S: String = f"a{x}b{f"in{y}"}{g(1, [2])}{{c}}{z:.2}\n";"#;
    let l = lexer::lex(FileId::from_raw(0), source);
    assert!(l.errors.is_empty(), "{:?}", l.errors);
    let kinds: Vec<_> = l
        .tokens
        .iter()
        .map(|t| t.kind)
        .filter(|k| !k.is_trivia())
        .skip_while(|k| *k != K::FSTRING_START)
        .collect();
    assert_eq!(
        kinds,
        vec![
            K::FSTRING_START,
            K::FSTRING_TEXT,
            K::L_CURLY,
            K::IDENT,
            K::R_CURLY,
            K::FSTRING_TEXT,
            K::L_CURLY,
            K::FSTRING_START,
            K::FSTRING_TEXT,
            K::L_CURLY,
            K::IDENT,
            K::R_CURLY,
            K::FSTRING_END,
            K::R_CURLY,
            K::L_CURLY,
            K::IDENT,
            K::L_PAREN,
            K::INT_LIT,
            K::COMMA,
            K::L_BRACK,
            K::INT_LIT,
            K::R_BRACK,
            K::R_PAREN,
            K::R_CURLY,
            K::FSTRING_TEXT,
            K::L_CURLY,
            K::IDENT,
            K::COLON,
            K::FSTRING_SPEC,
            K::R_CURLY,
            K::FSTRING_TEXT,
            K::FSTRING_END,
            K::SEMI,
            K::EOF
        ]
    );
    let p = parser::parse(FileId::from_raw(0), source);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.syntax().to_string(), source);
    // Unterminated, in the text and in a hole; a lone `}`.
    assert_code(r#"const S: String = f"abc"#, "BLS0002");
    assert_code(r#"const S: String = f"a{x"#, "BLS0002");
    assert_code(r#"const S: String = f"a}b";"#, "BLS0005");
}
#[test]
fn trees_child_heads_and_spreads_parse() {
    // docs/design/SUGAR.md §§2, 3, 5.
    let source = r#"tree html { node elem(id, parent, pos, tag); props attr(id, name, value); content text(id, s); }
page: while screen(s), bird(y, v) {
    emit html svg[id: "game"](viewBox: "10 0 80 100", width: 480, stroke-width: 0.5, "aria-label": "x") {
        rect(x: 0);
        g[key: k, pos: 2](transform: f"t({y})") { ellipse(rx: 5); }
        if s == "game" { text[id: "score"](x: 50) { n } } else if s == "menu" { text(x: 1) { "menu" } } else { text(x: 1) { "over"; } }
        for obstacle_at(k, x, h) { g[key: k] { rect(width: 10); } }
        font-face(x: 1);
        br();
    }
    emit html br();
    emit order(id: o, customer: c) { line(sku: "a", qty: 2); if b { line(sku: "b", qty: x - y); } }
    emit attr("x", ..{rx: 5, fill-opacity: 0.5});
    emit header(r, ..m);
    emit plain(a - b, c);
}
"#;
    let p = parser::parse(FileId::from_raw(0), source);
    assert!(p.errors.is_empty(), "{:?}", p.errors);
    assert_eq!(p.syntax().to_string(), source);
    let kinds: std::collections::BTreeSet<_> = p.syntax().descendants().map(|n| n.kind()).collect();
    use blossom_syntax::SyntaxKind as K;
    for k in [
        K::TREEITEM,
        K::TREEROLE,
        K::TREENAME,
        K::ELEMENT,
        K::ELEMNAME,
        K::META,
        K::CHILDREN,
        K::IFCHILD,
        K::FORCHILD,
        K::CONTENT,
        K::PROPNAME,
        K::RECORDLIT,
    ] {
        assert!(kinds.contains(&k), "no {k:?}");
    }
}

// ---------------------------------------------------------------- the formatter (LANGUAGE §3.5)

/// Every source the formatter must handle: the examples, the corpus programs, and LANGUAGE.md's blocks that parse.
#[cfg(test)]
fn fmt_sources() -> Vec<(String, String)> {
    let mut paths = Vec::new();
    files(&root().join("examples"), "*.bls", &mut paths);
    files(&root().join("tests/corpus"), "program.bls", &mut paths);
    let mut out: Vec<(String, String)> = paths
        .iter()
        .map(|p| (p.display().to_string(), fs::read_to_string(p).unwrap()))
        .collect();
    let md = fs::read_to_string(root().join("docs/design/LANGUAGE.md")).unwrap();
    let mut block: Option<String> = None;
    for (i, l) in md.lines().enumerate() {
        match (&mut block, l) {
            (None, "```blossom") => block = Some(String::new()),
            (Some(b), "```") => {
                let text = std::mem::take(b);
                block = None;
                if parser::parse(FileId::from_raw(0), &text).errors.is_empty() {
                    out.push((format!("LANGUAGE.md block ending at line {}", i + 1), text));
                }
            }
            (Some(b), l) => {
                b.push_str(l);
                b.push('\n');
            }
            _ => {}
        }
    }
    out
}

/// The non-trivia tokens of a source, in order.
#[cfg(test)]
fn significant(text: &str) -> Vec<(blossom_syntax::SyntaxKind, String)> {
    lexer::lex(FileId::from_raw(0), text)
        .tokens
        .iter()
        .filter(|t| !t.kind.is_trivia() && t.kind != blossom_syntax::SyntaxKind::EOF)
        .map(|t| (t.kind, t.text(text).unwrap_or("").to_owned()))
        .collect()
}

/// The comments of a source, in order, as the formatter writes them (a `#` comment as `//`).
#[cfg(test)]
fn comments(text: &str) -> Vec<String> {
    use blossom_syntax::SyntaxKind as K;
    lexer::lex(FileId::from_raw(0), text)
        .tokens
        .iter()
        .filter(|t| {
            matches!(
                t.kind,
                K::LINE_COMMENT | K::DOC_COMMENT | K::INNER_DOC_COMMENT | K::BLOCK_COMMENT | K::HASH_COMMENT
            )
        })
        .map(|t| {
            let s = t.text(text).unwrap_or("").trim_end();
            match s.strip_prefix('#') {
                Some(rest) if t.kind == K::HASH_COMMENT && !(t.span.lo == 0 && rest.starts_with('!')) => {
                    if rest.is_empty() || rest.starts_with(' ') {
                        format!("//{rest}")
                    } else {
                        format!("// {rest}")
                    }
                }
                _ => s.to_owned(),
            }
        })
        .collect()
}

#[test]
fn fmt_idempotent() {
    let mut n = 0;
    for (name, text) in fmt_sources() {
        let once = blossom_syntax::fmt::format(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
        let p = parser::parse(FileId::from_raw(0), &once);
        assert!(
            p.errors.is_empty(),
            "{name}: the formatted source does not parse:\n{once}"
        );
        let twice = blossom_syntax::fmt::format(&once).unwrap_or_else(|e| panic!("{name} (formatted): {e}"));
        assert_eq!(once, twice, "{name}: formatting is not idempotent");
        assert!(once.lines().all(|l| l == l.trim_end()), "{name}: a line ends in spaces");
        n += 1;
    }
    assert!(n > 400, "only {n} sources");
}

#[test]
fn fmt_preserves_cst_modulo_trivia() {
    for (name, text) in fmt_sources() {
        let out = blossom_syntax::fmt::format(&text).unwrap();
        assert_eq!(significant(&out), significant(&text), "{name}: the tokens changed");
        assert_eq!(comments(&out), comments(&text), "{name}: the comments changed");
    }
}

#[test]
fn fmt_never_reorders() {
    // Items, statements and literals keep their order: the item kinds of each file, in order, are unchanged (the
    // token stream, checked above, pins the rest).
    for (name, text) in fmt_sources() {
        let out = blossom_syntax::fmt::format(&text).unwrap();
        let kinds = |s: &str| -> Vec<blossom_syntax::SyntaxKind> {
            parser::parse(FileId::from_raw(0), s)
                .syntax()
                .children()
                .map(|n| n.kind())
                .collect()
        };
        assert_eq!(kinds(&out), kinds(&text), "{name}: the items moved");
    }
}

#[test]
fn fmt_refuses_a_source_that_does_not_parse() {
    assert!(blossom_syntax::fmt::format("program p version 1;\ntable t(x: u64;\n").is_err());
}

/// A source with its whitespace shuffled where it means nothing: runs of spaces, indentation, and line breaks
/// between two tokens (never next to a comment, never a blank line made or lost).
#[cfg(test)]
fn perturb(text: &str, seed: u64) -> String {
    use blossom_syntax::SyntaxKind as K;
    let tokens = lexer::lex(FileId::from_raw(0), text).tokens;
    let mut rng = seed;
    let mut next = move || {
        rng = rng
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        rng >> 33
    };
    let mut out = String::new();
    let is_comment = |k: K| {
        matches!(
            k,
            K::LINE_COMMENT | K::DOC_COMMENT | K::INNER_DOC_COMMENT | K::BLOCK_COMMENT | K::HASH_COMMENT
        )
    };
    for (i, t) in tokens.iter().enumerate() {
        let s = t.text(text).unwrap_or("");
        if t.kind != K::WHITESPACE {
            out.push_str(s);
            continue;
        }
        let prev = i.checked_sub(1).and_then(|j| tokens.get(j)).map(|t| t.kind);
        let after = tokens.get(i + 1).map(|t| t.kind);
        let near_comment = prev.is_some_and(is_comment) || after.is_some_and(is_comment);
        let newlines = s.matches('\n').count();
        if newlines >= 2 || near_comment {
            // Blank lines and comments' lines stay; the indentation of the line after may change.
            let lines = s.matches('\n').count();
            for _ in 0..lines {
                out.push('\n');
            }
            for _ in 0..(next() % 9) {
                out.push(' ');
            }
        } else if next() % 3 == 0 {
            out.push('\n');
            for _ in 0..(next() % 9) {
                out.push(' ');
            }
        } else {
            for _ in 0..(1 + next() % 3) {
                out.push(' ');
            }
        }
    }
    out
}

proptest::proptest! {
    #![proptest_config(proptest::prelude::ProptestConfig::with_cases(96))]
    #[test]
    fn formatter_fuzz_mirror(pick in 0usize..10_000, seed in proptest::prelude::any::<u64>()) {
        let sources = fmt_sources();
        let (name, text) = &sources[pick % sources.len()];
        let shuffled = perturb(text, seed);
        let a = blossom_syntax::fmt::format(text).unwrap();
        let b = blossom_syntax::fmt::format(&shuffled).unwrap_or_else(|e| panic!("{name} shuffled: {e}\n{shuffled}"));
        proptest::prop_assert_eq!(a, b, "{}: whitespace changed the format", name);
    }
}

#[test]
fn repository_sources_are_formatted() {
    // The examples and the corpus are kept in the canonical format (`blossom fmt examples tests/corpus`).
    let mut paths = Vec::new();
    files(&root().join("examples"), "*.bls", &mut paths);
    files(&root().join("tests/corpus"), "*.bls", &mut paths);
    let unformatted: Vec<String> = paths
        .iter()
        .filter(|p| {
            let text = fs::read_to_string(p).unwrap();
            blossom_syntax::fmt::format(&text).is_ok_and(|out| out != text)
        })
        .map(|p| p.display().to_string())
        .collect();
    assert!(
        unformatted.is_empty(),
        "not formatted (run `blossom fmt examples tests/corpus`): {unformatted:?}"
    );
}
