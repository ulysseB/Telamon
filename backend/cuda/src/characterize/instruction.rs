//! Microbenchmarks to get the description of each instruction.
use std::sync::Arc;

use crate::characterize::{create_table, gen, math, Table};
use crate::{Context, Executor, Gpu, InstDesc, Kernel, PerfCounter};
use itertools::Itertools;
use log::*;
use num::Zero;
use telamon::codegen;
use telamon::device::{Device, ScalarArgument};
use telamon::helper::tensor::DimSize;
use telamon::ir;
use utils::*;

/// Instruments a single thread with a loop containing chained instructions.
fn inst_chain<T>(
    gpu: &Gpu,
    executor: &Executor,
    counters_list: &[PerfCounter],
    n: u64,
    range: &[u32],
    inst_gen: &gen::InstGenerator,
) -> Table<u64>
where
    T: ScalarArgument + Zero,
{
    let args = [("n", ir::Type::I(32)), ("arg", T::t())];
    let base = Arc::new(gen::base(&args, &["out"], gpu));

    let mut table = create_table(&["n"], counters_list);
    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_scalar("arg", T::zero(), &mut context);
    gen::bind_array::<f32>("out", 1, &mut context);
    gen::bind_scalar("n", n as i32, &mut context);
    let counters = executor.create_perf_counter_set(counters_list);
    let n_size = DimSize::new_param("n", n as u32);
    for &n_chained in range {
        let fun = gen::inst_chain::<T>(
            Arc::clone(&base),
            Arc::<Gpu>::clone(context.gpu()),
            inst_gen,
            &n_size,
            n_chained,
            "arg",
            "out",
        );
        let entry = [u64::from(n_chained)];
        gen::run(&mut context, &fun, &[], &counters, &entry, &mut table);
    }
    table
}

/// Instruments an instruction.
fn inst<T>(gpu: &Gpu, executor: &Executor, inst_gen: &gen::InstGenerator) -> InstDesc
where
    T: ScalarArgument + Zero,
{
    let perf_counters = [PerfCounter::InstExecuted, PerfCounter::ElapsedCyclesSM];
    let range = (10..129).collect_vec();
    let n = 1000;
    let table = inst_chain::<T>(gpu, executor, &perf_counters, n, &range, inst_gen);
    trace!("{}", table.pretty());
    let range_f64 = range.iter().map(|&x| f64::from(x)).collect_vec();
    let insts = table.column(1).map(|x| (x / n) as f64).collect_vec();
    let cycles = table
        .column(2)
        .map(|x| (x / n) as f64 / f64::from(gpu.num_smx))
        .collect_vec();
    let inst_pred = math::LinearRegression::train(&range_f64, &insts);
    let cycle_pred = math::LinearRegression::train(&range_f64, &cycles);
    info!("Number of instructions: {}", inst_pred);
    info!("Number of cycles: {}", cycle_pred);
    InstDesc {
        latency: cycle_pred.slope.round(),
        issue: inst_pred.slope.round(),
        alu: inst_pred.slope.round(),
        ..InstDesc::default()
    }
}

pub fn add_f32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Add f32");
    inst::<f32>(gpu, executor, &|init, arg, b| b.add(init, arg))
}

pub fn add_f64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Add f64");
    inst::<f64>(gpu, executor, &|init, arg, b| b.add(init, arg))
}

pub fn add_i32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Add i32");
    inst::<i32>(gpu, executor, &|init, arg, b| b.add(init, arg))
}

pub fn add_i64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Add i64");
    inst::<i64>(gpu, executor, &|init, arg, b| b.add(init, arg))
}

pub fn mul_f32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Mul f32");
    inst::<f32>(gpu, executor, &|init, arg, b| b.mul(init, arg))
}

pub fn mul_f64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Mul f64");
    inst::<f64>(gpu, executor, &|init, arg, b| b.mul(init, arg))
}

pub fn mul_i32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Mul i32");
    inst::<i32>(gpu, executor, &|init, arg, b| b.mul(init, arg))
}

pub fn mul_i64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Mul i64");
    inst::<i64>(gpu, executor, &|init, arg, b| b.mul(init, arg))
}

