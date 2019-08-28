use std::borrow::Cow;
use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::sync::atomic;

use itertools::*;
use serde_json;
use structopt::StructOpt;

use telamon::device;
use telamon::explorer::{
    self,
    choice::{default_list, ActionEx as Action, Choice},
    config,
    eventlog::EventLog,
    mcts, Candidate,
};
use telamon::model::{bound, Bound};
use telamon::offline_analysis::tree::CandidateTree;
use telamon::search_space::SearchSpace;
use telamon_kernels::statistics::estimate_mean;

use telamon_cli::{Bench, KernelBundle, KernelParam, Platform, ReplayPath};

/// Compute the bound for a given candidate.
#[derive(StructOpt)]
struct ComputeBound {
    #[structopt(long = "platform", default_value = "cuda")]
    platform: Platform,

    /// Kernel specification to use.
    #[structopt(short = "k", long = "kernel")]
    kernel: KernelParam,

    /// Path to a saved replay file to load before computing the bound.
    #[structopt(parse(from_os_str), short = "r", long = "replay")]
    replay: Option<ReplayPath>,
}

impl ComputeBound {
    fn run(&self, _args: &Opt) -> io::Result<()> {
        let builder = self.platform.to_builder();
        let mut context = builder.build_context();
        let (bundle, context) = context.kernel_bundle(&self.kernel);
        let mut candidates = bundle.candidates;

        assert!(candidates.len() == 1);
        let mut candidate = candidates.swap_remove(0).space;

        // Apply replay if there is some
        if let Some(replay) = &self.replay {
            for action in &replay.load()? {
                candidate = action
                    .apply_to(candidate)
                    .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
            }
        }

        let start = std::time::Instant::now();
        let bound = bound(&candidate, context);
        let duration = start.elapsed();
        println!("Bound is {:?} (computed in {:?})", bound, duration);

        Ok(())
    }
}

/// Compute bounds.csv
#[derive(StructOpt)]
struct Bounds {
    #[structopt(long = "platform", default_value = "cuda")]
    platform: Platform,

    #[structopt(long = "order")]
    order: Option<config::ChoiceOrdering>,

    #[structopt(long = "kernel")]
    kernel: KernelParam,

    #[structopt(long = "num-runs", default_value = "500")]
    num_runs: usize,
}

/// Ignore candidates with a too big bound in tests.
const CUT: f64 = 2e8f64;

impl Bounds {
    fn next_choice(&self, space: &SearchSpace) -> Option<Choice> {
        if let Some(order) = &self.order {
            explorer::choice::list(order, space).next()
        } else {
            explorer::choice::default_list(space).next()
        }
    }

    /// Descends along a path in the search tree and stores the bounds encountered on the way.
    fn random_descent(
        &self,
        candidates: &[Candidate],
        context: &dyn device::Context,
    ) -> Option<(Candidate, Vec<Bound>)> {
        let order = explorer::config::NewNodeOrder::Random;
        let mut candidates = Cow::Borrowed(candidates);
        let mut bounds = Vec::new();
        loop {
            let idx = if let Some(idx) = order.pick_candidate(&candidates, CUT) {
                idx
            } else {
                break None;
            };
            bounds.push(candidates[idx].bound.clone());
            let choice_opt = self.next_choice(&candidates[idx].space);
            if let Some(choice) = choice_opt {
                let new_nodes = candidates[idx]
                    .apply_choice(context, choice)
                    .into_iter()
                    .filter(|x| x.bound.value() < CUT)
                    .collect::<Vec<_>>();
                candidates = std::borrow::Cow::Owned(new_nodes);
            } else {
                break Some((
                    match candidates {
                        Cow::Borrowed(candidates) => candidates[idx].clone(),
                        Cow::Owned(mut candidates) => candidates.swap_remove(idx),
                    },
                    bounds,
                ));
            }
        }
    }

