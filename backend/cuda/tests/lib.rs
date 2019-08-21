#![cfg(feature = "cuda")]
use itertools::Itertools;
use log::debug;

use telamon::device::{ArrayArgumentExt, Context, EvalMode};
use telamon::search_space::*;
use telamon::{codegen, explorer};
use telamon::{helper, ir};
use telamon_cuda as cuda;

/// Find the best candidate for a function and outputs it.
pub fn gen_best(context: &Context, space: SearchSpace) {
    let mut config = explorer::Config::from_settings_toml();
    config.num_workers = 1;
    let best = explorer::find_best(&config, context, vec![space], None).unwrap();
    context.device().gen_code(&best, &mut std::io::sink());
}

/// Checks the result of all valid candidates.
pub fn check_candidates<F>(space: SearchSpace, ctx: &Context, mut check: F)
where
    F: FnMut(),
{
    explorer::gen_space(
        ctx,
        space,
        |_| (),
        |candidate| {
            debug!("testing candidate with actions {:?}", candidate.actions);
            let fun = codegen::Function::build(&candidate.space);
            ctx.evaluate(&fun, EvalMode::FindBest).unwrap();
            check();
        },
    );
}

/// Tests the printing of unrolled dimensions.
#[test]
fn unrolled_dims() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let context = cuda::Context::new(&executor);
    let signature = ir::Signature::new("empty");
    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_64 = builder.cst_size(64);
    let d0 = builder.open_dim_ex(size_64, DimKind::UNROLL);
    let i0 = builder.mov(&d0);
    builder.mov(&i0);
    gen_best(&context, builder.get());
}

/// Ensures thread an block dimension is correctly printed.
#[test]
fn block_thread_dims() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let context = cuda::Context::new(&executor);
    let signature = ir::Signature::new("empty");
    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_64 = builder.cst_size(64);
    let d0 = builder.open_dim_ex(size_64.clone(), DimKind::BLOCK);
    let d1 = builder.open_dim_ex(size_64, DimKind::THREAD);
    builder.mov(&d0);
    builder.mov(&d1);
    gen_best(&context, builder.get());
}

/// Ensure parameters are correctly printed.
#[test]
fn params() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let mut context = cuda::Context::new(&executor);
    let signature = {
        let mut builder = helper::SignatureBuilder::new("params", &mut context);
        builder.scalar("a", 42);
        builder.get()
    };
    let mut builder = helper::Builder::new(signature.into(), context.device());
    builder.mov(&"a");
    gen_best(&context, builder.get());
}

/// Ensure contexts are correcly created and dropped.
#[test]
fn context() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let mut context = cuda::Context::new(&executor);
    let mut builder = helper::SignatureBuilder::new("params", &mut context);
    builder.scalar("scalar", 42);
    builder.array::<i32>("array", 1024);
}

/// Ensure cache directives are working properly.
#[test]
fn cache_directive() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let mut context = cuda::Context::new(&executor);
    let signature = {
        let mut builder = helper::SignatureBuilder::new("params", &mut context);
        builder.array::<f32>("a", 1);
        builder.get()
    };
    let mut builder = helper::Builder::new(signature.into(), context.device());
    let pattern = ir::AccessPattern::Unknown(None);
    builder.ld_ex(
        ir::Type::F(32),
        &"a",
        pattern.clone(),
        InstFlag::CACHE_GLOBAL,
    );
    builder.st_ex(&"a", &0i32, true, pattern, InstFlag::CACHE_GLOBAL);
    gen_best(&context, builder.get());
}

/// Tests code generation when a syncthread is insterted just after a reduction init.
#[test]
fn thread_reduction_map() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let context = cuda::Context::new(&executor);
    let signature = ir::Signature::new("empty");
    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_32 = builder.cst_size(32);
    let d0 = builder.open_dim_ex(size_32, DimKind::THREAD);
    let init = builder.mov(&0f32);
    builder.open_mapped_dim(&d0);
    let cst_size_2 = builder.cst_size(2);
    builder.open_dim_ex(cst_size_2, DimKind::LOOP);
    builder.add(&helper::Reduce(init), &1f32);

    gen_best(&context, builder.get());
}

/// Tests the ordering of instructions with dimensions.
#[test]
fn inst_order() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let context = cuda::Context::new(&executor);
    let signature = ir::Signature::new("empty");
    let mut builder = helper::Builder::new(signature.into(), context.device());

    let size_32 = builder.cst_size(32);
    let d0 = builder.open_dim(size_32.clone());
    builder.mov(&0i32);
    builder.mov(&0i32);
    let d1 = builder.open_dim(size_32);
    builder.mov(&0i32);

    builder.action(Action::DimKind(d0[0], DimKind::LOOP));
    builder.action(Action::DimKind(d1[0], DimKind::THREAD));

    gen_best(&context, builder.get());
}

