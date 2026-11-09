use super::*;
use cudarc::driver::{CudaEvent, CudaSlice, CudaStream, DevicePtrMut, result, sys};
use orbite::{IntentType, Priority, RegisteredMemory};
use std::mem::ManuallyDrop;
use std::num::NonZeroU32;

const ITERATIONS: u32 = 2_000_000;
const WARMUP: usize = 5;
const TIMEOUT: Duration = Duration::from_secs(120);

// On any error this installed probe keeps native allocations until process exit.
// In particular, an error querying its separate compute stream is not a receipt.
struct Compute {
    stream: Arc<CudaStream>,
    function: sys::CUfunction,
    module: sys::CUmodule,
    output: CudaSlice<u32>,
    blocks: u32,
    start: CudaEvent,
    end: CudaEvent,
    expected: [u32; 2],
}
impl Compute {
    fn new(ctx: &Arc<CudaContext>) -> GateResult<Self> {
        let stream = ctx.new_stream()?; // Nonblocking, including with the legacy copy stream.
        let blocks = u32::try_from(
            ctx.attribute(sys::CUdevice_attribute::CU_DEVICE_ATTRIBUTE_MULTIPROCESSOR_COUNT)?,
        )? * 4;
        if !(1..=65535).contains(&blocks) {
            return Err("invalid compute grid".into());
        }
        let output = ManuallyDrop::new(stream.alloc_zeros::<u32>(blocks as usize * 256)?);
        // SAFETY: static NUL-terminated PTX, loaded in the retained caller context.
        let module = unsafe {
            result::module::load_data(concat!(include_str!("compute.ptx"), "\0").as_ptr().cast())
        }?;
        let function =
            // SAFETY: the live module declares this entry; keep it until every end event retires.
            unsafe { result::module::get_function(module, c"orbite_calibration_compute".into()) }?;
        let start = ctx.new_event(Some(sys::CUevent_flags::CU_EVENT_DEFAULT))?;
        let end = ctx.new_event(Some(sys::CUevent_flags::CU_EVENT_DEFAULT))?;
        stream.synchronize()?;
        Ok(Self {
            stream,
            function,
            module,
            output: ManuallyDrop::into_inner(output),
            blocks,
            start,
            end,
            expected: [compute_value(0), compute_value(1)],
        })
    }
    fn launch(&mut self) -> GateResult<()> {
        self.start.record(&self.stream)?;
        let (mut output, _record) = self.output.device_ptr_mut(&self.stream);
        let mut iterations = ITERATIONS;
        let mut args = [
            (&mut output as *mut u64).cast(),
            (&mut iterations as *mut u32).cast(),
        ];
        // SAFETY: one output per thread, sized for the entire grid; this owner is
        // retained through the end event, and the fixed PTX matches the arguments.
        unsafe {
            result::launch_kernel(
                self.function,
                (self.blocks, 1, 1),
                (256, 1, 1),
                0,
                self.stream.cu_stream(),
                &mut args,
            )?;
        }
        self.end.record(&self.stream)?;
        Ok(())
    }
    fn close(&self) -> GateResult<()> {
        self.stream.context().bind_to_thread()?;
        // SAFETY: called once after the final observed end event and output check.
        unsafe { result::module::unload(self.module) }?;
        Ok(())
    }
    fn complete(&self) -> GateResult<bool> {
        // SAFETY: the recorded event is retained and never rerecorded until this query succeeds.
        match unsafe { result::event::query(self.end.cu_event()) } {
            Ok(()) => Ok(true),
            Err(result::DriverError(sys::cudaError_enum::CUDA_ERROR_NOT_READY)) => Ok(false),
            Err(e) => Err(e.into()),
        }
    }
    fn finish(&self) -> GateResult<f32> {
        let start = Instant::now();
        while !self.complete()? {
            if start.elapsed() > TIMEOUT {
                return Err("compute receipt timed out".into());
            }
            std::hint::spin_loop();
        }
        let elapsed = self.start.elapsed_ms(&self.end)?;
        for (i, value) in self
            .stream
            .clone_dtoh(&self.output)?
            .into_iter()
            .enumerate()
        {
            if value != self.expected[i % 2] ^ i as u32 {
                return Err("interference kernel output mismatch".into());
            }
        }
        Ok(elapsed)
    }
}

fn compute_value(mut value: u32) -> u32 {
    for _ in 0..ITERATIONS {
        value ^= value >> 13;
        value = value.wrapping_mul(1664525).wrapping_add(1013904223);
    }
    value
}

