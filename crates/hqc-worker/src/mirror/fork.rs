//! Forks in a transcript (SPEC §9.3).
//!
//! Records hang off their `parentUuid`. A writer that resumes from a record that is not the
//! end of the file — VS Code continuing from the last record it holds in memory after hqc
//! wrote turns, or `--resume-session-at` (fixture `fork-2.1.308`) — starts a second line of
//! turns from that record; the other line's turns are not in its context.
//!
//! A record with two children is not a fork by itself: parallel tool calls give a
//! `tool_use` record a second child (the next `tool_use` and a `tool_result`, fixtures
//! `auto-mode`, `auto-classifier`). So the tree counts prompts (records that become
//! `user.message`) below every record, and a prompt makes a fork when a record above it has
//! another child with prompts below. The lowest such record is the fork point; the prompts
//! of its other children are the orphaned turns. Each point is reported once.

use std::collections::{HashMap, HashSet};

use uuid::Uuid;

#[derive(Debug, Default)]
pub struct Tree {
    parent: HashMap<Uuid, Option<Uuid>>,
    children: HashMap<Uuid, Vec<Uuid>>,
    /// Prompts in the subtree of a record, itself included.
    prompts: HashMap<Uuid, u32>,
    reported: HashSet<Uuid>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Fork {
    pub at: Uuid,
    pub orphaned_turns: u32,
}

impl Tree {
    /// Add a record; for a prompt, the fork it makes, if that point was not reported yet.
    /// A record seen before (a file read again) changes nothing.
    pub fn add(&mut self, uuid: Uuid, parent: Option<Uuid>, prompt: bool) -> Option<Fork> {
        if self.parent.contains_key(&uuid) {
            return None;
        }
        self.parent.insert(uuid, parent);
        if let Some(p) = parent {
            self.children.entry(p).or_default().push(uuid);
        }
        if !prompt {
            return None;
        }
        let mut found = None;
        let mut cur = uuid;
        // A broken file could loop; no path is longer than the number of records.
        for _ in 0..=self.parent.len() {
            *self.prompts.entry(cur).or_default() += 1;
            let Some(Some(p)) = self.parent.get(&cur).copied() else {
                break;
            };
            if found.is_none() {
                let other: u32 = self.children[&p]
                    .iter()
                    .filter(|c| **c != cur)
                    .map(|c| self.prompts.get(c).copied().unwrap_or(0))
                    .sum();
                if other > 0 {
                    found = Some(Fork {
                        at: p,
                        orphaned_turns: other,
                    });
                }
            }
            cur = p;
        }
        found.filter(|f| self.reported.insert(f.at))
    }
}

/// «N ходов» with the Russian plural of «ход» and the verb «попасть» agreeing with it.
pub fn turns_phrase(n: u32) -> (String, &'static str) {
    let (m10, m100) = (n % 10, n % 100);
    if m10 == 1 && m100 != 11 {
        (format!("{n} ход"), "не попал")
    } else if (2..=4).contains(&m10) && !(12..=14).contains(&m100) {
        (format!("{n} хода"), "не попали")
    } else {
        (format!("{n} ходов"), "не попали")
    }
}

/// The text of `system session_forked`: who started the later line, and how many turns of
/// the other one it does not see.
pub fn fork_text(branch_from_hqc: bool, orphaned: u32) -> String {
    let (turns, verb) = turns_phrase(orphaned);
    if branch_from_hqc {
        format!("hqc продолжил с более ранней точки: {turns} из VS Code {verb} в его контекст")
    } else {
        format!("VS Code продолжил с более ранней точки: {turns} из hqc {verb} в его контекст")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(n: usize) -> Vec<Uuid> {
        (0..n).map(|_| Uuid::new_v4()).collect()
    }

    #[test]
    fn parallel_tool_calls_are_not_a_fork() {
        // prompt → use1 → {use2 → result2 → answer → prompt2, result1}
        let u = ids(7);
        let mut t = Tree::default();
        assert_eq!(t.add(u[0], None, true), None);
        t.add(u[1], Some(u[0]), false);
        t.add(u[2], Some(u[1]), false);
        t.add(u[3], Some(u[1]), false); // a result beside the next tool_use
        t.add(u[4], Some(u[2]), false);
        t.add(u[5], Some(u[4]), false);
        assert_eq!(t.add(u[6], Some(u[5]), true), None);
    }

    #[test]
    fn a_second_line_of_turns_is_a_fork_once() {
        // p0 → a0 → {p1 → a1 → p2 → a2 (hqc), p3 → a3 (VS Code)}
        let u = ids(9);
        let mut t = Tree::default();
        t.add(u[0], None, true);
        t.add(u[1], Some(u[0]), false);
        t.add(u[2], Some(u[1]), true);
        t.add(u[3], Some(u[2]), false);
        t.add(u[4], Some(u[3]), true);
        t.add(u[5], Some(u[4]), false);
        let f = t.add(u[6], Some(u[1]), true).unwrap();
        assert_eq!(
            f,
            Fork {
                at: u[1],
                orphaned_turns: 2
            }
        );
        // The same file read again, or a later prompt on either line: no second report.
        assert_eq!(t.add(u[6], Some(u[1]), true), None);
        t.add(u[7], Some(u[6]), false);
        assert_eq!(t.add(u[8], Some(u[5]), true), None);
    }

    #[test]
    fn a_cycle_does_not_hang() {
        let u = ids(2);
        let mut t = Tree::default();
        t.add(u[0], Some(u[1]), false);
        t.add(u[1], Some(u[0]), true);
    }

    #[test]
    fn russian_plurals() {
        assert_eq!(
            fork_text(false, 1),
            "VS Code продолжил с более ранней точки: 1 ход из hqc не попал в его контекст"
        );
        assert_eq!(turns_phrase(3).0, "3 хода");
        assert_eq!(turns_phrase(11).0, "11 ходов");
        assert_eq!(turns_phrase(21), ("21 ход".into(), "не попал"));
        assert_eq!(turns_phrase(37).0, "37 ходов");
        assert!(fork_text(true, 2).starts_with("hqc продолжил"));
    }
}
