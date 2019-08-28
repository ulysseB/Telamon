use crate::NameGenerator;
use itertools::Itertools;
use std::fmt::Write as WriteFmt;
use telamon::codegen::llir::IntoVector;
use telamon::codegen::*;
use telamon::ir::{self, op, Type};
use telamon::search_space::{DimKind, Domain, InstFlag, MemSpace};
use utils::unwrap;
// TODO(cc_perf): avoid concatenating strings.

#[derive(Default)]
pub struct MppaPrinter {
    buffer: String,
}

impl MppaPrinter {
    /// Declares all parameters of the function with the appropriate type
    fn param_decl(&mut self, param: &ParamVal, name_map: &NameMap<'_>) -> String {
        let name = name_map.name_param(param.key());
        match param {
            ParamVal::External(_, par_type) => {
                format!("{} {}", Self::get_type(*par_type), name)
            }
            ParamVal::Size(_) => format!("uint32_t {}", name),
            ParamVal::GlobalMem(_, _, par_type) => {
                format!("{} {}", Self::get_type(*par_type), name)
            }
        }
    }

    /// Declared all variables that have been required from the namegen
    fn var_decls(&mut self, namegen: &NameGenerator) -> String {
        let print_decl = |(&t, &n)| {
            // Type is never supposed to be PtrTo here as we handle ptr types in a different way
            if let ir::Type::PtrTo(..) = t {
                unreachable!("Type PtrTo are never inserted in this map");
            }
            let prefix = NameGenerator::gen_prefix(t);
            let mut s = format!("{} ", NameGenerator::get_string(t));
            s.push_str(
                &(0..n)
                    .map(|i| format!("{}{}", prefix, i))
                    .collect_vec()
                    .join(", "),
            );
            s.push_str(";\n  ");
            s
        };
        let other_var_decl = namegen
            .num_var
            .iter()
            .map(print_decl)
            .collect_vec()
            .join("\n  ");
        if namegen.num_glob_ptr == 0 {
            other_var_decl
        } else {
            format!(
                "intptr_t {};\n{}",
                &(0..namegen.num_glob_ptr)
                    .map(|i| format!("ptr{}", i))
                    .collect_vec()
                    .join(", "),
                other_var_decl,
            )
        }
    }

    /// Declares block and thread indexes.
    fn decl_par_indexes(
        &mut self,
        function: &Function,
        name_map: &NameMap<'_>,
    ) -> String {
        assert!(function.block_dims().is_empty());
        let mut decls = vec![];
        // Compute thread indexes.
        for (ind, dim) in function.thread_dims().iter().enumerate() {
            decls.push(format!(
                "{} = tid->t{};\n",
                name_map.name_index(dim.id()).name(),
                ind
            ));
        }
        decls.join("\n  ")
    }

