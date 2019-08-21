//! Benchmarks the exploration on CUDA gpus.
use std::io::{self, Write};

use telamon::device::{ArgMap, Context};
use telamon::explorer::config::Config;
use telamon::helper::MemInit;
use telamon_cli::{
    Bench, CommonOpt, ContextBuilder, CublasHandle, KernelParam, Reference,
};
use telamon_kernels::statistics::estimate_mean;
use telamon_kernels::{linalg, Kernel};

use structopt::StructOpt;

/// The number of times to run the generated code to evaluate its performance.
const NUM_CODE_RUNS: usize = 40;
/// Search timeout in minutes.
const TIMEOUT: u64 = 240;

/// Benchmarks a kernel against a reference implementation.
fn benchmark<'a, K, REF, CB>(
    mut config: Config,
    params: K::Parameters,
    executor: CB,
    reference: &REF,
) where
    K: Kernel<'a>,
    CB: ContextBuilder<'a>,
    REF: Reference<'a, K, Context = CB::Context>,
{
    config.timeout.get_or_insert(TIMEOUT);

    let mut context = executor.build_context();
    let runtime = K::benchmark(
        &config,
        params.clone(),
        NUM_CODE_RUNS,
        MemInit::RandomFill,
        &mut context,
    );
    let ref_runtime = Bench::default()
        .warmup(4)
        .runs(NUM_CODE_RUNS)
        .benchmark_fn(|| reference.eval_reference(&params, &context));
    let mut f =
        std::fs::File::create(config.output_path("benchmark.txt").unwrap()).unwrap();
    writeln!(f, "runtimes: {:?}", runtime).unwrap();
    let mean = estimate_mean(runtime, 0.95, "ns");
    let ref_mean = estimate_mean(ref_runtime, 0.95, "ns");
    writeln!(
        f,
        "{}: {}, reference: {}, speedup: {:.2}",
        K::name(),
        mean,
        ref_mean,
        ref_mean.value / mean.value
    )
    .unwrap();
}

#[derive(StructOpt)]
struct Opt {
    #[structopt(flatten)]
    common: CommonOpt,

    #[structopt(short = "r", long = "repeat", default_value = "10")]
    repeat: usize,

    #[structopt(short = "k", long = "kernel")]
    kernels: Vec<KernelParam>,
}

trait BenchRun<'a, B, R> {
    fn run(self, config: &Config, builder: B, reference: &R);
}

struct Benchmark<'a, K>
where
    K: Kernel<'a>,
{
    params: K::Parameters,
    name: String,
    iteration: usize,
}

impl<'a, K> Benchmark<'a, K>
where
    K: Kernel<'a>,
{
    fn new(params: K::Parameters, name: String, iteration: usize) -> Self {
        Benchmark {
            params,
            name,
            iteration,
        }
    }

    fn output_dir(&self) -> String {
        format!("{}/{}", self.name, self.iteration)
    }
}

impl<'a, K, B, R> BenchRun<'a, B, R> for Benchmark<'a, K>
where
    K: Kernel<'a>,
    B: ContextBuilder<'a>,
    R: Reference<'a, K, Context = B::Context>,
{
    fn run(self, config: &Config, builder: B, reference: &R) {
        let mut config = config.clone();
        config.output_dir = std::path::Path::new(&config.output_dir)
            .join(self.output_dir())
            .to_str()
            .unwrap()
            .to_string();
        benchmark::<K, _, _>(config.clone(), self.params, builder, reference)
    }
}

macro_rules! benchmark {
    (Sgemm($m:expr, $n:expr, $k:expr)[$iter:expr]) => {{
        self::Benchmark::<'_, linalg::FusedMM<'_, f32>>::new(
            linalg::FusedMMP::new($m, $n, $k),
            format!("Sgemm_{}_{}_{}", $m, $n, $k),
            $iter,
        )
    }};

    (BatchMM($b:expr, $m:expr, $n:expr, $k:expr)[$iter:expr]) => {{
        self::Benchmark::<'_, linalg::BatchMM<'_, f32>>::new(
            linalg::BatchMMP::new($b, $m, $n, $k),
            format!("BatchMM_{}_{}_{}_{}", $b, $m, $n, $k),
            $iter,
        )
    }};
}

