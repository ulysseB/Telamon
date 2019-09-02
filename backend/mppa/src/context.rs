//! MPPA evaluation context.
use crate::printer::MppaPrinter;
use crate::{mppa, NameGenerator};
use crossbeam;
use crossbeam::queue::ArrayQueue;
use fxhash::FxHashMap;
use itertools::Itertools;
use libc;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    mpsc, Arc,
};
use std::time::Instant;
use std::{self, fmt};
use telamon::codegen::{Function, NameMap, ParamVal};
use telamon::device::{
    self, ArrayArgument, AsyncCallback, Context as ContextTrait, EvalMode,
    KernelEvaluator, ScalarArgument,
};
use telamon::explorer;
use telamon::ir;
use utils::unwrap;

#[cfg(not(feature = "real_mppa"))]
use crate::fake_telajax as telajax;
#[cfg(feature = "real_mppa")]
use telajax;

// This atomic id is needed as because of a bug in Kalray OpenCL, we have to give a unique name to
// every kernel, otherwise we get strange effects (as a kernel run multiple times)
static ATOMIC_KERNEL_ID: AtomicUsize = AtomicUsize::new(0);
const EXECUTION_QUEUE_SIZE: usize = 32;

pub trait Argument: Sync + Send {
    /// Returns a pointer to the object.
    fn raw_ptr(&self) -> *const libc::c_void;
    /// Returns the argument value if it can represent a size.
    fn as_size(&self) -> Option<u32> {
        None
    }
}

impl<'a> Argument for Box<dyn ScalarArgument + 'a> {
    fn raw_ptr(&self) -> *const libc::c_void {
        device::ScalarArgument::raw_ptr(&**self as &dyn ScalarArgument)
    }

    fn as_size(&self) -> Option<u32> {
        device::ScalarArgument::as_size(&**self as &dyn ScalarArgument)
    }
}

/// Wrapper around Buffer
/// We need it to implement ArrayArgument for Buffer (orphan rule)
struct MppaArray(telajax::Buffer<i8>);

impl MppaArray {
    pub fn new(executor: &'static telajax::Device, len: usize) -> Self {
        MppaArray(telajax::Buffer::new(executor, len))
    }
}

impl device::ArrayArgument for MppaArray {
    fn read_i8(&self) -> Vec<i8> {
        self.0.read().unwrap()
    }

    fn write_i8(&self, slice: &[i8]) {
        self.0.write(slice).unwrap();
    }
}

impl Argument for MppaArray {
    fn as_size(&self) -> Option<u32> {
        Some(self.0.len as u32)
    }

    fn raw_ptr(&self) -> *const libc::c_void {
        self.0.raw_ptr()
    }
}

/// MPPA evaluation context.
/// We need to keep the arguments allocated for the kernel somewhere
enum KernelArg {
    GlobalMem(MppaArray),
    Size(u32),
    External(*const libc::c_void),
}

impl KernelArg {
    fn raw_ptr(&self) -> *const libc::c_void {
        match self {
            KernelArg::GlobalMem(mem) => mem.raw_ptr(),
            KernelArg::Size(size) => size as *const u32 as *const libc::c_void,
            KernelArg::External(ptr) => *ptr,
        }
    }
}

pub struct Context {
    device: Arc<mppa::Mppa>,
    executor: &'static telajax::Device,
    parameters: FxHashMap<String, Arc<dyn Argument>>,
    writeback_slots: ArrayQueue<MppaArray>,
}

impl Default for Context {
    fn default() -> Self {
        Context::new()
    }
}

impl Context {
    /// Creates a new `Context`. Blocks until the MPPA device is ready to be
    /// used.
    pub fn new() -> Self {
        let executor = telajax::Device::get();
        let writeback_slots = ArrayQueue::new(EXECUTION_QUEUE_SIZE);
        for _ in 0..EXECUTION_QUEUE_SIZE {
            writeback_slots.push(MppaArray::new(executor, 4)).unwrap();
        }
        Context {
            device: Arc::new(mppa::Mppa::default()),
            executor,
            parameters: FxHashMap::default(),
            writeback_slots,
        }
    }

    fn bind_param(&mut self, name: String, value: Arc<dyn Argument>) {
        self.parameters.insert(name, value);
    }

