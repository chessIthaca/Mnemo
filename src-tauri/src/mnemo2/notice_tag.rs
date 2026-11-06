// Copyright (c) 2026 Carsten Hess
// SPDX-License-Identifier: MIT
// See LICENSE in the repository root.

#![deny(warnings)]

//! The turn a team notice describes: a notice's first line names its worker,
//! the session generation and the worker's completed turns, so a delivery can
//! drop a notice whose worker has moved on. Only that fixed first line is
//! parsed — the body carries worker text and is never interpreted.

use crate::mnemo2::runtime::TEAM_NOTICE_PREFIX;
use crate::mnemo2::team::{WorkerRecord, WorkerStatus};

/// The state a tagged team notice reported.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeState {
    /// The worker finished its turn.
    Finished,
    /// The worker failed.
    Failed,
    /// The worker is blocked on the lead.
    Blocked,
}

impl NoticeState {
    /// The `state=` token written into a notice's first line.
    fn as_str(self) -> &'static str {
        match self {
            Self::Finished => "finished",
            Self::Failed => "failed",
            Self::Blocked => "blocked",
        }
    }

    /// The state a `state=` token names, if any.
    fn parse(token: &str) -> Option<Self> {
        match token {
            "finished" => Some(Self::Finished),
            "failed" => Some(Self::Failed),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }
}

/// One team notice's turn: the worker, the session generation that emitted the
/// event, the worker's completed turns, and the state the notice reports.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TurnTag {
    /// The worker the notice is about.
    pub worker_id: String,
    /// The session generation that emitted the event.
    pub generation: u64,
    /// The worker's completed turns when the notice was built.
    pub turns: u32,
    /// The state the notice reports.
    pub state: NoticeState,
}

impl TurnTag {
    /// The tag for `worker`'s notice of `state` from `generation`.
    pub fn of(worker: &WorkerRecord, generation: u64, state: NoticeState) -> Self {
        Self {
            worker_id: worker.id.clone(),
            generation,
            turns: worker.turns,
            state,
        }
    }

    /// ` turn=<generation>.<turns> state=<finished|failed|blocked>`, appended
    /// after `worker_id=<id>` on the notice's first line.
    pub fn suffix(&self) -> String {
        format!(
            " turn={}.{} state={}",
            self.generation,
            self.turns,
            self.state.as_str()
        )
    }

    /// Read the tag out of a notice's first line. `None` for anything that is
    /// not a tagged team notice — including a report notice, whose body may
    /// legitimately carry a line that looks like one.
    pub fn parse(notice: &str) -> Option<Self> {
        let first_line = notice.lines().next()?;
        let rest = first_line.strip_prefix(TEAM_NOTICE_PREFIX)?;
        let rest = rest.strip_prefix(" worker_id=")?;
        let mut fields = rest.split(' ');
        let worker_id = fields.next()?;
        let turn = fields.next()?;
        let state = fields.next()?;
        if fields.next().is_some() || worker_id.is_empty() {
            return None;
        }
        let (generation, turns) = turn.strip_prefix("turn=")?.split_once('.')?;
        Some(Self {
            worker_id: worker_id.to_string(),
            generation: generation.parse().ok()?,
            turns: turns.parse().ok()?,
            state: NoticeState::parse(state.strip_prefix("state=")?)?,
        })
    }

