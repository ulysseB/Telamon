//! Linera algebra kernels.
#![allow(clippy::many_single_char_names)]
use std::sync::Arc;

pub use crate::compose;
use crate::kernel::Kernel;
use crate::{build_candidate, check_output, create_size, infer_tiling, Scalar};
use ::ndarray::{Array0, Array1, Array2, Array3, ArrayD};
use compose::{
    array_activate_inplace, array_softmax_inplace, matrix_matrix_multiply,
    matrix_vector_multiply, tensor_activate, tensor_add, tensor_elementwise_div,
    tensor_elementwise_mul, tensor_mad, tensor_map, tensor_sum, ActivationFunction,
};
use rand;
use serde::{Deserialize, Serialize};
use telamon::explorer::Candidate;
use telamon::helper::tensor::*;
use telamon::helper::{self, Builder, SignatureBuilder};
use telamon::ir::DimMapScope::Global as GlobalScope;
use telamon::search_space::*;
use telamon::{device, ir};
use utils::*;

/// Computes `z = alpha*x+y`.
pub struct Axpy<'a, S>
where
    S: Scalar,
{
    n: i32,
    x: Tensor<'a, S>,
    y: Tensor<'a, S>,
    z: Tensor<'a, S>,
}

impl<'a, S> Kernel<'a> for Axpy<'a, S>
where
    S: Scalar,
{
    type Parameters = (i32, bool);
    type ExpectedOutput = ArrayD<S>;

    fn name() -> &'static str {
        "axpy"
    }

    fn build_signature<AM>(
        (n, generic): (i32, bool),
        builder: &mut SignatureBuilder<AM>,
    ) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let n_size = create_size(n, "n", generic, builder);
        builder.scalar("alpha", S::one());
        let x = builder.tensor::<S>("x", vec![n_size.clone()], true);
        let y = builder.tensor::<S>("y", vec![n_size.clone()], true);
        let z = builder.tensor::<S>("z", vec![n_size], false);
        Axpy { n, x, y, z }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let tiling = helper::TilingPattern::infer_pattern(self.n as u32, &[1024, 4]);
        let mut builder = Builder::new(signature, ctx.device());

        let x = self.x.load(vec![tiling.clone()], &mut builder);
        let y = self.y.load(vec![tiling], &mut builder);

        let mad = tensor_mad(&mut builder, &x, &"alpha", &y);

        mad.store(&self.z, &mut builder);
        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> ArrayD<S> {
        self.x.read_to_host(context) + self.y.read_to_host(context)
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let z = self.z.read_to_host(context);
        if let Err(invalid) = check_output(&z, expected) {
            Err(format!("Invalid axpy output: {}", invalid))
        } else {
            Ok(())
        }
    }
}

/// Computes `y = A.x`.
pub struct MatVec<'a, S>
where
    S: Scalar,
{
    m: i32,
    n: i32,
    x: Tensor<'a, S>,
    a: Tensor<'a, S>,
    y: Tensor<'a, S>,
}

impl<'a, S> Kernel<'a> for MatVec<'a, S>
where
    S: Scalar,
{
    type Parameters = (i32, i32, bool);
    type ExpectedOutput = Array1<S>;

    fn name() -> &'static str {
        "mv"
    }

    fn build_signature<AM>(
        (m, n, generic): (i32, i32, bool),
        builder: &mut SignatureBuilder<AM>,
    ) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(m, "m", generic, builder);
        let n_size = create_size(n, "n", generic, builder);
        let x = builder.tensor::<S>("x", vec![n_size.clone()], true);
        let a = builder.tensor::<S>("a", vec![m_size.clone(), n_size], true);
        let y = builder.tensor::<S>("y", vec![m_size], false);
        MatVec { m, n, x, a, y }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = helper::TilingPattern::infer_pattern(self.m as u32, &[128, 16]);
        let n_tiling = helper::TilingPattern::infer_pattern(self.n as u32, &[128]);
        let mut builder = Builder::new(signature, ctx.device());
        let x = self.x.load(vec![n_tiling.clone()], &mut builder);
        let a = self.a.load(vec![m_tiling, n_tiling], &mut builder);

        let ax = matrix_vector_multiply(&mut builder, &a, &x);
        ax.store(&self.y, &mut builder);

        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array1<S> {
        let a_shape = (self.m as usize, self.n as usize);
        self.a
            .read_to_host(context)
            .into_shape(a_shape)
            .unwrap()
            .dot(
                &self
                    .x
                    .read_to_host(context)
                    .into_shape(self.n as usize)
                    .unwrap(),
            )
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let y = self
            .y
            .read_to_host(context)
            .into_shape(self.m as usize)
            .unwrap();
        if let Err(invalid) = check_output(&y, expected) {
            Err(format!("Invalid mv output: {}", invalid))
        } else {
            Ok(())
        }
    }
}

/// Computes `y = (alpha*A + beta*B).x`.
pub struct Gesummv<'a, S: Scalar> {
    m: i32,
    n: i32,
    alpha: S,
    beta: S,
    a: Tensor<'a, S>,
    b: Tensor<'a, S>,
    x: Tensor<'a, S>,
    y: Tensor<'a, S>,
}