    /// Compiles and sets the arguments of a kernel.
    fn setup_kernel(&self, fun: &Function) -> (telajax::Kernel, Vec<KernelArg>) {
        let id = ATOMIC_KERNEL_ID.fetch_add(1, Ordering::SeqCst);
        let kernel_code = MppaPrinter::default().wrapper_function(fun, id);
        let wrapper = self.get_wrapper(fun, id);

        // Compiler and linker flags
        let cflags = std::ffi::CString::new("-mhypervisor").unwrap();
        let lflags = std::ffi::CString::new("-mhypervisor -lutask -lvbsp").unwrap();

        let kernel_code = unwrap!(std::ffi::CString::new(kernel_code));
        let mut kernel = self
            .executor
            .build_kernel(&kernel_code, &cflags, &lflags, &*wrapper)
            .unwrap();
        kernel.set_num_clusters(1).unwrap();

        // Setting kernel arguments
        let (mut arg_sizes, mut kernel_args) = self.process_kernel_argument(fun);
        // This memory chunk is used to get the time taken by the kernel
        let out_mem = self.writeback_slots.pop().unwrap();
        kernel_args.push(KernelArg::GlobalMem(out_mem));
        arg_sizes.push(telajax::Mem::get_mem_size());
        let args_ptr = kernel_args
            .iter()
            .map(|k_arg| k_arg.raw_ptr())
            .collect_vec();
        kernel.set_args(&arg_sizes[..], &args_ptr[..]).unwrap();
        (kernel, kernel_args)
    }

    /// Returns the wrapper for the given signature.
    fn get_wrapper(&self, fun: &Function, id: usize) -> Arc<telajax::Wrapper> {
        let ocl_code = MppaPrinter::default().print_ocl_wrapper(fun, id);
        let name = std::ffi::CString::new(format!("wrapper_{}", id)).unwrap();
        let ocl_code = std::ffi::CString::new(ocl_code).unwrap();
        Arc::new(self.executor.build_wrapper(&name, &ocl_code).unwrap())
    }

    /// Returns a parameter given its name.
    pub fn get_param(&self, name: &str) -> &dyn Argument {
        self.parameters[name].as_ref()
    }

    /// Process parameters so they can be passed to telajax correctly
    /// Returns a tuple of (Vec<argument size>, Vec<argument>)
    fn process_kernel_argument(&self, fun: &Function) -> (Vec<usize>, Vec<KernelArg>) {
        fun.device_code_args()
            .map(|p| match p {
                ParamVal::External(p, _) => {
                    let arg = self.get_param(&p.name);
                    (get_type_size(p.t), KernelArg::External(arg.raw_ptr()))
                }
                ParamVal::GlobalMem(_, size, _) => {
                    let size = self.eval_size(size);
                    let mem = MppaArray::new(self.executor, size as usize);
                    (telajax::Mem::get_mem_size(), KernelArg::GlobalMem(mem))
                }
                ParamVal::Size(size) => {
                    let size = self.eval_size(size);
                    (get_type_size(p.t()), KernelArg::Size(size))
                }
            })
            .unzip()
    }
}

fn get_type_size(t: ir::Type) -> usize {
    t.len_byte()
        .map(|x| x as usize)
        .unwrap_or_else(telajax::Mem::get_mem_size)
}

impl device::Context for Context {
    fn device(&self) -> Arc<dyn device::Device> {
        Arc::<mppa::Mppa>::clone(&self.device)
    }

    fn benchmark(&self, _function: &Function, _num_samples: usize) -> Vec<f64> {
        unimplemented!()
    }

    fn evaluate(&self, fun: &Function, _mode: EvalMode) -> Result<f64, ()> {
        let (mut kernel, mut kernel_args) = self.setup_kernel(fun);
        self.executor.execute_kernel(&mut kernel).unwrap();
        let out_mem = if let KernelArg::GlobalMem(mem) = kernel_args.pop().unwrap() {
            mem
        } else {
            panic!()
        };
        // FIXME:
        // We better be careful here. Mppa manipulates u32 on clusters.
        // This is a little endian architecture, so we ought to read in little endian way
        // Anyway, we can see with printing that results make sense
        // Actually this should be checked again. I'm not sure we are reading on the cluster and
        // getting the right result could be a happy coincidence
        let vec_u8: Vec<u8> = out_mem
            .read_i8()
            .iter()
            .map(|byte| i8::to_le_bytes(*byte)[0])
            .collect();
        let mut buf: [u8; 4] = [0; 4];
        buf.copy_from_slice(vec_u8.as_slice());
        let res = u32::from_le_bytes(buf);
        self.writeback_slots.push(out_mem).unwrap();
        Ok(f64::from(res))
    }

