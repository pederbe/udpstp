//! The order a Round's Round Runs go in, as the Server-scale round's
//! Python orders them (`scripts/bench/scale.py:247-266, 933-990`): each
//! of several things going first in turn, a point's runs spread over the
//! repeats, the queue that runs an invalid run again a few runs later,
//! and the streaks that stop a Round.
//!
//! Nothing here runs anything: the schedule that decides what runs next
//! (`udpstp-bench`'s) asks these, and a test sets every input.

use std::collections::{BTreeMap, VecDeque};

/// `rotate(items, k)`: the items from the `k`-th on, then those before
/// it, `k` taken modulo their number as Python takes it (so `-1` starts
/// at the last).
pub fn rotate<T: Clone>(items: &[T], k: i64) -> Vec<T> {
    if items.is_empty() {
        return Vec::new();
    }
    let k = k.rem_euclid(items.len() as i64) as usize;
    items[k..].iter().chain(&items[..k]).cloned().collect()
}

/// `turn(items, t)`: the order of `items` the `t`-th time (from 0) they
/// run together. Each goes first in turn, and the order reverses every
/// other time, so that none always runs right after another.
pub fn turn<T: Clone>(items: &[T], t: i64) -> Vec<T> {
    let mut order = rotate(items, t.div_euclid(2));
    if t.rem_euclid(2) == 1 {
        order.reverse();
    }
    order
}

/// `spread(k, total)`: the repeats, from 1 to `total`, in which a point
/// with `k` runs runs, spread evenly over the Round:
/// `int((j + 0.5) * total / k) + 1` for each of its runs `j`, without
/// repeats, in order. None for `k` of 0 or less.
pub fn spread(k: i64, total: i64) -> Vec<i64> {
    let mut repeats: Vec<i64> = (0..k.max(0))
        .map(|j| ((j as f64 + 0.5) * total as f64 / k as f64).trunc() as i64 + 1)
        .collect();
    repeats.sort_unstable();
    repeats.dedup();
    repeats
}

/// The runs still to come (`scale.py`'s `Queue`): the matrix's in order,
/// and the re-runs of invalid ones, each `gap` runs after its original was
/// taken and one between two of the matrix's, the rest at the end.
#[derive(Clone, Debug)]
pub struct Queue<T> {
    pending: VecDeque<T>,
    /// Each re-run with the count of runs taken at which it is due.
    again: VecDeque<(usize, T)>,
    taken: usize,
    last_again: bool,
    gap: usize,
}

impl<T> Queue<T> {
    /// The matrix's runs, in order; a re-run is due `gap` runs later.
    pub fn new(runs: impl IntoIterator<Item = T>, gap: usize) -> Queue<T> {
        Queue {
            pending: runs.into_iter().collect(),
            again: VecDeque::new(),
            taken: 0,
            last_again: false,
            gap,
        }
    }

    /// Whether any run is still to come (the Python's `bool(queue)`).
    pub fn is_empty(&self) -> bool {
        self.pending.is_empty() && self.again.is_empty()
    }

    /// Runs `run` again, `gap` runs from now.
    pub fn rerun(&mut self, run: T) {
        self.again.push_back((self.taken + self.gap, run));
    }

    /// The next run: the first re-run once it is due, if the run before
    /// wasn't a re-run, or whenever the matrix's runs are done; else the
    /// matrix's next.
    #[allow(clippy::should_implement_trait)]
    pub fn next(&mut self) -> Option<T> {
        let due = self
            .again
            .front()
            .is_some_and(|&(due, _)| due <= self.taken && !self.last_again);
        let (run, again) = if !self.again.is_empty() && (self.pending.is_empty() || due) {
            (self.again.pop_front().map(|(_, run)| run)?, true)
        } else {
            (self.pending.pop_front()?, false)
        };
        self.last_again = again;
        self.taken += 1;
        Some(run)
    }

    /// How many runs still to come `of` takes (the Python's `left(point)`,
    /// with `of` saying whether a run is of the point): each can still be
    /// valid.
    pub fn left(&self, of: impl Fn(&T) -> bool) -> usize {
        self.pending.iter().filter(|r| of(r)).count()
            + self.again.iter().filter(|(_, r)| of(r)).count()
    }

    /// Drops every run still to come that `of` takes (the Python's
    /// `drop(point)`).
    pub fn drop_runs(&mut self, of: impl Fn(&T) -> bool) {
        self.pending.retain(|r| !of(r));
        self.again.retain(|(_, r)| !of(r));
    }
}

/// `streak(history, n)`: whether the last `n` entries are all true, with
/// at least `n` of them. As in the Python, `n` of 0 asks for all of them.
pub fn streak(history: &[bool], n: usize) -> bool {
    history.len() >= n && last(history, n).iter().all(|&b| b)
}

/// `history[-n:]`, where `-0` is the whole list, as Python slices it.
fn last(history: &[bool], n: usize) -> &[bool] {
    if n == 0 {
        history
    } else {
        &history[history.len().saturating_sub(n)..]
    }
}

