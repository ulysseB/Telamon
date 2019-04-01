//! Linera algebra kernels.
#![allow(clippy::many_single_char_names)]

use crate::kernel::Kernel;
use crate::{build_candidate, create_size, infer_tiling, Scalar};
#[rustfmt::skip]
use ::ndarray::{Array1, Array2, Array3, ArrayD};
use itertools::Itertools;
use rand;
use serde::{Deserialize, Serialize};
use telamon::helper::tensor::*;
use telamon::helper::{self, Builder, SignatureBuilder};
use telamon::ir::DimMapScope::Global as GlobalScope;
use telamon::search_space::Candidate;
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
        signature: &'b ir::Signature,
        ctx: &'b device::Context,
    ) -> Vec<Candidate<'b>> {
        let tiling = helper::TilingPattern::infer_pattern(self.n as u32, &[1024, 4]);
        let mut builder = Builder::new(signature, ctx.device());

        let ld_x = self.x.load(vec![tiling.clone()], &mut builder);
        let ld_y = self.y.load(vec![tiling], &mut builder);
        let mad_dim = builder.open_mapped_dim(&ld_x[0]);
        let x_op = ld_x.dim_map(&[&mad_dim], GlobalScope(()), &mut builder);
        let y_op = ld_y.dim_map(&[&mad_dim], GlobalScope(()), &mut builder);
        let mad = VirtualTensor::new(builder.mad(&x_op, &"alpha", &y_op), vec![mad_dim]);
        mad.store(&self.z, &mut builder);
        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &device::Context) -> ArrayD<S> {
        self.x.read_to_host(context) + self.y.read_to_host(context)
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &device::Context,
    ) -> Result<(), String> {
        let z = self.z.read_to_host(context);
        if z.iter()
            .zip_eq(expected)
            .any(|(&z0, &z1)| (z0 - z1).is_err_ok())
        {
            let x = self.x.read_to_host(context);
            let y = self.y.read_to_host(context);
            Err(format!(
                "expected: {}, got {} with x = {} and y = {}",
                expected, z, x, y
            ))
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
        signature: &'b ir::Signature,
        ctx: &'b device::Context,
    ) -> Vec<Candidate<'b>> {
        let m_tiling = helper::TilingPattern::infer_pattern(self.m as u32, &[128, 16]);
        let n_tiling = helper::TilingPattern::infer_pattern(self.n as u32, &[128]);
        let mut builder = Builder::new(&signature, ctx.device());
        let ld_x = self.x.load(vec![n_tiling.clone()], &mut builder);
        let ld_a = self.a.load(vec![m_tiling, n_tiling], &mut builder);
        let init_dim_m = builder.open_mapped_dim(&ld_a[0]);
        let init = builder.mov(&0f32);
        let acc_dim_m = builder.open_mapped_dim(&init_dim_m);
        let acc_dim_n = builder.open_mapped_dim(&ld_x[0]);
        let a_op = ld_a.dim_map(&[&acc_dim_m, &acc_dim_n], GlobalScope(()), &mut builder);
        let x_op = ld_x.dim_map(&[&acc_dim_n], GlobalScope(()), &mut builder);
        let acc = builder.mad(&a_op, &x_op, &helper::Reduce(init));
        builder.close_dim(&acc_dim_n);
        let sum = VirtualTensor::new(acc, vec![acc_dim_m]);
        let st_y = sum.store(&self.y, &mut builder);

        builder.order(&acc_dim_n, &st_y.inst(), Order::BEFORE);
        // TODO(search_space): explore inst flags
        builder.action(Action::InstFlag(ld_x.inst(), InstFlag::CACHE_GLOBAL));
        builder.action(Action::InstFlag(ld_a.inst(), InstFlag::CACHE_GLOBAL));
        builder.action(Action::InstFlag(st_y.inst(), InstFlag::NO_CACHE));
        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &device::Context) -> Array1<S> {
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
        context: &device::Context,
    ) -> Result<(), String> {
        let y = self
            .y
            .read_to_host(context)
            .into_shape(self.m as usize)
            .unwrap();
        if y.iter()
            .zip_eq(expected)
            .any(|(&y0, &y1)| (y0 - y1).is_err_ok())
        {
            let x = self.x.read_to_host(context);
            let a = self.a.read_to_host(context);
            Err(format!(
                "expected: {}, got {} with x = {} and a = {}",
                expected, y, x, a
            ))
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
        signature: &'b ir::Signature,
        ctx: &'b device::Context,
    ) -> Vec<Candidate<'b>> {
        let m_tiling = helper::TilingPattern::infer_pattern(self.m as u32, &[128, 16]);
        let n_tiling = helper::TilingPattern::infer_pattern(self.n as u32, &[128]);
        let mut builder = helper::Builder::new(&signature, ctx.device());
        let ld_x = self.x.load(vec![n_tiling.clone()], &mut builder);
        let ab_tiling = vec![m_tiling, n_tiling];
        let ld_a = self.a.load(ab_tiling.clone(), &mut builder);
        let ld_b = self.b.load(ab_tiling, &mut builder);
        let init_dim_m = builder.open_mapped_dim(&ld_a[0]);
        let init_a = builder.mov(&0f32);
        let init_b = builder.mov(&0f32);
        let acc_dim_m = builder.open_mapped_dim(&init_dim_m);
        let acc_dim_n = builder.open_mapped_dim(&ld_x[0]);
        let a_op = ld_a.dim_map(&[&acc_dim_m, &acc_dim_n], GlobalScope(()), &mut builder);
        let b_op = ld_b.dim_map(&[&acc_dim_m, &acc_dim_n], GlobalScope(()), &mut builder);
        let x_op = ld_x.dim_map(&[&acc_dim_n], GlobalScope(()), &mut builder);
        let acc_a = builder.mad(&a_op, &x_op, &helper::Reduce(init_a));
        let acc_b = builder.mad(&b_op, &x_op, &helper::Reduce(init_b));
        builder.close_dim(&acc_dim_n);
        let y_a = builder.mul(&acc_a, &"alpha");
        let sum = builder.mad(&acc_b, &"beta", &y_a);
        let sum = VirtualTensor::new(sum, vec![acc_dim_m]);
        let st_y = sum.store(&self.y, &mut builder);

        builder.order(&acc_dim_n, &y_a, Order::BEFORE);
        // TODO(search_space): explore inst flags
        builder.action(Action::InstFlag(ld_x.inst(), InstFlag::CACHE_GLOBAL));
        builder.action(Action::InstFlag(ld_a.inst(), InstFlag::CACHE_GLOBAL));
        builder.action(Action::InstFlag(ld_b.inst(), InstFlag::CACHE_GLOBAL));
        builder.action(Action::InstFlag(st_y.inst(), InstFlag::NO_CACHE));
        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &device::Context) -> Array1<S> {
        let (m, n) = (self.m as usize, self.n as usize);
        let a = unwrap!(self.a.read_to_host(context).into_shape((m, n)));
        let b = unwrap!(self.b.read_to_host(context).into_shape((m, n)));
        let x = unwrap!(self.x.read_to_host(context).into_shape(m));
        a.dot(&x) * self.alpha + b.dot(&x) * self.beta
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &device::Context,
    ) -> Result<(), String> {
        let y = unwrap!(self.y.read_to_host(context).into_shape(self.m as usize));
        if y.iter()
            .zip_eq(expected)
            .any(|(&y0, &y1)| (y0 - y1).is_err_ok())
        {
            let x = self.x.read_to_host(context);
            let a = self.a.read_to_host(context);
            let b = self.b.read_to_host(context);
            Err(format!("expected: {}, got {} with alpha = {}, beta = {}, x = {}, a = {} and b = {}", 
                        expected, y, self.alpha, self.beta, x, a, b))
        } else {
            Ok(())
        }
    }
}