pub fn mad_f32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Mad f32");
    inst::<f32>(gpu, executor, &|init, arg, b| b.mad(init, arg, arg))
}

pub fn mad_f64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Mad f64");
    inst::<f64>(gpu, executor, &|init, arg, b| b.mad(init, arg, arg))
}

pub fn mad_i32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Mad i32");
    inst::<i32>(gpu, executor, &|init, arg, b| b.mad(init, arg, arg))
}

pub fn mad_i64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Mad i64");
    inst::<i64>(gpu, executor, &|init, arg, b| b.mad(arg, arg, init))
}

pub fn mad_wide(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Mad wide");
    // FIXME: `n` should not be used here.
    inst::<i64>(gpu, executor, &|init, _, b| b.mad(&"n", &"n", init))
}

pub fn div_f32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Div f32");
    inst::<f32>(gpu, executor, &|init, arg, b| b.div(init, arg))
}

pub fn div_f64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Div f64");
    inst::<f64>(gpu, executor, &|init, arg, b| b.div(init, arg))
}

pub fn div_i32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Div i32");
    inst::<i32>(gpu, executor, &|init, arg, b| b.div(init, arg))
}

pub fn div_i64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Div i64");
    inst::<i64>(gpu, executor, &|init, arg, b| b.div(init, arg))
}

pub fn max_f32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Max f32");
    inst::<f32>(gpu, executor, &|init, arg, b| b.max(init, arg))
}

pub fn max_f64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Max f64");
    inst::<f64>(gpu, executor, &|init, arg, b| b.max(init, arg))
}

pub fn max_i32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Max i32");
    inst::<i32>(gpu, executor, &|init, arg, b| b.max(init, arg))
}

pub fn max_i64(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Max i64");
    inst::<i64>(gpu, executor, &|init, arg, b| b.max(init, arg))
}

pub fn exp_f32(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: Exp f32");
    inst::<f32>(gpu, executor, &|init, _arg, b| b.exp(init))
}

/// Micro-bench a load instruction.
///
/// * `stride` is the stride between accesses in number of `i64`.
/// * `num_load` is the number of different addresses to load from the array.
fn load(gpu: &Gpu, executor: &Executor, stride: u32, num_load: u32) -> f64 {
    let n_chained_range = (10..129).collect_vec();
    let n = std::cmp::max(1000, div_ceil(num_load, 10));

    let array_size = std::cmp::max(num_load * stride, 1) as usize;

    let init_base = Arc::new(gen::base(&[], &["array"], gpu));
    let init_fun = gen::init_stride_array(
        Arc::clone(&init_base),
        Arc::new(gpu.clone()),
        "array",
        num_load,
        stride as i32,
    );
    let init_dev_fun = codegen::Function::build(&init_fun);
    let init_dev_kernel = Kernel::compile(&init_dev_fun, gpu, executor, 1);

    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_array::<i64>("array", array_size as usize, &mut context);
    unwrap!(init_dev_kernel.evaluate(&context));

    let perf_counters = [PerfCounter::InstExecuted, PerfCounter::ElapsedCyclesSM];
    let counters = executor.create_perf_counter_set(&perf_counters);
    let mut table = create_table(&["chain"], &perf_counters);

    let base = Arc::new(gen::base(&[("n", ir::Type::I(32))], &["array", "out"], gpu));
    gen::bind_scalar("n", n as i32, &mut context);
    gen::bind_array::<i64>("out", 1, &mut context);
    let n_size = DimSize::new_param("n", n);
    for &n_chained in &n_chained_range {
        let fun = gen::load_chain(
            Arc::clone(&base),
            Arc::<Gpu>::clone(context.gpu()),
            1,
            &n_size,
            n_chained,
            "array",
            "out",
        );
        let prefix = [u64::from(n_chained)];
        gen::run(&mut context, &fun, &[], &counters, &prefix, &mut table);
    }

    let nf = f64::from(n);
    let range_f64 = n_chained_range.iter().map(|&x| f64::from(x)).collect_vec();
    let cycles = table
        .column(2)
        .map(|&x| x as f64 / (nf * f64::from(gpu.num_smx)))
        .collect_vec();
    let cycle_pred = math::LinearRegression::train(&range_f64, &cycles);
    info!("Number of cycles: {}", cycle_pred);
    cycle_pred.slope.round()
}

