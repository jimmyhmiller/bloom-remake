//! A linearizability checker for recorded client histories (Wing & Gong's search with Lowe's memoization, as in
//! Knossos and Porcupine).
//!
//! A history is a set of operations, each with the instant its client invoked it and the instant the client saw its
//! response, or no response at all (the client timed out, or the server died: the operation may or may not have taken
//! effect, and if it did, at any point after its invocation). The history is linearizable if the operations can be
//! put in one sequence that respects real time (an operation that returned before another was invoked comes first)
//! and that a sequential [`Model`] accepts with the recorded responses.
//!
//! Operations that returned at the same instant another was invoked are treated as concurrent: client clocks cannot
//! order them. A model that is a set of independent objects (the keys of a key-value store) should be checked per
//! object with [`check_partitioned`]; linearizability is compositional, so that is equivalent and exponentially
//! cheaper.

use std::collections::{BTreeMap, BTreeSet};

/// A sequential specification.
pub trait Model {
    type State: Clone + Ord;
    type Input;
    type Output;
    fn init(&self) -> Self::State;
    /// Applies `input` to `state`. `output` is the recorded response, or `None` when the operation never responded
    /// (then any response is acceptable). Returns the next state if the model allows the response.
    fn step(&self, state: &Self::State, input: &Self::Input, output: Option<&Self::Output>) -> Option<Self::State>;
}

/// One operation of a history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Operation<I, O> {
    /// When the client invoked it (any monotonic unit shared by the clients).
    pub call: u64,
    /// When the client saw the response; `None` if it never did.
    pub ret: Option<u64>,
    pub input: I,
    pub output: Option<O>,
}

/// The verdict on a history.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Linearizable,
    /// No linearization exists. `longest` is the longest prefix of operations (indices into the history, in
    /// linearization order) the search could place: the operation after it is where every ordering breaks.
    NotLinearizable {
        longest: Vec<usize>,
    },
    /// The search visited more than the budget of states without an answer.
    Unknown,
}

#[derive(Clone, Copy, Debug)]
struct Entry {
    op: usize,
    is_call: bool,
    /// The position of the matching call or return.
    matching: usize,
    prev: usize,
    next: usize,
}