    fn async_eval<'d>(
        &self,
        num_workers: usize,
        _mode: EvalMode,
        inner: &(dyn Fn(&mut dyn device::AsyncEvaluator<'d>) + Sync),
    ) {
        // FIXME: execute in parallel
        let (send, recv) = mpsc::sync_channel(EXECUTION_QUEUE_SIZE);
        crossbeam::scope(move |scope| {
            // Start the explorer threads.
            for _ in 0..num_workers {
                let mut evaluator = AsyncEvaluator {
                    context: self,
                    sender: send.clone(),
                };
                unwrap!(scope
                    .builder()
                    .name("Telamon - Explorer Thread".to_string())
                    .spawn(move |_| inner(&mut evaluator)));
            }
            // Start the evaluation thread.
            let eval_thread_name = "Telamon - CPU Evaluation Thread".to_string();
            unwrap!(scope.builder().name(eval_thread_name).spawn(move |_| {
                while let Ok((candidate, kernel, callback)) = recv.recv() {
                    callback.call(
                        candidate,
                        &mut Code {
                            kernel,
                            executor: self.executor,
                        },
                    );
                }
            }));
        })
        .unwrap();
    }

    fn param_as_size(&self, name: &str) -> Option<u32> {
        self.get_param(name).as_size()
    }
}

impl<'a> device::ArgMap<'a> for Context {
    fn bind_erased_scalar(
        &mut self,
        param: &ir::Parameter,
        value: Box<dyn ScalarArgument>,
    ) {
        assert_eq!(param.t, value.get_type());
        self.bind_param(param.name.clone(), Arc::new(value));
    }

    fn bind_erased_array(
        &mut self,
        param: &ir::Parameter,
        t: ir::Type,
        len: usize,
    ) -> Arc<dyn ArrayArgument + 'a> {
        let size = len * unwrap!(t.len_byte()) as usize;
        let buffer_arc = Arc::new(MppaArray::new(self.executor, size));
        self.bind_param(
            param.name.clone(),
            Arc::clone(&buffer_arc) as Arc<dyn Argument>,
        );
        buffer_arc
    }
}

type AsyncPayload<'b> = (explorer::Candidate, telajax::Kernel, AsyncCallback<'b>);

/// Asynchronous evaluator.
struct AsyncEvaluator<'b> {
    context: &'b Context,
    sender: mpsc::SyncSender<AsyncPayload<'b>>,
}

impl<'b, 'c> device::AsyncEvaluator<'c> for AsyncEvaluator<'b>
where
    'c: 'b,
{
    fn add_dyn_kernel(
        &mut self,
        candidate: explorer::Candidate,
        callback: device::AsyncCallback<'c>,
    ) {
        let (kernel, _) = {
            let dev_fun = Function::build(&candidate.space);
            self.context.setup_kernel(&dev_fun)
        };
        unwrap!(self.sender.send((candidate, kernel, callback)));
    }
}

struct Code<'a> {
    kernel: telajax::Kernel,
    executor: &'a telajax::Device,
}

impl<'a> fmt::Display for Code<'a> {
    fn fmt(&self, fmt: &mut fmt::Formatter) -> fmt::Result {
        write!(fmt, "<mppa kernel>")
    }
}

impl<'a> KernelEvaluator for Code<'a> {
    fn evaluate(&mut self) -> Option<f64> {
        // TODO: measure time directly on MPPA
        let t0 = Instant::now();
        self.executor.execute_kernel(&mut self.kernel).unwrap();
        let d = t0.elapsed();
        Some(f64::from(d.subsec_nanos()) + d.as_secs() as f64 * 1_000_000_000.)
    }
}
