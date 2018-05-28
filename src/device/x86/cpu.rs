//! Describes CUDA-enabled GPUs.
use device;
use codegen::Function;
use ir::{self, Type};
use model::{self, HwPressure};
use num_cpus;
use search_space::{DimKind, SearchSpace};
use std::io::Write;
use utils::*;


/// Represents CUDA GPUs.
#[derive(Clone)]
pub struct Cpu {
    /// The name of the CPU.
    pub name: String,
}

impl Cpu {
    pub fn dummy_cpu() -> Self {
        Cpu {
            name: String::from("x86"),
        }
    }
}


impl device::Device for Cpu {
    fn print(&self, _fun: &Function, out: &mut Write) { unwrap!(write!(out, "Basic CPU")); }

    fn is_valid_type(&self, t: &Type) -> bool {
        match *t {
            Type::I(i) | Type::F(i) => i == 32 || i == 64,
            Type::Void | Type::PtrTo(_) => true,
        }
    }

    // block dimensions do not make sense on cpu
    fn max_block_dims(&self) -> u32 { 0 }

    fn max_threads(&self) -> u32 { (num_cpus::get() ) as u32 }

    fn max_unrolling(&self) -> u32 { 512 }

    fn can_vectorize(&self, _dim: &ir::Dimension, _op: &ir::Operator) -> bool {false}

    fn shared_mem(&self) -> u32 {0}

    fn supports_nc_access(&self) -> bool {false}

    fn supports_l1_access(&self) -> bool {true}

    fn supports_l2_access(&self) -> bool {true}

    fn name(&self) -> &str { &self.name }

    fn add_block_overhead(&self, _predicated_dims_size: u64,
                          _max_threads_per_blocks: u64,
                          _pressure: &mut HwPressure) {
    }

    fn lower_type(&self, t: ir::Type, _space: &SearchSpace) -> Option<ir::Type> {
        Some(t)
    }

    fn hw_pressure(&self, _space: &SearchSpace,
                   _dim_sizes: &HashMap<ir::dim::Id, u32>,
                   _nesting: &HashMap<ir::BBId, model::Nesting>,
                   _bb: &ir::BasicBlock,
                   _: &device::Context) -> model::HwPressure {
        // TODO(model): implement model
        model::HwPressure::zero(self)
    }

    fn loop_iter_pressure(&self, _kind: DimKind) -> (HwPressure, HwPressure) {
        //TODO(model): implement minimal model
        (model::HwPressure::zero(self), model::HwPressure::zero(self))
    }

    fn thread_rates(&self) -> HwPressure {
        //TODO(model): implement minimal model
        model::HwPressure::new(1.0, vec![]) }

    fn block_rates(&self) -> HwPressure {
        //TODO(model): implement minimal model
        model::HwPressure::new(1.0, vec![]) 
    }

    fn total_rates(&self) -> HwPressure {
        //TODO(model): implement minimal model
        model::HwPressure::new(1.0, vec![]) 
    }

    fn bottlenecks(&self) -> &[&'static str] {
        &[]
    }

    fn block_parallelism(&self, _space: &SearchSpace) -> u32 {
        1
    }

    fn additive_indvar_pressure(&self, _t: &ir::Type) -> HwPressure {
        //TODO(model): implement minimal model
        model::HwPressure::zero(self) 
    }

    fn multiplicative_indvar_pressure(&self, _t: &ir::Type) -> HwPressure {
        //TODO(model): implement minimal model
        model::HwPressure::zero(self) 
    }
}
