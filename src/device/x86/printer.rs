use codegen::*;
use device::x86::Namer;
use ir::{self, op, Type};
use itertools::Itertools;
use search_space::{DimKind, Domain};
use std::fmt::Write as WriteFmt;
use utils::*;
// TODO(cc_perf): avoid concatenating strings.

/// Prints a `Type` for the host.
fn cpu_type(t: &Type) -> &'static str {
    match *t {
        Type::Void => "void",
        //Type::PtrTo(..) => " uint8_t *",
        Type::PtrTo(..) => "intptr_t",
        Type::F(32) => "float",
        Type::F(64) => "double",
        Type::I(1) => "int8_t",
        Type::I(8) => "int8_t",
        Type::I(16) => "int16_t",
        Type::I(32) => "int32_t",
        Type::I(64) => "int64_t",
        ref t => panic!("invalid type for the host: {}", t)
    }
}

fn param_decl(param: &ParamVal, namer: &NameMap) -> String {
    let name = namer.name_param(param.key());
    match param {
        ParamVal::External(_, par_type) => format!("{} {}", cpu_type(par_type), name),
        ParamVal::Size(_) => format!("uint32_t {}", name),
        ParamVal::GlobalMem(_, _, par_type) => format!("{} {}", cpu_type(par_type), name),
    }
    //format!(
    //    "{t} {name}",
    //    t = cpu_type(&param.t()),
    //    name = namer.name_param(param.key()),
    //    )
}

/// Returns the string representing a binary operator.
fn binary_op(op: ir::BinOp) -> &'static str {
    match op {
        ir::BinOp::Add => "+",
        ir::BinOp::Sub => "-",
        ir::BinOp::Div => "/",
    }
}

/// Prints an instruction.
fn inst(inst: &Instruction, namer: &mut NameMap ) -> String {
    //let assignement = format!("{} =", namer.name_inst(inst).to_string());
    match *inst.operator() {
        op::BinOp(op, ref lhs, ref rhs, round) => {
            assert_eq!(round, op::Rounding::Nearest);
            let assignement = format!("{} = ",namer.name_inst(inst));
            format!("{} {} {} {};", assignement, namer.name_op(lhs), binary_op(op), namer.name_op(rhs))
        }
        op::Mul(ref lhs, ref rhs, round, _) => {
            assert_eq!(round, op::Rounding::Nearest);
            let assignement = format!("{} = ",namer.name_inst(inst));
            format!("{} {} * {};", assignement, namer.name_op(lhs), namer.name_op(rhs))
        },
        op::Mad(ref mul_lhs, ref mul_rhs, ref add_rhs, round) => {
            assert_eq!(round, op::Rounding::Nearest);
            let assignement = format!("{} = ",namer.name_inst(inst));
            format!("{} {} * {} + {};", assignement, namer.name_op(mul_lhs), namer.name_op(mul_rhs), namer.name_op(add_rhs))
        },
        op::Mov(ref op) => {

            let assignement = format!("{} = ",namer.name_inst(inst));
            format!("{} {};", assignement, namer.name_op(op).to_string())
        },
        op::Ld(ld_type, ref addr, _) => {

            let assignement = format!("{} = ",namer.name_inst(inst));
            format!("{} *({} *){};", assignement, cpu_type(&ld_type), namer.name_op(addr))
        },
        op::St(ref addr, ref val, _,  _) => {
            let op_type = val.t();
            let guard = if inst.has_side_effects() {
                namer.side_effect_guard()
            } else { None };
            let pred = if let Some(ref pred) = guard {
                format!("if({}) ", pred)
            } else { String::new() };
            format!("{}*({} *){} = {};", 
                    pred,
                    cpu_type(&op_type),
                    namer.name_op(addr),
                    namer.name_op(val).to_string())
        },
        op::Cast(ref op, t) => {
            let assignement = format!("{} = ",namer.name_inst(inst));
            format!("{} ({}) {};", assignement, cpu_type(&t), namer.name_op(op))
        },
        op::TmpLd(..) | op::TmpSt(..) => panic!("non-printable instruction")
    }
}