pub fn load_ram(gpu: &Gpu, executor: &Executor) -> f64 {
    info!("RAM Load");
    let stride = gpu.l2_cache_line / 8;
    let num_load = 2 * gpu.l2_cache_size / gpu.l2_cache_line;
    load(gpu, executor, stride, num_load)
}

pub fn load_l2(gpu: &Gpu, executor: &Executor) -> f64 {
    info!("L2 Load");
    load(gpu, executor, 1, 1)
}

pub fn load_shared(gpu: &Gpu, executor: &Executor) -> f64 {
    info!("Shared Load");
    let n_chained_range = (10..129).collect_vec();
    let n_iter: i32 = 1000;
    let perf_counters = [PerfCounter::InstExecuted, PerfCounter::ElapsedCyclesSM];
    let counters = executor.create_perf_counter_set(&perf_counters);
    let mut table = create_table(&["chain"], &perf_counters);

    let base = Arc::new(gen::base(&[("n_iter", ir::Type::I(32))], &["out"], gpu));
    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_scalar("n_iter", n_iter, &mut context);
    gen::bind_array::<i64>("out", 1, &mut context);
    let n_size = DimSize::new_param("n_iter", n_iter as u32);
    for &n_chained in &n_chained_range {
        let fun = gen::shared_load_chain(
            Arc::clone(&base),
            Arc::<Gpu>::clone(context.gpu()),
            &n_size,
            n_chained,
            32,
            "out",
        );
        let prefix = [u64::from(n_chained)];
        gen::run(&mut context, &fun, &[], &counters, &prefix, &mut table);
    }

    let nf = f64::from(n_iter);
    let range_f64 = n_chained_range.iter().map(|&x| f64::from(x)).collect_vec();
    let insts = table.column(1).map(|&x| x as f64 / nf).collect_vec();
    let cycles = table
        .column(2)
        .map(|&x| x as f64 / (nf * f64::from(gpu.num_smx)))
        .collect_vec();
    let inst_pred = math::LinearRegression::train(&range_f64, &insts);
    let cycle_pred = math::LinearRegression::train(&range_f64, &cycles);
    info!("Number of instructions: {}", inst_pred);
    info!("Number of cycles: {}", cycle_pred);
    cycle_pred.slope.round()
}

/// Measures the number of L1 cache lines an SMX can fetch.
pub fn smx_bandwidth_l1_lines(gpu: &Gpu, executor: &Executor) -> f64 {
    info!("L1 lines SMX bandwidth");
    let wraps = gpu.max_threads() / gpu.wrap_size;
    let strides = (16..33).collect_vec();
    infer_smx_bandwidth(gpu, executor, wraps, &strides, true)
}

/// Measures the number of L2 cache lines an SMX can fetch.
pub fn smx_read_bandwidth_l2_lines(gpu: &Gpu, executor: &Executor) -> f64 {
    info!("L2 lines SMX read bandwidth");
    let wraps = gpu.max_threads() / gpu.wrap_size;
    let line_len = gpu.l2_cache_line / 4;
    let strides = (1..=line_len).collect_vec();
    let access_per_wrap = f64::from(gpu.wrap_size / line_len);
    infer_smx_bandwidth(gpu, executor, wraps, &strides, true) * access_per_wrap
}

/// Measures the number of L2 cache lines an SMX can fetch.
pub fn smx_write_bandwidth_l2_lines(gpu: &Gpu, executor: &Executor) -> f64 {
    info!("L2 lines SMX write bandwidth");
    let wraps = gpu.max_threads() / gpu.wrap_size;
    let line_len = gpu.l2_cache_line / 4;
    let strides = (1..=line_len).collect_vec();
    let access_per_wrap = f64::from(gpu.wrap_size / line_len);
    infer_smx_bandwidth(gpu, executor, wraps, &strides, false) * access_per_wrap
}