    fn test_bound<F>(
        &self,
        candidates: Vec<Candidate>,
        context: &dyn device::Context,
        body_fn: F,
    ) where
        F: Fn((f64, Vec<f64>)) + Sync,
    {
        let num_tested = atomic::AtomicUsize::new(0);
        let stabilizer = &context.stabilizer();
        context.async_eval(
            num_cpus::get(),
            device::EvalMode::TestBound,
            &|evaluator| loop {
                // We want to keep the collapsible if to make the order in which `fetch_add` and
                // `fetch_sub` explicit.
                #[allow(clippy::collapsible_if)]
                {
                    if num_tested.fetch_add(1, atomic::Ordering::SeqCst) >= self.num_runs
                    {
                        if num_tested.fetch_sub(1, atomic::Ordering::SeqCst)
                            > self.num_runs
                        {
                            break;
                        }
                    }
                }

                if let Some((leaf, mut bounds)) =
                    self.random_descent(&candidates, context)
                {
                    evaluator.add_kernel(leaf, {
                        let body_fn = &body_fn;
                        move |leaf, kernel| {
                            let bound = leaf.bound.clone();
                            let runtime = stabilizer
                                .wrap(kernel)
                                .bound(Some(bound.value()))
                                .evaluate()
                                .unwrap();
                            bounds.push(bound);
                            body_fn((
                                runtime,
                                bounds.into_iter().map(|bound| bound.value()).collect(),
                            ))
                        }
                    });
                } else {
                    num_tested.fetch_sub(1, atomic::Ordering::SeqCst);
                }
            },
        );
    }

    fn run(&self, _args: &Opt) -> io::Result<()> {
        let builder = self.platform.to_builder();
        let mut context = builder.build_context();
        let (bundle, context) = context.kernel_bundle(&self.kernel);
        let stdout = std::io::stdout();
        self.test_bound(bundle.candidates, context, |(runtime, bounds)| {
            let mut handle = stdout.lock();
            write!(handle, "{},{}", self.kernel, runtime).unwrap();
            for bound in bounds {
                write!(handle, ",{}", bound).unwrap();
            }
            writeln!(handle).unwrap();
        });

        Ok(())
    }
}

/// Prints code to stdout for a given kernel.
#[derive(StructOpt)]
struct Codegen {
    /// Path to the replay file to generate code for.  Must be compatible with the provided kernel.
    #[structopt(parse(from_os_str))]
    replay: ReplayPath,

    /// Kernel specification to use.
    #[structopt(short = "k", long = "kernel")]
    kernel: KernelParam,

    /// Platform to generate code for.
    #[structopt(long = "platform", short = "p", default_value = "cuda")]
    platform: Platform,
}

impl Codegen {
    fn run(&self, _args: &Opt) -> io::Result<()> {
        let builder = self.platform.to_builder();
        let mut context = builder.build_context();
        let (bundle, context) = context.kernel_bundle(&self.kernel);
        let mut candidates = bundle.candidates;
        assert!(candidates.len() == 1);

        let mut candidate = candidates.swap_remove(0).space;
        for action in &self.replay.load()? {
            candidate = action
                .apply_to(candidate)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
        }

        let code = telamon::codegen::Function::build(&candidate);
        context.device().print(&code, &mut std::io::stdout());

        Ok(())
    }
}

#[derive(StructOpt)]
struct Benchmark {
    #[structopt(parse(from_os_str))]
    replays: Vec<OsString>,

    /// Kernel specification to use.
    #[structopt(short = "k", long = "kernel")]
    kernel: KernelParam,

    #[structopt(long = "platform", short = "p", default_value = "cuda")]
    platform: Platform,

    /// Batch mode.  If enabled will print data in CSV format.
    #[structopt(long = "batch")]
    batch_mode: bool,

    /// Name to use for the reference in batch mode.  Ignored if batch mode is not enabled.
    #[structopt(long = "reference", default_value = "cublas")]
    reference_name: String,

    /// Number of times to run each benchmark.
    #[structopt(long = "bench-runs", default_value = "40")]
    num_bench_runs: usize,
}

