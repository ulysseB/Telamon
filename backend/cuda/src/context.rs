use crate::api::Argument;
use crate::kernel::Thunk;
use crate::{Executor, Gpu, JITDaemon, Kernel};
///! Defines the CUDA evaluation context.
use crossbeam;
use itertools::{process_results, Itertools};
use log::{debug, info};
use std::f64;
use std::sync::{atomic, mpsc, Arc};
use telamon::device::{self, AsyncCallback, Device, EvalMode, ScalarArgument};
use telamon::{codegen, ir, search_space::Candidate};
use utils::*;

/// Max number of candidates waiting to be evaluated.
const EVAL_BUFFER_SIZE: usize = 100;
// TODO(perf): enable optimizations when possible
const JIT_OPT_LEVEL: usize = 2;

/// Candidates with a runtime above `SKIP_THRESHOLD * cut` are skipped after the first
/// evaluation.
const SKIP_THRESHOLD: f64 = 3.;
// FIXME: tune values + add a second threshold after a few iterations
/// Number of evaluations of perform on each candidate.
const NUM_EVALS: usize = 20;
/// Number of outlier evaluations to discard.
const NUM_OUTLIERS: usize = 4;

/// A CUDA evaluation context.
pub struct Context<'a> {
    gpu_model: Gpu,
    executor: &'a Executor,
    parameters: FnvHashMap<String, Arc<Argument + 'a>>,
}

impl<'a> Context<'a> {
    /// Create a new evaluation context. The GPU model if infered.
    pub fn new(executor: &'a Executor) -> Context {
        Context {
            gpu_model: Gpu::from_executor(executor),
            executor,
            parameters: FnvHashMap::default(),
        }
    }

    /// Creates a context from the given GPU.
    pub fn from_gpu(gpu: Gpu, executor: &'a Executor) -> Self {
        Context {
            gpu_model: gpu,
            executor,
            parameters: FnvHashMap::default(),
        }
    }

    /// Returns the GPU description.
    pub fn gpu(&self) -> &Gpu {
        &self.gpu_model
    }

    /// Returns the execution queue.
    pub fn executor(&self) -> &'a Executor {
        self.executor
    }

    /// Returns a parameter given its name.
    pub fn get_param(&self, name: &str) -> &Argument {
        self.parameters[name].as_ref()
    }

    /// Binds a parameter to the gien name.
    pub fn bind_param(&mut self, name: String, arg: Arc<Argument + 'a>) {
        self.parameters.insert(name, arg);
    }

    /// Returns the optimization level to use.
    fn opt_level(mode: EvalMode) -> usize {
        match mode {
            EvalMode::TestBound => 1,
            EvalMode::FindBest | EvalMode::TestEval => JIT_OPT_LEVEL,
        }
    }

    /// Evaluates `thunk` multiple times to obtain accurate execution times.
    fn eval_runtime(
        &self,
        thunk: &Thunk,
        bound: f64,
        current_best: f64,
        mode: EvalMode,
    ) -> Result<f64, ()> {
        if bound >= current_best && mode.skip_bad_candidates() {
            info!("candidate skipped because of its bound");
            return Ok(std::f64::INFINITY);
        }
        let t0 = self.ticks_to_ns(thunk.execute()?);
        if mode.skip_bad_candidates() && t0 * SKIP_THRESHOLD >= bound {
            info!("candidate skipped after its first evaluation");
            return Ok(t0);
        }
        // Avoid spending too much time on very slow candidates.
        let num_evals = std::cmp::max(1, std::cmp::min(NUM_EVALS, (1.0e9 / t0) as usize));
        let num_samples = std::cmp::max(1, num_evals.saturating_sub(NUM_OUTLIERS));
        // TODO(cc_perf): becomes the limiting factor after a few hours. We should stop
        // earlier and make tests to know when (for example, measure the MAX delta between
        // min and median with N outliers).
        let runtimes = (0..num_evals).map(|_| thunk.execute());
        let runtimes_by_value =
            process_results(runtimes, |iter| iter.sorted())?.collect_vec();
        let median = self.ticks_to_ns(runtimes_by_value[num_evals / 2]);
        let runtimes_by_delta = runtimes_by_value
            .into_iter()
            .map(|t| self.ticks_to_ns(t))
            .sorted_by(|lhs, rhs| cmp_f64((lhs - median).abs(), (rhs - median).abs()))
            .collect_vec();
        let average = runtimes_by_delta[..num_samples]
            .iter()
            .cloned()
            .sum::<f64>()
            / num_samples as f64;
        Ok(average)
    }

    /// Converts a number of clock ticks into a number of nanoseconds.
    fn ticks_to_ns(&self, ticks: u64) -> f64 {
        ticks as f64 / self.gpu_model.smx_clock
    }
}

