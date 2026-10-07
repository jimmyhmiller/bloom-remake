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