/// Tests the generation of code for induction variables.
#[test]
fn induction_var_nested() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let mut context = cuda::Context::new(&executor);
    let (k, k4, out);
    let signature = {
        let mut builder = helper::SignatureBuilder::new("ind_var_test", &mut context);
        k = builder.max_size("k", 12);
        k4 = builder.max_size("k4", 12 / 4);
        out = builder.array::<i32>("out", 1);
        builder.get()
    };
    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_1 = builder.cst_size(1);
    let size_4 = builder.cst_size(4);
    let size_5 = builder.cst_size(5);
    let size_k = k.to_ir_size(&builder);
    let size_k_tile_4 = k4.to_ir_size(&builder);
    let d0 = builder.open_dim_ex(size_k_tile_4.clone(), DimKind::LOOP);
    let d1 = builder.open_dim_ex(size_4, DimKind::LOOP);
    let d2 = builder.open_dim_ex(size_5, DimKind::UNROLL);
    let ind_var = builder.induction_var(
        &0i32,
        vec![(&d0, size_1), (&d1, size_k_tile_4), (&d2, size_k)],
    );
    let pattern = ir::AccessPattern::Unknown(None);
    let _ = builder.st(&"out", &ind_var, pattern);

    check_candidates(builder.get(), &context, || {
        let res = out.as_ref().read::<i32>();
        // 1*(k/4 - 1) + (k/4)*(4 - 1) + k*(5 - 1) = 5*k - 1 = 59
        assert_eq!(res[0], 59);
    });
}

/// Tests the generation of code for a single level of induction variable.
#[test]
fn induction_var_simple() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let mut context = cuda::Context::new(&executor);
    let out;
    let signature = {
        let mut builder = helper::SignatureBuilder::new("ind_var_test", &mut context);
        out = builder.array::<i32>("out", 1);
        builder.get()
    };
    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_3 = builder.cst_size(3);
    let size_4 = builder.cst_size(4);
    let d0 = builder.open_dim_ex(size_3, DimKind::LOOP);
    let ind_var = builder.induction_var(&0i32, vec![(&d0, size_4)]);
    let pattern = ir::AccessPattern::Unknown(None);
    let _ = builder.st(&"out", &ind_var, pattern);

    check_candidates(builder.get(), &context, || {
        let res = out.as_ref().read::<i32>();
        assert_eq!(res[0], 8);
    });
}

/// Tries to perform a vectorized load from global memory.
#[test]
fn global_vector_load() {
    const DATA_TYPE: ir::Type = ir::Type::I(32);
    const D0_LEN: u32 = 4;

    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let (input, output);

    let mut context = cuda::Context::new(&executor);
    let signature = {
        let mut builder = helper::SignatureBuilder::new("sgemm", &mut context);
        input = builder.array::<i32>("input", D0_LEN as usize);
        output = builder.array::<i32>("output", 1);
        builder.get()
    };
    input
        .as_ref()
        .write(&(0..D0_LEN).map(|i| i as i32 + 10).collect_vec()[..]);

    let mut builder = helper::Builder::new(signature.into(), context.device());
    // Load B from global memory
    let d0_size = builder.cst_size(D0_LEN);
    let d0 = builder.open_dim_ex(d0_size, DimKind::VECTOR);
    let (addr, input_pattern) = builder.tensor_access(&"input", None, DATA_TYPE, &[&d0]);
    let ld = builder.ld_ex(DATA_TYPE, &addr, input_pattern, InstFlag::NO_CACHE);
    builder.close_dim(&d0);
    // Store B in shared memory.
    let output_pattern = ir::AccessPattern::Unknown(None);
    builder.st_ex(&"output", &ld, true, output_pattern, InstFlag::NO_CACHE);

    check_candidates(builder.get(), &context, || {
        let res = output.as_ref().read::<i32>()[0];
        assert_eq!(res, 13);
    });
}

/// Test induction variables that requires a size cast.
#[test]
fn size_cast() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let out;
    let mut context = cuda::Context::new(&executor);
    let signature = {
        let mut builder = helper::SignatureBuilder::new("ind_var_test", &mut context);
        out = builder.array::<i64>("out", 1);
        builder.get()
    };
    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_3 = builder.cst_size(3);
    let size_4 = builder.cst_size(4);
    let d0 = builder.open_dim_ex(size_3, DimKind::LOOP);
    let ind_var = builder.induction_var(&0i64, vec![(&d0, size_4)]);
    let pattern = ir::AccessPattern::Unknown(None);
    let _ = builder.st(&"out", &ind_var, pattern);

    check_candidates(builder.get(), &context, || {
        let res = out.as_ref().read::<i64>();
        assert_eq!(res[0], 8);
    });
}

