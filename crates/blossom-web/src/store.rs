//! Persistence (BROWSER.md "Persistence"): a program's durable tables, saved after every round and restored at
//! boot, as JSON (what the page keeps in `localStorage`). Each table is saved with its schema hash (the runtime's,
//! `blossom_wire::catalog::schema_hash`): a table whose schema changed is not restored, and the host says so.
//!
//! The host keeps one entry per row, so a round costs the rows it changed: [`changes`] gives the rows added and
//! removed since the last call (all of them, `full`, the first time), and a restore takes them back as [`save`]'s
//! JSON, which the host assembles from its entries.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;

use blossom_ir::core::Program;
use blossom_ir::tick::{Instance, Row};
use blossom_value::Value;
use serde::{Deserialize, Serialize};

use crate::HostError;

#[derive(Serialize, Deserialize)]
struct Saved {
    tables: Vec<Table>,
}

#[derive(Serialize, Deserialize)]
struct Table {
    name: String,
    /// The schema hash, in hex.
    schema: String,
    rows: Vec<Vec<Value>>,
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The program's durable tables in `carried`, as JSON.
pub fn save(program: &Program, carried: &Instance) -> Result<String, HostError> {
    let tables = program
        .rels
        .iter_enumerated()
        .filter(|(_, r)| r.durable)
        .map(|(id, r)| Table {
            name: r.name.to_string(),
            schema: hex(&blossom_wire::catalog::schema_hash(program, id)),
            rows: carried.rows(id).map(|row| row.to_vec()).collect(),
        })
        .collect();
    serde_json::to_string(&Saved { tables }).map_err(|e| HostError::Store(format!("saving the durable tables: {e}")))
}

/// The durable rows a host adds and removes since it last saved: all of them (`full`: it drops what it had) the first
/// time after a start, then the changes. Each row as its JSON, which is also the key a host keeps it under.
#[derive(Serialize)]
pub struct SaveChanges {
    pub full: bool,
    /// Each durable table's schema hash, in hex, by name.
    pub tables: BTreeMap<String, String>,
    /// (table, row as JSON).
    pub put: Vec<(String, String)>,
    pub delete: Vec<(String, String)>,
}

/// The durable tables of `program` with their schema hashes, by name.
pub fn schemas(program: &Program) -> BTreeMap<String, String> {
    program
        .rels
        .iter_enumerated()
        .filter(|(_, r)| r.durable)
        .map(|(id, r)| {
            (
                r.name.to_string(),
                hex(&blossom_wire::catalog::schema_hash(program, id)),
            )
        })
        .collect()
}

/// A row as the JSON a host keeps it under.
pub fn row_json(row: &Row) -> Result<String, HostError> {
    serde_json::to_string(&row.to_vec()).map_err(|e| HostError::Store(format!("saving a row: {e}")))
}

/// What a restore found: the durable rows to start from, and the tables it could not restore (gone, or with
/// another schema).
pub struct Restored {
    pub carried: Instance,
    pub dropped: Vec<String>,
}

/// The durable tables `json` saved, for `program`.
pub fn restore(program: &Program, json: &str) -> Result<Restored, HostError> {
    let saved: Saved =
        serde_json::from_str(json).map_err(|e| HostError::Store(format!("reading the saved tables: {e}")))?;
    let mut rels: BTreeMap<_, BTreeSet<Row>> = BTreeMap::new();
    let mut dropped = Vec::new();
    for t in saved.tables {
        let found = program
            .rels
            .iter_enumerated()
            .find(|(_, r)| r.durable && r.name.to_string() == t.name)
            .map(|(id, _)| id);
        match found {
            Some(id) if hex(&blossom_wire::catalog::schema_hash(program, id)) == t.schema => {
                rels.insert(id, t.rows.into_iter().map(Arc::from).collect());
            }
            _ => dropped.push(t.name),
        }
    }
    Ok(Restored {
        carried: Instance { rels },
        dropped,
    })
}