// Keep validation memory bounded independently of the largest (16 GiB) payload.
fn fill(memory: &RegisteredMemory, length: usize, value: u8) -> GateResult<()> {
    let buf = vec![value; (2 << 20).min(length)];
    for off in (0..length).step_by(buf.len()) {
        memory.write(off, &buf[..buf.len().min(length - off)])?;
    }
    Ok(())
}
fn pattern(bytes: &mut [u8], row: usize, offset: usize) {
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let word =
            ((row as u64) << 32) ^ ((offset / 8 + i) as u64).wrapping_mul(0x9e37_79b9_7f4a_7c15);
        chunk.copy_from_slice(&word.to_le_bytes()[..chunk.len()]);
    }
}
fn verify(
    memory: &RegisteredMemory,
    offsets: &[usize],
    width: usize,
    length: usize,
    source: bool,
) -> GateResult<()> {
    let mut buf = vec![0; (2 << 20).min(length)];
    let guard = if source { 0xa5 } else { 0xcd };
    let mut expected = vec![guard; buf.len()];
    for off in (0..length).step_by(buf.len()) {
        let n = buf.len().min(length - off);
        memory.read(off, &mut buf[..n])?;
        expected[..n].fill(guard);
        let first = offsets.partition_point(|&start| start + width <= off);
        for (row, &start) in offsets.iter().enumerate().skip(first) {
            if start >= off + n {
                break;
            }
            let lo = start.max(off) - off;
            let hi = (start + width).min(off + n) - off;
            pattern(&mut expected[lo..hi], row, off + lo - start);
        }
        if buf[..n] != expected[..n] {
            return Err(format!("payload/guard mismatch in chunk at {off}").into());
        }
    }
    Ok(())
}