/// Prints a cfg.
fn cfg<'a>(fun: &Function, c: &Cfg<'a>, namer: &mut NameMap) -> String {
    match *c {
        Cfg::Root(ref cfgs) => cfg_vec(fun, cfgs, namer),
        Cfg::Loop(ref dim, ref cfgs) => cpu_loop(fun, dim, cfgs, namer),
        // FIXME: handle this
        Cfg::Threads(ref dims, ref ind_levels, ref inner) => {
            let mut res = enable_threads(fun, dims, namer);
            for level in ind_levels {
                res.push_str(&parallel_induction_level(level, namer));
                res.push_str("\n  ");
            }
            res.push_str(&cfg_vec(fun, inner, namer));
            res.push_str("pthread_barrier_wait(tid.barrier);\n");
            res
        }
        Cfg::Instruction(ref i) => inst(i, namer),
    }
}

/// Change the side-effect guards so that only thre specified threads are enabled.
fn enable_threads(fun: &Function, threads: &[bool], namer: &mut NameMap) -> String {
    let mut ops = String::new();
    let mut guard = None;
    for (&is_active, dim) in threads.iter().zip_eq(fun.thread_dims().iter()) {
        if is_active { continue; }
        let new_guard = namer.gen_name(ir::Type::I(1));
        let index = namer.name_index(dim.id());
        //unwrap!(writeln!(ops, "  setp.eq.s32 {}, {}, 0;", new_guard, index));
        unwrap!(writeln!(ops, "   {} = ({} == 0);", new_guard, index));
        if let Some(ref guard) = guard {
            unwrap!(writeln!(ops, "   {} = {} && {};", guard, guard, new_guard));
        } else {
            guard = Some(new_guard);
        };
    }
    namer.set_side_effect_guard(guard.map(RcStr::new));
    ops
}


/// Prints a vector of cfgs.
fn cfg_vec(fun: &Function, cfgs: &[Cfg], namer: &mut NameMap) -> String {
    cfgs.iter().map(|c| cfg(fun, c, namer)).collect_vec().join("\n  ")
}


/// Prints a multiplicative induction var level.
fn parallel_induction_level(level: &InductionLevel, namer: &NameMap) -> String {
    let dim_id = level.increment.map(|(dim, _)| dim);
    let ind_var = namer.name_induction_var(level.ind_var, dim_id);
    let base_components =  level.base.components().map(|v| namer.name(v)).collect_vec();
    if let Some((dim, increment)) = level.increment {
        let index = namer.name_index(dim);
        let step = namer.name_size(increment, Type::I(32));
        match base_components[..] {
            [] => format!("{} = {} * {};//Induction initialized", ind_var, index, step),
            [ref base] =>
                format!(" {} = {} * {} + {};//Induction initialized", ind_var, index, step, base),
            _ => panic!()
        }
    } else {
        match base_components[..] {
            [] => format!("{} = 0;//Induction initialized",  ind_var),
            [ref base] => format!(" {} = {};//Induction initialized", ind_var, base),
            [ref lhs, ref rhs] => format!("{} = {} + {};//Induction initialized", ind_var, lhs, rhs),
            _ => panic!()
        }
    }
}


fn var_decls(namer: &Namer) -> String {
    let print_decl = |(&t, &n)| {
        match t {
            ir::Type::PtrTo(..) => String::new(),
            _ => {
                let prefix = Namer::gen_prefix(&t);
                let mut s = format!("{} ", cpu_type(&t));
                s.push_str(&(0..n).map(|i| format!("{}{}", prefix, i)).collect_vec().join(", "));
                s.push_str(";\n  ");
                s
            }
        }
    };
    let mut ptr_decl = String::from("intptr_t  ");
    ptr_decl.push_str(&(0..namer.num_glob_ptr).map( |i| format!("ptr{}", i)).collect_vec().join(", "));
    ptr_decl.push_str(&";\n");
    let other_var_decl = namer.num_var.iter().map(print_decl).collect_vec().join("\n  ");
    ptr_decl.push_str(&other_var_decl);
    ptr_decl
}

fn cpu_loop(fun: &Function, dim: &Dimension, cfgs: &[Cfg], namer: &mut NameMap)
    -> String
{
    match dim.kind() {
        DimKind::LOOP => {
            standard_loop(fun, dim, cfgs, namer)
        }
        DimKind::UNROLL => {unroll_loop(fun, dim, cfgs, namer)}
        DimKind::VECTOR => {unroll_loop(fun, dim, cfgs, namer)}
        _ => { unimplemented!() }
    }
}

