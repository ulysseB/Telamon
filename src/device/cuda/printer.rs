//! Provides functions to print PTX code.
use device::cuda::{Gpu, Namer};
use codegen::Printer;
use codegen::*;
use ir::{self, dim, op, Operand, Size, Type};
use itertools::Itertools;
use search_space::{DimKind, Domain, InstFlag};
use std;
use std::io::Write;
use std::fmt::Write as WriteFmt;
use utils::*;
// TODO(cc_perf): avoid concatenating strings.

pub struct CudaPrinter {
    out_function: String,
}

impl CudaPrinter {

    fn mul_mode(mode: MulMode) -> &'static str {
        match mode {
            MulMode::Wide => ".wide",
            MulMode::Low => ".lo",
            MulMode::High => ".hi",
            MulMode::Empty => "",
        }
    }

    fn get_inst_type(mode: MulMode, ret_type: Type) -> Type {
        match mode {
            MulMode::Wide => if let Type::I(n) = ret_type {Type::I( n / 2)} else {panic!("get_inst_type should only be called with integer types")}
            ,
            _ => ret_type,
        }
    }

    /// Prints a load operator.
    fn ld_operator(flag: InstFlag) -> &'static str {
        match flag {
            InstFlag::MEM_SHARED => "ld.shared",
            InstFlag::MEM_CA => "ld.global.ca",
            InstFlag::MEM_CG => "ld.global.cg",
            InstFlag::MEM_CS => "ld.global.cs",
            InstFlag::MEM_NC => "ld.global.nc",
            _ => panic!("invalid load flag {:?}", flag),
        }
    }

    /// Prints a store operator.
    fn st_operator(flag: InstFlag) -> &'static str {
        match flag {
            InstFlag::MEM_SHARED => "st.shared",
            InstFlag::MEM_CA => "st.global.wb",
            InstFlag::MEM_CG => "st.global.cg",
            InstFlag::MEM_CS => "st.global.cs",
            _ => panic!("invalid store flag {:?}", flag),
        }
    }
    /// Prints the variables declared by the `Namer`.
    fn var_decls(&mut self, namer: &Namer) -> String {
        let print_decl = |(&t, n)| {
            let prefix = Namer::gen_prefix(&t);
            format!(".reg.{} %{}<{}>;", self.get_type(t), prefix, n)
        };
        namer.num_var.iter().map(print_decl).collect_vec().join("\n  ")
    }

    /// Declares block and thread indexes.
    fn decl_par_indexes(function: &Function, namer: &mut NameMap) -> String {
        let mut decls = vec![];
        // Load block indexes.
        for (dim, dir) in function.block_dims().iter().zip(&["x", "y", "z"])  {
            let index = namer.name_index(dim.id());
            decls.push(format!("mov.u32 {}, %ctaid.{};", index, dir));
        }
        // Compute thread indexes.
        for (dim, dir) in function.thread_dims().iter().rev().zip(&["x", "y", "z"]) {
            decls.push(format!("mov.s32 {}, %tid.{};", namer.name_index(dim.id()), dir));
        }
        decls.join("\n  ")
    }

    /// Declares a shared memory block.
    fn shared_mem_decl(&mut self, block: &InternalMemBlock, namer: &mut NameMap)  {
        let ptr_type_name = self.get_type(Type::I(32));
        let name = namer.name_addr(block.id());
        let mem_decl = format!(".shared.align 16 .u8 {vec_name}[{size}];\
            \n  mov.{t} {name}, {vec_name};\n",
            vec_name = &name[1..],
            name = name,
            t = ptr_type_name,
            size = unwrap!(block.alloc_size().as_int()));
        self.out_function.push_str(&mem_decl);
    }

    pub fn new() -> Self {
        CudaPrinter{out_function: String::new() }
    }

    /// Prints a `Type` for the host.
    fn host_type(t: &Type) -> &'static str {
        match *t {
            Type::Void => "void",
            Type::PtrTo(..) => "CUdeviceptr",
            Type::F(32) => "float",
            Type::F(64) => "double",
            Type::I(8) => "int8_t",
            Type::I(16) => "int16_t",
            Type::I(32) => "int32_t",
            Type::I(64) => "int64_t",
            ref t => panic!("invalid type for the host: {}", t)
        }
    }

    /// Returns the string representation of thread and block sizes on the host.
    fn host_3sizes<'a, IT>(dims: IT) -> [String; 3]
        where IT: Iterator<Item=&'a Dimension<'a>>  + 'a {
            let mut sizes = ["1".to_string(), "1".to_string(), "1".to_string()];
            for (i, d) in dims.into_iter().enumerate() {
                assert!(i < 3);
                sizes[i] = Self::host_size(d.size())
            }
            sizes
        }

    /// Prints a size on the host.
    fn host_size(size: &Size) -> String {
        let dividend = size.dividend().iter().map(|p| format!("* {}", &p.name));
        format!("{}{}/{}", size.factor(), dividend.format(""), size.divisor())
    }

    fn binary_op(op: ir::BinOp) -> &'static str {
        match op {
            ir::BinOp::Add => "add",
            ir::BinOp::Sub => "sub",
            ir::BinOp::Div => "div",
        }
    }

    /// Prints a parameter decalartion.
    fn param_decl(&mut self, param: &ParamVal, namer: &NameMap) -> String {
        format!(
            ".param .{t}{attr} {name}",
            t = self.get_type(param.t()),
            attr = if param.is_pointer() { ".ptr.global.align 16" } else { "" },
            name = namer.name_param(param.key()),
            )
    }
    /// Prints a rounding mode selector.
    fn rounding(rounding: op::Rounding) -> &'static str {
        match rounding {
            op::Rounding::Exact => "",
            op::Rounding::Nearest => ".rn",
            op::Rounding::Zero => ".rz",
            op::Rounding::Positive => ".rp",
            op::Rounding::Negative => ".rm",
        }
    }

    /// Prints a `Function`.
    pub fn function(&mut self, function: &Function, gpu: &Gpu) -> String {
        let mut namer = Namer::default();
        let (param_decls, ld_params);
        let mut body = String::new();
        {
            let name_map = &mut NameMap::new(function, &mut namer);
            param_decls = function.device_code_args()
                .map(|v| self.param_decl(v, name_map))
                .collect_vec().join(",\n  ");
            ld_params = function.device_code_args().map(|val| {
                format!("ld.param.{t} {var_name}, [{name}];",
                        t = self.get_type(val.t()),
                        var_name = name_map.name_param_val(val.key()),
                        name = name_map.name_param(val.key()))
            }).collect_vec().join("\n  ");
            self.out_function.push_str(&ld_params);
            self.out_function.push_str(&"\n");
            // INDEX LOAD
            let idx_loads = Self::decl_par_indexes(function, name_map);
            self.out_function.push_str(&idx_loads);
            self.out_function.push_str(&"\n");
            //MEM DECL
            for block in function.mem_blocks() {
                match block.alloc_scheme() {
                    AllocationScheme::Shared =>
                        self.shared_mem_decl(block, name_map),
                    AllocationScheme::PrivatisedGlobal =>
                        self.privatise_global_block(block, name_map, function),
                    AllocationScheme::Global => (),
                }
            }
            // Compute size casts
            for dim in function.dimensions() {
                if !dim.kind().intersects(DimKind::UNROLL | DimKind::LOOP) { continue; }
                for level in dim.induction_levels() {
                    if let Some((_, incr)) = level.increment {
                        let name = name_map.declare_size_cast(incr, level.t());
                        if let Some(name) = name {
                            let ptx_t = self.get_type(level.t());
                            let old_name = name_map.name_size(incr, Type::I(32));
                            self.out_function.push_str(&format!("cvt.{}.s32 {}, {};", ptx_t, name, old_name));
                            self.out_function.push_str(&"\n");
                        }
                    }
                }
            }
            let ind_levels = function.init_induction_levels().into_iter()
                .chain(function.block_dims().iter().flat_map(|d| d.induction_levels()));
            //init.extend(ind_levels.map(|level| self.parallel_induction_level(level, name_map)));
            for level in ind_levels {
                self.parallel_induction_level(level, name_map);
            }
            self.cfg(function, function.cfg(), name_map);
        }
        let var_decls = self.var_decls(&namer);
        body.push_str(&var_decls);
        self.out_function.push_str(&"\n");
        body.push_str(&self.out_function);
        format!(include_str!("template/device.ptx"),
        sm_major = gpu.sm_major,
        sm_minor = gpu.sm_minor,
        addr_size = gpu.addr_size,
        name = function.name,
        params = param_decls,
        num_thread = function.num_threads(),
        body = body
        )
    }

    pub fn host_function(&mut self, fun: &Function, gpu: &Gpu, out: &mut Write) {
        let block_sizes = Self::host_3sizes(fun.block_dims().iter());
        let thread_sizes = Self::host_3sizes(fun.thread_dims().iter().rev());
        let extern_param_names =  fun.params.iter()
            .map(|x| &x.name as &str).collect_vec().join(", ");
        let mut next_extra_var_id = 0;
        let mut extra_def = vec![];
        let mut extra_cleanup = vec![];
        let params = fun.device_code_args().map(|p| match *p {
            ParamVal::External(p, _) => format!("&{}", p.name),
            ParamVal::Size(size) => {
                let extra_var = format!("_extra_{}", next_extra_var_id);
                next_extra_var_id += 1;
                extra_def.push(format!("int32_t {} = {};", extra_var, Self::host_size(size)));
                format!("&{}", extra_var)
            },
            ParamVal::GlobalMem(_, ref size, _) => {
                let extra_var = format!("_extra_{}", next_extra_var_id);
                next_extra_var_id += 1;
                let size = Self::host_size(size);
                extra_def.push(format!("CUDeviceptr {};", extra_var));
                extra_def.push(format!("CHECK_CUDA(cuMemAlloc(&{}, {}));", extra_var, size));
                extra_cleanup.push(format!("CHECK_CUDA(cuMemFree({}));", extra_var));
                format!("&{}", extra_var)
            },
        }).collect_vec().join(", ");
        let extern_params = fun.params.iter()
            .map(|p| format!("{} {}", Self::host_type(&p.t), p.name))
            .collect_vec().join(", ");
        let res = write!(out, include_str!("template/host.c"),
        name = fun.name,
        ptx_code = self.function(fun, gpu).replace("\n", "\\n\\\n"),
        extern_params = extern_params,
        extern_param_names = extern_param_names,
        param_vec = format!("{{ {} }}", params),
        extra_def = extra_def.join("  \n"),
        extra_cleanup = extra_cleanup.join("  \n"),
        t_dim_x = &thread_sizes[0],
        t_dim_y = &thread_sizes[1],
        t_dim_z = &thread_sizes[2],
        b_dim_x = &block_sizes[0],
        b_dim_y = &block_sizes[1],
        b_dim_z = &block_sizes[2],
        );
        unwrap!(res);
    }
}