pub fn run(args: &[String]) -> GateResult<()> {
    let [count, width, samples, mode, cap, interference] = args else {
        return Err("--calibrate COUNT WIDTH SAMPLES dma|batch|kernel CAP_OR_0 0|1".into());
    };
    let (count, width, samples, cap, interference) = (
        count.parse::<usize>()?,
        width.parse::<usize>()?,
        samples.parse::<usize>()?,
        cap.parse::<u32>()?,
        interference.parse::<u8>()?,
    );
    if !(1..=8192).contains(&count)
        || !(4096..=2 << 20).contains(&width)
        || !count.is_power_of_two()
        || !width.is_power_of_two()
        || !(9..=100).contains(&samples)
        || cap > 65535
        || interference > 1
        || !matches!(mode.as_str(), "dma" | "batch" | "kernel")
        || (mode != "kernel" && cap != 0)
    {
        return Err("invalid bounded calibration dimensions".into());
    }
    let ctx = CudaContext::new(0)?;
    let uuid = ctx
        .uuid()?
        .bytes
        .map(|b| format!("{:02x}", b.to_ne_bytes()[0]))
        .concat();
    let uuid = format!(
        "GPU-{}-{}-{}-{}-{}",
        &uuid[..8],
        &uuid[8..12],
        &uuid[12..16],
        &uuid[16..20],
        &uuid[20..]
    );
    let stream = if mode == "dma" {
        ctx.default_stream()
    } else {
        ctx.new_stream()?
    };
    let backend = if mode == "kernel" {
        LocalCopyBackend::Kernel
    } else {
        LocalCopyBackend::Dma
    };
    let engine = TransferEngine::new(EngineConfig::default())?;
    let mut offsets = Vec::with_capacity(count);
    let mut total = GUARD;
    for i in 0..count {
        offsets.push(total);
        // Alternating gaps prevent equal-width strided coalescing; reverse-order
        // descriptors below prevent even two adjacent rows becoming one 2D DMA.
        total += width + GUARD * (1 + i % 2);
    }
    total += GUARD;
    let gpu = engine
        .register_cuda_memory_batch(
            &[(Arc::new(CudaMemory::allocate(0, total)?), 0, total)],
            Permission::LocalOnly,
        )?
        .remove(0);
    let host = engine
        .register_host_memory_batch(
            &[(allocate_pinned_host(0, total)?, 0, total)],
            Permission::LocalOnly,
        )?
        .remove(0);
    let rows: Vec<_> = offsets
        .iter()
        .rev()
        .map(|&off| LocalCopyDesc::new(&gpu, off, &host, off, width))
        .collect();
    let mut config = LocalCopyConfig::default();
    config.backend = backend;
    config.kernel_cta_limit = NonZeroU32::new(cap);
    config.max_descriptors = count;
    config.max_inflight = 1;
    config.max_bytes = count * width + count * 48 + 1;
    ctx.bind_to_thread()?;
    let worker =
        // SAFETY: this gate retains the caller stream/registrations; compute uses a disjoint allocation.
        unsafe { LocalCopyEngine::bind(&gpu, stream.cu_stream().cast(), stream.clone(), config) }?;
    // Never drop a compute allocation on an uncertain launch/query failure.
    let mut compute = ManuallyDrop::new(Compute::new(&ctx)?);
    for dir in [CopyDirection::HostToDevice, CopyDirection::DeviceToHost] {
        let (src, dst) = if dir == CopyDirection::HostToDevice {
            (&host, &gpu)
        } else {
            (&gpu, &host)
        };
        fill(src, total, 0xa5)?;
        let mut payload = vec![0; width];
        for (i, &off) in offsets.iter().enumerate() {
            pattern(&mut payload, i, 0);
            src.write(off, &payload)?;
        }
        let mut copy_ms = Vec::new();
        let mut compute_alone_ms = Vec::new();
        let mut compute_with_copy_ms = Vec::new();
        let options = {
            let mut o = LocalCopyOptions::default();
            o.intent = if dir == CopyDirection::HostToDevice {
                IntentType::ForegroundGet
            } else {
                IntentType::Checkpoint
            };
            o.priority = if dir == CopyDirection::HostToDevice {
                Priority::High
            } else {
                Priority::Low
            };
            o
        };
        for trial in 0..WARMUP + samples {
            // Reset every destination so a missing copy cannot pass on stale bytes.
            fill(dst, total, 0xcd)?;
            if interference == 1 {
                compute.launch()?;
                let time = compute.finish()?;
                if trial >= WARMUP {
                    compute_alone_ms.push(time);
                }
            }
            if interference == 1 {
                compute.launch()?;
            }
            let start = Instant::now();
            let flight = worker.submit(dir, &rows, options.clone())?;
            if flight.backend() != backend {
                return Err("wrong calibration backend".into());
            }
            if interference == 1 && compute.complete()? {
                return Err(
                    "compute finished before copy enqueue; no interference evidence".into(),
                );
            }
            while !flight.poll()? {
                if start.elapsed() > TIMEOUT {
                    return Err("copy receipt timed out".into());
                }
                std::hint::spin_loop();
            }
            let copy = start.elapsed().as_secs_f64() * 1000.0;
            if interference == 1 {
                let time = compute.finish()?;
                if trial >= WARMUP {
                    compute_with_copy_ms.push(time);
                }
            }
            verify(src, &offsets, width, total, true)?;
            verify(dst, &offsets, width, total, false)?;
            if trial >= WARMUP {
                copy_ms.push(copy);
            }
        }
        let stats = worker.stats()?;
        let class = stats
            .classes
            .iter()
            .find(|s| s.direction == dir)
            .ok_or("missing class diagnostics")?;
        let expected_batches = if mode == "batch" && count > 1 {
            (WARMUP + samples) as u64
        } else {
            0
        };
        if class.dma_batch_calls != expected_batches {
            return Err("batch DMA not executed as requested; calibration unavailable".into());
        }
        println!(
            "{}",
            json!({"case":"calibration", "cuda_device_uuid":uuid, "direction":if dir == CopyDirection::HostToDevice {"h2d"} else {"d2h"}, "backend":mode, "cta_limit":cap, "kernel_ctas":class.last_kernel_ctas, "dma_batch_calls":class.dma_batch_calls, "intent":format!("{:?}",options.intent), "priority":format!("{:?}",options.priority), "count":count, "width":width, "bytes":count*width, "samples":samples, "warmup":WARMUP, "copy_ms":copy_ms, "compute_alone_ms":compute_alone_ms, "compute_with_copy_ms":compute_with_copy_ms, "interference":interference == 1, "compute_blocks":compute.blocks, "compute_threads":256, "compute_iterations":ITERATIONS, "byte_source_guard_oracle":"passed"})
        );
    }
    worker.close(TIMEOUT)?;
    compute.close()?;
    // SAFETY: every compute end event was observed, including its final output check.
    unsafe {
        ManuallyDrop::drop(&mut compute);
    }
    Ok(())
}