impl<'a, S: Scalar> Kernel<'a> for Gesummv<'a, S> {
    type Parameters = (i32, i32, bool);
    type ExpectedOutput = Array1<S>;

    fn name() -> &'static str {
        "gesummv"
    }

    fn build_signature<AM>(
        (m, n, generic): (i32, i32, bool),
        builder: &mut SignatureBuilder<AM>,
    ) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(m, "m", generic, builder);
        let n_size = create_size(n, "n", generic, builder);
        let mut rng = rand::thread_rng();
        let alpha = S::gen_random(&mut rng);
        let beta = S::gen_random(&mut rng);
        builder.scalar("alpha", alpha);
        builder.scalar("beta", beta);
        Gesummv {
            m,
            n,
            alpha,
            beta,
            x: builder.tensor::<S>("x", vec![n_size.clone()], true),
            a: builder.tensor::<S>("a", vec![m_size.clone(), n_size.clone()], true),
            b: builder.tensor::<S>("b", vec![m_size.clone(), n_size], true),
            y: builder.tensor::<S>("y", vec![m_size], false),
        }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = helper::TilingPattern::infer_pattern(self.m as u32, &[128, 16]);
        let n_tiling = helper::TilingPattern::infer_pattern(self.n as u32, &[128]);
        let ab_tiling = vec![m_tiling, n_tiling.clone()];

        let mut builder = helper::Builder::new(signature, ctx.device());

        let x = self.x.load(vec![n_tiling], &mut builder);
        let a = self.a.load(ab_tiling.clone(), &mut builder);
        let b = self.b.load(ab_tiling, &mut builder);

        let ax = matrix_vector_multiply(&mut builder, &a, &x);
        let aax = tensor_elementwise_mul(&mut builder, &"alpha", &ax);

        let bx = matrix_vector_multiply(&mut builder, &b, &x);

        let aaxpbbx = tensor_mad(&mut builder, &bx, &"beta", &aax);

        aaxpbbx.store(&self.y, &mut builder);

        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array1<S> {
        let (m, n) = (self.m as usize, self.n as usize);
        let a = unwrap!(self.a.read_to_host(context).into_shape((m, n)));
        let b = unwrap!(self.b.read_to_host(context).into_shape((m, n)));
        let x = unwrap!(self.x.read_to_host(context).into_shape(m));
        a.dot(&x) * self.alpha + b.dot(&x) * self.beta
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let y = unwrap!(self.y.read_to_host(context).into_shape(self.m as usize));
        if let Err(invalid) = check_output(&y, expected) {
            Err(format!("Invalid gesummv output: {}", invalid))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct FusedMMP {
    pub m: i32,
    pub n: i32,
    pub k: i32,
    pub a_stride: u32,
    pub transpose_a: bool,
    pub transpose_b: bool,
    pub generic: bool,
    pub m_tiling: Option<helper::TilingPattern>,
    pub n_tiling: Option<helper::TilingPattern>,
    pub k_tiling: Option<helper::TilingPattern>,
    pub activation_fun: Option<ActivationFunction>,
}

impl FusedMMP {
    pub fn new(m: i32, n: i32, k: i32) -> Self {
        FusedMMP {
            m,
            n,
            k,
            a_stride: 1,
            transpose_a: false,
            transpose_b: false,
            generic: true,
            m_tiling: None,
            n_tiling: None,
            k_tiling: None,
            activation_fun: None,
        }
    }

    pub fn transpose_a(mut self) -> Self {
        self.transpose_a = true;
        self
    }

    pub fn transpose_b(mut self) -> Self {
        self.transpose_b = true;
        self
    }

    pub fn stride_a(mut self, stride: u32) -> Self {
        self.a_stride = stride;
        self
    }

    pub fn activation_fun<F>(mut self, fun: F) -> Self
    where
        F: Into<Option<ActivationFunction>>,
    {
        self.activation_fun = fun.into();
        self
    }

    /// Inline the sizes in the generated code.
    pub fn static_sizes(mut self) -> Self {
        self.generic = false;
        self
    }
}

/// Computes `C = A.B` and applies an activation function to each
/// element of C.
pub struct FusedMM<'a, S: Scalar> {
    pub params: FusedMMP,
    a: Tensor<'a, S>,
    b: Tensor<'a, S>,
    c: Tensor<'a, S>,
}

impl<'a, S: Scalar> Kernel<'a> for FusedMM<'a, S> {
    type Parameters = FusedMMP;
    type ExpectedOutput = Array2<S>;

    fn name() -> &'static str {
        "fused_mm"
    }

    fn build_signature<AM>(params: FusedMMP, builder: &mut SignatureBuilder<AM>) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(params.m, "m", params.generic, builder);
        let n_size = create_size(params.n, "n", params.generic, builder);
        let k_size = create_size(params.k, "k", params.generic, builder);
        let a_dims = vec![m_size.clone(), k_size.clone(), params.a_stride.into()];
        let a = TensorBuilder::new("a", a_dims)
            .doif(params.transpose_a, |b| b.transpose(0, 1))
            .stride_dim(2)
            .finish(builder);
        let b = TensorBuilder::new("b", vec![k_size, n_size.clone()])
            .doif(params.transpose_b, |b| b.transpose(0, 1))
            .finish(builder);
        let c = builder.tensor::<S>("c", vec![m_size, n_size], false);
        FusedMM { params, a, b, c }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = infer_tiling(self.params.m, &self.params.m_tiling, &[32, 4]);
        let n_tiling = infer_tiling(self.params.n, &self.params.n_tiling, &[32, 4]);
        let k_tiling = infer_tiling(self.params.k, &self.params.k_tiling, &[32]);

        let mut builder = helper::Builder::new(signature, ctx.device());

        let a = self.a.load(vec![m_tiling, k_tiling.clone()], &mut builder);
        let b = self.b.load(vec![k_tiling, n_tiling], &mut builder);

        let ab = matrix_matrix_multiply(&mut builder, &a, &b);

        let res = tensor_activate(&mut builder, ab, &self.params.activation_fun);

        res.store(&self.c, &mut builder);

        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array2<S> {
        let a_shape = (self.params.m as usize, self.params.k as usize);
        let b_shape = (self.params.k as usize, self.params.n as usize);
        let a = unwrap!(self.a.read_to_host(context).into_shape(a_shape));
        let b = unwrap!(self.b.read_to_host(context).into_shape(b_shape));

        let mut res = a.dot(&b);
        array_activate_inplace(&mut res, &self.params.activation_fun);

        res
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let c_shape = (self.params.m as usize, self.params.n as usize);
        let c = unwrap!(self.c.read_to_host(context).into_shape(c_shape));
        if let Err(invalid) = check_output(&c, expected) {
            Err(format!("Invalid fused_mm output: {}", invalid))
        } else {
            Ok(())
        }
    }
}

/// Batch transposed matrix-matrix multiplication.
pub struct BatchMM<'a, S>
where
    S: Scalar,
{
    params: BatchMMP,
    a: Tensor<'a, S>,
    b: Tensor<'a, S>,
    c: Tensor<'a, S>,
}

#[derive(Copy, Clone, Deserialize, Serialize)]
pub struct BatchMMP {
    pub m: i32,
    pub n: i32,
    pub k: i32,
    pub batch: i32,
    pub transpose_a: bool,
    pub transpose_b: bool,
    pub batch_b: bool,
    pub generic: bool,
}

impl BatchMMP {
    pub fn new(batch: i32, m: i32, n: i32, k: i32) -> Self {
        BatchMMP {
            m,
            n,
            k,
            batch,
            transpose_a: false,
            transpose_b: false,
            batch_b: true,
            generic: true,
        }
    }

    pub fn transpose_a(mut self) -> Self {
        self.transpose_a = true;
        self
    }

    pub fn transpose_b(mut self) -> Self {
        self.transpose_b = true;
        self
    }

    /// Generate code that is onyl valid for the given sizes. The batch size is still
    /// generic.
    pub fn static_sizes(mut self) -> Self {
        self.generic = false;
        self
    }