    /// Prints a `Function`.
    pub fn function<'a: 'b, 'b>(
        &mut self,
        function: &'b Function<'a>,
        name_map: &mut NameMap<'_>,
    ) -> String {
        let param_decls = function
            .device_code_args()
            .map(|v| self.param_decl(v, name_map))
            .collect_vec()
            .join(",\n  ");
        // SIGNATURE AND OPEN BRACKET
        let mut return_string = format!(
            include_str!("template/signature.c.template"),
            name = function.name(),
            params = param_decls
        );
        // INDEX LOADS
        let idx_loads = self.decl_par_indexes(function, name_map);
        unwrap!(writeln!(self.buffer, "{}", idx_loads));
        // LOAD PARAM
        for val in function.device_code_args() {
            unwrap!(writeln!(
                self.buffer,
                "{var_name} = {name};// LD_PARAM",
                var_name = name_map.name_param_val(val.key()).c99(),
                name = name_map.name_param(val.key())
            ));
        }
        // MEM DECL
        for block in function.mem_blocks() {
            match block.alloc_scheme() {
                AllocationScheme::Shared => panic!("No shared mem in cpu!!"),
                AllocationScheme::PrivatisedGlobal => {
                    Printer::new(self, name_map).privatise_global_block(block, function)
                }
                AllocationScheme::Global => (),
            }
        }
        // Compute size casts
        for dim in function.dimensions() {
            if !dim.kind().intersects(DimKind::UNROLL | DimKind::LOOP) {
                continue;
            }
            for level in dim.induction_levels() {
                if let Some((_, ref incr)) = level.increment {
                    let reg = name_map.declare_size_cast(incr, level.t());
                    if let Some(reg) = reg {
                        let old_name = name_map.name_size(incr, Type::I(32));
                        self.print_unary_op(
                            [1, 1],
                            ir::UnaryOp::Cast(level.t()),
                            Type::I(32),
                            reg.into_vector(),
                            old_name.into_vector(),
                        );
                    }
                }
            }
        }
        // INIT
        let ind_levels = function.init_induction_levels().iter().chain(
            function
                .block_dims()
                .iter()
                .flat_map(|d| d.induction_levels()),
        );
        for level in ind_levels {
            Printer::new(self, name_map).parallel_induction_level(level);
        }
        // BODY
        Printer::new(self, name_map).cfg(function, function.cfg());
        let var_decls = self.var_decls(&namegen);
        return_string.push_str(&var_decls);
        return_string.push_str(&self.buffer);
        // Close function bracket
        return_string.push('}');
        return_string
    }

    /// Function takes parameters as an array of void* pointers
    /// This function converts back these pointers into their original types
    fn fun_params_cast(&mut self, function: &Function) -> String {
        function
            .device_code_args()
            .enumerate()
            .map(|(i, v)| {
                match v {
                    ParamVal::External(..) if v.is_pointer() => format!(
                        "uintptr_t p{i} = (uintptr_t)*(args + {i});\n//printf(\"p{i} = \
                         %p\\n\", (void *)p{i});\n",
                        i = i
                    ),
                    ParamVal::External(_, par_type) => format!(
                        "{t} p{i} = *({t}*)*(args + {i})",
                        t = Self::get_type(*par_type),
                        i = i
                    ),
                    ParamVal::Size(_) => format!(
                        "uint32_t p{i} = *(uint32_t*)(void *)*(args + {i}); \
                         //printf(\"p{i} = %d\\n\")",
                        i = i
                    ),
                    // Are we sure we know the size at compile time ? I think we do
                    ParamVal::GlobalMem(_, _, par_type) => format!(
                        "{t} p{i} = ({t})*(args + {i})",
                        t = Self::get_type(*par_type),
                        i = i
                    ),
                }
            })
            .collect_vec()
            .join(";\n  ")
    }

    /// Declares the variables that will be used in C function call
    fn params_call(&mut self, function: &Function) -> String {
        function
            .device_code_args()
            .enumerate()
            .map(|x| x.0)
            .map(|i| format!("p{}", i))
            .collect_vec()
            .join(", ")
    }

    /// Build the right call for a nested loop on dimensions with linearized accesses that is, for
    /// a 3 dimensional arrays a[2][5][3] returns d0 + d1 * 3 + d2 * 5
    fn build_index_call(&mut self, func: &Function) -> String {
        let mut vec_ret = vec![];
        let dims = func.thread_dims();
        let n = dims.len();
        for i in 0..n {
            let start = format!("d{}", i);
            let mut vec_str = vec![start];
            for dim in dims.iter().take(i) {
                vec_str.push(format!("{}", unwrap!(dim.size().as_int())));
            }
            vec_ret.push(vec_str.join(" * "));
        }
        vec_ret.join(" + ")
    }

    /// Helper for building a structure containing as many thread ids (one id per dim) as required.
    fn build_thread_id_struct(&mut self, func: &Function) -> String {
        let mut ret = String::new();
        if func.num_threads() == 1 {
            return String::from("int t0;\n");
        }
        for (ind, _dim) in func.thread_dims().iter().enumerate() {
            ret.push_str(&format!("int t{};\n", ind));
        }
        ret
    }

    /// Prints code that generates the required number of threads, stores the handles in an array
    fn thread_gen(&mut self, func: &Function) -> String {
        if func.num_threads() == 1 {
            return include_str!("template/monothread_init.c.template").to_string();
        }
        let mut loop_decl = String::new();
        let mut ind_vec = Vec::new();
        let mut loop_nesting = 0;

        for (ind, dim) in func.thread_dims().iter().enumerate() {
            ind_vec.push(format!("d{}", ind));
            unwrap!(writeln!(
                loop_decl,
                "for(d{ind} = 0; d{ind} < {size}; d{ind}++) {{",
                ind = ind,
                size = unwrap!(dim.size().as_int()),
            ));
            loop_nesting += 1;
        }
        let ind_dec_inter = ind_vec.join(", ");
        let ind_var_decl = format!("int {};", ind_dec_inter);
        let loop_ends = "}\n".repeat(loop_nesting);
        let mut tid_struct = String::new();
        for (ind, _) in func.thread_dims().iter().enumerate() {
            tid_struct.push_str(&format!(
                "tids[{index}].t{dim_id} = d{dim_id};\n",
                index = self.build_index_call(func),
                dim_id = ind
            ));
        }
        format!(
            include_str!("template/multithread_init.c.template"),
            num_threads = func.num_threads(),
            ind = self.build_index_call(func),
            ind_var_decl = ind_var_decl,
            loop_init = loop_decl,
            tid_struct = tid_struct,
            loop_jump = loop_ends
        )
    }

    /// Prints code that joins all previously generated threads
    fn thread_join(&mut self, func: &Function) -> String {
        if func.num_threads() == 1 {
            return String::new();
        }
        let mut loop_decl = String::new();
        let mut loop_nesting = 0;

        for (ind, dim) in func.thread_dims().iter().enumerate() {
            unwrap!(writeln!(
                loop_decl,
                "for(d{ind} = 0; d{ind} < {size}; d{ind}++) {{",
                ind = ind,
                size = unwrap!(dim.size().as_int()),
            ));
            loop_nesting += 1;
        }

        let loop_ends = "}\n".repeat(loop_nesting);

        format!(
            include_str!("template/join_thread.c.template"),
            ind = self.build_index_call(func),
            loop_init = loop_decl,
            loop_jump = loop_ends
        )
    }

    /// Turns the argument of wrapper into an array of void pointers
    /// Necessary to call pthread with it
    fn build_ptr_struct(&self, func: &Function, name_map: &NameMap<'_>) -> String {
        func.device_code_args()
            .enumerate()
            .map(|(i, arg)| {
                let name = name_map.name_param(arg.key());
                if arg.is_pointer() {
                    format!("args[{}] = (void *){}", i, name)
                } else {
                    format!("args[{}] = (void *)&{}", i, name)
                }
            })
            .join(";\n")
    }

    /// wrap the kernel call into a function with a fixed interface
    pub fn wrapper_function<'a: 'b, 'b>(
        &mut self,
        func: &'b Function<'a>,
        name_map: &mut NameMap<'b, '_>,
        id: usize,
    ) -> String {
        let fun_str = self.function(func, name_map);
        let fun_params = self.params_call(func);
        let (lower_bound, upper_n_arg) = func.device_code_args().size_hint();
        let n_args = if let Some(upper_bound) = upper_n_arg {
            assert_eq!(upper_bound, lower_bound);
            upper_bound
        } else {
            20
        };
        let cl_arg_def = func
            .device_code_args()
            .map(|v| self.param_decl(v, name_map))
            .collect_vec()
            .join(",  ");
        format!(
            include_str!("template/host.c.template"),
            id = id,
            cl_arg_def = cl_arg_def,
            n_arg = n_args,
            build_ptr_struct = self.build_ptr_struct(func, name_map),
            fun_name = func.name(),
            fun_str = fun_str,
            fun_params_cast = self.fun_params_cast(func),
            fun_params = fun_params,
            gen_threads = self.thread_gen(func),
            dim_decl = self.build_thread_id_struct(func),
            thread_join = self.thread_join(func),
        )
    }

    /// Returns the name of a type.
    fn type_name(t: ir::Type) -> &'static str {
        match t {
            ir::Type::PtrTo(..) => "void*",
            ir::Type::F(32) => "float",
            ir::Type::F(64) => "double",
            ir::Type::I(1) => "bool",
            ir::Type::I(8) => "uint8_t",
            ir::Type::I(16) => "uint16_t",
            ir::Type::I(32) => "uint32_t",
            ir::Type::I(64) => "uint64_t",
            _ => panic!("non-printable type"),
        }
    }

    /// Returns the name of a type.
    fn cl_type_name(t: ir::Type) -> &'static str {
        match t {
            ir::Type::PtrTo(..) => "__global void*",
            ir::Type::I(8) => "char",
            ir::Type::I(16) => "short",
            ir::Type::I(32) => "int",
            ir::Type::I(64) => "long",
            _ => Self::type_name(t),
        }
    }
    /// Prints the OpenCL wrapper for a candidate implementation.
    pub fn print_ocl_wrapper(
        &mut self,
        fun: &Function,
        name_map: &mut NameMap<'_>,
        id: usize,
    ) -> String {
        let arg_names = fun
            .device_code_args()
            .format_with(", ", |p, f| {
                f(&format_args!("{}", name_map.name_param(p.key())))
            })
            .to_string();
        let cl_arg_defs = fun
            .device_code_args()
            .format_with(", ", |p, f| {
                f(&format_args!(
                    "{} {}",
                    Self::cl_type_name(p.t()),
                    name_map.name_param(p.key())
                ))
            })
            .to_string();
        format!(
            include_str!("template/ocl_wrap.c.template"),
            fun_id = id,
            arg_names = arg_names,
            cl_arg_defs = cl_arg_defs,
        )
    }

    fn get_type(t: ir::Type) -> String {
        match t {
            ir::Type::PtrTo(..) => String::from("intptr_t"),
            ir::Type::F(32) => String::from("float"),
            ir::Type::F(64) => String::from("double"),
            ir::Type::I(1) => String::from("int8_t"),
            ir::Type::I(8) => String::from("int8_t"),
            ir::Type::I(16) => String::from("int16_t"),
            ir::Type::I(32) => String::from("int32_t"),
            ir::Type::I(64) => String::from("int64_t"),
            ref t => panic!("invalid type for the host: {}", t),
        }
    }
}

