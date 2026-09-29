use crate::backends::*;
use crate::*;
#[test]
fn sat_suite_exhaustive_selfcheck() {
    conformance::sat_suite(|| Box::new(ExhaustiveSolver::default())).unwrap();
}
#[cfg(feature = "sat-cadical")]
#[test]
fn sat_suite_cadical() {
    conformance::sat_suite(|| Box::new(CadicalSolver::default())).unwrap();
}
#[cfg(feature = "sat-cadical")]
#[test]
fn sat_suite_cadical_plain() {
    conformance::sat_suite(|| Box::new(CadicalSolver::plain().unwrap())).unwrap();
    assert_eq!(select_backend("cadical-plain").unwrap().backend(), "cadical");
}
#[cfg(feature = "sat-batsat")]
#[test]
fn sat_suite_batsat() {
    conformance::sat_suite(|| Box::new(BatSolver::default())).unwrap();
}
#[test]
fn totalizer_all_assignments_and_bounds() {
    check_card(&[1, 1, 1], 3);
}
#[test]
fn totalizer_truncated_bounds() {
    check_card(&[1, 1, 1], 1);
}
#[test]
fn generalized_totalizer_all_assignments_and_bounds() {
    check_card(&[1, 2, 3], 5);
}
#[test]
fn generalized_totalizer_zero_and_large_weights() {
    check_card(&[0, 3], 2);
    check_card(&[100, 1], 1);
}
#[test]
fn generalized_totalizer_duplicate_inputs() {
    let mut s = ExhaustiveSolver::default();
    let v = s.new_var().positive();
    let out = card::generalized_totalizer(&mut s, &[(v, 2), (!v, 1)], 2).unwrap();
    assert_eq!(
        s.solve(&[v, !out[1]], &SolveLimits::default()).unwrap(),
        SatOutcome::Unsat
    );
    assert_eq!(
        s.solve(&[!v, out[1]], &SolveLimits::default()).unwrap(),
        SatOutcome::Unsat
    );
}
fn check_card(weights: &[u64], cap: u64) {
    let mut s = ExhaustiveSolver::default();
    let inputs = weights.iter().map(|_| s.new_var().positive()).collect::<Vec<_>>();
    let weighted = inputs.iter().copied().zip(weights.iter().copied()).collect::<Vec<_>>();
    let out = card::generalized_totalizer(&mut s, &weighted, cap).unwrap();
    assert_eq!(out.len() as u64, weights.iter().sum::<u64>().min(cap + 1));
    for bits in 0..1usize << inputs.len() {
        let fixed = inputs
            .iter()
            .enumerate()
            .map(|(i, l)| if bits >> i & 1 != 0 { *l } else { !*l })
            .collect::<Vec<_>>();
        let sum = weights
            .iter()
            .enumerate()
            .filter(|(i, _)| bits >> i & 1 != 0)
            .map(|(_, w)| w)
            .sum::<u64>();
        assert_eq!(s.solve(&fixed, &SolveLimits::default()).unwrap(), SatOutcome::Sat);
        for (i, l) in out.iter().enumerate() {
            assert_eq!(s.value(l.var()).unwrap() != l.is_negative(), sum > i as u64);
            let mut wrong = fixed.clone();
            wrong.push(if sum > i as u64 { !*l } else { *l });
            assert_eq!(
                s.solve(&wrong, &SolveLimits::default()).unwrap(),
                SatOutcome::Unsat,
                "weights={weights:?},bits={bits},threshold={i}"
            );
            s.solve(&fixed, &SolveLimits::default()).unwrap();
        }
    }
}
#[test]
fn dimacs_roundtrip() {
    let mut dump = DimacsDump::default();
    let a = dump.new_var();
    let b = dump.new_var();
    dump.add_clause(&[a.positive(), b.negative()]).unwrap();
    dump.add_clause(&[]).unwrap();
    let mut bytes = Vec::new();
    dump.write(&mut bytes).unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(text, "p cnf 2 2\n1 -2 0\n0\n");
    let mut parsed = ExhaustiveSolver::default();
    for _ in 0..2 {
        parsed.new_var();
    }
    for line in text.lines().skip(1) {
        let clause = line
            .split_whitespace()
            .map(|n| n.parse::<i64>().unwrap())
            .take_while(|n| *n != 0)
            .map(|n| {
                let v = Var((n.unsigned_abs() - 1) as u32);
                if n < 0 { v.negative() } else { v.positive() }
            })
            .collect::<Vec<_>>();
        parsed.add_clause(&clause).unwrap();
    }
    assert_eq!(parsed.solve(&[], &SolveLimits::default()).unwrap(), SatOutcome::Unsat);
    assert!(dump.solve(&[], &SolveLimits::default()).is_err());
}
#[test]
fn backend_unavailable_error() {
    #[cfg(not(feature = "sat-batsat"))]
    assert!(matches!(
        select_backend("batsat"),
        Err(SatError::BackendUnavailable {
            feature: "sat-batsat",
            ..
        })
    ));
    #[cfg(not(feature = "sat-cadical"))]
    assert!(matches!(
        select_backend("cadical"),
        Err(SatError::BackendUnavailable {
            feature: "sat-cadical",
            ..
        })
    ));
    assert!(matches!(select_backend("missing"), Err(SatError::UnknownBackend(_))));
}
#[test]
fn solver_states_and_assumption_core() {
    for name in ["exhaustive", "cadical", "batsat"] {
        let Ok(mut s) = select_backend(name) else { continue };
        let a = s.new_var();
        let b = s.new_var();
        assert!(s.value(a).is_err());
        s.add_clause(&[a.positive()]).unwrap();
        assert_eq!(
            s.solve(&[a.negative(), b.positive()], &SolveLimits::default()).unwrap(),
            SatOutcome::Unsat
        );
        let core = [a.negative(), b.positive()]
            .into_iter()
            .filter(|l| s.failed_assumption(*l).unwrap())
            .collect::<Vec<_>>();
        assert!(core.contains(&a.negative()));
        assert!(s.value(a).is_err());
        assert_eq!(s.solve(&[], &SolveLimits::default()).unwrap(), SatOutcome::Sat);
        assert!(s.value(a).unwrap());
        assert!(s.failed_assumption(a.negative()).is_err());
        s.new_var();
        assert!(s.value(a).is_err());
        assert!(s.add_clause(&[Var(99).positive()]).is_err());
    }
}
#[test]
fn budgets_unknown_and_resumable() {
    for name in ["exhaustive", "cadical", "batsat"] {
        let Ok(mut s) = select_backend(name) else { continue };
        let a = s.new_var();
        s.add_clause(&[a.positive()]).unwrap();
        for limits in [
            SolveLimits {
                conflicts: Some(0),
                time: None,
            },
            SolveLimits {
                conflicts: None,
                time: Some(std::time::Duration::ZERO),
            },
        ] {
            assert!(matches!(s.solve(&[], &limits).unwrap(), SatOutcome::Unknown(_)));
            assert!(s.value(a).is_err());
        }
        assert_eq!(s.solve(&[], &SolveLimits::default()).unwrap(), SatOutcome::Sat);
    }
}
#[test]
fn exhaustive_variable_limit() {
    let mut s = ExhaustiveSolver::default();
    for _ in 0..25 {
        s.new_var();
    }
    assert_eq!(
        s.solve(&[], &SolveLimits::default()).unwrap(),
        SatOutcome::Unknown(LimitHit::Variables)
    );
}

#[test]
fn exhaustive_core_excludes_irrelevant_assumption() {
    let mut s = ExhaustiveSolver::default();
    let a = s.new_var();
    let b = s.new_var();
    s.add_clause(&[a.positive()]).unwrap();
    assert_eq!(
        s.solve(&[a.negative(), b.positive()], &SolveLimits::default()).unwrap(),
        SatOutcome::Unsat
    );
    assert!(s.failed_assumption(a.negative()).unwrap());
    assert!(!s.failed_assumption(b.positive()).unwrap());
}