/*/// Measures the number of L1 cache lines a thread can fetch.
pub fn thread_bandwidth_l1_lines(gpu: &Gpu, executor: &Executor) -> f64 {
    info!("L1 lines thread bandwidth");
    let strides = (1..33).collect_vec();
    // FIXME: value is per wrap, change per thread
    infer_smx_bandwidth(gpu, executor, 1, &strides)
}*/

pub fn infer_smx_bandwidth(
    gpu: &Gpu,
    executor: &Executor,
    wraps: u32,
    strides: &[u32],
    bench_reads: bool,
) -> f64 {
    const N: i32 = 100;
    const CHAINED: u32 = 8;
    const UNROLL: u32 = 16;
    let n_values = [10, N + 10];
    // Table: wraps, stride, blocks, n, inst, cycles, replays
    let table = if bench_reads {
        smx_bandwidth(
            gpu,
            executor,
            &[1],
            &n_values,
            CHAINED,
            UNROLL,
            &[wraps],
            strides,
        )
    } else {
        smx_store_bandwidth(
            gpu,
            executor,
            &[1],
            &n_values,
            CHAINED,
            UNROLL,
            &[wraps],
            strides,
        )
    };
    let cycles = table
        .column(5)
        .batching(|it| it.next().map(|n10| it.next().unwrap() - n10))
        .map(|cycles| cycles as f64 / f64::from(gpu.num_smx))
        .collect_vec();
    let l1_access = strides
        .iter()
        .map(|&s| f64::from(s * wraps * N as u32 * CHAINED * UNROLL))
        .collect_vec();
    let cycle_pred = math::LinearRegression::train(&l1_access, &cycles);
    info!("Number of cycles per access: {}", cycle_pred);
    1.0 / cycle_pred.slope
}

/// In-depth analysis of memory accesses bandwidth.
#[allow(clippy::too_many_arguments)]
pub fn smx_bandwidth(
    gpu: &Gpu,
    executor: &Executor,
    blocks: &[i32],
    n: &[i32],
    chained: u32,
    unroll: u32,
    wraps: &[u32],
    strides: &[u32],
) -> Table<u64> {
    const MAX_WRAPS: u32 = 32;
    // This should probably be `l1_cache_sector`; but changing this causes crashes down the line.
    // Since this benchmark is designed for Kepler architectures only (it relies on architectural
    // behavior to distinguish between l1/l2 usage), the values are bogus for other architectures
    // already.  Kepler has `l1_cache_sector == l1_cache_line`.
    //
    // NB: The l1 bandwidth is currently not used in the performance model.
    let array_size =
        gpu.l1_cache_line() / 4 * gpu.wrap_size * chained * unroll * MAX_WRAPS;
    // Setup the results table.
    let perf_counters = [PerfCounter::InstExecuted, PerfCounter::ElapsedCyclesSM];
    let counters = executor.create_perf_counter_set(&perf_counters);
    let mut table = create_table(&["wraps", "stride", "blocks", "n"], &perf_counters);
    // Setup the context
    let scalar_args = [("blocks", ir::Type::I(32)), ("n", ir::Type::I(32))];
    let base = Arc::new(gen::base(&scalar_args, &["array", "out"], gpu));
    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_array::<f32>("array", array_size as usize, &mut context);
    gen::bind_array::<f32>("out", 1, &mut context);
    // Fill the table
    let block_size = DimSize::new_param("blocks", *unwrap!(blocks.last()) as u32);
    let n_size = DimSize::new_param("n", *unwrap!(n.last()) as u32);
    for &num_wraps in wraps {
        assert!(num_wraps <= MAX_WRAPS);
        for &stride in strides {
            let fun = gen::parallel_load(
                Arc::clone(&base),
                Arc::<Gpu>::clone(context.gpu()),
                &block_size,
                &n_size,
                chained,
                unroll,
                num_wraps,
                stride,
                "array",
                "out",
            );
            let params = [u64::from(num_wraps), u64::from(stride)];
            let vars = [("blocks", blocks), ("n", n)];
            gen::run(&mut context, &fun, &vars, &counters, &params, &mut table);
        }
    }
    table
}