impl InstPrinter for MppaPrinter {
    fn print_binop(
        &mut self,
        vector_factors: [u32; 2],
        op: ir::BinOp,
        _: Type,
        _: op::Rounding,
        result: llir::RegVec<'_>,
        lhs: llir::OpVec<'_>,
        rhs: llir::OpVec<'_>,
    ) {
        assert_eq!(vector_factors, [1, 1]);

        let (op, is_infix_op) = match op {
            ir::BinOp::Add => ("+", true),
            ir::BinOp::Sub => ("-", true),
            ir::BinOp::Div => ("/", true),
            ir::BinOp::And => ("&", true),
            ir::BinOp::Or => ("|", true),
            ir::BinOp::Lt => ("<", true),
            ir::BinOp::Leq => ("<=", true),
            ir::BinOp::Equals => ("==", true),
            ir::BinOp::Max => ("telamon_op_max", false),
        };

        if is_infix_op {
            unwrap!(writeln!(
                self.buffer,
                "{} = {} {} {};",
                result.c99(),
                lhs.c99(),
                op,
                rhs.c99(),
            ));
        } else {
            unwrap!(writeln!(
                self.buffer,
                "{} = {}({}, {});",
                result.c99(),
                op,
                lhs.c99(),
                rhs.c99(),
            ));
        }
    }

