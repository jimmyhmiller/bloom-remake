use crate::{Sexp, SmtError, Sort, Term};
use std::{
    collections::BTreeMap,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, Write},
    path::PathBuf,
    process::{Child, ChildStdin, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, RecvTimeoutError},
    },
    thread::{self, JoinHandle},
    time::Duration,
};
static SESSION: AtomicU64 = AtomicU64::new(0);
/// Supported SMT-LIB2 subprocesses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SolverKind {
    Z3,
    Cvc5,
}
impl SolverKind {
    fn name(self) -> &'static str {
        match self {
            Self::Z3 => "z3",
            Self::Cvc5 => "cvc5",
        }
    }
    fn args(self) -> &'static [&'static str] {
        match self {
            Self::Z3 => &["-in", "-smt2"],
            Self::Cvc5 => &["--lang=smt2", "--incremental", "--produce-models"],
        }
    }
}
/// Solver launch and transcript configuration.
#[derive(Clone, Debug)]
pub struct SmtConfig {
    pub solver: SolverKind,
    pub log_dir: Option<PathBuf>,
    pub command_timeout: Duration,
}
impl Default for SmtConfig {
    fn default() -> Self {
        Self {
            solver: SolverKind::Z3,
            log_dir: None,
            command_timeout: Duration::from_secs(30),
        }
    }
}
/// Find a solver from its explicit environment override, PATH, then local `.tools/bin`.
pub fn discover(solver: &str) -> Result<PathBuf, SmtError> {
    let env_name = match solver {
        "z3" => "BLOSSOM_Z3",
        "cvc5" => "BLOSSOM_CVC5",
        "clingo" => "BLOSSOM_CLINGO",
        _ => return Err(SmtError::Process(format!("unknown solver {solver}"))),
    };
    let mut searched = Vec::new();
    if let Some(path) = std::env::var_os(env_name) {
        let p = PathBuf::from(path);
        searched.push(p.clone());
        if p.is_file() {
            return Ok(p);
        }
        return Err(SmtError::SolverNotFound {
            solver: solver.into(),
            searched,
        });
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for dir in std::env::split_paths(&paths) {
            let p = dir.join(solver);
            searched.push(p.clone());
            if p.is_file() {
                return Ok(p);
            }
        }
    }
    let p = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(".tools/bin")
        .join(solver);
    searched.push(p.clone());
    if p.is_file() {
        return Ok(p);
    }
    Err(SmtError::SolverNotFound {
        solver: solver.into(),
        searched,
    })
}
/// Solver verdict; unknown never asserts a proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum SmtAnswer {
    Sat,
    Unsat,
    Unknown(Arc<str>),
}
/// Decoded model value for the supported SMT-LIB scalar and array sorts.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ModelValue {
    Int(i128),
    Bool(bool),
    BitVec { value: u128, width: u32 },
    Array(Sexp),
    Uninterpreted(Arc<str>),
    Other(Sexp),
}
fn model_value(sort: &Sexp, value: &Sexp, args: &[Sexp]) -> ModelValue {
    if !args.is_empty() {
        return ModelValue::Other(value.clone());
    }
    match sort {
        Sexp::Atom(a) if a.as_ref() == "Int" => {
            if let Some(n) = parse_integer(value) {
                ModelValue::Int(n)
            } else {
                ModelValue::Other(value.clone())
            }
        }
        Sexp::Atom(a) if a.as_ref() == "Bool" => match value {
            Sexp::Atom(b) if b.as_ref() == "true" => ModelValue::Bool(true),
            Sexp::Atom(b) if b.as_ref() == "false" => ModelValue::Bool(false),
            _ => ModelValue::Other(value.clone()),
        },
        Sexp::List(xs) if matches!(xs.first(),Some(Sexp::Atom(a)) if a.as_ref()=="Array") => {
            ModelValue::Array(value.clone())
        }
        Sexp::List(xs)
            if xs.len() == 3
                && matches!(xs.first(),Some(Sexp::Atom(a)) if a.as_ref()=="_")
                && matches!(xs.get(1),Some(Sexp::Atom(a)) if a.as_ref()=="BitVec") =>
        {
            let width = xs.get(2).and_then(|x| match x {
                Sexp::Atom(a) => a.parse().ok(),
                _ => None,
            });
            if let Some(width) = width
                && let Some(value) = parse_bitvec(value)
            {
                ModelValue::BitVec { value, width }
            } else {
                ModelValue::Other(value.clone())
            }
        }
        Sexp::Atom(_) => match value {
            Sexp::Atom(a) => ModelValue::Uninterpreted(a.clone()),
            _ => ModelValue::Other(value.clone()),
        },
        _ => ModelValue::Other(value.clone()),
    }
}
fn parse_integer(value: &Sexp) -> Option<i128> {
    match value {
        Sexp::Atom(a) => a.parse().ok(),
        Sexp::List(xs) if xs.len() == 2 && matches!(xs.first(),Some(Sexp::Atom(a)) if a.as_ref()=="-") => {
            match xs.get(1) {
                Some(Sexp::Atom(a)) => a.parse::<i128>().ok()?.checked_neg(),
                _ => None,
            }
        }
        _ => None,
    }
}
fn parse_bitvec(value: &Sexp) -> Option<u128> {
    match value {
        Sexp::Atom(a) if a.starts_with("#x") => u128::from_str_radix(a.strip_prefix("#x")?, 16).ok(),
        Sexp::Atom(a) if a.starts_with("#b") => u128::from_str_radix(a.strip_prefix("#b")?, 2).ok(),
        Sexp::List(xs) if xs.len() == 3 && matches!(xs.first(),Some(Sexp::Atom(a)) if a.as_ref()=="_") => {
            match xs.get(1) {
                Some(Sexp::Atom(a)) => a.strip_prefix("bv")?.parse().ok(),
                _ => None,
            }
        }
        _ => None,
    }
}
/// A function or constant definition returned by get-model.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelDecl {
    pub name: Arc<str>,
    pub args: Vec<Sexp>,
    pub sort: Sexp,
    pub value: Sexp,
    pub interpreted: ModelValue,
}
/// Parsed model, retaining array stores, uninterpreted elements, and bit-vectors without loss.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SmtModel {
    pub raw: Sexp,
    pub declarations: BTreeMap<Arc<str>, ModelDecl>,
}
impl SmtModel {
    /// Parse standard `(model (define-fun ...) ...)` form.
    pub fn parse(raw: Sexp) -> Result<Self, SmtError> {
        let mut declarations = BTreeMap::new();
        let xs = match &raw {
            Sexp::List(xs) => xs,
            _ => {
                return Err(SmtError::Parse {
                    offset: 0,
                    message: "model must be a list".into(),
                });
            }
        };
        let definitions = if matches!(xs.first(),Some(Sexp::Atom(a)) if a.as_ref()=="model") {
            xs.get(1..).unwrap_or_default()
        } else {
            xs.as_slice()
        };
        for item in definitions {
            let Sexp::List(fields) = item else {
                return Err(SmtError::Parse {
                    offset: 0,
                    message: "definition must be a list".into(),
                });
            };
            if fields.len() != 5 || !matches!(fields.first(),Some(Sexp::Atom(a)) if a.as_ref()=="define-fun") {
                return Err(SmtError::Parse {
                    offset: 0,
                    message: "define-fun expected".into(),
                });
            }
            let Some(Sexp::Atom(name)) = fields.get(1) else {
                return Err(SmtError::Parse {
                    offset: 0,
                    message: "definition name".into(),
                });
            };
            let Some(Sexp::List(args)) = fields.get(2) else {
                return Err(SmtError::Parse {
                    offset: 0,
                    message: "definition args".into(),
                });
            };
            let sort = fields
                .get(3)
                .cloned()
                .ok_or_else(|| SmtError::Process("model sort missing".into()))?;
            let value = fields
                .get(4)
                .cloned()
                .ok_or_else(|| SmtError::Process("model value missing".into()))?;
            let decl = ModelDecl {
                name: name.clone(),
                args: args.clone(),
                interpreted: model_value(&sort, &value, args),
                sort,
                value,
            };
            if declarations.insert(name.clone(), decl).is_some() {
                return Err(SmtError::Parse {
                    offset: 0,
                    message: "duplicate model definition".into(),
                });
            }
        }
        Ok(Self { raw, declarations })
    }
}
/// Stateful SMT solver seam.
pub trait SmtSolver: Send {
    fn set_logic(&mut self, logic: &str) -> Result<(), SmtError>;
    fn declare_sort(&mut self, name: &str, arity: u32) -> Result<(), SmtError>;
    fn declare_fun(&mut self, name: &str, args: &[Sort], ret: &Sort) -> Result<(), SmtError>;
    fn assert(&mut self, t: &Term, name: Option<&str>) -> Result<(), SmtError>;
    fn push(&mut self) -> Result<(), SmtError>;
    fn pop(&mut self, n: u32) -> Result<(), SmtError>;
    fn check(&mut self, assumptions: &[Term], timeout: Duration) -> Result<SmtAnswer, SmtError>;
    fn model(&mut self) -> Result<SmtModel, SmtError>;
    fn unsat_core(&mut self) -> Result<Vec<Arc<str>>, SmtError>;
}
/// Child process and reply reader. A timed-out check kills the child and reconstructs the prior solver state.
pub struct SmtProcess {
    cfg: SmtConfig,
    child: Child,
    input: ChildStdin,
    replies: Receiver<Result<Sexp, SmtError>>,
    reader: Option<JoinHandle<()>>,
    history: Vec<String>,
    transcript: Option<File>,
    last: Option<SmtAnswer>,
    depth: u32,
}
impl SmtProcess {
    /// Spawn, initialize print-success, and start a bounded response reader.
    pub fn spawn(cfg: &SmtConfig) -> Result<Self, SmtError> {
        let (child, input, replies, reader) = launch(cfg)?;
        let transcript = if let Some(dir) = &cfg.log_dir {
            std::fs::create_dir_all(dir)?;
            let id = SESSION.fetch_add(1, Ordering::Relaxed);
            Some(
                OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .open(dir.join(format!("session-{}-{id}.smt2", std::process::id())))?,
            )
        } else {
            None
        };
        let mut this = Self {
            cfg: cfg.clone(),
            child,
            input,
            replies,
            reader: Some(reader),
            history: Vec::new(),
            transcript,
            last: None,
            depth: 0,
        };
        this.command("(set-option :print-success true)", cfg.command_timeout)?;
        this.success("(set-option :produce-unsat-cores true)".into())?;
        Ok(this)
    }
    fn log(&mut self, command: &str) -> Result<(), SmtError> {
        if let Some(file) = &mut self.transcript {
            writeln!(file, "{command}")?;
            file.flush()?;
        }
        Ok(())
    }
    fn command(&mut self, command: &str, timeout: Duration) -> Result<Sexp, SmtError> {
        self.log(command)?;
        writeln!(self.input, "{command}")?;
        self.input.flush()?;
        match self.replies.recv_timeout(timeout) {
            Ok(Ok(Sexp::List(xs))) if matches!(xs.first(),Some(Sexp::Atom(a)) if a.as_ref()=="error") => {
                let message = xs.get(1).map(ToString::to_string).unwrap_or_default();
                Err(SmtError::Solver {
                    command: command.into(),
                    message,
                })
            }
            Ok(Ok(reply)) => Ok(reply),
            Ok(Err(e)) => Err(e),
            Err(RecvTimeoutError::Timeout) => Err(SmtError::Process("response timeout".into())),
            Err(RecvTimeoutError::Disconnected) => Err(SmtError::Process("solver closed response stream".into())),
        }
    }
    fn success(&mut self, command: String) -> Result<(), SmtError> {
        let reply = self.command(&command, self.cfg.command_timeout)?;
        if reply != Sexp::atom("success") {
            return Err(SmtError::Solver {
                command,
                message: format!("expected success, got {reply}"),
            });
        }
        self.history.push(command);
        self.last = None;
        Ok(())
    }
    fn restart(&mut self) -> Result<(), SmtError> {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
        let (child, input, replies, reader) = launch(&self.cfg)?;
        self.child = child;
        self.input = input;
        self.replies = replies;
        self.reader = Some(reader);
        self.last = None;
        let init = self.command("(set-option :print-success true)", self.cfg.command_timeout)?;
        if init != Sexp::atom("success") {
            return Err(SmtError::Process("restart initialization failed".into()));
        }
        for command in self.history.clone() {
            let reply = self.command(&command, self.cfg.command_timeout)?;
            if reply != Sexp::atom("success") {
                return Err(SmtError::Process(format!("restart replay failed: {command}")));
            }
        }
        Ok(())
    }
}
impl Drop for SmtProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}
impl SmtSolver for SmtProcess {
    fn set_logic(&mut self, logic: &str) -> Result<(), SmtError> {
        self.success(format!("(set-logic {})", Sexp::symbol(logic)))
    }
    fn declare_sort(&mut self, name: &str, arity: u32) -> Result<(), SmtError> {
        self.success(format!("(declare-sort {} {arity})", Sexp::symbol(name)))
    }
    fn declare_fun(&mut self, name: &str, args: &[Sort], ret: &Sort) -> Result<(), SmtError> {
        self.success(format!(
            "(declare-fun {} ({}) {ret})",
            Sexp::symbol(name),
            args.iter().map(ToString::to_string).collect::<Vec<_>>().join(" ")
        ))
    }
    fn assert(&mut self, t: &Term, name: Option<&str>) -> Result<(), SmtError> {
        let term = if let Some(n) = name {
            format!("(! {t} :named {})", Sexp::symbol(n))
        } else {
            t.to_string()
        };
        self.success(format!("(assert {term})"))
    }
    fn push(&mut self) -> Result<(), SmtError> {
        self.success("(push 1)".into())?;
        self.depth = self
            .depth
            .checked_add(1)
            .ok_or(SmtError::InvalidState("stack depth overflow"))?;
        Ok(())
    }
    fn pop(&mut self, n: u32) -> Result<(), SmtError> {
        if n > self.depth {
            return Err(SmtError::InvalidState("a matching push"));
        }
        self.success(format!("(pop {n})"))?;
        self.depth -= n;
        Ok(())
    }
    fn check(&mut self, assumptions: &[Term], timeout: Duration) -> Result<SmtAnswer, SmtError> {
        self.last = None;
        if timeout.is_zero() {
            return Ok(SmtAnswer::Unknown("timeout".into()));
        }
        let ms = timeout.as_millis().max(1).min(u128::from(u32::MAX));
        let option = match self.cfg.solver {
            SolverKind::Z3 => format!("(set-option :timeout {ms})"),
            SolverKind::Cvc5 => format!("(set-option :tlimit-per {ms})"),
        };
        let reply = self.command(&option, self.cfg.command_timeout)?;
        if reply != Sexp::atom("success") {
            return Err(SmtError::Solver {
                command: option,
                message: reply.to_string(),
            });
        }
        let command = if assumptions.is_empty() {
            "(check-sat)".to_string()
        } else {
            format!(
                "(check-sat-assuming ({}))",
                assumptions
                    .iter()
                    .map(ToString::to_string)
                    .collect::<Vec<_>>()
                    .join(" ")
            )
        };
        let reply = self.command(&command, timeout);
        let answer = match reply {
            Ok(Sexp::Atom(a)) if a.as_ref() == "sat" => SmtAnswer::Sat,
            Ok(Sexp::Atom(a)) if a.as_ref() == "unsat" => SmtAnswer::Unsat,
            Ok(Sexp::Atom(a)) if a.as_ref() == "unknown" => SmtAnswer::Unknown("solver returned unknown".into()),
            Err(SmtError::Process(m)) if m == "response timeout" => {
                self.restart()?;
                return Ok(SmtAnswer::Unknown("timeout".into()));
            }
            Ok(other) => {
                return Err(SmtError::Solver {
                    command,
                    message: format!("unexpected answer {other}"),
                });
            }
            Err(e) => return Err(e),
        };
        self.last = Some(answer.clone());
        Ok(answer)
    }
    fn model(&mut self) -> Result<SmtModel, SmtError> {
        if self.last != Some(SmtAnswer::Sat) {
            return Err(SmtError::InvalidState("a SAT result"));
        }
        SmtModel::parse(self.command("(get-model)", self.cfg.command_timeout)?)
    }
    fn unsat_core(&mut self) -> Result<Vec<Arc<str>>, SmtError> {
        if self.last != Some(SmtAnswer::Unsat) {
            return Err(SmtError::InvalidState("an UNSAT result"));
        }
        let reply = self.command("(get-unsat-core)", self.cfg.command_timeout)?;
        match reply {
            Sexp::List(xs) => xs
                .into_iter()
                .map(|x| match x {
                    Sexp::Atom(a) => Ok(a),
                    _ => Err(SmtError::Parse {
                        offset: 0,
                        message: "unsat core element".into(),
                    }),
                })
                .collect(),
            _ => Err(SmtError::Parse {
                offset: 0,
                message: "unsat core list".into(),
            }),
        }
    }
}
type Launch = (Child, ChildStdin, Receiver<Result<Sexp, SmtError>>, JoinHandle<()>);
fn launch(cfg: &SmtConfig) -> Result<Launch, SmtError> {
    let path = discover(cfg.solver.name())?;
    let mut child = Command::new(path)
        .args(cfg.solver.args())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()?;
    let input = child
        .stdin
        .take()
        .ok_or_else(|| SmtError::Process("missing solver stdin".into()))?;
    let output = child
        .stdout
        .take()
        .ok_or_else(|| SmtError::Process("missing solver stdout".into()))?;
    let (tx, rx) = mpsc::channel();
    let reader = thread::spawn(move || {
        let mut stream = BufReader::new(output);
        let mut buffer = String::new();
        loop {
            let mut line = String::new();
            match stream.read_line(&mut line) {
                Ok(0) => {
                    let _ = tx.send(Err(SmtError::Process("solver stdout closed".into())));
                    break;
                }
                Ok(_) => {
                    buffer.push_str(&line);
                    if buffer.len() > 8 * 1024 * 1024 {
                        let _ = tx.send(Err(SmtError::Parse {
                            offset: buffer.len(),
                            message: "response exceeds 8 MiB".into(),
                        }));
                        break;
                    }
                }
                Err(e) => {
                    let _ = tx.send(Err(e.into()));
                    break;
                }
            }
            loop {
                match Sexp::parse_prefix(&buffer) {
                    Ok(Some((reply, used))) => {
                        buffer.drain(..used);
                        if tx.send(Ok(reply)).is_err() {
                            return;
                        }
                    }
                    Ok(None) => break,
                    Err(e) => {
                        let _ = tx.send(Err(e));
                        return;
                    }
                }
            }
        }
    });
    Ok((child, input, rx, reader))
}