/// In-depth analysis of memory stores bandwidth.
#[allow(clippy::too_many_arguments)]
pub fn smx_store_bandwidth(
    gpu: &Gpu,
    executor: &Executor,
    blocks: &[i32],
    n: &[i32],
    chained: u32,
    unroll: u32,
    wraps: &[u32],
    strides: &[u32],
) -> Table<u64> {
    const MAX_WRAPS: u32 = 32;
    // This should probably be `l1_cache_sector`; see the comment in `smx_bandwidth`.
    let array_size =
        gpu.l1_cache_line() / 4 * gpu.wrap_size * chained * unroll * MAX_WRAPS;
    // Setup the results table.
    let perf_counters = [PerfCounter::InstExecuted, PerfCounter::ElapsedCyclesSM];
    let counters = executor.create_perf_counter_set(&perf_counters);
    let mut table = create_table(&["wraps", "stride", "blocks", "n"], &perf_counters);
    // Setup the context
    let scalar_args = [("blocks", ir::Type::I(32)), ("n", ir::Type::I(32))];
    let base = Arc::new(gen::base(&scalar_args, &["array"], gpu));
    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_array::<f32>("array", array_size as usize, &mut context);
    // Fill the table
    let block_size = DimSize::new_param("blocks", *unwrap!(blocks.last()) as u32);
    let n_size = DimSize::new_param("n", *unwrap!(n.last()) as u32);
    for &num_wraps in wraps {
        assert!(num_wraps <= MAX_WRAPS);
        for &stride in strides {
            let fun = gen::parallel_store(
                Arc::clone(&base),
                Arc::<Gpu>::clone(context.gpu()),
                &block_size,
                &n_size,
                chained,
                unroll,
                num_wraps,
                stride,
                "array",
            );
            let params = [u64::from(num_wraps), u64::from(stride)];
            let vars = [("blocks", blocks), ("n", n)];
            gen::run(&mut context, &fun, &vars, &counters, &params, &mut table);
        }
    }
    table
}

#[allow(dead_code)]
pub fn print_load_in_loop(gpu: &Gpu, executor: &Executor) {
    const K: i32 = 1024;
    // Setup the result table.
    let perf_counters = [
        PerfCounter::InstExecuted,
        PerfCounter::ElapsedCyclesSM,
        PerfCounter::GlobalLoadReplay,
    ];
    let counters = executor.create_perf_counter_set(&perf_counters);
    let mut table = create_table(&["threads"], &perf_counters);
    // Setup the context.
    let scalar_args = [("k", ir::Type::I(32))];
    let base = Arc::new(gen::base(&scalar_args, &["out"], gpu));
    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_scalar("k", K, &mut context);
    gen::bind_array::<f32>("out", 32 * 4 * 4, &mut context);
    // Fill the table
    let k_size = DimSize::new_param("k", K as u32);
    for &num_threads in &[1, 2, 4, 8, 16, 32, 64, 128, 256, 512, 1024] {
        let fun = gen::load_in_loop(
            Arc::clone(&base),
            Arc::<Gpu>::clone(context.gpu()),
            &k_size,
            num_threads,
            "out",
        );
        let num_threads = u64::from(num_threads);
        gen::run(
            &mut context,
            &fun,
            &[],
            &counters,
            &[num_threads],
            &mut table,
        );
    }
    let output = ::std::fs::File::create("load_in_loop.csv").unwrap();
    table.pretty().to_csv(output).unwrap();
}

#[allow(dead_code)]
pub fn print_smx_bandwidth(gpu: &Gpu, executor: &Executor) {
    let output = ::std::fs::File::create("smx_bandwidth.csv").unwrap();
    let wraps = [1, 2, 4, 6, 8, 16, 32];
    let strides = (0..33).collect_vec();
    let blocks = [1, gpu.num_smx as i32];
    let table = smx_bandwidth(gpu, executor, &blocks, &[100], 8, 16, &wraps, &strides);
    table.pretty().to_csv(output).unwrap();
}