fn standard_loop(fun: &Function, dim: &Dimension, cfgs: &[Cfg], namer: &mut NameMap) -> String {
    let idx = namer.name_index(dim.id()).to_string();
    let ind_levels = dim.induction_levels().iter();
    let (var_init, var_step): (String, String) = ind_levels.map(|level| {
        let dim_id = level.increment.map(|(dim, _)| dim);
        let ind_var = namer.name_induction_var(level.ind_var, dim_id);
        let base_components = level.base.components().map(|v| namer.name(v));
        let init = match base_components.collect_vec()[..] {
            [ref base] => format!("{} = {};//Induction variable\n  ", ind_var, base),
            [ref lhs, ref rhs] =>
                format!(" {} = {} + {};\n  ", ind_var, lhs, rhs),
            _ => panic!(),
        };
        let step = if let Some((_, increment)) = level.increment {
            let step = namer.name_size(increment, level.t());
            format!("{} += {};\n  ", ind_var, step)
        } else { String::new() };
        (init, step)
    }).unzip();
    let loop_id = namer.gen_loop_id();
    format!(include_str!("template/loop.c.template"),
    id = loop_id,
    body = cfg_vec(fun, cfgs, namer),
    idx = idx,
    size = namer.name_size(dim.size(), Type::I(32)),
    induction_var_init = var_init,
    induction_var_step = var_step,
    )
}

fn unroll_loop(fun: &Function, dim: &Dimension, cfgs: &[Cfg], namer: &mut NameMap)-> String {
    let mut body = Vec::new();
    let mut incr_levels = Vec::new();
    for level in dim.induction_levels() {
        let t = cpu_type(&level.t());
        let dim_id = level.increment.map(|(dim, _)| dim);
        let ind_var = namer.name_induction_var(level.ind_var, dim_id).to_string();
        let base_components = level.base.components().map(|v| namer.name(v));
        let base = match base_components.collect_vec()[..] {
            [ref base] => base.to_string(),
            [ref lhs, ref rhs] => {
                let tmp = namer.gen_name(level.t());
                body.push(format!(" {} = {} + {};", tmp, lhs, rhs));
                tmp
            },
            _ => panic!(),
        };
        body.push(format!("{} = {};", ind_var, base));
        if let Some((_, incr)) = level.increment {
            incr_levels.push((level, ind_var, t, incr, base));
        }
    }
    for i in 0..unwrap!(dim.size().as_int()) {
        namer.set_current_index(dim, i);
        if i > 0 {
            for &(level, ref ind_var, _, incr, ref base) in &incr_levels {
                let incr =  if let Some(step) = incr.as_int() {
                    format!(" {} = {} + {};", ind_var, step*i, base)
                } else {
                    let step = namer.name_size(incr, level.t());
                    format!(" {} = {} + {};", ind_var, step, ind_var)
                };
                body.push(incr);
            }
        }
        body.push(cfg_vec(fun, cfgs, namer));
    }
    namer.unset_current_index(dim);
    body.join("\n  ")
}

/// Declares block and thread indexes.
fn decl_par_indexes(function: &Function, namer: &mut NameMap) -> String {
    assert!(function.block_dims().is_empty());
    let mut decls = vec![];
    // Compute thread indexes.
    for (ind, dim) in function.thread_dims().iter().enumerate() {
        //FIXME: fetch proper thread index
        decls.push(format!("{} = tid.t{};", namer.name_index(dim.id()), ind));
    }
    decls.join("\n  ")
}


fn privatise_global_block(block: &InternalMemBlock, namer: &mut NameMap, fun: &Function
                          ) -> String {
    if fun.block_dims().is_empty() { return "".to_string(); }
    let addr = namer.name_addr(block.id());
    let size = namer.name_size(block.local_size(), Type::I(32));
    let d0 = namer.name_index(fun.block_dims()[0].id()).to_string();
    let (var, mut insts) = fun.block_dims()[1..].iter()
        .fold((d0, vec![]), |(old_var, mut insts), dim| {
            let var = namer.gen_name(Type::I(32));
            let size = namer.name_size(dim.size(), Type::I(32));
            let idx = namer.name_index(dim.id());
            insts.push(format!("{} = {} * {} + {};",
                               var, old_var, size, idx));
            (var, insts)
        });
    insts.push(format!("{} = {} * {} + {};",
                       addr, var, size, addr));
    insts.join("\n  ")
}

