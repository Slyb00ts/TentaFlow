// =============================================================================
// Plik: forge_metal_shim.m
// Opis: Cienki shim C nad Metalem — odpowiednik forge_hip_shim.c po stronie
//       Apple. Rust nie mówi po Objective-C, a wciąganie całego drzewa bindingów
//       kosztowałoby więcej niż te sto linii.
//
//       Dwa rozstrzygnięcia wynikają wprost z pomiaru EKS-A3
//       (docs/pomiary/eks-a1-a3-apple-m4.md):
//       * dyspozycja wewnątrz command buffera kosztuje 0,61 us, a osobny
//         command buffer 19,6 us — dlatego shim wystawia bufor poleceń jako
//         obiekt o własnym cyklu życia, do którego dokłada się wiele dyspozycji,
//         a nie funkcję „uruchom kernel";
//       * pamięć jest unified, więc bufory powstają jako `Shared` i oddają
//         wskaźnik hosta; kopiowanie host->device nie istnieje.
// =============================================================================

#import <Foundation/Foundation.h>
#import <Metal/Metal.h>

static void fm_copy_error(NSError *err, char *out, int out_len) {
    if (out == NULL || out_len <= 0) {
        return;
    }
    const char *msg = err ? [[err localizedDescription] UTF8String] : "unknown Metal error";
    snprintf(out, (size_t)out_len, "%s", msg ? msg : "unknown Metal error");
}

void *fm_device_create(void) {
    id<MTLDevice> dev = MTLCreateSystemDefaultDevice();
    return (__bridge_retained void *)dev;
}

const char *fm_device_name(void *device) {
    id<MTLDevice> dev = (__bridge id<MTLDevice>)device;
    return [[dev name] UTF8String];
}

unsigned long long fm_device_working_set(void *device) {
    id<MTLDevice> dev = (__bridge id<MTLDevice>)device;
    return [dev recommendedMaxWorkingSetSize];
}

unsigned long long fm_device_threadgroup_memory(void *device) {
    id<MTLDevice> dev = (__bridge id<MTLDevice>)device;
    return [dev maxThreadgroupMemoryLength];
}

int fm_device_has_unified_memory(void *device) {
    id<MTLDevice> dev = (__bridge id<MTLDevice>)device;
    return [dev hasUnifiedMemory] ? 1 : 0;
}

unsigned long long fm_device_max_threads(void *device) {
    id<MTLDevice> dev = (__bridge id<MTLDevice>)device;
    return [dev maxThreadsPerThreadgroup].width;
}

void *fm_queue_create(void *device) {
    id<MTLDevice> dev = (__bridge id<MTLDevice>)device;
    return (__bridge_retained void *)[dev newCommandQueue];
}

/// Bufor `Shared`: ta sama pamięć widziana przez CPU i GPU.
void *fm_buffer_new(void *device, unsigned long long length) {
    id<MTLDevice> dev = (__bridge id<MTLDevice>)device;
    id<MTLBuffer> buf = [dev newBufferWithLength:(NSUInteger)length
                                         options:MTLResourceStorageModeShared];
    return (__bridge_retained void *)buf;
}

void *fm_buffer_contents(void *buffer) {
    id<MTLBuffer> buf = (__bridge id<MTLBuffer>)buffer;
    return [buf contents];
}

void *fm_library_new(void *device, const char *source, char *err_out, int err_len) {
    id<MTLDevice> dev = (__bridge id<MTLDevice>)device;
    NSError *error = nil;
    NSString *src = [NSString stringWithUTF8String:source];
    id<MTLLibrary> lib = [dev newLibraryWithSource:src options:nil error:&error];
    if (lib == nil) {
        fm_copy_error(error, err_out, err_len);
        return NULL;
    }
    return (__bridge_retained void *)lib;
}