impl Benchmark {
    fn build(
        &self,
        bundle: &KernelBundle<'_>,
        replay: &ReplayPath,
    ) -> io::Result<SearchSpace> {
        assert!(
            bundle.candidates.len() == 1,
            "Multi-candidates bundle not supported"
        );

        let mut candidate = bundle.candidates[0].space.clone();
        for action in &replay.load()? {
            candidate = action
                .apply_to(candidate)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?;
        }

        if default_list(&candidate).next().is_some() {
            return Err(io::Error::new(
                io::ErrorKind::Other,
                "Final candidate is not fixed",
            ));
        }

        Ok(candidate)
    }

    fn iter_replays(&self) -> impl Iterator<Item = io::Result<ReplayPath>> + '_ {
        self.replays.iter().flat_map(|replay| {
            if replay.to_str().map(|s| s.starts_with('@')).unwrap_or(false) {
                let replay = &replay.to_str().unwrap()[1..];
                match replay {
                    "-" => {
                        let stdin = io::stdin();
                        stdin
                            .lock()
                            .lines()
                            .map(|line| Ok(ReplayPath::from(&*line?)))
                            .collect::<Vec<_>>()
                    }
                    _ => match fs::File::open(replay) {
                        Ok(file) => BufReader::new(file)
                            .lines()
                            .map(|line| Ok(ReplayPath::from(&*line?)))
                            .collect::<Vec<_>>(),
                        Err(e) => vec![Err(e)],
                    },
                }
            } else {
                vec![Ok(ReplayPath::from(replay as &OsStr))]
            }
        })
    }

    fn run(&self, _args: &Opt) -> io::Result<()> {
        let builder = self.platform.to_builder();
        let mut context = builder.build_context();
        let (bundle, context) = context.kernel_bundle(&self.kernel);
        assert!(bundle.candidates.len() == 1);

        let reference = Bench::default()
            .runs(self.num_bench_runs)
            .benchmark_fn(&bundle.reference_fn);
        (bundle.check_fn)(context)
            .or_else(|err| Err(io::Error::new(io::ErrorKind::Other, err)))?;

        if self.batch_mode {
            println!("{},{}", self.reference_name, reference.iter().format(","));
        };

        let reference_estimate = estimate_mean(reference, 0.95, "ns");

        let mut failed = false;
        for replay in self.iter_replays() {
            let replay = match replay {
                Ok(replay) => replay,
                Err(err) => {
                    eprintln!("Failed to load replay: {}", err);
                    failed = true;
                    continue;
                }
            };

            let candidate = match self.build(&bundle, &replay) {
                Ok(candidate) => candidate,
                Err(err) => {
                    eprintln!(
                        "Unable to rebuild candidate from {}: {}",
                        replay.display(),
                        err
                    );
                    failed = true;
                    continue;
                }
            };

            let code = telamon::codegen::Function::build(&candidate);
            let runtimes = context.benchmark(&code, self.num_bench_runs);
            if let Err(err) = (bundle.check_fn)(context) {
                eprintln!("Check error for {}: {}", replay.display(), err);
                failed = true;
                continue;
            }

            if self.batch_mode {
                println!("{},{}", replay.display(), runtimes.into_iter().format(","));
            } else {
                let bound = bound(&candidate, context);
                println!("bound: {}", bound);

                let self_estimate = estimate_mean(runtimes, 0.95, "ns");
                let speedup = reference_estimate.value / self_estimate.value;
                println!(
                    "runtime: {}, reference: {} (speedup: {:.2})",
                    self_estimate, reference_estimate, speedup,
                );
            }
        }

        if failed {
            Err(io::Error::new(io::ErrorKind::Other, "Benchmark failed."))
        } else {
            Ok(())
        }
    }
}

