//! Slice 1: every failure-free `.ded` case of the Molly corpus (`[backend.oracle]`) runs from source text through
//! the `.ded` frontend, the oracle and the synchronous-round world, and matches its expected rows.

use std::path::{Path, PathBuf};

use blossom_artifact::ded::DedArtifact;
use blossom_driver::ded::compile_files;
use blossom_driver::render::render;
use blossom_front::ded::DedError;
use blossom_sim::FaultSchedule;
use blossom_sim::ded::DedSim;
use blossom_value::time::Tick;
use blossom_value::types::IntTy;
use blossom_value::value::IntValue;
use blossom_value::{TypeDef, Value};

#[cfg(test)]
fn corpus() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../corpus/ldfi/molly")
}

#[cfg(test)]
fn manifest(case: &Path) -> toml::Table {
    toml::from_str(&std::fs::read_to_string(case.join("manifest.toml")).unwrap()).unwrap()
}

#[cfg(test)]
fn compile_case(case: &Path, m: &toml::Table, nodes: &[String]) -> DedArtifact {
    let program = case.join(m["program"].as_str().unwrap());
    let nodes: Vec<&str> = nodes.iter().map(String::as_str).collect();
    let (result, sources) = compile_files(&[program.to_str().unwrap()], &nodes);
    match result {
        Ok(a) => a,
        Err(DedError::Rejected(d)) => panic!(
            "{}:\n{}",
            case.display(),
            d.iter().map(|d| render(d, &sources)).collect::<String>()
        ),
        Err(e) => panic!("{}: {e}", case.display()),
    }
}

/// A manifest value as a Blossom value of the column's type: strings in node columns name nodes.
#[cfg(test)]
fn value(a: &DedArtifact, v: &toml::Value, ty: &TypeDef) -> Value {
    match (v, ty) {
        (toml::Value::Integer(i), TypeDef::Int(IntTy::U64)) => Value::Int(IntValue::U64(*i as u64)),
        (toml::Value::Integer(i), _) => Value::Int(IntValue::I64(*i)),
        (toml::Value::String(s), TypeDef::Node(_)) => Value::Node(a.node_id(s).unwrap()),
        (toml::Value::String(s), _) => Value::Str(s.as_str().into()),
        other => panic!("unsupported manifest value {other:?}"),
    }
}

/// `k`, `a..=b`, `a..` (to the last tick) or `..=b` (from tick 0).
#[cfg(test)]
fn ticks(range: &toml::Value, last: u64) -> Vec<u64> {
    let r = match range {
        toml::Value::Integer(i) => return vec![*i as u64],
        toml::Value::String(s) => s.clone(),
        other => panic!("bad range {other:?}"),
    };
    match r.split_once("..") {
        None => vec![r.parse().unwrap()],
        Some((lo, hi)) => {
            let lo = if lo.is_empty() { 0 } else { lo.parse().unwrap() };
            let hi = match hi.strip_prefix('=') {
                Some(h) => h.parse().unwrap(),
                None => last,
            };
            (lo..=hi).collect()
        }
    }
}

#[test]
fn ded_failure_free_cases_match_their_expectations() {
    let mut cases: Vec<PathBuf> = std::fs::read_dir(corpus())
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.join("manifest.toml").is_file())
        .collect();
    cases.sort();
    let mut checked = 0;
    let mut failures = Vec::new();
    for case in cases {
        let m = manifest(&case);
        if m.get("backend").and_then(|b| b.get("oracle")).is_none() {
            continue;
        }
        let nodes: Vec<String> = m["deploy"]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| n["name"].as_str().unwrap().to_owned())
            .collect();
        let a = compile_case(&case, &m, &nodes);
        let run_ticks = m["run"]["ticks"].as_integer().unwrap() as u64;
        let last = run_ticks - 1;
        let sim = DedSim::new(&a).unwrap();
        let run = sim.run(Tick(last), &FaultSchedule::default(), false).unwrap();
        let name = case.file_name().unwrap().to_string_lossy().into_owned();
        for x in m["expect"].as_array().unwrap() {
            let node = a.node_id(x["node"].as_str().unwrap()).unwrap();
            let rel_name = x["rel"].as_str().unwrap();
            let ded = a
                .rels
                .iter()
                .find(|r| r.name.as_str() == rel_name)
                .unwrap_or_else(|| panic!("{name}: no relation {rel_name}"));
            let rel = ded.protocol.unwrap();
            let decl = a.protocol.get().rels.get(rel).unwrap();
            let col_types: Vec<TypeDef> = decl
                .schema
                .cols
                .iter()
                .map(|c| a.protocol.get().types.get(c.ty).unwrap().clone())
                .collect();
            let to_row = |vals: &toml::Value| -> Vec<Value> {
                vals.as_array()
                    .unwrap()
                    .iter()
                    .zip(&col_types)
                    .map(|(v, ty)| value(&a, v, ty))
                    .collect()
            };
            let at = |t: u64| -> Vec<Vec<Value>> {
                run.node_tick(Tick(t), node)
                    .map(|nt| nt.instance.rows(rel).map(|r| r.to_vec()).collect())
                    .unwrap_or_default()
            };
            if let Some(t) = x.get("tick") {
                let t = t.as_integer().unwrap() as u64;
                let mut want: Vec<Vec<Value>> = x["rows"].as_array().unwrap().iter().map(to_row).collect();
                want.sort();
                let got = at(t);
                if got != want {
                    failures.push(format!(
                        "{name}: {rel_name}@{} tick {t}: got {got:?}, want {want:?}",
                        node.0
                    ));
                }
                continue;
            }
            let row = to_row(&x["row"]);
            for (key, present) in [("holds", true), ("absent", false)] {
                if let Some(r) = x.get(key) {
                    for t in ticks(r, last) {
                        if at(t).contains(&row) != present {
                            failures.push(format!(
                                "{name}: {rel_name}{row:?} at node {} tick {t}: expected {}",
                                x["node"].as_str().unwrap(),
                                if present { "present" } else { "absent" }
                            ));
                        }
                    }
                }
            }
        }
        checked += 1;
    }
    assert!(checked >= 19, "only {checked} failure-free cases found");
    assert!(
        failures.is_empty(),
        "{} mismatches:\n{}",
        failures.len(),
        failures.join("\n")
    );
}
