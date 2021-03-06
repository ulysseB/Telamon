//! Linera algebra kernels.
#![allow(clippy::many_single_char_names)]
use std::sync::Arc;

pub use crate::compose::ActivationFunction;
use crate::compose::{
    matrix_matrix_multiply, matrix_vector_multiply, tensor_elementwise_mul, tensor_mad,
};
use crate::kernel::Kernel;
use crate::{build_candidate, check_output, create_size, infer_tiling, Scalar};
use ::ndarray::{Array1, Array2, Array3, ArrayD};
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

        if let Some(activation_fun) = &self.params.activation_fun {
            let res = activation_fun.apply::<S>(&mut builder, &ab);
            res.store(&self.c, &mut builder);
        } else {
            ab.store(&self.c, &mut builder);
        }

        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &dyn device::Context) -> Array2<S> {
        let a_shape = (self.params.m as usize, self.params.k as usize);
        let b_shape = (self.params.k as usize, self.params.n as usize);
        let a = unwrap!(self.a.read_to_host(context).into_shape(a_shape));
        let b = unwrap!(self.b.read_to_host(context).into_shape(b_shape));
        let mut res = a.dot(&b);

        match self.params.activation_fun {
            Some(ActivationFunction::ReLU) => {
                res.mapv_inplace(|c| c.max(S::zero()));
            }

            Some(ActivationFunction::Sigmoid) => {
                let one = S::one();
                res.mapv_inplace(|c| one / (one + S::exp(c)));
            }

            None => {}
        };

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

        if let Some(activation_fun) = &self.params.activation_fun {
            let res = activation_fun.apply::<S>(&mut builder, &aabcpbd);
            res.store(&self.e, &mut builder);
        } else {
            aabcpbd.store(&self.e, &mut builder);
        }

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

        match self.params.activation_fun {
            Some(ActivationFunction::ReLU) => {
                aabcpbd.mapv_inplace(|c| c.max(S::zero()));
            }

            Some(ActivationFunction::Sigmoid) => {
                let one = S::one();
                aabcpbd.mapv_inplace(|c| one / (one + S::exp(c)));
            }

            None => {}
        };

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
