use blossom_smt::{AspSolver, ClingoProcess, SmtConfig, SmtProcess, conformance, discover};
use libtest_mimic::{Arguments, Trial};
use std::time::Duration;
fn required(name: &str) -> bool {
    let value = std::env::var("BLOSSOM_REQUIRE_SOLVERS").unwrap_or_default();
    value == "1" || value == "all" || value.split(',').any(|entry| entry.trim() == name)
}
fn run_solver(name: &'static str, kind: blossom_smt::SolverKind) -> Trial {
    let trial = Trial::test(format!("{name}_suite_conformance"), move || {
        let cfg = SmtConfig {
            solver: kind,
            ..SmtConfig::default()
        };
        let mut process = SmtProcess::spawn(&cfg).map_err(|e| e.to_string())?;
        conformance::smt_suite(&mut process).map_err(|e| e.to_string().into())
    });
    if discover(name).is_err() && !required(name) {
        Trial::test(
            format!(
                "{name}_suite_conformance — not run: {name} not found (set BLOSSOM_{})",
                name.to_ascii_uppercase()
            ),
            || Ok(()),
        )
        .with_ignored_flag(true)
    } else {
        trial
    }
}
fn main() {
    let mut trials = vec![
        run_solver("z3", blossom_smt::SolverKind::Z3),
        run_solver("cvc5", blossom_smt::SolverKind::Cvc5),
    ];
    let trial = Trial::test("clingo_suite_models", || {
        let mut clingo = ClingoProcess::default();
        let result = clingo
            .solve("a :- not b. b :- not a.", 0, Duration::from_secs(10))
            .map_err(|e| e.to_string())?;
        if result.models.len() != 2 {
            return Err(format!("expected 2 stable models, got {}", result.models.len()).into());
        }
        Ok(())
    });
    trials.push(if discover("clingo").is_err() && !required("clingo") {
        Trial::test(
            "clingo_suite_models — not run: clingo not found (set BLOSSOM_CLINGO)",
            || Ok(()),
        )
        .with_ignored_flag(true)
    } else {
        trial
    });
    libtest_mimic::run(&Arguments::from_args(), trials).exit();
}
