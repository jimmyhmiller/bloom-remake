//! Integration tests of the `blossom` binary: dispatch, exit codes and placeholders.

mod kafka_kill9;
mod kill9;
mod raft3;

use std::process::Command;

/// Runs the built `blossom` binary (test-only helper).
#[cfg(test)]
fn blossom(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_blossom")).args(args).output().unwrap()
}

#[test]
fn cli_placeholders_exit_7_naming_feature_and_wp() {
    let expected = [
        ("check", "LANG-002 (WP M6.3)"),
        ("fmt", "LANG-002 (WP M6.3)"),
        ("build", "LANG-002 (WP M6.3)"),
        ("plan", "ENG-007 (WP M6.3)"),
        ("explain", "TEST-091 (WP M6.3)"),
        ("run", "DIST-040 (WP M7.4)"),
        ("deploy", "DIST-043 (WP M7.4)"),
        ("node", "DIST-040 (WP M7.4)"),
        ("config", "DIST-040 (WP M7.4)"),
        ("sim", "TEST-001 (WP M7.2)"),
        ("trace", "TEST-010 (WP M7.2)"),
        ("ldfi", "TEST-029 (WP M8.1)"),
        ("verify", "VER-002 (WP M9.5)"),
        ("why", "TEST-050 (WP M8.1)"),
        ("whynot", "TEST-051 (WP M8.1)"),
        ("compat", "ANA-100 (WP M6.5)"),
        ("release", "LANG-260 (WP M6.5)"),
        ("store", "DIST-021 (WP M5.4)"),
        ("self-check", "ENG-067 (WP M12.4)"),
        ("repl", "TEST-090 (WP M12.4)"),
        ("upgrade", "DIST-085 (WP M12.3)"),
        ("admin", "DIST-066 (WP M11.5)"),
        ("completions", "shell completions (WP M12.4)"),
        ("lsp", "TEST-092 (WP M15.1)"),
    ];
    for (cmd, what) in expected {
        // Arbitrary arguments reach the placeholder (a usage error would be exit code 2).
        let out = blossom(&[cmd, "examples/e01_kvs.bls", "--flag"]);
        // Each placeholder reports itself unimplemented until its WP implements the command (PLAN §2.6: this
        // test must survive later milestones). An implemented command may exit with any other code of the
        // ARCHITECTURE §12.5 table (these arguments are not meant to be valid), but never crash.
        match out.status.code() {
            Some(7) => {
                assert_eq!(
                    String::from_utf8(out.stderr).unwrap(),
                    format!("not implemented yet: {what}\n"),
                    "{cmd}"
                );
                assert!(out.stdout.is_empty(), "{cmd}");
            }
            Some(code) => assert!(
                code <= 6,
                "{cmd}: exit code {code} is not in the ARCHITECTURE §12.5 table"
            ),
            None => panic!("{cmd} was terminated by a signal"),
        }
    }
}

#[test]
fn cli_usage_errors_exit_2_and_help_lists_exit_codes() {
    let out = blossom(&["no-such-command"]);
    assert_eq!(out.status.code(), Some(2));
    let help = blossom(&["--help"]);
    assert_eq!(help.status.code(), Some(0));
    let text = String::from_utf8(help.stdout).unwrap();
    assert!(
        text.contains("Exit codes:") && text.contains("7  unimplemented feature"),
        "{text}"
    );
    assert!(!text.contains("corpus"), "the corpus runs through xtask (PLAN §4 D8)");
    let version = blossom(&["--version"]);
    assert_eq!(version.status.code(), Some(0));
    assert!(String::from_utf8(version.stdout).unwrap().starts_with("blossom "));
}

/// `blossom sim` feeds a Blossom program's byte streams scripted connections and chunks, prints each tick's requests
/// to the host, and refuses a script that breaks the runtime's order (a chunk in its connection's opening tick).
#[test]
fn sim_scripts_stream_chunks_and_prints_the_writes() {
    let echo = concat!(env!("CARGO_MANIFEST_DIR"), "/../../tests/integration/fixtures/streams/echo.bls");
    let out = blossom(&[
        "sim",
        echo,
        "--nodes",
        "n1",
        "--ticks",
        "4",
        "--open",
        "n1:echo:1:1",
        "--chunk",
        "n1:echo:1:2:hel",
        "--chunk",
        "n1:echo:1:3:lo\\nwor",
        "--chunk",
        "n1:echo:1:4:ld\\n",
    ]);
    let stdout = String::from_utf8(out.stdout).unwrap();
    assert_eq!(out.status.code(), Some(0), "{stdout}{}", String::from_utf8_lossy(&out.stderr));
    assert!(stdout.contains("=> echo.write(conn#1, 0, [Bytes(b\"hello\\n\")])"), "{stdout}");
    assert!(stdout.contains("=> echo.write(conn#1, 1, [Bytes(b\"world\\n\")])"), "{stdout}");
    let bad = blossom(&["sim", echo, "--nodes", "n1", "--ticks", "3", "--open", "n1:echo:1:2", "--chunk", "n1:echo:1:2:x"]);
    assert_eq!(bad.status.code(), Some(2));
    assert!(String::from_utf8_lossy(&bad.stderr).contains("opening tick"));
}