void *fm_pipeline_new(void *device, void *library, const char *name, char *err_out, int err_len) {
    id<MTLDevice> dev = (__bridge id<MTLDevice>)device;
    id<MTLLibrary> lib = (__bridge id<MTLLibrary>)library;
    id<MTLFunction> fn = [lib newFunctionWithName:[NSString stringWithUTF8String:name]];
    if (fn == nil) {
        snprintf(err_out, (size_t)err_len, "brak funkcji '%s' w bibliotece", name);
        return NULL;
    }
    NSError *error = nil;
    id<MTLComputePipelineState> pipe = [dev newComputePipelineStateWithFunction:fn error:&error];
    if (pipe == nil) {
        fm_copy_error(error, err_out, err_len);
        return NULL;
    }
    return (__bridge_retained void *)pipe;
}

unsigned long long fm_pipeline_max_threads(void *pipeline) {
    id<MTLComputePipelineState> pipe = (__bridge id<MTLComputePipelineState>)pipeline;
    return [pipe maxTotalThreadsPerThreadgroup];
}

void *fm_cmdbuf_new(void *queue) {
    id<MTLCommandQueue> q = (__bridge id<MTLCommandQueue>)queue;
    return (__bridge_retained void *)[q commandBuffer];
}

/// Jedna dyspozycja dokładana do OTWARTEGO bufora poleceń.
///
/// Argument o numerze i trafia na indeks i — bufor przez `setBuffer`, skalar
/// przez `setBytes`. Skalary idą zawsze ośmioma bajtami: shader czyta tyle,
/// ile deklaruje jego typ, a nadmiar na little-endian nie zmienia wartości.
void fm_dispatch(void *cmdbuf,
                 void *pipeline,
                 void *const *buffers,
                 const unsigned long long *offsets,
                 const unsigned long long *scalars,
                 const int *is_buffer,
                 int arg_count,
                 unsigned int threadgroups_x,
                 unsigned int threadgroups_y,
                 unsigned int threads_per_group) {
    id<MTLCommandBuffer> cb = (__bridge id<MTLCommandBuffer>)cmdbuf;
    id<MTLComputePipelineState> pipe = (__bridge id<MTLComputePipelineState>)pipeline;
    id<MTLComputeCommandEncoder> enc = [cb computeCommandEncoder];
    [enc setComputePipelineState:pipe];
    for (int i = 0; i < arg_count; ++i) {
        if (is_buffer[i]) {
            id<MTLBuffer> buf = (__bridge id<MTLBuffer>)buffers[i];
            [enc setBuffer:buf offset:(NSUInteger)offsets[i] atIndex:(NSUInteger)i];
        } else {
            [enc setBytes:&scalars[i] length:sizeof(unsigned long long) atIndex:(NSUInteger)i];
        }
    }
    [enc dispatchThreadgroups:MTLSizeMake(threadgroups_x, threadgroups_y, 1)
        threadsPerThreadgroup:MTLSizeMake(threads_per_group, 1, 1)];
    [enc endEncoding];
}

void fm_commit(void *cmdbuf) {
    id<MTLCommandBuffer> cb = (__bridge id<MTLCommandBuffer>)cmdbuf;
    [cb commit];
}

void fm_wait(void *cmdbuf) {
    id<MTLCommandBuffer> cb = (__bridge id<MTLCommandBuffer>)cmdbuf;
    [cb waitUntilCompleted];
}

int fm_cmdbuf_completed(void *cmdbuf) {
    id<MTLCommandBuffer> cb = (__bridge id<MTLCommandBuffer>)cmdbuf;
    MTLCommandBufferStatus status = [cb status];
    return (status == MTLCommandBufferStatusCompleted || status == MTLCommandBufferStatusError) ? 1 : 0;
}

int fm_cmdbuf_failed(void *cmdbuf, char *err_out, int err_len) {
    id<MTLCommandBuffer> cb = (__bridge id<MTLCommandBuffer>)cmdbuf;
    if ([cb status] == MTLCommandBufferStatusError) {
        fm_copy_error([cb error], err_out, err_len);
        return 1;
    }
    return 0;
}

void fm_release(void *object) {
    if (object == NULL) {
        return;
    }
    id obj = (__bridge_transfer id)object;
    (void)obj;
}