    fn print_unary_op(
        &mut self,
        vector_factors: [u32; 2],
        operator: ir::UnaryOp,
        _: Type,
        result: llir::RegVec<'_>,
        operand: llir::OpVec<'_>,
    ) {
        assert_eq!(vector_factors, [1, 1]);
        unwrap!(write!(self.buffer, "{} = ", result.c99()));
        match operator {
            ir::UnaryOp::Mov => {
                unwrap!(writeln!(self.buffer, "{};", operand.c99()));
            }

            ir::UnaryOp::Cast(t) => {
                unwrap!(write!(
                    self.buffer,
                    "({}){};",
                    Self::get_type(t),
                    operand.c99()
                ));
            }

            ir::UnaryOp::Exp(t) => match t {
                ir::Type::F(32) => {
                    unwrap!(write!(self.buffer, "expf({});", operand.c99()))
                }
                _ => panic!("Exp not implemented for type {}", t),
            },
        };
    }

    fn print_mul(
        &mut self,
        vector_factors: [u32; 2],
        _: Type,
        _: op::Rounding,
        mode: MulMode,
        result: llir::RegVec<'_>,
        op1: llir::OpVec<'_>,
        op2: llir::OpVec<'_>,
    ) {
        assert_ne!(mode, MulMode::High);
        assert_eq!(vector_factors, [1, 1]);
        unwrap!(writeln!(
            self.buffer,
            "{} = {} * {};",
            result.c99(),
            op1.c99(),
            op2.c99()
        ));
    }

