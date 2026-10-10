//! The SQL tree (docs/design/SQL-TABLES.md) against the tree model (`blossom_store::tree::tree_suite`), over the
//! memory store, SQLite, and Postgres (scripts/test-services.sh; ignored in the fast tier): every key's presence as
//! of every version, scans, ranges, pages, an amendment, a flush; and, opened again from its meta after its changes
//! committed, the same.

use std::path::Path;
use std::sync::Arc;

use blossom_driver::bls::compile_deployed;
use blossom_front::api::NodeSpec;
use blossom_runtime::deploy::DeploymentSpec;
use blossom_runtime::stateless::sqltree::{SqlTree, TableMap, TreeMeta};
use blossom_statestore::{MemStore, Owner, StateStore};
use blossom_store::StoreError;
use blossom_store::tree::{KeyTree, tree_suite};

#[cfg(test)]
fn map() -> Arc<TableMap> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/web/keyed_chat.deploy.toml");
    let text = std::fs::read_to_string(&path).unwrap();
    let spec = DeploymentSpec::parse(&text, path.parent().unwrap()).unwrap();
    let nodes: Vec<NodeSpec> = spec
        .nodes
        .iter()
        .map(|n| NodeSpec {
            name: n.name.clone(),
            role: n.role.clone(),
        })
        .collect();
    let (compiled, _) = compile_deployed(&spec.source.to_string_lossy(), &nodes, &Default::default());
    let artifact = compiled.unwrap().0;
    Arc::new(TableMap::of(&artifact.program, spec.names(), "tree-test").unwrap())
}

/// The suite on a fresh tree over `store`; opening it again commits what it has and starts from its meta.
#[cfg(test)]
fn suite(store: Arc<dyn StateStore>) {
    let map = map();
    store.tables().unwrap().ensure_tables("tree-test", map.defs()).unwrap();
    let owner = Owner {
        node: "rooms".into(),
        member: "tree".into(),
    };
    let tree = SqlTree::new(store.clone(), owner.clone(), map.clone(), TreeMeta::default(), 1);
    let reopen = || -> Result<Box<dyn KeyTree>, StoreError> {
        let bad = |e: String| StoreError::Invalid(e);
        let (rows, prune, meta) = tree.changes().map_err(|e| bad(e.to_string()))?;
        let version = store.version("tree-test").map_err(|e| bad(e.to_string()))?;
        store
            .tables()
            .ok_or_else(|| bad("no tables".into()))?
            .commit_rows("tree-test", version, &[], &owner, &rows, prune)
            .map_err(|e| bad(e.to_string()))?;
        tree.committed(meta.clone()).map_err(|e| bad(e.to_string()))?;
        Ok(Box::new(SqlTree::new(
            store.clone(),
            owner.clone(),
            map.clone(),
            meta,
            1,
        )))
    };
    tree_suite(&tree, Some(&reopen)).unwrap();
}

#[test]
fn the_sql_tree_holds_to_the_tree_model_in_memory() {
    suite(Arc::new(MemStore::new()));
}

#[test]
fn the_sql_tree_holds_to_the_tree_model_on_sqlite() {
    let dir = std::env::temp_dir().join(format!("blossom-sql-tree-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    suite(Arc::new(
        blossom_statestore_sqlite::SqliteStore::open(dir.join("state.db")).unwrap(),
    ));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[ignore = "needs Postgres: scripts/test-services.sh"]
fn the_sql_tree_holds_to_the_tree_model_on_postgres() {
    let url = std::env::var("BLOSSOM_TEST_POSTGRES")
        .unwrap_or_else(|_| panic!("BLOSSOM_TEST_POSTGRES is not set: scripts/test-services.sh"));
    let schema = format!("tree_{}", std::process::id());
    let store = blossom_statestore_postgres::PostgresStore::from_url(&format!("{url}&schema={schema}")).unwrap();
    suite(Arc::new(store));
    let plain = url.split('?').next().unwrap().to_string();
    let mut c = postgres::Client::connect(&format!("{plain}?sslmode=disable"), postgres::NoTls).unwrap();
    c.batch_execute(&format!("drop schema if exists \"{schema}\" cascade"))
        .unwrap();
}