    /// Reuse the `B` matrix across the batch.
    pub fn reuse_b(mut self) -> Self {
        self.batch_b = false;
        self
    }
}

impl<'a, S: Scalar> Kernel<'a> for BatchMM<'a, S> {
    type Parameters = BatchMMP;
    type ExpectedOutput = Array3<S>;

    fn name() -> &'static str {
        "batch_mm"
    }

    fn build_signature<AM>(params: BatchMMP, builder: &mut SignatureBuilder<AM>) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(params.m, "m", params.generic, builder);
        let n_size = create_size(params.n, "n", params.generic, builder);
        let k_size = create_size(params.k, "k", params.generic, builder);
        let batch = create_size(params.batch, "batch", true, builder);
        let a_dims = vec![batch.clone(), m_size.clone(), k_size.clone()];
        let a = TensorBuilder::new("a", a_dims)
            .doif(params.transpose_a, |b| b.transpose(1, 2))
            .finish(builder);
        let b = TensorBuilder::new("b", vec![batch.clone(), k_size, n_size.clone()])
            .doif(params.transpose_b, |b| b.transpose(1, 2))
            .doif(!params.batch_b, |b| b.stride_dim(0))
            .finish(builder);
        let c = builder.tensor::<S>("c", vec![batch, m_size, n_size], false);
        BatchMM { params, a, b, c }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = helper::TilingPattern::infer_pattern(self.params.m as u32, &[64]);
        let n_tiling = helper::TilingPattern::infer_pattern(self.params.n as u32, &[64]);
        let k_tiling = helper::TilingPattern::infer_pattern(self.params.k as u32, &[64]);
        let batch_tiling =
            helper::TilingPattern::infer_pattern(self.params.batch as u32, &[128]);
        let mut builder = helper::Builder::new(signature, ctx.device());
        let a_tiling = vec![batch_tiling.clone(), m_tiling, k_tiling.clone()];
        let ld_a = self.a.load(a_tiling, &mut builder);
        let b_tiling = if self.params.batch_b {
            vec![batch_tiling, k_tiling, n_tiling]
        } else {
            vec![k_tiling, n_tiling]
        };
        let ld_b = self.b.load(b_tiling, &mut builder);

        let init_batch = builder.open_mapped_dim(&ld_a[0]);
        let init_dim_m = builder.open_mapped_dim(&ld_a[1]);
        let dim_n = &ld_b[if self.params.batch_b { 2 } else { 1 }];
        let init_dim_n = builder.open_mapped_dim(dim_n);
        let acc_init = builder.mov(&0f32);
        let acc_batch = builder.open_mapped_dim(&init_batch);
        let acc_dim_m = builder.open_mapped_dim(&init_dim_m);
        let acc_dim_n = builder.open_mapped_dim(&init_dim_n);
        let acc_dim_k = builder.open_mapped_dim(&ld_a[2]);
        let a_op = ld_a.dim_map(
            &[&acc_batch, &acc_dim_m, &acc_dim_k],
            GlobalScope(()),
            &mut builder,
        );
        let b_op = {
            let b_dims = [&acc_batch, &acc_dim_k, &acc_dim_n];
            let b_dims = if self.params.batch_b {
                &b_dims
            } else {
                &b_dims[1..]
            };
            ld_b.dim_map(b_dims, GlobalScope(()), &mut builder)
        };
        let acc = builder.mad(&a_op, &b_op, &helper::Reduce(acc_init));
        builder.close_dim(&acc_dim_k);

        let acc = VirtualTensor::new(acc, vec![acc_batch, acc_dim_m, acc_dim_n]);
        let st_c = acc.store(&self.c, &mut builder);

        // Order for correctness.
        builder.order(&st_c.inst(), &acc_dim_k, Order::AFTER);
        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array3<S> {
        let batch = self.params.batch as usize;
        let m = self.params.m as usize;
        let n = self.params.n as usize;
        let k = self.params.k as usize;
        let a = self
            .a
            .read_to_host(context)
            .into_shape((batch, m, k))
            .unwrap();
        let b = self
            .b
            .read_to_host(context)
            .into_shape((batch, k, n))
            .unwrap();
        let mut c = Array3::zeros((batch, m, n));
        for (mut c, (a, b)) in c.outer_iter_mut().zip(a.outer_iter().zip(b.outer_iter()))
        {
            c.assign(&a.dot(&b));
        }
        c
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let batch = self.params.batch as usize;
        let c_shape = (batch, self.params.m as usize, self.params.n as usize);
        let c = self.c.read_to_host(context).into_shape(c_shape).unwrap();
        if let Err(invalid) = check_output(&c, expected) {
            Err(format!("Invalid batched_gemm output: {}", invalid))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct Fused2MMP {
    pub m: i32,
    pub n: i32,
    pub k: i32,
    pub p: i32,
    pub alpha: f32,
    pub beta: f32,
    pub transpose_a: bool,
    pub transpose_b: bool,
    pub transpose_c: bool,
    pub transpose_d: bool,
    pub generic: bool,
    pub m_tiling: Option<helper::TilingPattern>,
    pub n_tiling: Option<helper::TilingPattern>,
    pub k_tiling: Option<helper::TilingPattern>,
    pub p_tiling: Option<helper::TilingPattern>,
    pub activation_fun: Option<ActivationFunction>,
}

impl Fused2MMP {
    pub fn new(m: i32, n: i32, k: i32, p: i32, alpha: f32, beta: f32) -> Self {
        Fused2MMP {
            m,
            n,
            k,
            p,
            alpha,
            beta,
            transpose_a: false,
            transpose_b: false,
            transpose_c: false,
            transpose_d: false,
            generic: true,
            m_tiling: None,
            n_tiling: None,
            k_tiling: None,
            p_tiling: None,
            activation_fun: None,
        }
    }

    pub fn transpose_a(mut self) -> Self {
        self.transpose_a = true;
        self
    }

    pub fn transpose_b(mut self) -> Self {
        self.transpose_b = true;
        self
    }

    pub fn transpose_c(mut self) -> Self {
        self.transpose_c = true;
        self
    }

    pub fn transpose_d(mut self) -> Self {
        self.transpose_d = true;
        self
    }

    pub fn activation_fun<F>(mut self, fun: F) -> Self
    where
        F: Into<Option<ActivationFunction>>,
    {
        self.activation_fun = fun.into();
        self
    }

    /// Inline the sizes in the generated code.
    pub fn static_sizes(mut self) -> Self {
        self.generic = false;
        self
    }
}

/// Computes `E = alpha*A.B.C + beta*D` and applies an activation
/// function to each element of E.
pub struct Fused2MM<'a, S: Scalar> {
    pub params: Fused2MMP,
    a: Tensor<'a, S>,
    b: Tensor<'a, S>,
    c: Tensor<'a, S>,
    d: Tensor<'a, S>,
    e: Tensor<'a, S>,
}

impl<'a, S: Scalar> Kernel<'a> for Fused2MM<'a, S> {
    type Parameters = Fused2MMP;
    type ExpectedOutput = Array2<S>;

    fn name() -> &'static str {
        "fused_2mm"
    }

    fn build_signature<AM>(params: Fused2MMP, builder: &mut SignatureBuilder<AM>) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(params.m, "m", params.generic, builder);
        let n_size = create_size(params.n, "n", params.generic, builder);
        let k_size = create_size(params.k, "k", params.generic, builder);
        let p_size = create_size(params.p, "p", params.generic, builder);

        let a = TensorBuilder::new("a", vec![m_size.clone(), k_size.clone()])
            .doif(params.transpose_a, |b| b.transpose(0, 1))
            .finish(builder);

        let b = TensorBuilder::new("b", vec![k_size, n_size.clone()])
            .doif(params.transpose_b, |b| b.transpose(0, 1))
            .finish(builder);

        let c = TensorBuilder::new("c", vec![n_size.clone(), p_size.clone()])
            .doif(params.transpose_c, |b| b.transpose(0, 1))
            .finish(builder);

        let d = TensorBuilder::new("d", vec![m_size.clone(), p_size.clone()])
            .doif(params.transpose_d, |b| b.transpose(0, 1))
            .finish(builder);

        builder.scalar("alpha", params.alpha);
        builder.scalar("beta", params.beta);

        let e = builder.tensor::<S>("e", vec![m_size, p_size], false);
        Fused2MM {
            params,
            a,
            b,
            c,
            d,
            e,
        }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = infer_tiling(self.params.m, &self.params.m_tiling, &[32, 4]);
        let n_tiling = infer_tiling(self.params.n, &self.params.n_tiling, &[32, 4]);
        let p_tiling = infer_tiling(self.params.p, &self.params.p_tiling, &[32, 4]);
        let k_tiling = infer_tiling(self.params.k, &self.params.k_tiling, &[32]);

        let mut builder = helper::Builder::new(signature, ctx.device());

        let a = self
            .a
            .load(vec![m_tiling.clone(), k_tiling.clone()], &mut builder);
        let b = self.b.load(vec![k_tiling, n_tiling.clone()], &mut builder);
        let c = self.c.load(vec![n_tiling, p_tiling.clone()], &mut builder);
        let d = self.d.load(vec![m_tiling, p_tiling], &mut builder);

        let ab = matrix_matrix_multiply(&mut builder, &a, &b);
        let aab = tensor_elementwise_mul(&mut builder, &"alpha", &ab);
        let aabc = matrix_matrix_multiply(&mut builder, &aab, &c);
        let aabcpbd = tensor_mad(&mut builder, &d, &"beta", &aabc);

        let res = tensor_activate(&mut builder, aabcpbd, &self.params.activation_fun);
        res.store(&self.e, &mut builder);

        let candidate = build_candidate(builder.get(), ctx);

        vec![candidate]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array2<S> {
        let a_shape = (self.params.m as usize, self.params.k as usize);
        let b_shape = (self.params.k as usize, self.params.n as usize);
        let c_shape = (self.params.n as usize, self.params.p as usize);
        let d_shape = (self.params.m as usize, self.params.p as usize);

        let a = unwrap!(self.a.read_to_host(context).into_shape(a_shape));
        let b = unwrap!(self.b.read_to_host(context).into_shape(b_shape));
        let c = unwrap!(self.c.read_to_host(context).into_shape(c_shape));
        let d = unwrap!(self.d.read_to_host(context).into_shape(d_shape));
        let ab = a.dot(&b);
        let aab = ab.mapv(|x| x * S::from(self.params.alpha).unwrap());
        let aabc = aab.dot(&c);
        let bd = d.mapv(|x| x * S::from(self.params.beta).unwrap());
        let mut aabcpbd = aabc + bd;

        array_activate_inplace(&mut aabcpbd, &self.params.activation_fun);

        aabcpbd
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let e_shape = (self.params.m as usize, self.params.p as usize);
        let e = unwrap!(self.e.read_to_host(context).into_shape(e_shape));
        if let Err(invalid) = check_output(&e, expected) {
            Err(format!("Invalid fused_2mm output: {}", invalid))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ResNetCellP {
    pub m: i32,
    pub n: i32,
    pub k: i32,
    pub transpose_a: bool,
    pub transpose_b: bool,
    pub transpose_c: bool,
    pub generic: bool,
    pub m_tiling: Option<helper::TilingPattern>,
    pub n_tiling: Option<helper::TilingPattern>,
    pub k_tiling: Option<helper::TilingPattern>,
    pub activation_fun: Option<ActivationFunction>,
}

impl ResNetCellP {
    pub fn new(m: i32, n: i32, k: i32) -> Self {
        ResNetCellP {
            m,
            n,
            k,
            transpose_a: false,
            transpose_b: false,
            transpose_c: false,
            generic: true,
            m_tiling: None,
            n_tiling: None,
            k_tiling: None,
            activation_fun: None,
        }
    }

    pub fn transpose_a(mut self) -> Self {
        self.transpose_a = true;
        self
    }

    pub fn transpose_b(mut self) -> Self {
        self.transpose_b = true;
        self
    }

    pub fn transpose_c(mut self) -> Self {
        self.transpose_c = true;
        self
    }

    pub fn activation_fun<F>(mut self, fun: F) -> Self
    where
        F: Into<Option<ActivationFunction>>,
    {
        self.activation_fun = fun.into();
        self
    }

    /// Inline the sizes in the generated code.
    pub fn static_sizes(mut self) -> Self {
        self.generic = false;
        self
    }
}

/// Computes `O = activation(activation(A.B).C) + A`
pub struct ResNetCell<'a, S: Scalar> {
    pub params: ResNetCellP,
    a: Tensor<'a, S>,
    b: Tensor<'a, S>,
    c: Tensor<'a, S>,
    o: Tensor<'a, S>,
}

impl<'a, S: Scalar> Kernel<'a> for ResNetCell<'a, S> {
    type Parameters = ResNetCellP;
    type ExpectedOutput = Array2<S>;

    fn name() -> &'static str {
        "resnetcell"
    }

    fn build_signature<AM>(
        params: ResNetCellP,
        builder: &mut SignatureBuilder<AM>,
    ) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(params.m, "m", params.generic, builder);
        let n_size = create_size(params.n, "n", params.generic, builder);
        let k_size = create_size(params.k, "k", params.generic, builder);

        let a = TensorBuilder::new("a", vec![m_size.clone(), k_size.clone()])
            .doif(params.transpose_a, |b| b.transpose(0, 1))
            .finish(builder);
        let b = TensorBuilder::new("b", vec![k_size.clone(), n_size.clone()])
            .doif(params.transpose_b, |b| b.transpose(0, 1))
            .finish(builder);

        let c = TensorBuilder::new("c", vec![n_size, k_size.clone()])
            .doif(params.transpose_c, |b| b.transpose(0, 1))
            .finish(builder);

        let o = builder.tensor::<S>("o", vec![m_size, k_size], false);
        ResNetCell { params, a, b, c, o }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = infer_tiling(self.params.m, &self.params.m_tiling, &[32, 4]);
        let n_tiling = infer_tiling(self.params.n, &self.params.n_tiling, &[32, 4]);
        let k_tiling = infer_tiling(self.params.k, &self.params.k_tiling, &[32]);

        let mut builder = helper::Builder::new(signature, ctx.device());

        let a = self
            .a
            .load(vec![m_tiling.clone(), k_tiling.clone()], &mut builder);
        let b = self
            .b
            .load(vec![k_tiling.clone(), n_tiling.clone()], &mut builder);
        let c = self
            .c
            .load(vec![n_tiling.clone(), k_tiling.clone()], &mut builder);

        let ab = matrix_matrix_multiply(&mut builder, &a, &b);
        let act_ab = tensor_activate::<S>(&mut builder, ab, &self.params.activation_fun);
        let act_ab_c = matrix_matrix_multiply(&mut builder, &act_ab, &c);
        let act_act_ab_c =
            tensor_activate::<S>(&mut builder, act_ab_c, &self.params.activation_fun);

        let a_copy = a.duplicate(&mut builder);

        let act_act_ab_c_pa = tensor_add(&mut builder, &act_act_ab_c, &a_copy);

        act_act_ab_c_pa.store(&self.o, &mut builder);

        let candidate = build_candidate(builder.get(), ctx);

        vec![candidate]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array2<S> {
        let a_shape = (self.params.m as usize, self.params.k as usize);
        let b_shape = (self.params.k as usize, self.params.n as usize);
        let c_shape = (self.params.n as usize, self.params.k as usize);

        let a = unwrap!(self.a.read_to_host(context).into_shape(a_shape));
        let b = unwrap!(self.b.read_to_host(context).into_shape(b_shape));
        let c = unwrap!(self.c.read_to_host(context).into_shape(c_shape));

        let mut ab = a.dot(&b);
        array_activate_inplace(&mut ab, &self.params.activation_fun);

        let mut act_ab_c = ab.dot(&c);
        array_activate_inplace(&mut act_ab_c, &self.params.activation_fun);

        act_ab_c + a
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let o_shape = (self.params.m as usize, self.params.k as usize);
        let o = unwrap!(self.o.read_to_host(context).into_shape(o_shape));
        if let Err(invalid) = check_output(&o, expected) {
            Err(format!("Invalid resnetcell output: {}", invalid))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ResNetCellTopHalfP {
    pub mm_params: FusedMMP,
}

impl ResNetCellTopHalfP {
    pub fn new<F>(m: i32, n: i32, k: i32, activation_fun: F) -> Self
    where
        F: Into<Option<ActivationFunction>>,
    {
        ResNetCellTopHalfP {
            mm_params: FusedMMP::new(m, n, k).activation_fun(activation_fun),
        }
    }
}

/// Computes `O = activation(A.B)`
pub struct ResNetCellTopHalf<'a, S: Scalar> {
    fmmp: FusedMM<'a, S>,
}

impl<'a, S: Scalar> Kernel<'a> for ResNetCellTopHalf<'a, S> {
    type Parameters = ResNetCellTopHalfP;
    type ExpectedOutput = Array2<S>;

    fn name() -> &'static str {
        "resnetcelltophalf"
    }

    fn build_signature<AM>(
        params: ResNetCellTopHalfP,
        builder: &mut SignatureBuilder<AM>,
    ) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        ResNetCellTopHalf {
            fmmp: FusedMM::build_signature(params.mm_params, builder),
        }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        self.fmmp.build_body(signature, ctx)
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array2<S> {
        self.fmmp.get_expected_output(context)
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        self.fmmp.check_result(expected, context)
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct ResNetCellBottomHalfP {
    pub m: i32,
    pub n: i32,
    pub k: i32,
    pub transpose_actab: bool,
    pub transpose_a: bool,
    pub transpose_c: bool,
    pub transpose_o: bool,
    pub generic: bool,
    pub m_tiling: Option<helper::TilingPattern>,
    pub n_tiling: Option<helper::TilingPattern>,
    pub k_tiling: Option<helper::TilingPattern>,
    pub activation_fun: Option<ActivationFunction>,
}

impl ResNetCellBottomHalfP {
    pub fn new<F>(m: i32, n: i32, k: i32, activation_fun: F) -> Self
    where
        F: Into<Option<ActivationFunction>>,
    {
        ResNetCellBottomHalfP {
            m,
            n,
            k,
            transpose_actab: false,
            transpose_a: false,
            transpose_c: false,
            transpose_o: false,
            generic: true,
            m_tiling: None,
            n_tiling: None,
            k_tiling: None,
            activation_fun: activation_fun.into(),
        }
    }

    pub fn transpose_actab(mut self) -> Self {
        self.transpose_actab = true;
        self
    }

    pub fn transpose_a(mut self) -> Self {
        self.transpose_a = true;
        self
    }

    pub fn transpose_c(mut self) -> Self {
        self.transpose_c = true;
        self
    }

    pub fn transpose_o(mut self) -> Self {
        self.transpose_o = true;
        self
    }

    pub fn activation_fun<F>(mut self, fun: F) -> Self
    where
        F: Into<Option<ActivationFunction>>,
    {
        self.activation_fun = fun.into();
        self
    }

    /// Inline the sizes in the generated code.
    pub fn static_sizes(mut self) -> Self {
        self.generic = false;
        self
    }
}

/// Computes `O = activation(ACTAB.C)+A`
pub struct ResNetCellBottomHalf<'a, S: Scalar> {
    pub params: ResNetCellBottomHalfP,
    act_ab: Tensor<'a, S>,
    c: Tensor<'a, S>,
    a: Tensor<'a, S>,
    o: Tensor<'a, S>,
}

