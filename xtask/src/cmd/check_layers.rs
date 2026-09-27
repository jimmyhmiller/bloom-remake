//! `check-layers`: every normal, build and dev dependency edge against `xtask/layers.toml` (ARCHITECTURE §1.2–§1.3,
//! ARCH-01). Implemented by WP M1.1.
//!
//! The rules are documented at the top of `xtask/layers.toml`. The workspace graph comes from
//! `cargo metadata`; the `normal-path` rules use `cargo tree -e normal -p <crate>`, which resolves features as if
//! the crate were built alone.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode};

use serde::Deserialize;

use crate::util;

/// Arguments of `check-layers`.
#[derive(Debug, clap::Args)]
pub struct Args {
    /// The repository root (default: the root this xtask was built in).
    #[arg(long)]
    pub root: Option<PathBuf>,
    /// Print the crate table of ARCHITECTURE §1.2 generated from layers.toml instead of checking.
    #[arg(long)]
    pub print_table: bool,
}

/// Runs the task.
pub fn run(args: Args) -> ExitCode {
    let root = util::root_or_default(args.root);
    let layers = match load_layers(&root.join("xtask").join("layers.toml")) {
        Ok(l) => l,
        Err(e) => return util::fail("check-layers", e),
    };
    let meta = match cargo_metadata(&root) {
        Ok(m) => m,
        Err(e) => return util::fail("check-layers", e),
    };
    if args.print_table {
        println!("{}", table(&layers, &meta));
        return ExitCode::SUCCESS;
    }
    let trees = CargoTree { root: root.clone() };
    let findings = check(&layers, &meta, &trees);
    let edges: usize = meta
        .members()
        .map(|p| p.dependencies.iter().filter(|d| meta.is_member(&d.name)).count())
        .sum();
    util::finish(
        "check-layers",
        &findings,
        &format!("{} crates, {edges} internal edges", meta.members().count()),
    )
}

// ---- layers.toml ----------------------------------------------------------------------------------------------

/// The parsed `xtask/layers.toml`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Layers {
    /// Layer names, lowest first.
    pub layers: Vec<String>,
    /// Rules by crate name.
    pub crates: BTreeMap<String, Rule>,
    /// Rules for every package under a path.
    #[serde(default)]
    pub groups: BTreeMap<String, GroupRule>,
    /// Forbidden paths.
    #[serde(default)]
    pub forbid: Vec<Forbid>,
}

/// The rule for one crate.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Its layer.
    pub layer: String,
    /// Allowed normal dependencies (and everything reachable from them).
    #[serde(default)]
    pub deps: Vec<String>,
    /// Additional allowed build dependencies.
    #[serde(default)]
    pub build_deps: Vec<String>,
    /// Additional allowed dev dependencies.
    #[serde(default)]
    pub dev_deps: Vec<String>,
    /// Only ever a dev-dependency of crates that list it.
    #[serde(default)]
    pub test_only: bool,
}

/// The rule for every package whose manifest is under `path`.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupRule {
    /// The path prefix, relative to the workspace root (e.g. `systems/`).
    pub path: String,
    /// Its layer.
    pub layer: String,
    /// Allowed normal dependencies.
    #[serde(default)]
    pub deps: Vec<String>,
    /// Additional allowed build dependencies.
    #[serde(default)]
    pub build_deps: Vec<String>,
    /// Additional allowed dev dependencies.
    #[serde(default)]
    pub dev_deps: Vec<String>,
}

impl GroupRule {
    fn as_rule(&self) -> Rule {
        Rule {
            layer: self.layer.clone(),
            deps: self.deps.clone(),
            build_deps: self.build_deps.clone(),
            dev_deps: self.dev_deps.clone(),
            test_only: false,
        }
    }
}

/// A forbidden dependency path.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Forbid {
    /// Crate names, or `group:<name>`.
    pub from: Vec<String>,
    /// Package names (internal or external).
    pub to: Vec<String>,
    /// How the path is checked.
    #[serde(default)]
    pub mode: ForbidMode,
    /// Why (printed with a violation).
    pub why: String,
}

