//! Persistence (BROWSER.md "Persistence"): a program's durable tables, saved after every round and restored at
//! boot, as JSON (what the page keeps in `localStorage`). Each table is saved with its schema hash (the runtime's,
//! `blossom_wire::catalog::schema_hash`): a table whose schema changed is not restored, and the host says so.

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