#[allow(dead_code)]
pub fn print_smx_store_bandwidth(gpu: &Gpu, executor: &Executor) {
    let output = ::std::fs::File::create("smx_store_bandwidth.csv").unwrap();
    let wraps = [1, 2, 4, 6, 8, 16, 32];
    let strides = (0..33).collect_vec();
    let blocks = [1, gpu.num_smx as i32];
    let table =
        smx_store_bandwidth(gpu, executor, &blocks, &[100], 8, 16, &wraps, &strides);
    table.pretty().to_csv(output).unwrap();
}

/// Gets a description of syncthreads overhead.
pub fn syncthread(gpu: &Gpu, executor: &Executor) -> InstDesc {
    info!("Instruction: syncthread");
    let perf_counters = [PerfCounter::InstExecuted, PerfCounter::ElapsedCyclesSM];
    // Set parameters
    let n = 1000;
    let chained_range = (10..129).collect_vec();
    let base = Arc::new(gen::base(&[("n", ir::Type::I(32))], &[], gpu));
    // Setup the execution context
    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_scalar("n", n as i32, &mut context);
    // Generate and evaluate the kernel for different number of chained syncthreads.
    let mut table = create_table(&["n_chained"], &perf_counters);
    let counters = executor.create_perf_counter_set(&perf_counters);
    let n_size = DimSize::new_param("n", n as u32);
    for &n_chained in &chained_range {
        let fun = gen::syncthread(
            Arc::clone(&base),
            Arc::<Gpu>::clone(context.gpu()),
            &n_size,
            n_chained,
            32,
        );
        let entry = [u64::from(n_chained)];
        gen::run(&mut context, &fun, &[], &counters, &entry, &mut table);
    }
    // Infer values from the table.
    trace!("{}", table.pretty());
    let range_f64 = chained_range.iter().map(|&x| f64::from(x)).collect_vec();
    let insts = table.column(1).map(|x| (x / n) as f64).collect_vec();
    let cycles = table
        .column(2)
        .map(|x| (x / n) as f64 / f64::from(gpu.num_smx))
        .collect_vec();
    let inst_pred = math::LinearRegression::train(&range_f64, &insts);
    let cycle_pred = math::LinearRegression::train(&range_f64, &cycles);
    info!("Number of instructions: {}", inst_pred);
    info!("Number of cycles: {}", cycle_pred);
    // Genereate the instruction descrition
    InstDesc {
        latency: cycle_pred.slope.round(),
        issue: inst_pred.slope.round(),
        sync: 1f64,
        ..InstDesc::default()
    }
}

/// Computes the overhead of a loop iteration.
pub fn loop_iter_overhead(gpu: &Gpu, executor: &Executor) -> InstDesc {
    const M: u32 = 1024;
    let n_range = (1..100).map(|i| i * 10).collect_vec();
    // Setup the table.
    info!("Loop iteration overhead");
    let perf_counters = [PerfCounter::InstExecuted, PerfCounter::ElapsedCyclesSM];
    let counters = executor.create_perf_counter_set(&perf_counters);
    let mut table = create_table(&["n"], &perf_counters);
    // Setup the context
    let base = Arc::new(gen::base(
        &[("m", ir::Type::I(32)), ("n", ir::Type::I(32))],
        &[],
        gpu,
    ));
    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_scalar("m", M as i32, &mut context);
    // Fill the table
    let n_size = DimSize::new_param("n", *unwrap!(n_range.last()) as u32);
    let m_size = DimSize::new_param("m", M);
    let fun = gen::two_empty_loops(
        Arc::clone(&base),
        Arc::<Gpu>::clone(context.gpu()),
        &m_size,
        &n_size,
    );
    gen::run(
        &mut context,
        &fun,
        &[("n", &n_range)],
        &counters,
        &[],
        &mut table,
    );
    // Interpret the table
    let range_f64 = n_range.iter().map(|&x| f64::from(x)).collect_vec();
    let insts = table
        .column(1)
        .map(|&x| x as f64 / f64::from(M))
        .collect_vec();
    let cycles = table
        .column(2)
        .map(|&x| x as f64 / f64::from(M * gpu.num_smx))
        .collect_vec();
    let inst_pred = math::LinearRegression::train(&range_f64, &insts);
    let cycle_pred = math::LinearRegression::train(&range_f64, &cycles);
    info!("Number of instructions: {}", inst_pred);
    info!("Number of cycles: {}", cycle_pred);
    // Genereate the instruction descrition
    InstDesc {
        latency: cycle_pred.slope.round(),
        issue: inst_pred.slope.round(),
        alu: 2.0, // An add and a comparison
        ..InstDesc::default()
    }
}