/// Test Telamon on a code that used to fail in the performance model.
#[test]
fn perf_model_0() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let mut context = cuda::Context::new(&executor);
    let n;
    let signature = {
        let mut builder = helper::SignatureBuilder::new("test", &mut context);
        n = builder.max_size("n", 16);
        builder.array::<i32>("input", 1024 * 1024);
        builder.get()
    };

    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_16 = builder.cst_size(16);
    let n_tiled = n.to_ir_size(&builder);

    let _d0 = builder.open_dim_ex(n_tiled.clone(), DimKind::LOOP);
    let d1 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);
    let d2 = builder.open_dim_ex(size_16.clone(), DimKind::UNROLL);
    let _ = builder.mov(&0f32);
    builder.close_dim(&d1);
    builder.close_dim(&d2);
    let _d3 = builder.open_dim_ex(n_tiled, DimKind::LOOP);
    let _d4 = builder.open_dim_ex(size_16, DimKind::UNROLL);
    let _ = builder.mov(&0f32);

    check_candidates(builder.get(), &context, || ());
}

/// Three merged loop nests.
#[test]
fn merge_0() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let context = cuda::Context::new(&executor);
    let signature = ir::Signature::new("empty");

    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_16 = builder.cst_size(16);

    let d0 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);
    builder.mov(&0f32);
    builder.close_dim(&d0);

    let d1 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);
    builder.mov(&0f32);
    builder.close_dim(&d1);

    let d2 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);
    builder.mov(&0f32);
    builder.close_dim(&d2);

    builder.order(&d0, &d1, Order::MERGED);
    builder.order(&d1, &d2, Order::MERGED);

    check_candidates(builder.get(), &context, || ());
}

/// Two merge loop nest, with a third dimension that is either merged or outer.
#[test]
fn merge_1() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let context = cuda::Context::new(&executor);
    let signature = ir::Signature::new("empty");

    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_16 = builder.cst_size(16);

    let d0 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);
    builder.mov(&0f32);
    builder.close_dim(&d0);

    let d1 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);
    builder.mov(&0f32);
    builder.close_dim(&d1);

    let d2 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);

    builder.order(&d1, &d2, Order::OUTER | Order::MERGED);
    builder.order(&d0, &d1, Order::MERGED);

    check_candidates(builder.get(), &context, || ());
}

#[test]
fn dim_map_reduce_0() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let context = cuda::Context::new(&executor);
    let signature = ir::Signature::new("empty");

    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_16 = builder.cst_size(16);

    let inst0 = builder.mov(&0f32);

    let _d0 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);
    let d1 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);
    let inst1 = builder.mov(&0f32);
    builder.close_dim(&d1);

    let d2 = builder.open_dim_ex(size_16.clone(), DimKind::LOOP);
    let op = builder.dim_map(inst1, &[(&d1, &d2)], ir::DimMapScope::Global(()));
    let inst2 = builder.add(&op, &helper::Reduce(inst0));

    builder.order(&inst2, &d1, Order::AFTER);

    check_candidates(builder.get(), &context, || ());
}

#[test]
fn dim_map_active() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let context = cuda::Context::new(&executor);
    let signature = ir::Signature::new("empty");

    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_32 = builder.cst_size(32);

    let d0 = builder.open_dim_ex(size_32.clone(), DimKind::THREAD);
    let d1 = builder.open_dim_ex(size_32.clone(), DimKind::THREAD);
    let a = builder.mov(&0f32);
    builder.close_dim(&d0);
    builder.close_dim(&d1);

    let d2 = builder.open_dim_ex(size_32.clone(), DimKind::THREAD);
    let _d3 = builder.open_dim_ex(size_32.clone(), DimKind::THREAD);
    let d4 = builder.open_dim_ex(size_32.clone(), DimKind::UNROLL);
    let op = builder.dim_map(a, &[(&d0, &d2), (&d1, &d4)], ir::DimMapScope::Global(()));
    builder.mov(&op);

    gen_best(&context, builder.get());
}

#[test]
fn test0() {
    let _ = env_logger::try_init();
    let executor = cuda::Executor::init();
    let context = cuda::Context::new(&executor);
    let signature = ir::Signature::new("empty");

    let mut builder = helper::Builder::new(signature.into(), context.device());
    let size_32 = builder.cst_size(32);

    let d0 = builder.open_dim_ex(size_32.clone(), DimKind::THREAD);
    let d1 = builder.open_dim(size_32.clone());
    let i0 = builder.mov(&0f32);
    builder.close_dim(&d0);
    builder.close_dim(&d1);

    let _d2 = builder.open_dim_ex(size_32.clone(), DimKind::THREAD);
    let d3 = builder.open_dim_ex(size_32.clone(), DimKind::THREAD);
    let d4 = builder.open_dim_ex(size_32.clone(), DimKind::SEQUENTIAL);
    let op = builder.dim_map(i0, &[(&d0, &d3), (&d1, &d4)], ir::DimMapScope::Global(()));
    let i1 = builder.mov(&op);

    builder.order(&i1, &d0, Order::AFTER);
    builder.order(&i1, &d1, Order::AFTER);

    let mut space = builder.get();
    space
        .apply_decisions(vec![Action::DimKind(d4[0], DimKind::UNROLL)])
        .unwrap();
}