/// Checks one history against `model`, visiting at most `budget` search states.
pub fn check<M: Model>(model: &M, history: &[Operation<M::Input, M::Output>], budget: u64) -> Verdict {
    let n = history.len();
    if n == 0 {
        return Verdict::Linearizable;
    }
    // Events: (time, 0 = call | 1 = return, op). Calls sort before returns at equal times (concurrent).
    let mut events: Vec<(u64, u8, usize)> = Vec::with_capacity(2 * n);
    for (i, op) in history.iter().enumerate() {
        events.push((op.call, 0, i));
        events.push((op.ret.unwrap_or(u64::MAX), 1, i));
    }
    events.sort();
    // A doubly linked list over events with a sentinel head at index 0.
    let len = events.len() + 1;
    let mut list: Vec<Entry> = Vec::with_capacity(len);
    list.push(Entry {
        op: usize::MAX,
        is_call: false,
        matching: 0,
        prev: 0,
        next: 1,
    });
    let mut call_at = vec![0usize; n];
    let mut ret_at = vec![0usize; n];
    for (k, (_, kind, op)) in events.iter().enumerate() {
        let pos = k + 1;
        if *kind == 0 {
            if let Some(c) = call_at.get_mut(*op) {
                *c = pos;
            }
        } else if let Some(r) = ret_at.get_mut(*op) {
            *r = pos;
        }
        list.push(Entry {
            op: *op,
            is_call: *kind == 0,
            matching: 0,
            prev: k,
            next: if pos + 1 < len { pos + 1 } else { 0 },
        });
    }
    for op in 0..n {
        let (Some(&c), Some(&r)) = (call_at.get(op), ret_at.get(op)) else {
            return Verdict::Unknown;
        };
        if let Some(e) = list.get_mut(c) {
            e.matching = r;
        }
        if let Some(e) = list.get_mut(r) {
            e.matching = c;
        }
    }
    let words = n.div_ceil(64);
    let mut linearized = vec![0u64; words];
    let mut state = model.init();
    let mut cache: BTreeSet<(Vec<u64>, M::State)> = BTreeSet::new();
    let mut stack: Vec<(usize, M::State)> = Vec::new();
    let mut longest: Vec<usize> = Vec::new();
    let mut visited: u64 = 0;
    let mut entry = list.first().map_or(0, |h| h.next);
    let head_next = |list: &[Entry]| list.first().map_or(0, |h| h.next);
    while head_next(&list) != 0 {
        visited += 1;
        if visited > budget {
            return Verdict::Unknown;
        }
        let Some(&e) = list.get(entry) else {
            return Verdict::Unknown;
        };
        if e.is_call {
            let Some(op) = history.get(e.op) else {
                return Verdict::Unknown;
            };
            let mut placed = false;
            if let Some(next_state) = model.step(&state, &op.input, op.output.as_ref()) {
                let mut lin = linearized.clone();
                if let Some(w) = lin.get_mut(e.op / 64) {
                    *w |= 1u64 << (e.op % 64);
                }
                let key = (lin, next_state);
                if !cache.contains(&key) {
                    let (lin, next_state) = key;
                    cache.insert((lin.clone(), next_state.clone()));
                    stack.push((entry, state));
                    state = next_state;
                    linearized = lin;
                    lift(&mut list, entry);
                    if stack.len() > longest.len() {
                        longest = stack.iter().filter_map(|(p, _)| list.get(*p).map(|x| x.op)).collect();
                    }
                    entry = head_next(&list);
                    placed = true;
                }
            }
            if !placed {
                entry = e.next;
            }
        } else {
            // A return: its operation had to be placed before this point. Backtrack.
            let Some((top, prev_state)) = stack.pop() else {
                return Verdict::NotLinearizable { longest };
            };
            let Some(&t) = list.get(top) else {
                return Verdict::Unknown;
            };
            if let Some(w) = linearized.get_mut(t.op / 64) {
                *w &= !(1u64 << (t.op % 64));
            }
            state = prev_state;
            unlift(&mut list, top);
            entry = t.next;
        }
    }
    Verdict::Linearizable
}

/// Removes a call and its return from the list.
fn lift(list: &mut [Entry], call: usize) {
    let Some(&c) = list.get(call) else { return };
    unlink(list, c.prev, c.next);
    let r_pos = c.matching;
    if let Some(&r) = list.get(r_pos) {
        unlink(list, r.prev, r.next);
    }
}

fn unlink(list: &mut [Entry], prev: usize, next: usize) {
    if let Some(p) = list.get_mut(prev) {
        p.next = next;
    }
    if next != 0
        && let Some(nx) = list.get_mut(next)
    {
        nx.prev = prev;
    }
}

/// Puts a lifted call and its return back (the reverse of [`lift`]).
fn unlift(list: &mut [Entry], call: usize) {
    let Some(&c) = list.get(call) else { return };
    if let Some(&r) = list.get(c.matching) {
        relink(list, c.matching, r.prev, r.next);
    }
    relink(list, call, c.prev, c.next);
}

fn relink(list: &mut [Entry], at: usize, prev: usize, next: usize) {
    if let Some(p) = list.get_mut(prev) {
        p.next = at;
    }
    if next != 0
        && let Some(nx) = list.get_mut(next)
    {
        nx.prev = at;
    }
}