impl Printer for CudaPrinter {

    /// Get a proper string representation of an integer in target language
    fn get_int(&self, n: u32) -> String {
        format!("{}", n)
    }

    /// Get a proper string representation of an integer in target language
    fn get_float(&self, f: f64) -> String {
        let binary = unsafe { std::mem::transmute::<f64, u64>(f) };
        format!("0D{:016X}", binary)
    }

    /// Print a type in the backend
    fn get_type(&self, t: Type) -> String {
       match t {
        Type::Void => panic!("void type cannot be printed"),
        Type::I(1) => "pred".to_string(),
        Type::I(size) => format!("s{size}", size = size),
        Type::F(size) => format!("f{size}", size = size),
        _ => panic!()
    }
 }

    /// Print return_id = op1 op op2
    fn print_binop(&mut self, return_id: &str, op_type: ir::BinOp, op1: &str, op2: &str, r_type: Type, round: op::Rounding) {
        let return_str = format!("{}{}.{} {}, {}, {};\n", Self::binary_op(op_type),  Self::rounding(round), self.get_type(r_type), return_id, op1, op2);
        self.out_function.push_str(&return_str);
    }

    /// Print return_id = op1 * op2
    fn print_mul(&mut self, return_type: Type, round: op::Rounding, mul_mode: MulMode, return_id: &str, lhs: &str, rhs: &str) {
        let operator = if round == op::Rounding::Exact {
            format!("mul{}", Self::mul_mode(mul_mode))
        } else {
            format!("mul{}", Self::rounding(round))
        };
        let return_str = format!("{}.{} {}, {}, {};\n", operator, self.get_type(Self::get_inst_type(mul_mode, return_type)), return_id, lhs, rhs);
        self.out_function.push_str(&return_str);
    }