/// Rebuild a specific list of actions from an event log.
///
/// This generates a .json replay file which can be used by the debugger, as well as by various
/// other utilities (eg the `compute_bound` subcommand).
#[derive(StructOpt)]
struct Rebuild {
    /// Path to the eventlog to rebuild from
    #[structopt(
        parse(from_os_str),
        short = "i",
        long = "input",
        default_value = "eventlog.tfrecord.gz"
    )]
    eventlog: PathBuf,

    /// Directory where the replay files should be stored into.
    ///
    /// `rebuild` will create one sub-directory for each requested candidate, containing the replay
    /// file as `actions.json`.  This matches the format of search output directories.
    #[structopt(parse(from_os_str), short = "o", long = "output", default_value = ".")]
    output: PathBuf,

    /// Identifier(s) of the candidate node(s) to rebuild.  This corresponds to the ID indicated in
    /// `watch.log`.
    ids: Vec<usize>,
}

impl Rebuild {
    fn run(&self, _args: &Opt) -> io::Result<()> {
        let mut nevals = 0;
        let mut tree = CandidateTree::new();

        let mut target = self.ids.clone();
        target.sort_unstable();
        target.dedup();
        target.reverse();

        for record_bytes in EventLog::open(&self.eventlog)?.records() {
            match bincode::deserialize(&record_bytes?)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?
            {
                mcts::Message::Node {
                    id,
                    parent,
                    mut children,
                    bound,
                    discovery_time,
                } => tree.extend(id, discovery_time, parent, bound, &mut children),
                mcts::Message::Trace { .. } => (),
                mcts::Message::Evaluation { id, value, .. } => {
                    if let Some(score) = value {
                        if Some(nevals) == target.last().cloned() {
                            println!("Found candidate {} (score: {})", id, score);
                            target.pop();
                            let actions = tree.get_node(id).actions();
                            let best_dir = self.output.join(format!("best_{}", nevals));
                            std::fs::create_dir_all(&best_dir)?;
                            let mut f =
                                std::fs::File::create(best_dir.join("actions.json"))?;
                            write!(f, "{}", serde_json::to_string(&actions)?)?;
                        }

                        if target.is_empty() {
                            return Ok(());
                        }

                        nevals += 1;
                    }
                }
            }
        }

        Err(io::Error::new(
            io::ErrorKind::Other,
            "Unable to find candidate",
        ))
    }
}

/// Compute statistics on an eventlog
#[derive(StructOpt)]
struct Stats {
    /// Path to the eventlog to compute stats for
    #[structopt(
        parse(from_os_str),
        short = "i",
        long = "input",
        default_value = "eventlog.tfrecord.gz"
    )]
    eventlog: PathBuf,

    /// Maximum number of implementations to consider
    #[structopt(long = "limit")]
    limit: Option<usize>,
}