/// Computes the latency overhead at the end of a loop iteration.
pub fn loop_iter_end_latency(gpu: &Gpu, executor: &Executor, add_latency: f64) -> f64 {
    let n_range = (1000..1500).map(|i| i * 100).collect_vec();
    // Setup the table.
    info!("Loop iteration end latency");
    let perf_counters = [PerfCounter::ElapsedCyclesSM];
    let counters = executor.create_perf_counter_set(&perf_counters);
    let mut table = create_table(&["n"], &perf_counters);
    // Setup the context.
    let base = Arc::new(gen::base(&[("n", ir::Type::I(32))], &["out"], gpu));
    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_array::<f32>("out", 1, &mut context);
    // Fill the table.
    let n_size = DimSize::new_param("n", *unwrap!(n_range.last()) as u32);
    let fun = gen::loop_chained_adds(
        Arc::clone(&base),
        Arc::<Gpu>::clone(context.gpu()),
        &n_size,
        10,
        "out",
    );
    gen::run(
        &mut context,
        &fun,
        &[("n", &n_range)],
        &counters,
        &[],
        &mut table,
    );
    // Interpret the table.
    let range_f64 = n_range.iter().map(|&x| f64::from(x)).collect_vec();
    let cycles = table
        .column(1)
        .map(|&x| x as f64 / f64::from(gpu.num_smx))
        .collect_vec();
    let cycle_pred = math::LinearRegression::train(&range_f64, &cycles);
    info!("Number of cycles: {}", cycle_pred);
    // Genereate the instruction descrition
    let latency = cycle_pred.slope.round() - 9.0 * add_latency;
    info!("Latency: {}", latency);
    latency
}

/// Computes the latency overhead at the end of a syncthread.
pub fn syncthread_end_latency(gpu: &Gpu, executor: &Executor, add_latency: f64) -> f64 {
    const N: i32 = 1024;
    let chained_range = (5..26).collect_vec();
    // Setup the table.
    info!("Syncthread end latency");
    let perf_counters = [PerfCounter::ElapsedCyclesSM];
    let counters = executor.create_perf_counter_set(&perf_counters);
    let mut table = create_table(&["chained"], &perf_counters);
    // Setup the context.
    let base = Arc::new(gen::base(&[("n", ir::Type::I(32))], &["out"], gpu));
    let mut context = Context::from_gpu(gpu.clone(), executor);
    gen::bind_scalar("n", N, &mut context);
    gen::bind_array::<f32>("out", 1, &mut context);
    // Fill the table.
    let n_size = DimSize::new_param("n", N as u32);
    for &n_chained in &chained_range {
        let fun = gen::chain_in_syncthread(
            Arc::clone(&base),
            Arc::<Gpu>::clone(context.gpu()),
            &n_size,
            n_chained,
            10,
            32,
            "out",
        );
        let entry = [u64::from(n_chained)];
        gen::run(&mut context, &fun, &[], &counters, &entry, &mut table);
    }
    // Interpret the table.
    let range_f64 = chained_range.iter().map(|&x| f64::from(x)).collect_vec();
    let cycles = table
        .column(1)
        .map(|&x| x as f64 / (f64::from(gpu.num_smx) * f64::from(N)))
        .collect_vec();
    let cycle_pred = math::LinearRegression::train(&range_f64, &cycles);
    info!("Number of cycles: {}", cycle_pred);
    // Genereate the instruction descrition
    let latency = cycle_pred.slope.round() - 9.0 * add_latency;
    info!("Latency: {}", latency);
    latency
}