/// Checks a history of independent objects: each partition (by `key`) on its own. Returns the first failing
/// partition's key with its verdict, or the combined verdict.
pub fn check_partitioned<M: Model, K: Ord + Clone>(
    model: &M,
    history: &[Operation<M::Input, M::Output>],
    key: impl Fn(&M::Input) -> K,
    budget: u64,
) -> (Verdict, Option<K>)
where
    M::Input: Clone,
    M::Output: Clone,
{
    type Part<M> = Vec<Operation<<M as Model>::Input, <M as Model>::Output>>;
    let mut parts: BTreeMap<K, Part<M>> = BTreeMap::new();
    for op in history {
        parts.entry(key(&op.input)).or_default().push(op.clone());
    }
    let mut unknown = None;
    for (k, ops) in parts {
        match check(model, &ops, budget) {
            Verdict::Linearizable => {}
            Verdict::Unknown => unknown = Some(k),
            v @ Verdict::NotLinearizable { .. } => return (v, Some(k)),
        }
    }
    match unknown {
        Some(k) => (Verdict::Unknown, Some(k)),
        None => (Verdict::Linearizable, None),
    }
}

/// A key-value register per key: the model of a linearizable key-value store.
#[derive(Clone, Copy, Debug, Default)]
pub struct KvModel;

/// A key-value operation.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KvInput {
    Put { key: Vec<u8>, val: Vec<u8> },
    Get { key: Vec<u8> },
    Delete { key: Vec<u8> },
}

impl KvInput {
    pub fn key(&self) -> &[u8] {
        match self {
            KvInput::Put { key, .. } | KvInput::Get { key } | KvInput::Delete { key } => key,
        }
    }
}

/// A key-value response.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KvOutput {
    PutOk,
    Value(Option<Vec<u8>>),
    /// Whether the key existed.
    Deleted(bool),
}

impl Model for KvModel {
    /// The value of the (single) key of a partition.
    type State = Option<Vec<u8>>;
    type Input = KvInput;
    type Output = KvOutput;

    fn init(&self) -> Self::State {
        None
    }