    fn print_mad(
        &mut self,
        vector_factors: [u32; 2],
        _: Type,
        _: op::Rounding,
        mode: MulMode,
        result: llir::RegVec<'_>,
        mlhs: llir::OpVec<'_>,
        mrhs: llir::OpVec<'_>,
        arhs: llir::OpVec<'_>,
    ) {
        assert_eq!(vector_factors, [1, 1]);
        assert_ne!(mode, MulMode::High);
        unwrap!(writeln!(
            self.buffer,
            "{} = {} * {} + {};",
            result.c99(),
            mlhs.c99(),
            mrhs.c99(),
            arhs.c99(),
        ));
    }

    fn print_ld(
        &mut self,
        vector_factors: [u32; 2],
        return_type: Type,
        _: MemSpace,
        _: InstFlag,
        result: llir::RegVec<'_>,
        addr: llir::Operand<'_>,
    ) {
        assert_eq!(vector_factors, [1, 1]);
        unwrap!(writeln!(
            self.buffer,
            "{} = *({}*){} ;",
            result.c99(),
            Self::get_type(return_type),
            addr.c99(),
        ));
    }

    fn print_st(
        &mut self,
        vector_factors: [u32; 2],
        val_type: Type,
        _: MemSpace,
        _: InstFlag,
        predicate: Option<llir::Register<'_>>,
        addr: llir::Operand<'_>,
        val: llir::OpVec<'_>,
    ) {
        assert_eq!(vector_factors, [1, 1]);
        if let Some(predicate) = predicate {
            unwrap!(write!(self.buffer, "if ({})", predicate.c99()));
        }
        unwrap!(writeln!(
            self.buffer,
            "*({}*){} = {} ;",
            Self::get_type(val_type),
            addr.c99(),
            val.c99(),
        ));
    }

    fn print_label(&mut self, label_id: &str) {
        unwrap!(writeln!(self.buffer, "LABEL_{}:", label_id));
    }

    fn print_cond_jump(&mut self, label_id: &str, cond: &str) {
        unwrap!(writeln!(
            self.buffer,
            "if({}) goto LABEL_{};",
            cond, label_id
        ));
    }

    fn print_sync(&mut self) {
        unwrap!(writeln!(
            self.buffer,
            "if (pthread_barrier_wait(tid->barrier)) {{printf(\"barrier error\\n\"); return;}}\n",
        ));
    }
}