/// Prints a `Function`.
pub fn function(function: &Function) -> String {
    let mut namer = Namer::default();
    let (param_decls, body, ld_params, idx_loads, mem_decls);
    let mut init = Vec::new();
    {
        let name_map = &mut NameMap::new(function, &mut namer);
        param_decls = function.device_code_args()
            .map(|v| param_decl(v, name_map))
            .collect_vec().join(",\n  ");
        idx_loads = decl_par_indexes(function, name_map);
        ld_params = function.device_code_args().map(|val| {
            format!("{var_name} = {name};",
                    var_name = name_map.name_param_val(val.key()),
                    name = name_map.name_param(val.key()))
        }).collect_vec().join("\n  ");
        mem_decls = function.mem_blocks().flat_map(|block| {
            match block.alloc_scheme() {
                AllocationScheme::Shared =>
                    panic!("No shared mem in cpu!!"),
                AllocationScheme::PrivatisedGlobal =>
                    Some(privatise_global_block(block, name_map, function)),
                AllocationScheme::Global => None,
            }
        }).format("\n  ").to_string();
        // Compute size casts
        for dim in function.dimensions() {
            if !dim.kind().intersects(DimKind::UNROLL | DimKind::LOOP) { continue; }
            for level in dim.induction_levels() {
                if let Some((_, incr)) = level.increment {
                    let name = name_map.declare_size_cast(incr, level.t());
                    if let Some(name) = name {
                        let cpu_t = cpu_type(&level.t());
                        let old_name = name_map.name_size(incr, Type::I(32));
                        init.push(format!("{} = ({}){};", name, cpu_t, old_name));
                    }
                }
            }
        }
        let ind_levels = function.init_induction_levels().into_iter()
            .chain(function.block_dims().iter().flat_map(|d| d.induction_levels()));
        init.extend(ind_levels.map(|level| parallel_induction_level(level, name_map)));
        body = cfg(function, function.cfg(), name_map);
    }
    let var_decls = var_decls(&namer);
    format!(include_str!("template/device.c.template"),
            name = function.name,
            idx_loads = idx_loads,
            ld_params = ld_params,
            params = param_decls,
            var_decls = var_decls,
            mem_decls = mem_decls,
            init = init.join("\n  "),
            body = body
           )
}

fn fun_params_cast(function: &Function) -> String {
    function.device_code_args()
        .enumerate()
        .map(|(i, v)| match v {
            ParamVal::External(..) if v.is_pointer() => format!("intptr_t p{i} = (intptr_t)*(args + {i})", 
                                                        i = i),
            ParamVal::External(_, par_type) => format!("{t} p{i} = *({t}*)*(args + {i})", 
                                                       t = cpu_type(par_type), i = i),
            ParamVal::Size(_) => format!("uint32_t p{i} = *(uint32_t*)*(args + {i})", i = i),
            // Are we sure we know the size at compile time ? I think we do
            ParamVal::GlobalMem(_, _, par_type) => format!("{t} p{i} = ({t})*(args + {i})", 
                                                              t = cpu_type(par_type), i = i)
        }
        )
        .collect_vec()
        .join(";\n  ")
} 

fn params_call(function: &Function) -> String {
    function.device_code_args()
        .enumerate().map(|x| x.0)
        .map(|i| format!("p{}", i))
        .collect_vec()
        .join(", ")
}

// Build the right call for a nested loop on dimensions
fn build_index_call(func: &Function) -> String {
    let mut vec_ret = vec![];
    let dims = func.thread_dims();
    let n = dims.len();
    for i in 0..n {
        let start = format!("d{}", i);
        let mut vec_str = vec![start];
        for j in 0.. i  {
            vec_str.push(format!("{}", unwrap!(dims[j].size().as_int())));
        }
        vec_ret.push(vec_str.join(" * "));
    }
    vec_ret.join(" + ")
}

fn build_thread_id_struct(func: &Function) -> String {
    let mut ret = String::new();
    if func.num_threads() == 1 {
        return String::from("int t0;\n");
    }
    for (ind, _dim) in func.thread_dims().iter().enumerate() {
        ret.push_str(&format!("int t{};\n", ind));
    }
    ret
}