/// Computes `C = A.B`.
pub struct MatMul<'a, S: Scalar> {
    pub params: MatMulP,
    a: Tensor<'a, S>,
    b: Tensor<'a, S>,
    c: Tensor<'a, S>,
}

#[derive(Clone, Deserialize, Serialize)]
pub struct MatMulP {
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
}

impl MatMulP {
    pub fn new(m: i32, n: i32, k: i32) -> Self {
        MatMulP {
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

    /// Inline the sizes in the generated code.
    pub fn static_sizes(mut self) -> Self {
        self.generic = false;
        self
    }
}

impl<'a, S: Scalar> Kernel<'a> for MatMul<'a, S> {
    type Parameters = MatMulP;
    type ExpectedOutput = Array2<S>;

    fn name() -> &'static str {
        "matmul"
    }

    fn build_signature<AM>(params: MatMulP, builder: &mut SignatureBuilder<AM>) -> Self
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
        MatMul { params, a, b, c }
    }

    fn build_body<'b>(
        &self,
        signature: &'b ir::Signature,
        ctx: &'b device::Context,
    ) -> Vec<Candidate<'b>> {
        let m_tiling = infer_tiling(self.params.m, &self.params.m_tiling, &[32, 4]);
        let n_tiling = infer_tiling(self.params.n, &self.params.n_tiling, &[32, 4]);
        let k_tiling = infer_tiling(self.params.k, &self.params.k_tiling, &[32]);
        let mut builder = helper::Builder::new(signature, ctx.device());

        let ld_a = self.a.load(vec![m_tiling, k_tiling.clone()], &mut builder);
        let ld_b = self.b.load(vec![k_tiling, n_tiling], &mut builder);

        let init_dim_m = builder.open_mapped_dim(&ld_a[0]);
        let init_dim_n = builder.open_mapped_dim(&ld_b[1]);
        let acc_init = builder.mov(&0f32);
        let acc_dim_m = builder.open_mapped_dim(&init_dim_m);
        let acc_dim_n = builder.open_mapped_dim(&init_dim_n);
        let acc_dim_k = builder.open_mapped_dim(&ld_a[1]);
        let a_op = ld_a.dim_map(&[&acc_dim_m, &acc_dim_k], GlobalScope(()), &mut builder);
        let b_op = ld_b.dim_map(&[&acc_dim_k, &acc_dim_n], GlobalScope(()), &mut builder);
        let acc = builder.mad(&a_op, &b_op, &helper::Reduce(acc_init));
        builder.close_dim(&acc_dim_k);

        let acc = VirtualTensor::new(acc, vec![acc_dim_m, acc_dim_n]);
        let st_c = acc.store(&self.c, &mut builder);

        // Order for correctness.
        builder.order(&st_c.inst(), &acc_dim_k, Order::AFTER);
        // Arbitrary constrains to reduce the search space
        //builder.action(Action::InstFlag(ld_a.inst(), InstFlag::CACHE_GLOBAL));
        //builder.action(Action::InstFlag(ld_b.inst(), InstFlag::CACHE_GLOBAL));
        //builder.action(Action::InstFlag(st_c.inst(), InstFlag::NO_CACHE));

        //builder.action(Action::DimKind(init_dim_n[0], DimKind::BLOCK));
        //builder.action(Action::DimKind(init_dim_m[0], DimKind::BLOCK));
        /*builder.action(Action::DimKind(unroll_dim_0_n, DimKind::UNROLL));
          builder.action(Action::DimKind(unroll_dim_0_m, DimKind::UNROLL));
          builder.order(unroll_dim_0_n.into(), unroll_dim_0_m.into(), Order::OUTER);
          builder.order(unroll_dim_1_n.into(), unroll_dim_1_m.into(), Order::INNER);

          builder.action(Action::DimKind(k0_dim, DimKind::LOOP));
          builder.order(ld_k0_dim.into(), k0_dim.into(), Order::MERGED);
          builder.action(Action::DimKind(a_ld_thread_dim_0, DimKind::THREAD_Y));
          builder.action(Action::DimKind(a_ld_thread_dim_1, DimKind::THREAD_X));
          builder.action(Action::DimKind(a_ld_unroll_dim, DimKind::UNROLL));
          builder.action(Action::DimKind(b_ld_unroll_dim, DimKind::VECTOR));
          builder.order(a_ld_thread_dim_1.into(), b_ld_thread_dim_1.into(), Order::MERGED);
          builder.order(a_ld_thread_dim_0.into(), b_ld_thread_dim_0.into(), Order::MERGED);

          builder.action(Action::DimKind(k1_dim, DimKind::UNROLL));
          builder.action(Action::DimKind(unroll_dim_2_n, DimKind::VECTOR));

          let mut space = builder.get();
          let mem_0 = ir::mem::InternalId(0);
          let (d23, d24, d25) = (ir::DimId {id: 23}, ir::DimId {id: 24}, ir::DimId {id: 25});
          let (d26, d27, d28) = (ir::DimId {id: 26}, ir::DimId {id: 27}, ir::DimId {id: 28});
          assert!(space.lower_layout(mem_0, vec![d23, d24, d25], vec![d26, d27, d28]).is_ok());
          let mem_1 = ir::mem::InternalId(1);
          let (d29, d30, d31) = (ir::DimId {id: 29}, ir::DimId {id: 30}, ir::DimId {id: 31});
          let (d32, d33, d34) = (ir::DimId {id: 32}, ir::DimId {id: 33}, ir::DimId {id: 34});
          assert!(space.lower_layout(mem_1, vec![d29, d30, d31], vec![d32, d33, d34]).is_ok());
          let actions = vec![
          Action::DimKind(d25, DimKind::VECTOR),
          Action::DimKind(d28, DimKind::VECTOR),
          Action::DimKind(d31, DimKind::VECTOR),
          Action::DimKind(d34, DimKind::VECTOR),
          Action::Order(d27.into(), d32.into(), Order::MERGED),
          Action::Order(d32.into(), k1_dim.into(), Order::MERGED),
          ];
        assert!(space.apply_decisions(actions).is_ok());*/
        vec![build_candidate(builder.get(), ctx)]
    }

    fn get_expected_output(&self, context: &device::Context) -> Array2<S> {
        let a_shape = (self.params.m as usize, self.params.k as usize);
        let b_shape = (self.params.k as usize, self.params.n as usize);
        let a = unwrap!(self.a.read_to_host(context).into_shape(a_shape));
        let b = unwrap!(self.b.read_to_host(context).into_shape(b_shape));
        a.dot(&b)
    }

    fn check_result(
        &self,
        expected: &Self::ExpectedOutput,
        context: &device::Context,
    ) -> Result<(), String> {
        let c_shape = (self.params.m as usize, self.params.n as usize);
        let c = unwrap!(self.c.read_to_host(context).into_shape(c_shape));
        if c.iter()
            .zip_eq(expected)
            .any(|(&c0, &c1)| (c0 - c1).is_err_ok())
        {
            let a = self.a.read_to_host(context);
            let b = self.b.read_to_host(context);
            Err(format!(
                "expected: {}, got {} with a = {} and b = {}",
                expected, c, a, b
            ))
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
        signature: &'b ir::Signature,
        ctx: &'b device::Context,
    ) -> Vec<Candidate<'b>> {
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

    fn get_expected_output(&self, context: &device::Context) -> Array3<S> {
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
        context: &device::Context,
    ) -> Result<(), String> {
        let batch = self.params.batch as usize;
        let c_shape = (batch, self.params.m as usize, self.params.n as usize);
        let c = self.c.read_to_host(context).into_shape(c_shape).unwrap();
        if c.iter()
            .zip_eq(expected)
            .any(|(&c0, &c1)| (c0 - c1).is_err_ok())
        {
            let a = self.a.read_to_host(context);
            let b = self.b.read_to_host(context);
            Err(format!(
                "expected: {}, got {} with a = {} and b = {}",
                expected, c, a, b
            ))
        } else {
            Ok(())
        }
    }
}
