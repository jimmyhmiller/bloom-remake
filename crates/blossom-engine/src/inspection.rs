//! Serializable inspection of the actual value-engine plan, not blossom-plan's separate IR.
use serde::{Deserialize, Serialize};

/// Clock-free counters; writes are support updates, not physical index operations.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct WorkReport {
    pub probe_rows: u64,
    pub expression_steps: u64,
    pub support_writes: u64,
    pub evaluations: u64,
}
/// One logical rule and its selected execution strategy.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RuleReport {
    pub id: u32,
    pub text: String,
    pub head: String,
    pub dependencies: Vec<String>,
    pub regime: String,
    pub operator: String,
    pub phase: String,
    pub recursive: bool,
    pub aggregate: bool,
    pub barriers: Vec<String>,
    pub parallel_eligible: bool,
    pub work: WorkReport,
    /// Opt-in elapsed time including evaluation, result application and skip checks, not CPU time.
    pub elapsed_ns: Option<u64>,
}
/// A physical store, including logical consumers and currently built in-memory indexes.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StoreReport {
    pub key: String,
    pub readers: Vec<u32>,
    pub writers: Vec<u32>,
    pub resident_rows: usize,
    pub indexes: Vec<Vec<usize>>,
    pub reasons: Vec<String>,
    pub operations: Option<StoreOperations>,
}
/// Versioned snapshot. Unknown metrics are deliberately not represented as zero measurements.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlanReport {
    pub schema_version: u32,
    pub program_digest: String,
    pub node: u32,
    pub evaluation_workers: usize,
    pub rules: Vec<RuleReport>,
    pub stores: Vec<StoreReport>,
    pub limitations: Vec<String>,
}

/// Opt-in physical-store counters. Probe calls count API entries, including nested calls.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct StoreOperations {
    pub probe_calls: u64,
    pub membership_changes: u64,
    pub secondary_index_row_updates: u64,
    pub secondary_index_build_rows: u64,
    pub bulk_retracted_rows: u64,
    pub bulk_index_clears: u64,
}

impl PlanReport {
    /// Graphviz graph of rules and materialization boundaries; elapsed labels are wall time.
    pub fn dot(&self) -> String {
        use std::fmt::Write;
        let mut out = String::from("digraph plan {\nrankdir=LR;\n");
        for store in &self.stores {
            let node = format!("store:{}", store.key);
            let label = format!(
                "{}\n{} resident rows\n{} readers",
                store.key,
                store.resident_rows,
                store.readers.len()
            );
            let _ = writeln!(out, "{node:?} [shape=box,label={label:?}];");
        }
        for rule in &self.rules {
            let label = format!(
                "rule {}: {}\n{} / {}\n{} support writes\n{} elapsed ns",
                rule.id,
                rule.operator,
                rule.phase,
                rule.regime,
                rule.work.support_writes,
                rule.elapsed_ns.map_or_else(|| "unmeasured".into(), |n| n.to_string())
            );
            let _ = writeln!(out, "r{} [label={label:?}];", rule.id);
            for dep in &rule.dependencies {
                let node = format!("store:{dep}");
                let _ = writeln!(out, "{node:?} -> r{};", rule.id);
            }
            let head = format!("store:{}", rule.head);
            let style = if rule.operator == "alias" || rule.operator == "virtual-frame" {
                "dashed"
            } else {
                "solid"
            };
            let _ = writeln!(out, "r{} -> {head:?} [style={style}];", rule.id);
        }
        out.push_str("}\n");
        out
    }
}