/// The stop rule (`scale.py`'s `stop_now`), and why it stops the Round, or
/// `None`. `easy` is whether each run so far that should always be valid
/// was, `tooling` whether each run so far failed in the tooling, and
/// `by_engine` the same for each engine's runs; all in order. It stops
/// after `easy_in_a_row` invalid easy runs in a row, `tooling_in_a_row`
/// failures of the tooling in a row, or as many of one engine's runs in a
/// row, whatever the others did between them; engines in order of name.
pub fn stop_now(
    easy: &[bool],
    tooling: &[bool],
    by_engine: &BTreeMap<String, Vec<bool>>,
    easy_in_a_row: usize,
    tooling_in_a_row: usize,
) -> Option<String> {
    if easy.len() >= easy_in_a_row && !last(easy, easy_in_a_row).iter().any(|&b| b) {
        return Some("runs that should always be valid keep coming out invalid".to_string());
    }
    if streak(tooling, tooling_in_a_row) {
        return Some(format!(
            "the tooling failed {tooling_in_a_row} runs in a row"
        ));
    }
    for (engine, history) in by_engine {
        if streak(history, tooling_in_a_row) {
            return Some(format!(
                "the tooling failed {engine}'s last {tooling_in_a_row} runs"
            ));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotate_and_turn_take_turns() {
        let items = ["a", "b", "c"];
        assert_eq!(rotate(&items, 1), ["b", "c", "a"]);
        assert_eq!(rotate(&items, -1), ["c", "a", "b"]);
        assert_eq!(rotate(&items, 4), ["b", "c", "a"]);
        assert_eq!(rotate::<&str>(&[], 3), Vec::<&str>::new());
        let turns: Vec<Vec<&str>> = (0..6).map(|t| turn(&items, t)).collect();
        assert_eq!(
            turns,
            [
                vec!["a", "b", "c"],
                vec!["c", "b", "a"],
                vec!["b", "c", "a"],
                vec!["a", "c", "b"],
                vec!["c", "a", "b"],
                vec!["b", "a", "c"],
            ]
        );
    }

    #[test]
    fn spread_spreads_a_points_runs_over_the_repeats() {
        assert_eq!(spread(3, 6), [2, 4, 6]);
        assert_eq!(spread(6, 6), [1, 2, 3, 4, 5, 6]);
        assert_eq!(spread(1, 6), [4]);
        // More runs than repeats: each repeat once.
        assert_eq!(spread(4, 2), [1, 2]);
        assert_eq!(spread(0, 6), Vec::<i64>::new());
        assert_eq!(spread(2, 0), [1]);
    }

    #[test]
    fn the_queue_runs_an_invalid_run_again_a_few_runs_later() {
        let mut queue = Queue::new(1..=8, 5);
        let mut taken = Vec::new();
        while let Some(run) = queue.next() {
            // Runs 1 and 2 come out invalid, once each.
            if run < 3 {
                queue.rerun(run + 100);
            }
            taken.push(run);
        }
        // Each due five runs after it was taken, one between two of the
        // matrix's: 101 after 6, 102 after 7.
        assert_eq!(taken, [1, 2, 3, 4, 5, 6, 101, 7, 102, 8]);
        assert!(queue.is_empty());
    }

    #[test]
    fn the_queue_counts_and_drops_a_points_runs() {
        let mut queue = Queue::new([("a", 1), ("b", 2), ("a", 3)], 5);
        assert_eq!(queue.next(), Some(("a", 1)));
        queue.rerun(("a", 4));
        assert_eq!(queue.left(|r| r.0 == "a"), 2);
        queue.drop_runs(|r| r.0 == "a");
        assert_eq!(queue.left(|r| r.0 == "a"), 0);
        assert_eq!(queue.next(), Some(("b", 2)));
        assert_eq!(queue.next(), None);
    }

    #[test]
    fn the_stop_rule_ends_on_its_three_streaks() {
        let none = BTreeMap::new();
        assert_eq!(stop_now(&[true, false, false], &[], &none, 3, 3), None);
        assert_eq!(
            stop_now(&[true, false, false, false], &[], &none, 3, 3).as_deref(),
            Some("runs that should always be valid keep coming out invalid")
        );
        assert_eq!(
            stop_now(&[], &[false, true, true, true], &none, 3, 3).as_deref(),
            Some("the tooling failed 3 runs in a row")
        );
        let by_engine = BTreeMap::from([
            ("udpstp".to_string(), vec![false, false]),
            ("prototype-b".to_string(), vec![true, true, true]),
        ]);
        assert_eq!(
            stop_now(&[], &[true, false, true, false, true], &by_engine, 3, 3).as_deref(),
            Some("the tooling failed prototype-b's last 3 runs")
        );
        // As in the Python, a streak of none asks for the whole history.
        assert!(streak(&[true, true], 0));
        assert!(!streak(&[true, false], 0));
    }
}
