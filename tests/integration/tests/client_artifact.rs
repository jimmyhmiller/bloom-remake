//! S22: what a client member's page is given (docs/design/CLIENTS.md §8). The shared TodoMVC projected onto
//! `Browser` holds the tab's rules and relations, the channels both ways and the link events, and nothing placed at
//! the server; it survives encoding, and a damaged or altered encoding is refused.

use std::path::Path;

use blossom_artifact::bls::BlsArtifact;
use blossom_artifact::client::ClientArtifact;
use blossom_driver::bls::compile_file;
use blossom_front::api::NodeSpec;

#[cfg(test)]
fn todos() -> BlsArtifact {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web/todos_shared.bls");
    let nodes = [NodeSpec {
        name: "s".into(),
        role: Some("Server".into()),
    }];
    compile_file(path.to_str().unwrap(), &nodes).0.unwrap().0
}

#[cfg(test)]
fn names(a: &ClientArtifact) -> (Vec<String>, Vec<String>) {
    let p = a.program.get();
    let rels = p.rels.iter().map(|r| r.name.to_string()).collect();
    let rules = p.rules.iter().map(|r| r.label.text.to_string()).collect();
    (rels, rules)
}

#[test]
fn the_page_gets_the_tabs_part_and_nothing_of_the_servers() {
    let full = todos();
    let client = ClientArtifact::project(&full, "Browser").unwrap();
    assert!(client.leaks(&full).is_empty(), "{:?}", client.leaks(&full));
    let (rels, rules) = names(&client);
    for want in ["todos", "next_n", "set", "todo", "Server$members"] {
        assert!(rels.iter().any(|r| r == want), "{want} missing from {rels:?}");
    }
    assert!(rels.iter().any(|r| r.contains("connected")), "{rels:?}");
    for server in ["items", "online", "latest", "version"] {
        assert!(!rels.iter().any(|r| r == server), "{server} leaked: {rels:?}");
    }
    for server in ["take", "tell", "greet", "join", "leave"] {
        assert!(
            !rules
                .iter()
                .any(|r| r == server || r.starts_with(&format!("{server}$"))),
            "{server} leaked: {rules:?}"
        );
    }
    assert!(rules.iter().any(|r| r.starts_with("add")), "{rules:?}");
    // Nor does any of the server's source text travel with it.
    let bytes = client.encode().unwrap();
    for word in ["greet", "online", "latest", "version"] {
        assert!(
            !bytes.windows(word.len()).any(|w| w == word.as_bytes()),
            "`{word}` is in the encoding"
        );
    }
    let back = ClientArtifact::decode(&bytes).unwrap();
    assert_eq!(back.program.digest().0, client.program.digest().0);
    assert_eq!(back.part(), client.part());
    assert_eq!((back.role, back.roles.clone()), (client.role, client.roles.clone()));
}

#[test]
fn a_damaged_or_foreign_encoding_is_refused() {
    let full = todos();
    let bytes = ClientArtifact::project(&full, "Browser").unwrap().encode().unwrap();
    assert!(ClientArtifact::decode(&bytes[..bytes.len() / 2]).is_err());
    assert!(ClientArtifact::decode(b"PK\x03\x04 not an artifact").is_err());
    let mut flipped = bytes.clone();
    let mid = flipped.len() / 2;
    flipped[mid] ^= 0x55;
    assert!(ClientArtifact::decode(&flipped).is_err());
    assert!(
        ClientArtifact::project(&full, "Server").is_err(),
        "Server is not a client role"
    );
}

/// The leak check is not vacuous: the whole program, posing as the tab's part, names the server's relations and rules.
#[test]
fn the_leak_check_finds_the_servers_part() {
    let full = todos();
    let honest = ClientArtifact::project(&full, "Browser").unwrap();
    let posing = ClientArtifact {
        program: full.program.clone(),
        ..honest
    };
    let leaks = posing.leaks(&full);
    assert!(leaks.iter().any(|l| l.contains("`items`")), "{leaks:?}");
    assert!(leaks.iter().any(|l| l.contains("rule `take")), "{leaks:?}");
}

/// A page's rules run as its client role, so `self` there is a `Node<Browser>` in the page's program as in the whole
/// one: it names itself (`to_string` of its own type) and goes into a `Node<Browser>` column.
#[test]
fn a_page_knows_itself_as_a_member_of_its_role() {
    let dir = std::env::temp_dir().join(format!("blossom-client-self-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("whoami.bls");
    std::fs::write(
        &path,
        r#"program whoami version 1;

role Server;
role Browser: client;

channel hi(name: String): Browser -> Server;

at Server {
    table heard(who: Node<Browser>, name: String);
    hear: on hi(n) from b {
        next heard(b, n);
    }
}

at Browser {
    table me(who: Node<Browser>, name: String) key();
    greet: on Server.connected(s, _) {
        send hi(self.to_string()) to s;
        upsert me(self, self.to_string());
    }
}
"#,
    )
    .unwrap();
    let nodes = [NodeSpec {
        name: "s".into(),
        role: Some("Server".into()),
    }];
    let (compiled, _) = compile_file(path.to_str().unwrap(), &nodes);
    let full = compiled.unwrap().0;
    let client = match ClientArtifact::project(&full, "Browser") {
        Ok(c) => c,
        Err(e) => panic!("{e}"),
    };
    assert!(client.leaks(&full).is_empty(), "{:?}", client.leaks(&full));
    let back = ClientArtifact::decode(&client.encode().unwrap()).unwrap();
    assert_eq!(back.program.digest().0, client.program.digest().0);
}