impl Stats {
    fn run(&self, _args: &Opt) -> io::Result<()> {
        let (mut nimpl, mut impld) = (0, 0u64);

        #[derive(Copy, Clone, Debug, PartialEq, Eq, Hash)]
        enum Cause {
            Constraints,
            PerfModel,
            Backtrack,
        };

        impl From<mcts::CauseOfDeath> for Cause {
            fn from(cause: mcts::CauseOfDeath) -> Self {
                match cause {
                    mcts::CauseOfDeath::Constraints => Cause::Constraints,
                    mcts::CauseOfDeath::PerfModel { .. } => Cause::PerfModel,
                    mcts::CauseOfDeath::Backtrack => Cause::Backtrack,
                }
            }
        }

        let mut deadinfo = HashMap::new();

        let mut evalns = self.limit.map(Vec::with_capacity).unwrap_or_default();
        let mut tree = CandidateTree::new();

        for record_bytes in EventLog::open(&self.eventlog)?.records() {
            match bincode::deserialize(&record_bytes?)
                .map_err(|err| io::Error::new(io::ErrorKind::Other, err))?
            {
                mcts::Message::Node {
                    id,
                    parent,
                    mut children,
                    bound,
                    discovery_time,
                } => tree.extend(id, discovery_time, parent, bound, &mut children),
                mcts::Message::Trace { events, .. } => {
                    let mut cause = None;
                    let mut len = 0;
                    let mut node = tree.get_root();
                    let mut has_size = false;

                    for event in &events {
                        match event.value {
                            mcts::Event::SelectNode(id) => {
                                node = tree.get_node(id);
                            }
                            mcts::Event::SelectChild(index, ..) => {
                                node = node
                                    .child(index.into())
                                    .unwrap_or_else(|| panic!("no child"));
                                if let Action::Action(
                                    telamon::search_space::Action::Size(..),
                                ) =
                                    node.action().unwrap_or_else(|| panic!("no action"))
                                {
                                    has_size = true
                                }
                                len += 1;
                            }
                            mcts::Event::KillChild(_index, cause_) => {
                                let info = deadinfo
                                    .entry((Cause::from(cause_), has_size))
                                    .or_insert((0u64, 0u32));
                                info.0 += len + 1;
                                info.1 += 1;
                            }
                            mcts::Event::Kill(cause_) => {
                                assert!(cause.is_none());

                                cause = Some(Cause::from(cause_));
                            }
                            mcts::Event::Implementation => {
                                assert!(cause.is_none());

                                impld += len;
                                nimpl += 1;
                            }
                            mcts::Event::Expand => (),
                        }
                    }

                    if let Some(cause) = cause {
                        let info =
                            deadinfo.entry((cause, has_size)).or_insert((0u64, 0u32));
                        info.0 += len;
                        info.1 += 1;
                    }
                }
                mcts::Message::Evaluation { value, .. } => {
                    if let Some(value) = value {
                        evalns.push(value.log(10.));
                    }
                }
            }

            if self.limit.map(|limit| nimpl >= limit).unwrap_or(false) {
                break;
            }
        }

        let stats = stats::OnlineStats::from_slice(&evalns);
        println!(
            "Average log10 runtime: {:.2} (± {:.2})",
            stats.mean(),
            stats.stddev(),
        );

        println!(
            "Implementations: {} (avg depth: {})",
            nimpl,
            impld as f64 / nimpl as f64
        );

        let ((ddepth, ndead), (ddepth_size, ndead_size)) = deadinfo.iter().fold(
            ((0, 0), (0, 0)),
            |((ddepth, ndead), (ddepth_size, ndead_size)),
             ((_, has_size), (depth, num))| {
                if *has_size {
                    ((ddepth, ndead), (ddepth_size + depth, ndead_size + num))
                } else {
                    ((ddepth + depth, ndead + num), (ddepth_size, ndead_size))
                }
            },
        );

        println!(
            "Deadends: {} (avg depth: {})",
            ndead + ndead_size,
            (ddepth + ddepth_size) as f64 / f64::from(ndead + ndead_size)
        );

        for ((cause, has_size), (cdepth, cnum)) in deadinfo.into_iter() {
            println!(
                "  - {:?} ({}): {} (avg depth: {})",
                cause,
                if has_size {
                    " (with size)"
                } else {
                    " (without size)"
                },
                cnum,
                cdepth as f64 / f64::from(cnum)
            );
        }

        Ok(())
    }
}

#[derive(StructOpt)]
enum Command {
    #[structopt(name = "benchmark")]
    Benchmark(Benchmark),

    #[structopt(name = "codegen")]
    Codegen(Codegen),

    #[structopt(name = "rebuild")]
    Rebuild(Rebuild),

    #[structopt(name = "bounds")]
    Bounds(Bounds),

    #[structopt(name = "stats")]
    Stats(Stats),

    #[structopt(name = "bound")]
    Bound(ComputeBound),
}

#[derive(StructOpt)]
#[structopt(name = "telamon")]
struct Opt {
    #[structopt(subcommand)]
    command: Command,
}

fn main() {
    let args = Opt::from_args();
    env_logger::init();

    let result = match &args.command {
        Command::Benchmark(benchmark) => benchmark.run(&args),
        Command::Codegen(codegen) => codegen.run(&args),
        Command::Rebuild(rebuild) => rebuild.run(&args),
        Command::Bounds(bounds) => bounds.run(&args),
        Command::Stats(stats) => stats.run(&args),
        Command::Bound(bound) => bound.run(&args),
    };

    match result {
        Ok(()) => (),
        Err(err) => panic!("An error occured: {}", err),
    }
}