impl<'a, S: Scalar> Kernel<'a> for ResNetCellBottomHalf<'a, S> {
    type Parameters = ResNetCellBottomHalfP;
    type ExpectedOutput = Array2<S>;

    fn name() -> &'static str {
        "resnetcellbottomhalf"
    }

    fn build_signature<AM>(
        params: ResNetCellBottomHalfP,
        builder: &mut SignatureBuilder<AM>,
    ) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(params.m, "m", params.generic, builder);
        let n_size = create_size(params.n, "n", params.generic, builder);
        let k_size = create_size(params.k, "k", params.generic, builder);

        let act_ab = TensorBuilder::new("act_ab", vec![m_size.clone(), n_size.clone()])
            .doif(params.transpose_actab, |b| b.transpose(0, 1))
            .finish(builder);
        let a = TensorBuilder::new("a", vec![m_size.clone(), k_size.clone()])
            .doif(params.transpose_a, |b| b.transpose(0, 1))
            .finish(builder);
        let c = TensorBuilder::new("c", vec![n_size, k_size.clone()])
            .doif(params.transpose_c, |b| b.transpose(0, 1))
            .finish(builder);
        let o = builder.tensor::<S>("o", vec![m_size, k_size], false);

        ResNetCellBottomHalf {
            params,
            act_ab,
            c,
            a,
            o,
        }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = infer_tiling(self.params.m, &self.params.m_tiling, &[32, 4]);
        let n_tiling = infer_tiling(self.params.n, &self.params.n_tiling, &[32, 4]);
        let k_tiling = infer_tiling(self.params.k, &self.params.k_tiling, &[32, 4]);

        let mut builder = helper::Builder::new(signature, ctx.device());

        let act_ab = self
            .act_ab
            .load(vec![m_tiling.clone(), n_tiling.clone()], &mut builder);
        let c = self.c.load(vec![n_tiling, k_tiling.clone()], &mut builder);
        let a = self.a.load(vec![m_tiling, k_tiling], &mut builder);

        let act_ab_c = matrix_matrix_multiply(&mut builder, &act_ab, &c);
        let act_act_ab_c =
            tensor_activate(&mut builder, act_ab_c, &self.params.activation_fun);

        let act_act_ab_c_pa = tensor_add(&mut builder, &act_act_ab_c, &a);

        act_act_ab_c_pa.store(&self.o, &mut builder);

        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array2<S> {
        let act_ab_shape = (self.params.m as usize, self.params.n as usize);
        let c_shape = (self.params.n as usize, self.params.k as usize);
        let a_shape = (self.params.m as usize, self.params.k as usize);

        let act_ab = unwrap!(self.act_ab.read_to_host(context).into_shape(act_ab_shape));
        let c = unwrap!(self.c.read_to_host(context).into_shape(c_shape));
        let a = unwrap!(self.a.read_to_host(context).into_shape(a_shape));

        let mut act_ab_c = act_ab.dot(&c);
        array_activate_inplace(&mut act_ab_c, &self.params.activation_fun);
        let act_act_ab_c_pa = act_ab_c + a;

        act_act_ab_c_pa
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let o_shape = (self.params.n as usize, self.params.k as usize);
        let o = unwrap!(self.o.read_to_host(context).into_shape(o_shape));
        if let Err(invalid) = check_output(&o, expected) {
            Err(format!("Invalid resnetcellbottomhalf output: {}", invalid))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct TransformerCellP {
    pub m: i32,
    pub n: i32,
    pub p: i32,
    pub r: i32,
    pub transpose_q: bool,
    pub transpose_k: bool,
    pub transpose_v: bool,
    pub generic: bool,
    pub m_tiling: Option<helper::TilingPattern>,
    pub n_tiling: Option<helper::TilingPattern>,
    pub p_tiling: Option<helper::TilingPattern>,
    pub r_tiling: Option<helper::TilingPattern>,
}

impl TransformerCellP {
    pub fn new(m: i32, n: i32, p: i32, r: i32) -> Self {
        TransformerCellP {
            m,
            n,
            p,
            r,
            transpose_q: false,
            transpose_k: false,
            transpose_v: false,
            generic: true,
            m_tiling: None,
            n_tiling: None,
            p_tiling: None,
            r_tiling: None,
        }
    }

    pub fn transpose_q(mut self) -> Self {
        self.transpose_q = true;
        self
    }

    pub fn transpose_k(mut self) -> Self {
        self.transpose_k = true;
        self
    }

    pub fn transpose_v(mut self) -> Self {
        self.transpose_v = true;
        self
    }

    /// Inline the sizes in the generated code.
    pub fn static_sizes(mut self) -> Self {
        self.generic = false;
        self
    }
}

/// Computes `O = softmax(scale(Q.K)).V`
pub struct TransformerCell<'a, S: Scalar> {
    pub params: TransformerCellP,
    q: Tensor<'a, S>,
    k: Tensor<'a, S>,
    v: Tensor<'a, S>,
    o: Tensor<'a, S>,
}

impl<'a, S: Scalar> TransformerCell<'a, S> {
    fn scaling_factor(&self) -> S {
        S::from(1f64 / f64::sqrt(self.params.p as f64 * self.params.n as f64)).unwrap()
    }
}

impl<'a, S: Scalar> Kernel<'a> for TransformerCell<'a, S> {
    type Parameters = TransformerCellP;
    type ExpectedOutput = Array2<S>;

    fn name() -> &'static str {
        "transformercell"
    }

    fn build_signature<AM>(
        params: TransformerCellP,
        builder: &mut SignatureBuilder<AM>,
    ) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(params.m, "m", params.generic, builder);
        let n_size = create_size(params.n, "n", params.generic, builder);
        let p_size = create_size(params.p, "p", params.generic, builder);
        let r_size = create_size(params.r, "r", params.generic, builder);

        let q = TensorBuilder::new("q", vec![m_size.clone(), p_size.clone()])
            .doif(params.transpose_q, |b| b.transpose(0, 1))
            .finish(builder);

        let k = TensorBuilder::new("k", vec![p_size, n_size.clone()])
            .doif(params.transpose_k, |b| b.transpose(0, 1))
            .finish(builder);

        let v = TensorBuilder::new("v", vec![n_size, r_size.clone()])
            .doif(params.transpose_v, |b| b.transpose(0, 1))
            .finish(builder);

        let o = builder.tensor::<S>("o", vec![m_size, r_size], false);
        TransformerCell { params, q, k, v, o }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = infer_tiling(self.params.m, &self.params.m_tiling, &[32, 4]);
        let n_tiling = infer_tiling(self.params.n, &self.params.n_tiling, &[32, 4]);
        let p_tiling = infer_tiling(self.params.p, &self.params.p_tiling, &[32, 4]);
        let r_tiling = infer_tiling(self.params.r, &self.params.r_tiling, &[32, 4]);

        let mut builder = helper::Builder::new(signature, ctx.device());

        let q = self.q.load(vec![m_tiling, p_tiling.clone()], &mut builder);
        let k = self
            .k
            .load(vec![p_tiling.clone(), n_tiling.clone()], &mut builder);
        let v = self.v.load(vec![n_tiling.clone(), r_tiling], &mut builder);

        let qk = matrix_matrix_multiply(&mut builder, &q, &k);
        let qk_scaled = tensor_elementwise_mul(&mut builder, &self.scaling_factor(), &qk);
        let qk_scaled_exp = tensor_map(&mut builder, &qk_scaled, |telem, builder| {
            builder.exp(telem)
        });

        let sum = tensor_sum(&mut builder, &qk_scaled_exp);
        let sum_op = sum.dim_map(&[], ir::DimMapScope::Global(()), &mut builder);

        let q_copy = q.duplicate(&mut builder);
        let k_copy = k.duplicate(&mut builder);

        let qk_copy = matrix_matrix_multiply(&mut builder, &q_copy, &k_copy);
        let qk_copy_scaled =
            tensor_elementwise_mul(&mut builder, &self.scaling_factor(), &qk_copy);
        let qk_copy_scaled_exp =
            tensor_map(&mut builder, &qk_copy_scaled, |telem, builder| {
                builder.exp(telem)
            });

        let qk_scaled_softmax =
            tensor_elementwise_div(&mut builder, &qk_copy_scaled_exp, &sum_op);

        let res = matrix_matrix_multiply(&mut builder, &qk_scaled_softmax, &v);

        res.store(&self.o, &mut builder);

        let candidate = build_candidate(builder.get(), ctx);

        vec![candidate]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array2<S> {
        let q_shape = (self.params.m as usize, self.params.p as usize);
        let k_shape = (self.params.p as usize, self.params.n as usize);
        let v_shape = (self.params.n as usize, self.params.r as usize);

        let q = unwrap!(self.q.read_to_host(context).into_shape(q_shape));
        let k = unwrap!(self.k.read_to_host(context).into_shape(k_shape));
        let v = unwrap!(self.v.read_to_host(context).into_shape(v_shape));

        let mut qk = q.dot(&k);
        qk.mapv_inplace(|c| c * self.scaling_factor());
        array_softmax_inplace(&mut qk);

        qk.dot(&v)
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let o_shape = (self.params.m as usize, self.params.r as usize);
        let o = unwrap!(self.o.read_to_host(context).into_shape(o_shape));
        if let Err(invalid) = check_output(&o, expected) {
            Err(format!("Invalid transformercell output: {}", invalid))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct TransformerCellTopHalfP {
    pub m: i32,
    pub n: i32,
    pub p: i32,
    pub transpose_q: bool,
    pub transpose_k: bool,
    pub generic: bool,
    pub m_tiling: Option<helper::TilingPattern>,
    pub n_tiling: Option<helper::TilingPattern>,
    pub p_tiling: Option<helper::TilingPattern>,
}

impl TransformerCellTopHalfP {
    pub fn new(m: i32, n: i32, p: i32) -> Self {
        TransformerCellTopHalfP {
            m,
            n,
            p,
            transpose_q: false,
            transpose_k: false,
            generic: true,
            m_tiling: None,
            n_tiling: None,
            p_tiling: None,
        }
    }

    pub fn transpose_q(mut self) -> Self {
        self.transpose_q = true;
        self
    }

    pub fn transpose_k(mut self) -> Self {
        self.transpose_k = true;
        self
    }

    /// Inline the sizes in the generated code.
    pub fn static_sizes(mut self) -> Self {
        self.generic = false;
        self
    }
}

/// Computes `O = elementwise_exp(scale(Q.V))` and `S =
/// scalar_sum(O)`, which corresponds to the first half of the
/// computation of `TransformerCell` (break in the middle of softmax).
pub struct TransformerCellTopHalf<'a, S: Scalar> {
    pub params: TransformerCellTopHalfP,
    q: Tensor<'a, S>,
    k: Tensor<'a, S>,
    o: Tensor<'a, S>,
    s: Tensor<'a, S>,
}

impl<'a, S: Scalar> TransformerCellTopHalf<'a, S> {
    fn scaling_factor(&self) -> S {
        S::from(1f64 / f64::sqrt(self.params.p as f64 * self.params.n as f64)).unwrap()
    }
}

impl<'a, S: Scalar> Kernel<'a> for TransformerCellTopHalf<'a, S> {
    type Parameters = TransformerCellTopHalfP;
    type ExpectedOutput = (Array2<S>, S);

    fn name() -> &'static str {
        "transformercelltophalf"
    }

    fn build_signature<AM>(
        params: TransformerCellTopHalfP,
        builder: &mut SignatureBuilder<AM>,
    ) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(params.m, "m", params.generic, builder);
        let n_size = create_size(params.n, "n", params.generic, builder);
        let p_size = create_size(params.p, "p", params.generic, builder);

        let q = TensorBuilder::new("q", vec![m_size.clone(), p_size.clone()])
            .doif(params.transpose_q, |b| b.transpose(0, 1))
            .finish(builder);

        let k = TensorBuilder::new("k", vec![p_size, n_size.clone()])
            .doif(params.transpose_k, |b| b.transpose(0, 1))
            .finish(builder);

        let o = builder.tensor::<S>("o", vec![m_size, n_size], false);
        let s = builder.tensor::<S>("s", vec![], false);

        TransformerCellTopHalf { params, q, k, o, s }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = infer_tiling(self.params.m, &self.params.m_tiling, &[32, 4]);
        let n_tiling = infer_tiling(self.params.n, &self.params.n_tiling, &[32, 4]);
        let p_tiling = infer_tiling(self.params.p, &self.params.p_tiling, &[32]);

        let mut builder = helper::Builder::new(signature, ctx.device());

        let q = self.q.load(vec![m_tiling, p_tiling.clone()], &mut builder);
        let k = self
            .k
            .load(vec![p_tiling.clone(), n_tiling.clone()], &mut builder);

        let qk = matrix_matrix_multiply(&mut builder, &q, &k);
        let qk_scaled = tensor_elementwise_mul(&mut builder, &self.scaling_factor(), &qk);
        let qk_scaled_exp = tensor_map(&mut builder, &qk_scaled, |telem, builder| {
            builder.exp(telem)
        });

        let sum = tensor_sum(&mut builder, &qk_scaled_exp);

        qk_scaled_exp.store(&self.o, &mut builder);
        sum.store(&self.s, &mut builder);

        let candidate = build_candidate(builder.get(), ctx);

        vec![candidate]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Self::ExpectedOutput {
        let q_shape = (self.params.m as usize, self.params.p as usize);
        let k_shape = (self.params.p as usize, self.params.n as usize);

        let q = unwrap!(self.q.read_to_host(context).into_shape(q_shape));
        let k = unwrap!(self.k.read_to_host(context).into_shape(k_shape));

        let mut qk = q.dot(&k);
        qk.mapv_inplace(|c| S::exp(c * self.scaling_factor()));
        let sum = qk.scalar_sum();

        (qk, sum)
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let o_shape = (self.params.m as usize, self.params.n as usize);
        let o = unwrap!(self.o.read_to_host(context).into_shape(o_shape));
        let s = unwrap!(self.s.read_to_host(context).into_shape(()));

        if let Err(invalid) = check_output(&o, &expected.0) {
            Err(format!(
                "Invalid transformercelltophalf matrix output: {}",
                invalid
            ))
        } else if let Err(invalid) = check_output(&s, &Array0::from_elem((), expected.1))
        {
            Err(format!(
                "Invalid transformercelltophalf sum output: {}",
                invalid
            ))
        } else {
            Ok(())
        }
    }
}

