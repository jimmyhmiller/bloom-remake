//! Inspect the value engine's actual physical plan and compare checked workload reports.
use crate::common::Context;
use crate::exit::Exit;
use blossom_engine::{Engine, EngineConfig, PlanReport};
use blossom_front::api::NodeSpec;
use std::path::PathBuf;
use std::process::ExitCode;
use std::sync::Arc;

#[derive(Debug, clap::Args)]
pub struct Args {
    /// Program source; without --nodes use the frontend's default deployment.
    pub file: Option<String>,
    /// Render a previously measured trace-plan JSON report.
    #[arg(long, conflicts_with_all = ["file", "deploy", "nodes", "node", "compare"])]
    pub report: Option<PathBuf>,
    /// Emit a Graphviz dependency graph instead of JSON.
    #[arg(long, conflicts_with = "compare")]
    pub dot: bool,
    /// Limit the graph to the neighborhood of a store key, e.g. sequenced:main.
    #[arg(long, requires = "dot")]
    pub focus: Option<String>,
    /// Dependency hops around --focus.
    #[arg(long, default_value_t = 1, requires = "dot")]
    pub depth: usize,
    /// Compile the actual deployment instead of a source file.
    #[arg(long, conflicts_with = "file")]
    pub deploy: Option<PathBuf>,
    /// Deployment nodes as NAME=ROLE, or NAME, comma separated.
    #[arg(long, value_delimiter = ',')]
    pub nodes: Vec<String>,
    /// Node to inspect (default: first deployment node).
    #[arg(long)]
    pub node: Option<String>,
    /// Compare two JSON reports produced by trace plan. Requires identical trace and window.
    #[arg(long, num_args = 2, conflicts_with_all = ["file", "deploy", "nodes", "node"])]
    pub compare: Vec<PathBuf>,
}

pub fn run(args: Args, _: &Context) -> ExitCode {
    match drive(args) {
        Ok(value) => {
            println!("{}", value);
            Exit::Ok.into()
        }
        Err(e) => {
            eprintln!("blossom plan: {e}");
            Exit::Fault.into()
        }
    }
}
fn drive(args: Args) -> Result<String, String> {
    if !args.compare.is_empty() {
        let read = |p: &PathBuf| -> Result<serde_json::Value, String> {
            serde_json::from_reader(std::fs::File::open(p).map_err(|e| e.to_string())?).map_err(|e| e.to_string())
        };
        let [reference, candidate] = args.compare.as_slice() else {
            return Err("comparison requires two reports".into());
        };
        return compare(&read(reference)?, &read(candidate)?)
            .and_then(|v| serde_json::to_string_pretty(&v).map_err(|e| e.to_string()));
    }
    if let Some(path) = args.report {
        let value: serde_json::Value = serde_json::from_reader(std::fs::File::open(path).map_err(|e| e.to_string())?)
            .map_err(|e| e.to_string())?;
        let plan: PlanReport =
            serde_json::from_value(value.get("plan").unwrap_or(&value).clone()).map_err(|e| e.to_string())?;
        return render(&plan, args.dot, args.focus.as_deref(), args.depth);
    }
    let mut physical = EngineConfig::default();
    let artifact = if let Some(deploy) = args.deploy {
        let (spec, artifact) = crate::cmd::run::load(&deploy).map_err(|_| "deployment did not load")?;
        physical.workers = spec.eval_workers;
        physical.map_cache_rows = spec.map_cache_rows;
        physical.map_cache_joins = spec.map_cache_joins;
        physical.lazy_indexes = spec.lazy_indexes;
        physical.pipeline_batches = spec.pipeline_batches;
        physical.infer_keys = spec.infer_keys;
        physical.compiled_joins = spec.compiled_joins;
        physical.packed_indexes = spec.packed_indexes;
        physical.fuse_relations = spec.fuse_relations;
        physical.hot_rows = spec.hot_rows;
        physical.in_memory = !spec.tiered;
        artifact
    } else {
        let file = args
            .file
            .ok_or("give FILE, --deploy FILE, or --compare REFERENCE CANDIDATE")?;
        let nodes: Vec<NodeSpec> = args
            .nodes
            .iter()
            .map(|n| match n.split_once('=') {
                Some((name, role)) => NodeSpec {
                    name: name.into(),
                    role: Some(role.into()),
                },
                None => NodeSpec {
                    name: n.clone(),
                    role: None,
                },
            })
            .collect();
        let (result, sources) = if nodes.is_empty() {
            blossom_driver::bls::check_file(&file)
        } else {
            blossom_driver::bls::compile_file(&file, &nodes)
        };
        let (artifact, warnings) = result.map_err(|e| format!("{e:?}"))?;
        for d in warnings.iter() {
            eprint!("{}", blossom_driver::render::render(d, &sources));
        }
        Arc::new(artifact)
    };
    let name = args
        .node
        .as_deref()
        .or_else(|| artifact.nodes.first().map(|n| n.as_str()))
        .ok_or("deployment has no nodes")?;
    let node = artifact.node_id(name).ok_or_else(|| format!("unknown node {name}"))?;
    let engine = Engine::new(
        artifact.program.clone(),
        node,
        EngineConfig {
            roles: artifact.roles.clone(),
            node_names: artifact.nodes.iter().map(|n| Arc::from(n.as_str())).collect(),
            externs: crate::common::std_externs().map_err(|e| e.to_string())?,
            members: Arc::new(artifact.members()?),
            ..physical
        },
    )
    .map_err(|e| e.to_string())?;
    render(&engine.plan_report(), args.dot, args.focus.as_deref(), args.depth)
}