    /// Print return_id = mlhs * mrhs + arhs
    fn print_mad(&mut self, ret_type: Type, round: op::Rounding, mul_mode: MulMode, return_id: &str,  mlhs: &str, mrhs: &str, arhs: &str) {
        let operator = if round == op::Rounding::Exact {
            format!("mad{}", Self::mul_mode(mul_mode))
        } else {
            format!("fma{}", Self::rounding(round))
        };
        let return_str = format!("{}.{} {}, {}, {}, {};\n", operator, self.get_type(Self::get_inst_type(mul_mode, ret_type)), return_id, mlhs, mrhs, arhs);
        self.out_function.push_str(&return_str);
    }

    /// Print return_id = op 
    fn print_mov(&mut self, return_id: &str, op: &str, r_type: Type) {
        let return_str = format!("mov.{} {}, {};\n", self.get_type(r_type), return_id, op);
        self.out_function.push_str(&return_str);
    }

    /// Print return_id = load [addr] 
    fn print_ld(&mut self, return_id: &str, cast_type: Type,  addr: &str, r_type: Type, mem_flag: InstFlag) {
        let return_str = format!("{}.{} {}, [{}];\n", Self::ld_operator(mem_flag), self.get_type(r_type), return_id,  addr);
        self.out_function.push_str(&return_str);
    }