    fn step(&self, state: &Self::State, input: &KvInput, output: Option<&KvOutput>) -> Option<Self::State> {
        match (input, output) {
            (KvInput::Put { val, .. }, None | Some(KvOutput::PutOk)) => Some(Some(val.clone())),
            (KvInput::Get { .. }, None) => Some(state.clone()),
            (KvInput::Get { .. }, Some(KvOutput::Value(v))) => (v == state).then(|| state.clone()),
            (KvInput::Delete { .. }, None) => Some(None),
            (KvInput::Delete { .. }, Some(KvOutput::Deleted(existed))) => (*existed == state.is_some()).then_some(None),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn op(call: u64, ret: Option<u64>, input: KvInput, output: Option<KvOutput>) -> Operation<KvInput, KvOutput> {
        Operation {
            call,
            ret,
            input,
            output,
        }
    }
    fn put(v: &str) -> KvInput {
        KvInput::Put {
            key: b"k".to_vec(),
            val: v.as_bytes().to_vec(),
        }
    }
    fn get() -> KvInput {
        KvInput::Get { key: b"k".to_vec() }
    }
    fn val(v: Option<&str>) -> Option<KvOutput> {
        Some(KvOutput::Value(v.map(|s| s.as_bytes().to_vec())))
    }

    #[test]
    fn sequential_histories() {
        let h = vec![
            op(0, Some(1), put("a"), Some(KvOutput::PutOk)),
            op(2, Some(3), get(), val(Some("a"))),
        ];
        assert_eq!(check(&KvModel, &h, 1_000_000), Verdict::Linearizable);
        let stale = vec![
            op(0, Some(1), put("a"), Some(KvOutput::PutOk)),
            op(2, Some(3), put("b"), Some(KvOutput::PutOk)),
            op(4, Some(5), get(), val(Some("a"))),
        ];
        assert!(matches!(
            check(&KvModel, &stale, 1_000_000),
            Verdict::NotLinearizable { .. }
        ));
    }

    #[test]
    fn concurrent_operations_may_order_either_way() {
        // The get overlaps both puts, so it may see either.
        for seen in ["a", "b"] {
            let h = vec![
                op(0, Some(10), put("a"), Some(KvOutput::PutOk)),
                op(1, Some(11), put("b"), Some(KvOutput::PutOk)),
                op(2, Some(12), get(), val(Some(seen))),
            ];
            assert_eq!(check(&KvModel, &h, 1_000_000), Verdict::Linearizable, "{seen}");
        }
        // But two sequential gets cannot see b then a when a was written first and b after it began... here the
        // puts are sequential: a then b; a get after both must see b.
        let h = vec![
            op(0, Some(1), put("a"), Some(KvOutput::PutOk)),
            op(2, Some(3), put("b"), Some(KvOutput::PutOk)),
            op(4, Some(6), get(), val(Some("b"))),
            op(7, Some(8), get(), val(Some("a"))),
        ];
        assert!(matches!(
            check(&KvModel, &h, 1_000_000),
            Verdict::NotLinearizable { .. }
        ));
    }

    #[test]
    fn a_read_cannot_go_back_in_time() {
        // Two overlapping reads that together see b then a, with a written before b.
        let h = vec![
            op(0, Some(1), put("a"), Some(KvOutput::PutOk)),
            op(2, Some(20), put("b"), Some(KvOutput::PutOk)),
            op(3, Some(4), get(), val(Some("b"))),
            op(5, Some(6), get(), val(Some("a"))),
        ];
        assert!(matches!(
            check(&KvModel, &h, 1_000_000),
            Verdict::NotLinearizable { .. }
        ));
    }

    #[test]
    fn unanswered_operations_may_or_may_not_take_effect() {
        let lost = vec![
            op(0, None, put("a"), None),
            op(5, Some(6), get(), val(None)),
            op(7, Some(8), get(), val(Some("a"))),
        ];
        assert_eq!(check(&KvModel, &lost, 1_000_000), Verdict::Linearizable);
        // But once seen it cannot be unseen.
        let flicker = vec![
            op(0, None, put("a"), None),
            op(5, Some(6), get(), val(Some("a"))),
            op(7, Some(8), get(), val(None)),
        ];
        assert!(matches!(
            check(&KvModel, &flicker, 1_000_000),
            Verdict::NotLinearizable { .. }
        ));
    }

    #[test]
    fn a_lost_acknowledged_write_is_caught() {
        let h = vec![
            op(0, Some(1), put("a"), Some(KvOutput::PutOk)),
            op(2, Some(3), get(), val(None)),
        ];
        assert!(matches!(
            check(&KvModel, &h, 1_000_000),
            Verdict::NotLinearizable { .. }
        ));
    }

    #[test]
    fn partitions_are_checked_independently() {
        let h = vec![
            op(
                0,
                Some(1),
                KvInput::Put {
                    key: b"x".to_vec(),
                    val: b"1".to_vec(),
                },
                Some(KvOutput::PutOk),
            ),
            op(
                0,
                Some(1),
                KvInput::Put {
                    key: b"y".to_vec(),
                    val: b"2".to_vec(),
                },
                Some(KvOutput::PutOk),
            ),
            op(2, Some(3), KvInput::Get { key: b"x".to_vec() }, val(Some("1"))),
            op(2, Some(3), KvInput::Get { key: b"y".to_vec() }, val(None)),
        ];
        let (v, k) = check_partitioned(&KvModel, &h, |i| i.key().to_vec(), 1_000_000);
        assert!(matches!(v, Verdict::NotLinearizable { .. }));
        assert_eq!(k, Some(b"y".to_vec()));
    }

    /// Random histories: a history generated by a sequentially consistent execution with real-time order is always
    /// linearizable, and corrupting one read's response to a value never written makes it not.
    #[test]
    fn random_real_histories_are_linearizable() {
        use proptest::prelude::*;
        let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(200));
        runner
            .run(
                &proptest::collection::vec((0u8..3, 0u8..4, 1u64..5, 0u64..4), 1..24),
                |steps| {
                    // Execute atomically at distinct instants, with each op's interval around its instant.
                    let mut state: Option<Vec<u8>> = None;
                    let mut h = Vec::new();
                    for (i, (kind, v, before, after)) in steps.iter().enumerate() {
                        let at = 10 * i as u64 + 5;
                        let (input, output) = match kind {
                            0 => {
                                state = Some(vec![*v]);
                                (
                                    KvInput::Put {
                                        key: b"k".to_vec(),
                                        val: vec![*v],
                                    },
                                    KvOutput::PutOk,
                                )
                            }
                            1 => (KvInput::Get { key: b"k".to_vec() }, KvOutput::Value(state.clone())),
                            _ => {
                                let existed = state.is_some();
                                state = None;
                                (KvInput::Delete { key: b"k".to_vec() }, KvOutput::Deleted(existed))
                            }
                        };
                        h.push(op(at - before, Some(at + after), input, Some(output)));
                    }
                    prop_assert_eq!(check(&KvModel, &h, 10_000_000), Verdict::Linearizable);
                    // A read of a value nobody wrote is never linearizable.
                    if let Some(pos) = h.iter().position(|o| matches!(o.input, KvInput::Get { .. })) {
                        let mut bad = h.clone();
                        if let Some(o) = bad.get_mut(pos) {
                            o.output = Some(KvOutput::Value(Some(vec![99])));
                        }
                        let is_bad = matches!(check(&KvModel, &bad, 10_000_000), Verdict::NotLinearizable { .. });
                        prop_assert!(is_bad);
                    }
                    Ok(())
                },
            )
            .unwrap();
    }

    /// Brute force: try every order of the operations that respects real time; an unanswered operation may also be
    /// left out.
    fn brute(h: &[Operation<KvInput, KvOutput>]) -> bool {
        fn go(h: &[Operation<KvInput, KvOutput>], used: &mut Vec<bool>, state: &Option<Vec<u8>>) -> bool {
            let remaining: Vec<usize> = (0..h.len()).filter(|i| !used[*i]).collect();
            if remaining.iter().all(|i| h[*i].ret.is_none()) {
                return true;
            }
            for &i in &remaining {
                // `i` may go next only if no remaining operation returned before `i` was called.
                let blocked = remaining
                    .iter()
                    .any(|&j| j != i && h[j].ret.is_some_and(|r| r < h[i].call));
                if blocked {
                    continue;
                }
                if let Some(next) = KvModel.step(state, &h[i].input, h[i].output.as_ref()) {
                    used[i] = true;
                    let ok = go(h, used, &next);
                    used[i] = false;
                    if ok {
                        return true;
                    }
                }
            }
            false
        }
        go(h, &mut vec![false; h.len()], &None)
    }

    #[test]
    fn agrees_with_brute_force_on_random_histories() {
        use proptest::prelude::*;
        let mut runner = proptest::test_runner::TestRunner::new(ProptestConfig::with_cases(3000));
        let op_strategy = (0u64..12, 0u64..8, any::<bool>(), 0u8..3, 0u8..3, 0u8..4);
        runner
            .run(&proptest::collection::vec(op_strategy, 1..7), |ops| {
                let h: Vec<Operation<KvInput, KvOutput>> = ops
                    .iter()
                    .map(|(call, dur, answered, kind, v, out)| {
                        let input = match kind {
                            0 => KvInput::Put {
                                key: b"k".to_vec(),
                                val: vec![*v],
                            },
                            1 => KvInput::Get { key: b"k".to_vec() },
                            _ => KvInput::Delete { key: b"k".to_vec() },
                        };
                        let output = answered.then(|| match kind {
                            0 => KvOutput::PutOk,
                            1 => KvOutput::Value((*out < 3).then(|| vec![*out])),
                            _ => KvOutput::Deleted(*out % 2 == 0),
                        });
                        op(*call, answered.then_some(call + dur), input, output)
                    })
                    .collect();
                let expect = brute(&h);
                let got = check(&KvModel, &h, 10_000_000);
                prop_assert_eq!(got == Verdict::Linearizable, expect, "{:?}", h);
                Ok(())
            })
            .unwrap();
    }
}