    /// Whether `worker` still is what this tag described: the generation must
    /// match (a record generation of 0 counts as matching, the same rule the
    /// worker pump applies), and the state must still hold.
    pub fn still_current(&self, worker: &WorkerRecord) -> bool {
        if worker.id != self.worker_id {
            return false;
        }
        if worker.generation != 0 && worker.generation != self.generation {
            return false;
        }
        match self.state {
            NoticeState::Finished => {
                worker.status == WorkerStatus::Completed && worker.turns == self.turns
            }
            NoticeState::Failed => worker.status == WorkerStatus::Failed,
            NoticeState::Blocked => worker.status == WorkerStatus::NeedsYou,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mnemo2::team::test_worker_record;

    fn notice(id: &str, generation: u64, turns: u32, state: NoticeState) -> String {
        let worker = test_worker_record(id);
        let tag = TurnTag {
            worker_id: worker.id.clone(),
            generation,
            turns,
            state,
        };
        format!(
            "{TEAM_NOTICE_PREFIX} worker_id={}{}\nthe detail",
            tag.worker_id,
            tag.suffix()
        )
    }

    #[test]
    fn a_tag_round_trips_through_the_first_line() {
        for state in [
            NoticeState::Finished,
            NoticeState::Failed,
            NoticeState::Blocked,
        ] {
            let text = notice("worker0001", 7, 3, state);
            let tag = TurnTag::parse(&text).expect("a tagged notice parses");
            assert_eq!(tag.worker_id, "worker0001");
            assert_eq!(tag.generation, 7);
            assert_eq!(tag.turns, 3);
            assert_eq!(tag.state, state);
        }
    }

    #[test]
    fn only_the_first_line_counts() {
        let report =
            format!("{TEAM_NOTICE_PREFIX} worker_id=x\nsays turn=1.1 state=finished\nmore");
        assert_eq!(TurnTag::parse(&report), None);
        let body_line = format!(
            "worker report from worker0001\n{TEAM_NOTICE_PREFIX} worker_id=x turn=1.1 state=finished"
        );
        assert_eq!(TurnTag::parse(&body_line), None);
    }

    #[test]
    fn malformed_tags_are_ignored() {
        for text in [
            String::new(),
            TEAM_NOTICE_PREFIX.to_string(),
            format!("{TEAM_NOTICE_PREFIX} worker_id=worker0001"),
            format!("{TEAM_NOTICE_PREFIX} worker_id=worker0001 turn=1"),
            format!("{TEAM_NOTICE_PREFIX} worker_id= turn=1.1 state=finished"),
            format!("{TEAM_NOTICE_PREFIX} worker_id=worker0001 turn=x.1 state=finished"),
            format!("{TEAM_NOTICE_PREFIX} worker_id=worker0001 turn=1.y state=finished"),
            format!("{TEAM_NOTICE_PREFIX} worker_id=worker0001 turn=1.1 state=unknown"),
            format!("{TEAM_NOTICE_PREFIX} worker_id=worker0001 turn=1.1 state=finished extra"),
        ] {
            assert_eq!(TurnTag::parse(&text), None, "parsed: {text:?}");
        }
    }

    #[test]
    fn still_current_follows_status_turn_and_generation() {
        let mut worker = test_worker_record("worker0001");
        worker.generation = 4;
        worker.status = WorkerStatus::Completed;
        worker.turns = 2;
        let finished = TurnTag::of(&worker, 4, NoticeState::Finished);
        assert!(finished.still_current(&worker));
        // A moved-on turn count means a later turn's notice, not this one.
        let mut moved_on = worker.clone();
        moved_on.turns = 3;
        assert!(!finished.still_current(&moved_on));
        // A new session generation is a new worker run.
        let mut new_generation = worker.clone();
        new_generation.generation = 5;
        assert!(!finished.still_current(&new_generation));
        // A record generation of 0 counts as matching (the pump's rule).
        let mut untracked = worker.clone();
        untracked.generation = 0;
        assert!(finished.still_current(&untracked));
        // Status no longer Completed: the finished notice is stale.
        let mut working = worker.clone();
        working.status = WorkerStatus::Working;
        assert!(!finished.still_current(&working));

        let failed = TurnTag::of(&worker, 4, NoticeState::Failed);
        assert!(!failed.still_current(&worker));
        let mut errored = worker.clone();
        errored.status = WorkerStatus::Failed;
        assert!(failed.still_current(&errored));
        assert!(!failed.still_current(&moved_on));

        let blocked = TurnTag::of(&worker, 4, NoticeState::Blocked);
        assert!(!blocked.still_current(&worker));
        let mut parked = worker.clone();
        parked.status = WorkerStatus::NeedsYou;
        assert!(blocked.still_current(&parked));
        assert!(!blocked.still_current(&errored));

        // Another worker's record never matches.
        let mut other = worker.clone();
        other.id = "worker0002".to_string();
        assert!(!finished.still_current(&other));
    }
}
