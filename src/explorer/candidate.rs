//! Exploration of the search space.
use crate::device::Context;
use crate::explorer::choice::{ActionError, ActionEx};
use crate::model::{bound, Bound};
use crate::search_space::SearchSpace;

use log::{debug, info, trace};
use rpds::List;
use std;
use std::cmp::{Ordering, PartialOrd};
use std::io::{self, Write};
use std::path::Path;

use itertools::Itertools;
use utils::unwrap;

/// A node of the search tree.
#[derive(Clone)]
pub struct Candidate {
    /// Represents a part of the full search space.
    pub space: SearchSpace,
    /// Gives a lower bound in nanoseconds on the execution time of `fun`.
    pub bound: Bound,
    /// The depth of the candidate in the search tree.
    pub depth: usize,
    /// The list of actions already taken.
    pub actions: List<ActionEx>,
}

impl Candidate {
    /// Creates a new candidate, with depth 0.
    pub fn new(space: SearchSpace, bound: Bound) -> Self {
        Self::with_actions(space, bound, std::iter::empty())
    }

    pub fn with_actions<II>(space: SearchSpace, bound: Bound, actions: II) -> Self
    where
        II: IntoIterator<Item = ActionEx>,
    {
        let actions = actions.into_iter().collect::<List<_>>();
        let depth = actions.len();
        Candidate {
            space,
            bound,
            depth,
            actions,
        }
    }

    pub fn apply_choice(
        &self,
        context: &dyn Context,
        choice: Vec<ActionEx>,
    ) -> Vec<Candidate> {
        let res = choice
            .into_iter()
            .flat_map(|action| {
                self.apply_decision(context, action)
                    .map_err(|err| trace!("invalid action encountered: {}", err))
                    .ok()
            })
            .collect_vec();
        if res.is_empty() {
            info!("deadend encountered in the search space");
        }
        res
    }

    /// Dump all pertinent information about the candidate into a directory.  Useful for debugging.
    pub fn dump_to<P: AsRef<Path>>(
        &self,
        path: P,
        context: &dyn Context,
        eval: f64,
        err: &str,
    ) -> io::Result<()> {
        std::fs::create_dir_all(path.as_ref()).unwrap();

        write!(
            std::fs::File::create(path.as_ref().join("actions.json"))?,
            "{}",
            serde_json::to_string(&self.actions).unwrap()
        )?;

        std::fs::File::create(path.as_ref().join("error.txt"))?
            .write_all(err.as_bytes())?;

        write!(
            std::fs::File::create(path.as_ref().join("human.txt")).unwrap(),
            "Invalid results ({:.4e}ns) for {}",
            eval,
            self
        )?;

        self.space.dump_code(context, path.as_ref().join("code"))
    }

    /// Applies a choice to a candidate.
    pub fn apply_decision(
        &self,
        context: &dyn Context,
        action: ActionEx,
    ) -> Result<Self, ActionError> {
        debug!("applying action {:?}", action);
        let space = action.apply_to(self.space.clone())?;
        let bound = bound(&space, context);
        let delta = 1.0e-2 * self.bound.value();
        if bound.value() + delta < self.bound.value() {
            debug!(
                "decreasing bound: {} > {}, with actions {:?} when applying {:?}",
                self.bound, bound, self.actions, action
            );
        }
        let actions = self.actions.push_front(action);
        Ok(Candidate {
            space,
            bound,
            depth: self.depth + 1,
            actions,
        })
    }
}

impl std::fmt::Display for Candidate {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        writeln!(
            f,
            "candidate at depth {}, with bound {} for actions:",
            self.depth, self.bound
        )?;
        for action in &self.actions {
            writeln!(f, "{:?}", action)?;
        }
        Ok(())
    }
}

impl PartialEq for Candidate {
    fn eq(&self, rhs: &Candidate) -> bool {
        self.bound == rhs.bound
    }
}

impl Eq for Candidate {}

impl PartialOrd for Candidate {
    fn partial_cmp(&self, rhs: &Candidate) -> Option<Ordering> {
        self.bound.partial_cmp(&rhs.bound)
    }
}

impl Ord for Candidate {
    fn cmp(&self, rhs: &Candidate) -> Ordering {
        unwrap!(self.partial_cmp(rhs))
    }
}