#[derive(Clone, Deserialize, Serialize)]
pub struct TransformerCellBottomHalfP {
    pub m: i32,
    pub n: i32,
    pub r: i32,
    pub transpose_qk_scexp: bool,
    pub transpose_v: bool,
    pub generic: bool,
    pub m_tiling: Option<helper::TilingPattern>,
    pub n_tiling: Option<helper::TilingPattern>,
    pub r_tiling: Option<helper::TilingPattern>,
}

impl TransformerCellBottomHalfP {
    pub fn new(m: i32, n: i32, r: i32) -> Self {
        TransformerCellBottomHalfP {
            m,
            n,
            r,
            transpose_qk_scexp: false,
            transpose_v: false,
            generic: true,
            m_tiling: None,
            n_tiling: None,
            r_tiling: None,
        }
    }

    pub fn transpose_qk_scexp(mut self) -> Self {
        self.transpose_qk_scexp = true;
        self
    }

    pub fn transpose_v(mut self) -> Self {
        self.transpose_v = true;
        self
    }

    /// Inline the sizes in the generated code.
    pub fn static_sizes(mut self) -> Self {
        self.generic = false;
        self
    }
}

/// Computes `O = (1/s_exp * QKSCEXP).V`, which corresponds to the
/// second half of the computation of `TransformerCell` (break in the
/// middle of softmax).
pub struct TransformerCellBottomHalf<'a, S: Scalar> {
    pub params: TransformerCellBottomHalfP,
    s_exp: Tensor<'a, S>,
    qk_scexp: Tensor<'a, S>,
    v: Tensor<'a, S>,
    o: Tensor<'a, S>,
}

