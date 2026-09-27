use crate::{SmtError, discover};
use std::{
    io::{Read, Write},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{Arc, Mutex, mpsc},
    thread,
    time::Duration,
};
/// One stable model from clingo JSON output.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AspModel {
    pub atoms: Vec<String>,
    pub costs: Vec<i64>,
}
/// ASP result. A watchdog timeout is unknown, never a proof of unsatisfiability.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AspStatus {
    Satisfiable,
    Unsatisfiable,
    Unknown(String),
}
/// Models and verdict from a single clingo run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AspResult {
    pub status: AspStatus,
    pub models: Vec<AspModel>,
}
/// ASP solver seam.
pub trait AspSolver: Send {
    fn solve(&mut self, program: &str, model_limit: u32, timeout: Duration) -> Result<AspResult, SmtError>;
}
/// One-shot clingo process driver; every solve starts a fresh isolated solver.
#[derive(Default)]
pub struct ClingoProcess {
    pub log_dir: Option<PathBuf>,
}
impl AspSolver for ClingoProcess {
    fn solve(&mut self, program: &str, model_limit: u32, timeout: Duration) -> Result<AspResult, SmtError> {
        if timeout.is_zero() {
            return Ok(AspResult {
                status: AspStatus::Unknown("timeout".into()),
                models: Vec::new(),
            });
        }
        let path = discover("clingo")?;
        let mut child = Command::new(path)
            .arg("--outf=2")
            .arg("-n")
            .arg(model_limit.to_string())
            .arg("-")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let mut input = child
            .stdin
            .take()
            .ok_or_else(|| SmtError::Process("missing clingo stdin".into()))?;
        input.write_all(program.as_bytes())?;
        drop(input);
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| SmtError::Process("missing clingo stdout".into()))?;
        let mut stderr = child
            .stderr
            .take()
            .ok_or_else(|| SmtError::Process("missing clingo stderr".into()))?;
        let output = thread::spawn(move || {
            let mut b = Vec::new();
            stdout.read_to_end(&mut b).map(|_| b)
        });
        let errors = thread::spawn(move || {
            let mut b = Vec::new();
            stderr.read_to_end(&mut b).map(|_| b)
        });
        let child: Arc<Mutex<Child>> = Arc::new(Mutex::new(child));
        let waiter_child = child.clone();
        let (tx, rx) = mpsc::channel();
        let waiter = thread::spawn(move || {
            loop {
                let result = waiter_child
                    .lock()
                    .map_err(|_| SmtError::Process("clingo lock poisoned".into()))
                    .and_then(|mut c| c.try_wait().map_err(SmtError::from));
                match result {
                    Ok(Some(status)) => {
                        let _ = tx.send(Ok(status));
                        break;
                    }
                    Ok(None) => thread::sleep(Duration::from_millis(2)),
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        break;
                    }
                }
            }
        });
        let status = match rx.recv_timeout(timeout) {
            Ok(result) => Some(result?),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                if let Ok(mut c) = child.lock() {
                    let _ = c.kill();
                }
                None
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => return Err(SmtError::Process("clingo waiter stopped".into())),
        };
        waiter
            .join()
            .map_err(|_| SmtError::Process("clingo waiter panicked".into()))?;
        let out = output
            .join()
            .map_err(|_| SmtError::Process("clingo reader panicked".into()))??;
        let err = errors
            .join()
            .map_err(|_| SmtError::Process("clingo stderr reader panicked".into()))??;
        if status.is_none() {
            return Ok(AspResult {
                status: AspStatus::Unknown("timeout".into()),
                models: Vec::new(),
            });
        }
        let raw: serde_json::Value = serde_json::from_slice(&out).map_err(|e| {
            SmtError::Process(format!(
                "invalid clingo JSON: {e}; stderr: {}",
                String::from_utf8_lossy(&err)
            ))
        })?;
        let result = raw
            .get("Result")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| SmtError::Process("clingo JSON has no Result".into()))?;
        let status = match result {
            "SATISFIABLE" | "OPTIMUM FOUND" => AspStatus::Satisfiable,
            "UNSATISFIABLE" => AspStatus::Unsatisfiable,
            "UNKNOWN" => AspStatus::Unknown("clingo returned unknown".into()),
            other => return Err(SmtError::Process(format!("unexpected clingo result {other}"))),
        };
        let mut models = Vec::new();
        if let Some(calls) = raw.get("Call").and_then(serde_json::Value::as_array) {
            for call in calls {
                if let Some(witnesses) = call.get("Witnesses").and_then(serde_json::Value::as_array) {
                    for witness in witnesses {
                        let atoms = witness
                            .get("Value")
                            .and_then(serde_json::Value::as_array)
                            .ok_or_else(|| SmtError::Process("clingo witness missing Value".into()))?
                            .iter()
                            .map(|v| {
                                v.as_str()
                                    .map(str::to_string)
                                    .ok_or_else(|| SmtError::Process("non-string clingo atom".into()))
                            })
                            .collect::<Result<Vec<_>, _>>()?;
                        let costs = witness
                            .get("Costs")
                            .and_then(serde_json::Value::as_array)
                            .map(|xs| {
                                xs.iter()
                                    .map(|v| {
                                        v.as_i64()
                                            .ok_or_else(|| SmtError::Process("non-integer clingo cost".into()))
                                    })
                                    .collect()
                            })
                            .transpose()?
                            .unwrap_or_default();
                        let mut atoms = atoms;
                        atoms.sort();
                        models.push(AspModel { atoms, costs });
                    }
                }
            }
        }
        models.sort_by(|a, b| a.atoms.cmp(&b.atoms).then_with(|| a.costs.cmp(&b.costs)));
        Ok(AspResult { status, models })
    }
}
