//! Exploration of the search space.
use crate::device::Context;
use crate::explorer::choice::ActionEx;
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
pub struct Candidate<'a> {
    /// Represents a part of the full search space.
    pub space: SearchSpace<'a>,
    /// Gives a lower bound in nanoseconds on the execution time of `fun`.
    pub bound: Bound,
    /// The depth of the candidate in the search tree.
    pub depth: usize,
    /// The list of actions already taken.
    pub actions: List<ActionEx>,
}

impl<'a> Candidate<'a> {
    /// Creates a new candidate, with depth 0.
    pub fn new(space: SearchSpace<'a>, bound: Bound) -> Self {
        Self::with_actions(space, bound, std::iter::empty())
    }

    pub fn with_actions<II>(space: SearchSpace<'a>, bound: Bound, actions: II) -> Self
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
    ) -> Vec<Candidate<'a>> {
        let res = choice
            .into_iter()
            .flat_map(|action| {
                self.apply_decision(context, action)
                    .map_err(|_| trace!("invalid action encountered"))
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
        err: &String,
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
    ) -> Result<Self, ()> {
        debug!("applying action {:?}", action);
        let mut space = self.space.clone();
        match action {
            ActionEx::Action(action) => space.apply_decisions(vec![action]),
            ActionEx::LowerLayout {
                mem,
                ref st_dims,
                ref ld_dims,
            } => space.lower_layout(mem, st_dims, ld_dims),
        }?;
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

impl<'a> std::fmt::Display for Candidate<'a> {
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

impl<'a> PartialEq for Candidate<'a> {
    fn eq(&self, rhs: &Candidate<'a>) -> bool {
        self.bound == rhs.bound
    }
}

impl<'a> Eq for Candidate<'a> {}

impl<'a> PartialOrd for Candidate<'a> {
    fn partial_cmp(&self, rhs: &Candidate<'a>) -> Option<Ordering> {
        self.bound.partial_cmp(&rhs.bound)
    }
}

impl<'a> Ord for Candidate<'a> {
    fn cmp(&self, rhs: &Candidate<'a>) -> Ordering {
        unwrap!(self.partial_cmp(rhs))
    }
}