fn render(plan: &PlanReport, dot: bool, focus: Option<&str>, depth: usize) -> Result<String, String> {
    if plan.schema_version != 1 {
        return Err("unsupported plan schema".into());
    }
    if dot {
        let mut plan = plan.clone();
        if let Some(focus) = focus {
            if !plan.stores.iter().any(|s| s.key == focus) && !plan.rules.iter().any(|r| r.head == focus) {
                return Err(format!("unknown store {focus}"));
            }
            let mut keys = std::collections::BTreeSet::from([focus.to_owned()]);
            let mut ids = std::collections::BTreeSet::new();
            for _ in 0..depth {
                let mut next = keys.clone();
                for rule in &plan.rules {
                    if keys.contains(&rule.head) || rule.dependencies.iter().any(|k| keys.contains(k)) {
                        ids.insert(rule.id);
                        next.insert(rule.head.clone());
                        next.extend(rule.dependencies.iter().cloned());
                    }
                }
                keys = next;
            }
            plan.rules.retain(|r| ids.contains(&r.id));
            plan.stores.retain(|s| keys.contains(&s.key));
        }
        Ok(plan.dot())
    } else {
        serde_json::to_string_pretty(plan).map_err(|e| e.to_string())
    }
}

pub(super) fn compare(a: &serde_json::Value, b: &serde_json::Value) -> Result<serde_json::Value, String> {
    for field in ["report_version", "workload", "ticks"] {
        if a.get(field).is_none() || a[field] != b[field] {
            return Err(format!(
                "reports differ or lack {field}; compare identical checked workload windows"
            ));
        }
    }
    if a.get("workload")
        .and_then(|w| w.get("trace_blake3"))
        .and_then(serde_json::Value::as_str)
        .is_none_or(|h| h.len() != 64 || !h.bytes().all(|b| b.is_ascii_hexdigit()))
    {
        return Err("missing or invalid workload trace hash".into());
    }
    if a["report_version"] != 1 || a["ticks"].as_u64().is_none_or(|n| n == 0) {
        return Err("unsupported report version or empty workload".into());
    }
    let parse = |v: &serde_json::Value| -> Result<PlanReport, String> {
        serde_json::from_value(v["plan"].clone()).map_err(|e| e.to_string())
    };
    let (a_plan, b_plan) = (parse(a)?, parse(b)?);
    if a_plan.schema_version != 1
        || b_plan.schema_version != 1
        || a_plan.program_digest != b_plan.program_digest
        || a_plan.node != b_plan.node
    {
        return Err("different program or unsupported plan schema".into());
    }
    let totals = |p: &PlanReport| {
        p.rules.iter().fold([0u64; 4], |mut t, r| {
            t[0] += r.work.probe_rows;
            t[1] += r.work.expression_steps;
            t[2] += r.work.support_writes;
            t[3] += r.work.evaluations;
            t
        })
    };
    let (at, bt) = (totals(&a_plan), totals(&b_plan));
    let rows: Vec<_> = ["probe_rows", "expression_steps", "support_writes", "evaluations"].iter().zip(at).zip(bt).map(|((metric, reference), candidate)| serde_json::json!({"metric":metric,"reference":reference,"candidate":candidate,"change_percent": if reference == 0 { None } else { Some((candidate as f64 / reference as f64 - 1.0)*100.0) }})).collect();
    let mut per_rule = Vec::new();
    for r in &b_plan.rules {
        let reference = a_plan.rules.iter().find(|a| a.id == r.id);
        per_rule.push(serde_json::json!({"id":r.id,"head":r.head,"reference":reference.map(|r| &r.work),"candidate":r.work,"operator":r.operator,"reference_elapsed_ns":reference.and_then(|r| r.elapsed_ns),"candidate_elapsed_ns":r.elapsed_ns}));
    }
    let physical = |p: &PlanReport| -> Option<serde_json::Value> {
        let mut totals = [0u64; 7];
        for s in &p.stores {
            let m = s.operations.as_ref()?;
            for (total, value) in totals.iter_mut().zip([
                m.probe_calls,
                m.membership_changes,
                m.secondary_index_row_updates,
                m.secondary_index_build_rows,
                m.bulk_retracted_rows,
                m.bulk_index_clears,
                m.batch_weight_updates,
            ]) {
                *total += value;
            }
        }
        let mut out = serde_json::Map::new();
        for (name, total) in [
            "probe_calls",
            "membership_changes",
            "secondary_index_row_updates",
            "secondary_index_build_rows",
            "bulk_retracted_rows",
            "bulk_index_clears",
            "batch_weight_updates",
        ]
        .iter()
        .zip(totals)
        {
            out.insert((*name).to_owned(), total.into());
        }
        Some(out.into())
    };
    Ok(
        serde_json::json!({"workload":a["workload"],"reference_execution":a["execution"],"candidate_execution":b["execution"],"ticks":a["ticks"],"totals":rows,"reference_physical_totals":physical(&a_plan),"candidate_physical_totals":physical(&b_plan),"rules":per_rule,"reference_stores":a_plan.stores,"candidate_stores":b_plan.stores,"reference_replay_elapsed_ns":a["replay_elapsed_ns"],"candidate_replay_elapsed_ns":b["replay_elapsed_ns"],"limitations":["Elapsed time is replay wall time, not CPU; repeat counterbalanced runs with other work idle.","Logical support writes exclude native membership and batch updates; inspect optional physical totals separately. Allocations, I/O waits and database index work are unmeasured.","Replay uses in-memory recovered state; broker durability and cold-storage costs need live measurement."]}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn comparison_rejects_unmatched_or_empty_workloads() {
        let a = serde_json::json!({"report_version":1,"workload":"a","ticks":1});
        let b = serde_json::json!({"report_version":1,"workload":"b","ticks":1});
        assert!(compare(&a, &b).is_err());
        assert!(compare(&serde_json::json!({}), &serde_json::json!({})).is_err());
    }
}

#[cfg(test)]
mod deployment_plan_tests {
    use super::*;
    #[test]
    fn deployment_inspection_uses_the_selected_execution_options() {
        let (path, file) = (0u64..)
            .find_map(|n| {
                let path = std::env::temp_dir().join(format!("blossom-plan-deploy-{}-{n}.toml", std::process::id()));
                match std::fs::OpenOptions::new().write(true).create_new(true).open(&path) {
                    Ok(file) => Some((path, file)),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => None,
                    Err(e) => panic!("cannot create test deployment: {e}"),
                }
            })
            .unwrap();
        struct Scratch(PathBuf);
        impl Drop for Scratch {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }
        let scratch = Scratch(path);
        use std::io::Write;
        let mut file = file;
        let source =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../tests/integration/fixtures/engine/retained_map.bls");
        writeln!(file,"format = 1\n[deployment]\nid = \"plan\"\nprogram = \"retained_map\"\nversion = 1\nsource = {:?}\n[[node]]\nname = \"n1\"\naddr = \"127.0.0.1:29999\"\nprincipal = \"spiffe://plan/n1\"\n[security]\nmode = \"insecure-dev\"\n[storage]\ndata_dir = \"plan-test-data\"\neval_workers = 2\nmap_cache_rows = 128\nmap_cache_joins = true\nlazy_indexes = true\npipeline_batches = true\ninfer_keys = true",source.to_str().unwrap()).unwrap();
        drop(file);
        let json = drive(Args {
            file: None,
            deploy: Some(scratch.0.clone()),
            nodes: vec![],
            node: None,
            compare: vec![],
            report: None,
            dot: false,
            focus: None,
            depth: 1,
        })
        .unwrap();
        let report: PlanReport = serde_json::from_str(&json).unwrap();
        assert_eq!(report.evaluation_workers, 2);
        assert_eq!(report.map_cache.row_budget, 128);
        assert!(report.lazy_indexes);
        assert!(report.rules.iter().any(|r| r.operator == "retained-pipeline-batch"));
        assert!(report.stores.iter().any(|s| s.batch_backed));
    }
}
