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