impl<'a, S: Scalar> Kernel<'a> for TransformerCellBottomHalf<'a, S> {
    type Parameters = TransformerCellBottomHalfP;
    type ExpectedOutput = Array2<S>;

    fn name() -> &'static str {
        "transformercellbottomhalf"
    }

    fn build_signature<AM>(
        params: TransformerCellBottomHalfP,
        builder: &mut SignatureBuilder<AM>,
    ) -> Self
    where
        AM: device::ArgMap<'a> + device::Context,
    {
        let m_size = create_size(params.m, "m", params.generic, builder);
        let n_size = create_size(params.n, "n", params.generic, builder);
        let r_size = create_size(params.r, "r", params.generic, builder);

        let s_exp = builder.tensor::<S>("s_exp", vec![], true);

        let qk_scexp =
            TensorBuilder::new("qk_scexp", vec![m_size.clone(), n_size.clone()])
                .doif(params.transpose_qk_scexp, |b| b.transpose(0, 1))
                .finish(builder);

        let v = TensorBuilder::new("v", vec![n_size, r_size.clone()])
            .doif(params.transpose_v, |b| b.transpose(0, 1))
            .finish(builder);

        let o = builder.tensor::<S>("o", vec![m_size, r_size], false);

        TransformerCellBottomHalf {
            params,
            s_exp,
            qk_scexp,
            v,
            o,
        }
    }

    fn build_body<'b>(
        &self,
        signature: Arc<ir::Signature>,
        ctx: &'b dyn device::Context,
    ) -> Vec<Candidate> {
        let m_tiling = infer_tiling(self.params.m, &self.params.m_tiling, &[32, 4]);
        let r_tiling = infer_tiling(self.params.n, &self.params.n_tiling, &[32, 4]);
        let n_tiling = infer_tiling(self.params.r, &self.params.r_tiling, &[32]);

        let mut builder = helper::Builder::new(signature, ctx.device());

        let s_exp = self.s_exp.load(vec![], &mut builder);
        let s_exp_op = s_exp.dim_map(&[], GlobalScope(()), &mut builder);

        let qk_scexp = self
            .qk_scexp
            .load(vec![m_tiling, r_tiling.clone()], &mut builder);

        let v = self
            .v
            .load(vec![r_tiling.clone(), n_tiling.clone()], &mut builder);

        let qk_scexp_div = tensor_elementwise_div(&mut builder, &qk_scexp, &s_exp_op);
        let o = matrix_matrix_multiply(&mut builder, &qk_scexp_div, &v);

        o.store(&self.o, &mut builder);

        let candidate = build_candidate(builder.get(), ctx);

        vec![candidate]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Self::ExpectedOutput {
        let qk_scexp_shape = (self.params.m as usize, self.params.n as usize);
        let v_shape = (self.params.n as usize, self.params.r as usize);

        let mut qk_scexp = unwrap!(self
            .qk_scexp
            .read_to_host(context)
            .into_shape(qk_scexp_shape));
        let v = unwrap!(self.v.read_to_host(context).into_shape(v_shape));
        let s_exp = unwrap!(self.s_exp.read_to_host(context).into_shape(()));

        qk_scexp.mapv_inplace(|c| c / s_exp[[]]);

        qk_scexp.dot(&v)
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &dyn device::Context,
    ) -> Result<(), String> {
        let o_shape = (self.params.m as usize, self.params.r as usize);
        let o = unwrap!(self.o.read_to_host(context).into_shape(o_shape));

        if let Err(invalid) = check_output(&o, &expected) {
            Err(format!(
                "Invalid transformercellbottomhalf output: {}",
                invalid
            ))
        } else {
            Ok(())
        }
    }
}