impl<'a> device::ArgMap<'a> for Context<'a> {
    fn bind_erased_scalar(
        &mut self,
        param: &ir::Parameter,
        value: Box<dyn ScalarArgument>,
    ) {
        assert_eq!(param.t, value.get_type());
        self.bind_param(param.name.clone(), Arc::new(value));
    }

    fn bind_erased_array(
        &mut self,
        param: &ir::Parameter,
        t: ir::Type,
        len: usize,
    ) -> Arc<dyn device::ArrayArgument + 'a> {
        let size = len * unwrap!(t.len_byte()) as usize;
        let array = Arc::new(self.executor.allocate_array::<i8>(size));
        self.bind_param(param.name.clone(), array.clone());
        array
    }
}

impl<'a> device::Context for Context<'a> {
    fn device(&self) -> &Device {
        &self.gpu_model
    }

    fn param_as_size(&self, name: &str) -> Option<u32> {
        self.get_param(name).as_size()
    }

    fn evaluate(&self, function: &codegen::Function, mode: EvalMode) -> Result<f64, ()> {
        let gpu = &self.gpu_model;
        let kernel = Kernel::compile(function, gpu, self.executor, Self::opt_level(mode));
        kernel
            .evaluate(self)
            .map(|t| t as f64 / self.gpu_model.smx_clock)
    }

    fn benchmark(&self, function: &codegen::Function, num_samples: usize) -> Vec<f64> {
        let gpu = &self.gpu_model;
        let kernel = Kernel::compile(function, gpu, self.executor, 4);
        kernel.evaluate_real(self, num_samples)
    }

    fn async_eval<'b, 'c>(
        &self,
        num_workers: usize,
        mode: EvalMode,
        inner: &(Fn(&mut device::AsyncEvaluator<'b, 'c>) + Sync),
    ) {
        // Setup the evaluator.
        let blocked_time = &atomic::AtomicUsize::new(0);
        let (send, recv) = mpsc::sync_channel(EVAL_BUFFER_SIZE);
        // Correct because the thread handle is not escaped.
        crossbeam::scope(move |scope| {
            // Start the explorer threads.
            for _ in 0..num_workers {
                let mut evaluator = AsyncEvaluator {
                    context: self,
                    sender: send.clone(),
                    ptx_daemon: self.executor.spawn_jit(Self::opt_level(mode)),
                    blocked_time,
                };
                unwrap!(scope
                    .builder()
                    .name("Telamon - Explorer Thread".to_string())
                    .spawn(move |_| inner(&mut evaluator)));
            }
            // Start the evaluation thread.
            let eval_thread_name = "Telamon - GPU Evaluation Thread".to_string();
            let res = scope.builder().name(eval_thread_name).spawn(move |_| {
                let mut best_eval = std::f64::INFINITY;
                while let Ok((candidate, thunk, callback)) = recv.recv() {
                    let bound = candidate.bound.value();
                    let eval = unwrap!(
                        self.eval_runtime(&thunk, bound, best_eval, mode),
                        "evaluation failed for actions {:?}, with kernel {:?}",
                        candidate.actions,
                        &thunk
                    );
                    if eval < best_eval {
                        best_eval = eval;
                    }
                    callback.call(candidate, eval);
                }
            });
            unwrap!(res);
            std::mem::drop(send);
        })
        .unwrap();
        let blocked_time = blocked_time.load(atomic::Ordering::SeqCst);
        info!(
            "Total time blocked on `add_kernel`: {:.4e}ns",
            blocked_time as f64
        );
    }
}

type AsyncPayload<'a, 'b> = (Candidate<'a>, Thunk<'b>, AsyncCallback<'a, 'b>);

pub struct AsyncEvaluator<'a, 'b>
where
    'a: 'b,
{
    context: &'b Context<'b>,
    sender: mpsc::SyncSender<AsyncPayload<'a, 'b>>,
    ptx_daemon: JITDaemon,
    blocked_time: &'b atomic::AtomicUsize,
}

impl<'a, 'b, 'c> device::AsyncEvaluator<'a, 'c> for AsyncEvaluator<'a, 'b>
where
    'a: 'b,
    'c: 'b,
{
    fn add_kernel(
        &mut self,
        candidate: Candidate<'a>,
        callback: device::AsyncCallback<'a, 'c>,
    ) {
        let thunk = {
            let dev_fun = codegen::Function::build(&candidate.space);
            let gpu = &self.context.gpu();
            debug!(
                "compiling kernel with bound {} and actions {:?}",
                candidate.bound, candidate.actions
            );
            // TODO(cc_perf): cuModuleLoadData is waiting the end of any running kernel
            let kernel = Kernel::compile_remote(
                &dev_fun,
                gpu,
                self.context.executor(),
                &mut self.ptx_daemon,
            );
            kernel.gen_thunk(self.context)
        };
        let t0 = std::time::Instant::now();
        unwrap!(self.sender.send((candidate, thunk, callback)));
        let t = std::time::Instant::now() - t0;
        let t_usize = t.as_secs() as usize * 1_000_000_000 + t.subsec_nanos() as usize;
        self.blocked_time
            .fetch_add(t_usize, atomic::Ordering::Relaxed);
    }
}