/// How a [`Forbid`] rule is evaluated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ForbidMode {
    /// Direct edges of every kind, then normal and build edges below them (the resolved workspace graph).
    #[default]
    Closure,
    /// The normal-only closure of the crate built alone.
    NormalPath,
}

fn load_layers(path: &Path) -> Result<Layers, String> {
    let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read {}: {e}", path.display()))?;
    toml::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))
}

// ---- cargo metadata ---------------------------------------------------------------------------------------------

/// The subset of `cargo metadata --format-version 1` this check reads.
#[derive(Debug, Deserialize)]
pub struct Metadata {
    /// Every package of the resolved graph.
    pub packages: Vec<Package>,
    /// Ids of the workspace members.
    pub workspace_members: Vec<String>,
    /// The resolved graph.
    pub resolve: Option<Resolve>,
    /// The workspace root.
    pub workspace_root: PathBuf,
}

/// A package.
#[derive(Debug, Deserialize)]
pub struct Package {
    /// Its name.
    pub name: String,
    /// Its id.
    pub id: String,
    /// Its manifest.
    pub manifest_path: PathBuf,
    /// Its description.
    #[serde(default)]
    pub description: Option<String>,
    /// Its declared dependencies.
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
}

/// A declared dependency.
#[derive(Debug, Deserialize)]
pub struct Dependency {
    /// The package name.
    pub name: String,
    /// `None` (normal), `"dev"` or `"build"`.
    #[serde(default)]
    pub kind: Option<String>,
}

/// The resolved graph.
#[derive(Debug, Deserialize)]
pub struct Resolve {
    /// One node per package.
    pub nodes: Vec<Node>,
}

/// A resolved package.
#[derive(Debug, Deserialize)]
pub struct Node {
    /// The package id.
    pub id: String,
    /// Its resolved dependencies.
    #[serde(default)]
    pub deps: Vec<NodeDep>,
}

/// A resolved dependency edge.
#[derive(Debug, Deserialize)]
pub struct NodeDep {
    /// The dependency's package id.
    pub pkg: String,
    /// The kinds of the edge.
    #[serde(default)]
    pub dep_kinds: Vec<DepKind>,
}

/// One kind of a resolved edge.
#[derive(Debug, Deserialize)]
pub struct DepKind {
    /// `None` (normal), `"dev"` or `"build"`.
    #[serde(default)]
    pub kind: Option<String>,
}

impl Metadata {
    fn members(&self) -> impl Iterator<Item = &Package> + '_ {
        self.packages.iter().filter(|p| self.workspace_members.contains(&p.id))
    }

    fn is_member(&self, name: &str) -> bool {
        self.members().any(|p| p.name == name)
    }

    fn package_by_id(&self, id: &str) -> Option<&Package> {
        self.packages.iter().find(|p| p.id == id)
    }
}