    /// Print store val [addr] 
    fn print_st(&mut self, addr: &str, val: &str, val_type: &str, mem_flag: InstFlag) {
        let operator = Self::st_operator(mem_flag);
        let return_str = format!("{}.{} [{}], {};\n", operator, val_type, addr, val);
        self.out_function.push_str(&return_str);
    }

    /// Print if (cond) store val [addr] 
    fn print_cond_st(&mut self, addr: &str, val: &str, cond: &str, val_type: &str, mem_flag: InstFlag) {
        let operator = Self::st_operator(mem_flag);
        let return_str = format!("@{} {}.{} [{}], {};\n", cond, operator, val_type, addr, val);
        self.out_function.push_str(&return_str);
    }

    /// Print return_id = (val_type) val
    fn print_cast(&mut self, return_id: &str, op1: &str, t: Type, round: op::Rounding) {
        let operator = format!("cvt{}.{}", Self::rounding(round), self.get_type(t));
        let return_str = format!("{} {}, {}\n",  operator, return_id, op1);
        self.out_function.push_str(&return_str);
    }

    /// print a label where to jump
    fn print_label(&mut self, label_id: &str) {
        self.out_function.push_str(&format!("LOOP_{}:\n", label_id));
    }

    /// Print return_id = op1 && op2
    fn print_and(&mut self, return_id: &str, op1: &str, op2: &str) {
        let return_str = format!("and.pred {}, {}, {};\n", return_id, op1, op2);
        self.out_function.push_str(&return_str);
    }

    /// Print return_id = op1 || op2
    fn print_or(&mut self, return_id: &str, op1: &str, op2: &str) {
        let return_str = format!("or.pred {}, {}, {};\n", return_id, op1, op2);
        self.out_function.push_str(&return_str);
    }

    /// Print return_id = op1 == op2
    fn print_equal(&mut self, return_id: &str, op1: &str, op2: &str) {
        let return_str = format!("setp.eq.u32 {}, {}, {};\n", return_id, op1, op2);
        self.out_function.push_str(&return_str);
    }

    /// Print return_id = op1 < op2
    fn print_lt(&mut self, return_id: &str, op1: &str, op2: &str) {
        let return_str = format!("setp.lt.u32 {}, {}, {};\n", return_id, op1, op2);
        self.out_function.push_str(&return_str);
    }

    /// Print return_id = op1 > op2
    fn print_gt(&mut self, return_id: &str, op1: &str, op2: &str) {
        let return_str = format!("setp.gt.u32 {}, {}, {};\n", return_id, op1, op2);
        self.out_function.push_str(&return_str);
    }

    /// Print if (cond) jump label(label_id)
    fn print_cond_jump(&mut self, label_id: &str, cond: &str) {
        let return_str = format!("@{} bra.uni LOOP_{};\n", cond, label_id);
        self.out_function.push_str(&return_str);
    }

    /// Print wait on all threads
    fn print_sync(&mut self) {
        self.out_function.push_str("bar.sync 0;\n");
    }
}