fn thread_gen(func: &Function) -> String {
    if func.num_threads() == 1 {
        let mut ret = format!("thread_arg_t thread_args;\n");
        ret.push_str(&format!(" thread_args.args = args;\n"));
        ret.push_str(&format!(" thread_args.tid.t0 = 0;\n"));
        ret.push_str(&format!(" thread_args.tid.barrier = &barrier;\n"));
        ret.push_str(&format!("pthread_barrier_init(&barrier, NULL,{});\n",   func.num_threads()));
        ret.push_str(&format!("exec_wrap((void *)&thread_args);\n"));
        return ret;
    }
    let mut ret = format!("pthread_t thr_ids[{}];\n", func.num_threads());
    let mut ind_var_decl = String::from("int ");
    let build_struct = format!("thread_arg_t thread_args[{}];\n", func.num_threads());
    let dim_tid_struct = format!("thread_dim_id_t thread_tids[{}];\n", func.num_threads());
    let barrier_init = format!("pthread_barrier_init(&barrier, NULL,{});\n",   func.num_threads() );
    let mut loop_decl = String::new();
    let mut ind_vec = Vec::new();
    let mut jmp_stack = Vec::new();
    for (ind, dim) in func.thread_dims().iter().enumerate() {
        let mut loop_jmp = String::new();
        ind_vec.push(format!("d{}", ind));
        loop_decl.push_str(&format!("d{}=0;\n", ind));
        loop_decl.push_str(&format!("LOOP_BEGIN_{}:\n", ind));
        loop_jmp.push_str(&format!("d{}++;\n", ind));
        loop_jmp.push_str(&format!("if (d{} < {})\n", ind, unwrap!(dim.size().as_int())));
        loop_jmp.push_str(&format!("    goto LOOP_BEGIN_{};\n", ind));
        jmp_stack.push(loop_jmp);
    }
    let ind_dec_inter = ind_vec.join(", ");
    ind_var_decl.push_str(&ind_dec_inter);
    ind_var_decl.push_str(&";\n");
    let mut loop_jmp = String::new(); 
    while let Some(j_str) = jmp_stack.pop() {
        loop_jmp.push_str(&j_str);
    }
    let arg_struct = format!("thread_args[{ind}].args = args;\n",  ind = build_index_call(func) );
    let mut tid_struct = String::new();
    for (ind, _) in func.thread_dims().iter().enumerate() {
        tid_struct.push_str(&format!("thread_args[{index}].tid.t{dim_id} = d{dim_id};\n",  index = build_index_call(func), dim_id = ind));
    }
    let barrier_str = format!("thread_args[{}].tid.barrier = &barrier;\n",  build_index_call(func) );
    let create_call = format!("pthread_create(&thr_ids[{}], NULL, exec_wrap, (void *)&thread_args[{ind}]);\n",  ind = build_index_call(func) );
    ret.push_str(&ind_var_decl);
    ret.push_str(&build_struct);
    ret.push_str(&dim_tid_struct);
    ret.push_str(&barrier_init);
    ret.push_str(&loop_decl);
    ret.push_str(&arg_struct);
    ret.push_str(&tid_struct);
    ret.push_str(&barrier_str);
    ret.push_str(&create_call);
    ret.push_str(&loop_jmp);
    ret
}

fn thread_join(func: &Function) -> String {
    if func.num_threads() == 1 {
        return String::new();
    }
    let mut ret = String::new();
    let mut loop_decl = String::new();
    let mut jmp_stack = Vec::new();
    for (ind, dim) in func.thread_dims().iter().enumerate() {
        let mut loop_jmp = String::new();
        loop_decl.push_str(&format!("d{} = 0;\n", ind));
        loop_decl.push_str(&format!("JOIN_LOOP_BEGIN_{}:\n", ind));
        loop_jmp.push_str(&format!("d{}++;\n", ind));
        loop_jmp.push_str(&format!("if (d{} < {})\n", ind, unwrap!(dim.size().as_int())));
        loop_jmp.push_str(&format!("    goto JOIN_LOOP_BEGIN_{};\n", ind));
        jmp_stack.push(loop_jmp);
    }
    let mut loop_jmp = String::new();
    while let Some(j_str) = jmp_stack.pop() {
        loop_jmp.push_str(&j_str);
    }
    let join_call = format!("pthread_join(thr_ids[{}], NULL);\n", build_index_call(func) );
    let barrier_destroy = format!("pthread_barrier_destroy(&barrier);\n");
    ret.push_str(&loop_decl);
    ret.push_str(&join_call);
    ret.push_str(&loop_jmp);
    ret.push_str(&barrier_destroy);
    ret

}

pub fn wrapper_function(func: &Function) -> String {
    let fun_str = function(func);
    let fun_params = params_call(func);
    format!(include_str!("template/host.c.template"),
            fun_name = func.name,
            fun_str = fun_str,
            fun_params_cast = fun_params_cast(func),
            fun_params = fun_params,
            gen_threads = thread_gen(func),
            dim_decl = build_thread_id_struct(func),
            thread_join = thread_join(func),
           )
}