fn cargo_metadata(root: &Path) -> Result<Metadata, String> {
    let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(root.join("Cargo.toml"))
        .output()
        .map_err(|e| format!("cannot run cargo metadata: {e}"))?;
    if !output.status.success() {
        return Err(format!(
            "cargo metadata failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    serde_json::from_slice(&output.stdout).map_err(|e| format!("cannot parse cargo metadata: {e}"))
}

/// The normal-only dependency closure of a crate built alone.
pub trait NormalClosure {
    /// Every package name in the closure, including the crate itself.
    fn normal_closure(&self, package: &str) -> Result<BTreeSet<String>, String>;
}

/// [`NormalClosure`] through `cargo tree`.
struct CargoTree {
    root: PathBuf,
}

impl NormalClosure for CargoTree {
    fn normal_closure(&self, package: &str) -> Result<BTreeSet<String>, String> {
        let output = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args([
                "tree",
                "-e",
                "normal",
                "--prefix",
                "none",
                "--format",
                "{p}",
                "-p",
                package,
                "--manifest-path",
            ])
            .arg(self.root.join("Cargo.toml"))
            .output()
            .map_err(|e| format!("cannot run cargo tree: {e}"))?;
        if !output.status.success() {
            return Err(format!(
                "cargo tree -p {package} failed:\n{}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        Ok(String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter_map(|line| line.split_whitespace().next())
            .map(str::to_string)
            .collect())
    }
}

// ---- the check --------------------------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum EdgeKind {
    Normal,
    Build,
    Dev,
}

impl EdgeKind {
    fn of(kind: Option<&str>) -> EdgeKind {
        match kind {
            Some("dev") => EdgeKind::Dev,
            Some("build") => EdgeKind::Build,
            _ => EdgeKind::Normal,
        }
    }

    fn name(self) -> &'static str {
        match self {
            EdgeKind::Normal => "normal",
            EdgeKind::Build => "build",
            EdgeKind::Dev => "dev",
        }
    }
}

/// The rule of every member, by name, and the members of each group.
struct Assigned {
    rules: BTreeMap<String, Rule>,
    groups: BTreeMap<String, Vec<String>>,
}

fn assign_rules(layers: &Layers, meta: &Metadata, findings: &mut Vec<String>) -> Assigned {
    let mut rules = BTreeMap::new();
    let mut groups: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for p in meta.members() {
        let manifest = util::display_relative(&p.manifest_path, &meta.workspace_root);
        if let Some(rule) = layers.crates.get(&p.name) {
            rules.insert(p.name.clone(), rule.clone());
        } else if let Some((group, rule)) = layers.groups.iter().find(|(_, g)| manifest.starts_with(&g.path)) {
            rules.insert(p.name.clone(), rule.as_rule());
            groups.entry(group.clone()).or_default().push(p.name.clone());
        } else {
            findings.push(format!("{} ({manifest}) is not listed in xtask/layers.toml", p.name));
        }
    }
    for name in layers.crates.keys() {
        if !meta.is_member(name) {
            findings.push(format!(
                "xtask/layers.toml lists {name}, which is not a workspace member"
            ));
        }
    }
    Assigned { rules, groups }
}

/// Expands `"*"` and checks names and layer order of a rule's lists.
fn expand(list: &[String], layers: &Layers) -> Vec<String> {
    let mut out = Vec::new();
    for entry in list {
        if entry == "*" {
            out.extend(
                layers
                    .crates
                    .iter()
                    .filter(|(_, r)| !r.test_only && r.layer.starts_with('L'))
                    .map(|(name, _)| name.clone()),
            );
        } else {
            out.push(entry.clone());
        }
    }
    out
}

fn check_table(layers: &Layers, findings: &mut Vec<String>) {
    let rank = |layer: &str| layers.layers.iter().position(|l| l == layer);
    let mut all: Vec<(String, Rule)> = layers.crates.iter().map(|(n, r)| (n.clone(), r.clone())).collect();
    all.extend(layers.groups.iter().map(|(n, g)| (format!("group:{n}"), g.as_rule())));
    for (name, rule) in &all {
        let Some(own) = rank(&rule.layer) else {
            findings.push(format!("xtask/layers.toml: {name} has unknown layer {}", rule.layer));
            continue;
        };
        for list in [&rule.deps, &rule.build_deps, &rule.dev_deps] {
            for dep in expand(list, layers) {
                match layers.crates.get(&dep) {
                    None => findings.push(format!("xtask/layers.toml: {name} lists unknown crate {dep}")),
                    Some(d) => {
                        if rank(&d.layer).is_some_and(|r| r > own) {
                            findings.push(format!(
                                "xtask/layers.toml: {name} ({}) lists {dep} of the higher layer {}",
                                rule.layer, d.layer
                            ));
                        }
                    }
                }
            }
        }
    }
    // The `deps` graph must be acyclic.
    let names: Vec<&String> = layers.crates.keys().collect();
    let index: BTreeMap<&String, usize> = names.iter().enumerate().map(|(i, n)| (*n, i)).collect();
    let mut graph = blossom_base::graph::AdjacencyList::new(names.len());
    for (name, rule) in &layers.crates {
        for dep in expand(&rule.deps, layers) {
            if let (Some(&from), Some(&to)) = (index.get(name), index.get(&dep))
                && from != to
                && let Err(e) = graph.add_edge(from, to)
            {
                findings.push(format!("xtask/layers.toml: cannot build the dependency graph: {e}"));
            }
        }
    }
    if let Err(blossom_base::graph::GraphError::Cycle { cycle }) = blossom_base::graph::topo_sort(&graph) {
        let path: Vec<&str> = cycle.iter().filter_map(|i| names.get(*i).map(|n| n.as_str())).collect();
        findings.push(format!(
            "xtask/layers.toml: the dependency graph has a cycle: {}",
            path.join(" -> ")
        ));
    }
}

/// Every crate reachable from `start` through the `deps` lists (including `start` itself).
fn reach(start: &[String], layers: &Layers) -> BTreeSet<String> {
    let mut seen = BTreeSet::new();
    let mut todo: VecDeque<String> = expand(start, layers).into();
    while let Some(name) = todo.pop_front() {
        if !seen.insert(name.clone()) {
            continue;
        }
        if let Some(rule) = layers.crates.get(&name) {
            todo.extend(expand(&rule.deps, layers));
        }
    }
    seen
}

/// Checks the workspace against the layer table; returns every violation, sorted.
pub fn check(layers: &Layers, meta: &Metadata, trees: &dyn NormalClosure) -> Vec<String> {
    let mut findings = Vec::new();
    check_table(layers, &mut findings);
    let assigned = assign_rules(layers, meta, &mut findings);
    let test_only: BTreeSet<&String> = layers
        .crates
        .iter()
        .filter(|(_, r)| r.test_only)
        .map(|(n, _)| n)
        .collect();

    for p in meta.members() {
        let Some(rule) = assigned.rules.get(&p.name) else {
            continue;
        };
        let normal = reach(&rule.deps, layers);
        let with = |extra: &[String]| normal.union(&reach(extra, layers)).cloned().collect::<BTreeSet<_>>();
        let build = with(&rule.build_deps);
        let dev = with(&rule.dev_deps);
        for dep in p.dependencies.iter().filter(|d| meta.is_member(&d.name)) {
            let kind = EdgeKind::of(dep.kind.as_deref());
            let allowed = match kind {
                EdgeKind::Normal => &normal,
                EdgeKind::Build => &build,
                EdgeKind::Dev => &dev,
            };
            if test_only.contains(&dep.name) && kind != EdgeKind::Dev {
                findings.push(format!(
                    "{} -> {} must be a dev-dependency: {} is test-only (ARCHITECTURE §1.3)",
                    p.name, dep.name, dep.name
                ));
            } else if !allowed.contains(&dep.name) {
                findings.push(format!(
                    "{} -> {} ({} dependency) is not allowed by xtask/layers.toml ({})",
                    p.name,
                    dep.name,
                    kind.name(),
                    util::display_relative(&p.manifest_path, &meta.workspace_root)
                ));
            }
        }
    }

    for rule in &layers.forbid {
        let mut sources = Vec::new();
        for from in &rule.from {
            match from.strip_prefix("group:") {
                Some(group) => sources.extend(assigned.groups.get(group).cloned().unwrap_or_default()),
                None if meta.is_member(from) => sources.push(from.clone()),
                None => findings.push(format!("xtask/layers.toml: forbid rule names unknown crate {from}")),
            }
        }
        for source in sources {
            match rule.mode {
                ForbidMode::Closure => match closure_paths(meta, &source) {
                    Ok(paths) => {
                        for (target, path) in paths {
                            if rule.to.contains(&target) {
                                findings.push(format!(
                                    "{source} reaches {target} via {} — {}",
                                    path.join(" -> "),
                                    rule.why
                                ));
                            }
                        }
                    }
                    Err(e) => findings.push(e),
                },
                ForbidMode::NormalPath => match trees.normal_closure(&source) {
                    Ok(closure) => {
                        for target in rule.to.iter().filter(|t| closure.contains(*t)) {
                            findings.push(format!(
                                "{source} has a normal dependency path to {target} (cargo tree -e normal -p {source}) — {}",
                                rule.why
                            ));
                        }
                    }
                    Err(e) => findings.push(e),
                },
            }
        }
    }
    findings.sort();
    findings.dedup();
    findings
}

/// Every package reachable from `source` in the resolved graph — its direct edges of every kind, then normal and
/// build edges — with one shortest path to each.
fn closure_paths(meta: &Metadata, source: &str) -> Result<BTreeMap<String, Vec<String>>, String> {
    let resolve = meta
        .resolve
        .as_ref()
        .ok_or("cargo metadata returned no resolved dependency graph, so forbid rules cannot be checked")?;
    let nodes: BTreeMap<&str, &Node> = resolve.nodes.iter().map(|n| (n.id.as_str(), n)).collect();
    let start = meta
        .members()
        .find(|p| p.name == source)
        .ok_or_else(|| format!("forbid rule source {source} is not a workspace member"))?;
    let mut paths: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut seen: BTreeSet<&str> = BTreeSet::from([start.id.as_str()]);
    let mut queue: VecDeque<(&str, Vec<String>, bool)> =
        VecDeque::from([(start.id.as_str(), vec![source.to_string()], true)]);
    while let Some((id, path, first)) = queue.pop_front() {
        let Some(node) = nodes.get(id) else { continue };
        for dep in &node.deps {
            // Below the first hop, dev edges are not part of the build.
            let follows = first
                || dep
                    .dep_kinds
                    .iter()
                    .any(|k| EdgeKind::of(k.kind.as_deref()) != EdgeKind::Dev);
            if !follows || !seen.insert(dep.pkg.as_str()) {
                continue;
            }
            let Some(pkg) = meta.package_by_id(&dep.pkg) else {
                continue;
            };
            let mut next = path.clone();
            next.push(pkg.name.clone());
            paths.entry(pkg.name.clone()).or_insert_with(|| next.clone());
            queue.push_back((dep.pkg.as_str(), next, false));
        }
    }
    Ok(paths)
}

/// The ARCHITECTURE §1.2 crate table generated from layers.toml and the package descriptions.
fn table(layers: &Layers, meta: &Metadata) -> String {
    let rank = |layer: &str| layers.layers.iter().position(|l| l == layer).unwrap_or(usize::MAX);
    let mut rows: Vec<(&String, &Rule)> = layers.crates.iter().collect();
    rows.sort_by(|a, b| rank(&a.1.layer).cmp(&rank(&b.1.layer)).then_with(|| a.0.cmp(b.0)));
    let mut out = String::from("| Crate | L | Purpose | Internal deps |\n|---|---|---|---|\n");
    for (name, rule) in rows {
        let purpose = meta
            .members()
            .find(|p| &p.name == name)
            .and_then(|p| p.description.clone())
            .unwrap_or_default();
        let deps: Vec<String> = rule
            .deps
            .iter()
            .map(|d| d.strip_prefix("blossom-").unwrap_or(d).to_string())
            .collect();
        let deps = if deps.is_empty() {
            "—".to_string()
        } else {
            deps.join(", ")
        };
        out.push_str(&format!("| `{name}` | {} | {purpose} | {deps} |\n", rule.layer));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const LAYERS: &str = r#"
        layers = ["L0", "L1", "L2", "tests", "systems"]
        [crates.base]
        layer = "L0"
        [crates.ir]
        layer = "L1"
        deps = ["base"]
        [crates.kernel]
        layer = "L1"
        deps = ["base"]
        [crates.engine]
        layer = "L2"
        deps = ["ir", "kernel"]
        [crates.kit]
        layer = "L2"
        deps = ["engine"]
        test_only = true
        [crates.itests]
        layer = "tests"
        deps = ["*"]
        dev_deps = ["kit"]
        [groups.systems]
        path = "systems/"
        layer = "systems"
        deps = ["engine"]
        [[forbid]]
        from = ["kernel"]
        to = ["ir"]
        why = "ARCH-04"
        [[forbid]]
        from = ["engine"]
        to = ["tokio"]
        why = "no async"
        [[forbid]]
        from = ["group:systems"]
        to = ["driver"]
        mode = "normal-path"
        why = "build.rs only"
    "#;

    /// A dependency: the package name and the kind (`None` normal, `"dev"`, `"build"`).
    type Dep = (&'static str, Option<&'static str>);

    struct World {
        /// Members as (name, directory, dependencies).
        packages: Vec<(&'static str, &'static str, Vec<Dep>)>,
        externals: Vec<&'static str>,
    }

    fn metadata(world: &World) -> Metadata {
        let id = |n: &str| format!("path+file:///ws#{n}@0.1.0");
        let mut packages = Vec::new();
        let mut nodes = Vec::new();
        for (name, dir, deps) in &world.packages {
            packages.push(Package {
                name: name.to_string(),
                id: id(name),
                manifest_path: PathBuf::from(format!("/ws/{dir}/Cargo.toml")),
                description: Some(format!("the {name} crate")),
                dependencies: deps
                    .iter()
                    .map(|(d, k)| Dependency {
                        name: d.to_string(),
                        kind: k.map(str::to_string),
                    })
                    .collect(),
            });
            nodes.push(Node {
                id: id(name),
                deps: deps
                    .iter()
                    .map(|(d, k)| NodeDep {
                        pkg: id(d),
                        dep_kinds: vec![DepKind {
                            kind: k.map(str::to_string),
                        }],
                    })
                    .collect(),
            });
        }
        for ext in &world.externals {
            packages.push(Package {
                name: ext.to_string(),
                id: id(ext),
                manifest_path: PathBuf::from(format!("/registry/{ext}/Cargo.toml")),
                description: None,
                dependencies: vec![],
            });
            nodes.push(Node {
                id: id(ext),
                deps: vec![],
            });
        }
        Metadata {
            workspace_members: world.packages.iter().map(|(n, _, _)| id(n)).collect(),
            packages,
            resolve: Some(Resolve { nodes }),
            workspace_root: PathBuf::from("/ws"),
        }
    }

    struct FakeTrees(BTreeMap<&'static str, Vec<&'static str>>);

    impl NormalClosure for FakeTrees {
        fn normal_closure(&self, package: &str) -> Result<BTreeSet<String>, String> {
            Ok(self
                .0
                .get(package)
                .into_iter()
                .flatten()
                .map(|s| s.to_string())
                .collect())
        }
    }

    fn good_world() -> World {
        World {
            packages: vec![
                ("base", "crates/base", vec![]),
                ("ir", "crates/ir", vec![("base", None)]),
                ("kernel", "crates/kernel", vec![("base", None)]),
                // A transitive edge (engine -> base) is allowed: base is below ir.
                (
                    "engine",
                    "crates/engine",
                    vec![("ir", None), ("kernel", None), ("base", None)],
                ),
                ("kit", "crates/kit", vec![("engine", None)]),
                (
                    "itests",
                    "tests/integration",
                    vec![("engine", None), ("kit", Some("dev"))],
                ),
                ("sys-a", "systems/a", vec![("engine", None)]),
            ],
            externals: vec![],
        }
    }

    fn run_check(world: &World, trees: &FakeTrees) -> Vec<String> {
        let layers: Layers = toml::from_str(LAYERS).unwrap();
        check(&layers, &metadata(world), trees)
    }

    fn no_trees() -> FakeTrees {
        FakeTrees(BTreeMap::new())
    }

    #[test]
    fn check_layers_accepts_allowed_edges() {
        assert_eq!(run_check(&good_world(), &no_trees()), Vec::<String>::new());
    }

    #[test]
    fn check_layers_rejects_disallowed_edges() {
        let mut world = good_world();
        // kernel -> engine goes up a layer and is not reachable from kernel's deps.
        world.packages[2].2.push(("engine", Some("dev")));
        let findings = run_check(&world, &no_trees());
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with("kernel -> engine (dev dependency) is not allowed")),
            "{findings:?}"
        );
    }

    #[test]
    fn check_layers_forbids_paths_through_the_graph() {
        let mut world = good_world();
        // kernel -> ir is not in the table and is forbidden (ARCH-04).
        world.packages[2].2.push(("ir", None));
        world.externals.push("tokio");
        world.packages[3].2.push(("tokio", None));
        let findings = run_check(&world, &no_trees());
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with("kernel -> ir (normal dependency) is not allowed")),
            "{findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with("kernel reaches ir via kernel -> ir — ARCH-04")),
            "{findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with("engine reaches tokio via engine -> tokio")),
            "{findings:?}"
        );
    }

    #[test]
    fn check_layers_test_only_crates() {
        let mut world = good_world();
        world.packages[5].2.push(("kit", None));
        world.packages[3].2.push(("kit", Some("dev")));
        let findings = run_check(&world, &no_trees());
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with("itests -> kit must be a dev-dependency")),
            "{findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with("engine -> kit (dev dependency) is not allowed")),
            "{findings:?}"
        );
    }

    #[test]
    fn check_layers_normal_path_rule_uses_the_crate_alone() {
        let trees = FakeTrees(BTreeMap::from([("sys-a", vec!["sys-a", "engine", "driver"])]));
        let findings = run_check(&good_world(), &trees);
        assert_eq!(findings.len(), 1, "{findings:?}");
        assert!(
            findings[0].starts_with("sys-a has a normal dependency path to driver"),
            "{findings:?}"
        );
    }

    #[test]
    fn check_layers_unlisted_and_stale_crates() {
        let mut world = good_world();
        world.packages.push(("stray", "crates/stray", vec![]));
        world.packages.retain(|(n, _, _)| *n != "kit");
        world.packages[4].2.retain(|(d, _)| *d != "kit");
        let findings = run_check(&world, &no_trees());
        assert!(
            findings
                .iter()
                .any(|f| f.starts_with("stray (crates/stray/Cargo.toml) is not listed")),
            "{findings:?}"
        );
        assert!(
            findings
                .iter()
                .any(|f| f.contains("lists kit, which is not a workspace member")),
            "{findings:?}"
        );
    }

    #[test]
    fn check_layers_table_is_checked() {
        let bad = LAYERS.replace(
            "[crates.base]\n        layer = \"L0\"",
            "[crates.base]\n        layer = \"L0\"\n        deps = [\"engine\"]",
        );
        let layers: Layers = toml::from_str(&bad).unwrap();
        let findings = check(&layers, &metadata(&good_world()), &no_trees());
        assert!(
            findings
                .iter()
                .any(|f| f.contains("base (L0) lists engine of the higher layer L2")),
            "{findings:?}"
        );
        assert!(
            findings.iter().any(|f| f.contains("the dependency graph has a cycle")),
            "{findings:?}"
        );
    }

    #[test]
    fn check_layers_missing_resolve_is_a_finding() {
        let mut meta = metadata(&good_world());
        meta.resolve = None;
        let layers: Layers = toml::from_str(LAYERS).unwrap();
        let findings = check(&layers, &meta, &no_trees());
        assert!(
            findings.iter().any(|f| f.contains("no resolved dependency graph")),
            "{findings:?}"
        );
    }

    #[test]
    fn check_layers_real_table_parses() {
        let layers = load_layers(&util::workspace_root().join("xtask").join("layers.toml")).unwrap();
        let mut findings = Vec::new();
        check_table(&layers, &mut findings);
        assert_eq!(findings, Vec::<String>::new());
        assert!(
            layers
                .crates
                .get("blossom-ir")
                .is_some_and(|r| !r.deps.contains(&"blossom-lattice".to_string()))
        );
    }
}
