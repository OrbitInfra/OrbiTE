// Fixed integer-ALU interference probe, not a model-compute proxy.
// Regenerate with: clang++-19 -x cuda --cuda-device-only --cuda-gpu-arch=sm_52 -nocudainc -nocudalib -O3 -S compute.cu -o compute.ptx
#define __global__ __attribute__((global))
#include <__clang_cuda_builtin_vars.h>
extern "C" __global__ void orbite_calibration_compute(unsigned* output, unsigned iterations) {
    const unsigned id = blockIdx.x * blockDim.x + threadIdx.x;
    unsigned value = id & 1u;
    for (unsigned i = 0; i < iterations; ++i) {
        value ^= value >> 13;
        value = value * 1664525u + 1013904223u;
    }
    output[id] = value ^ id;
}