fn main() {
    env_logger::init();
    let args = Opt::from_args();

    let executor = telamon_cuda::Executor::init();
    let reference = CublasHandle::new();

    let config = args.common.config().unwrap();

    for idx in 0..args.repeat {
        for kernel in &args.kernels {
            use KernelParam::*;

            match *kernel {
                Axpy { n } => Benchmark::<'_, linalg::Axpy<f32>>::new(
                    (n, true),
                    format!("Axpy_{}", n),
                    idx,
                )
                .run(&config, &executor, &reference),
                MatVec { m, n } => Benchmark::<'_, linalg::MatVec<f32>>::new(
                    (m, n, true),
                    format!("Sgemv_{}_{}", m, n),
                    idx,
                )
                .run(&config, &executor, &reference),
                Gesummv { m, n } => Benchmark::<'_, linalg::Gesummv<f32>>::new(
                    (m, n, true),
                    format!("Gesummv_{}_{}", m, n),
                    idx,
                )
                .run(&config, &executor, &reference),
                Gemm { m, n, k } => Benchmark::<'_, linalg::FusedMM<'_, f32>>::new(
                    linalg::FusedMMP::new(m, n, k),
                    format!("Sgemm_{}_{}_{}", m, n, k),
                    idx,
                )
                .run(&config, &executor, &reference),
                BatchMM { b, m, n, k } => Benchmark::<'_, linalg::BatchMM<'_, f32>>::new(
                    linalg::BatchMMP::new(b, m, n, k),
                    format!("BatchMM_{}_{}_{}_{}", b, m, n, k),
                    idx,
                )
                .run(&config, &executor, &reference),
                ResNetCell { m, n, k, activation_fun } => Benchmark::<'_, linalg::ResNetCell<'_, f32>>::new(
                    linalg::ResNetCellP::new(m, n, k).activation_fun(activation_fun),
                    format!("ResNetCell_{}_{}_{}_{}", m, n, k, telamon_kernels::linalg::compose::ActivationFunction::opt_to_display(&activation_fun)),
                    idx,
                )
                .run(&config, &executor, &reference),
                ResNetCellTopHalf { m, n, k, activation_fun } => Benchmark::<'_, linalg::ResNetCellTopHalf<'_, f32>>::new(
                    linalg::ResNetCellTopHalfP::new(m, n, k, activation_fun),
                    format!("ResNetCellTopHalf_{}_{}_{}_{}", m, n, k, telamon_kernels::linalg::compose::ActivationFunction::opt_to_display(&activation_fun)),
                    idx,
                )
                .run(&config, &executor, &reference),
                ResNetCellBottomHalf { m, n, k, activation_fun } => Benchmark::<'_, linalg::ResNetCellBottomHalf<'_, f32>>::new(
                    linalg::ResNetCellBottomHalfP::new(m, n, k, activation_fun),
                    format!("ResNetCellBottomHalf_{}_{}_{}_{}", m, n, k, telamon_kernels::linalg::compose::ActivationFunction::opt_to_display(&activation_fun)),
                    idx,
                )
                .run(&config, &executor, &reference),
                TransformerCell { m, n, p, r } => Benchmark::<'_, linalg::TransformerCell<'_, f32>>::new(
                    linalg::TransformerCellP::new(m, n, p, r),
                    format!("TransformerCell_{}_{}_{}_{}", m, n, p, r),
                    idx,
                )
                .run(&config, &executor, &reference),
                TransformerCellTopHalf { m, n, p } => Benchmark::<'_, linalg::TransformerCellTopHalf<'_, f32>>::new(
                    linalg::TransformerCellTopHalfP::new(m, n, p),
                    format!("TransformerCellTopHalf_{}_{}_{}", m, n, p),
                    idx,
                )
                .run(&config, &executor, &reference),
                TransformerCellBottomHalf { m, n, r } => Benchmark::<'_, linalg::TransformerCellBottomHalf<'_, f32>>::new(
                    linalg::TransformerCellBottomHalfP::new(m, n, r),
                    format!("TransformerCellBottomHalf_{}_{}_{}", m, n, r),
                    idx,
                )
                .run(&config, &executor, &reference),
            }
        }
    }
}
