#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>
#include <mach/mach.h>

#include <inttypes.h>
#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

#include "../webrtc-probe/vm_probe.h"

enum { MaxFrames = 90, FramesPerSecond = 30 };
enum { MemoryFrames = 3 };
enum MemoryStage { BothStages, EncodeStage, DecodeStage };
static const OSType NV12 = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;

struct Probe {
    int width;
    int height;
    unsigned frames;
    CVPixelBufferRef fixedInput;
    _Atomic(CMSampleBufferRef) encoded[MaxFrames];
    _Atomic bool encodedOnce[MaxFrames];
    _Atomic uint64_t decodedAt[MaxFrames];
    _Atomic OSStatus callbackError;
    uint64_t submittedAt[MaxFrames];
};

static uint64_t nowNs(void) {
    struct timespec time = {0};
    clock_gettime(CLOCK_MONOTONIC, &time);
    return (uint64_t)time.tv_sec * 1000000000 + (uint64_t)time.tv_nsec;
}

static int frameIndex(CMTime timestamp, unsigned frames) {
    if (!CMTIME_IS_NUMERIC(timestamp) || timestamp.epoch != 0) {
        return -1;
    }
    CMTime aligned = CMTimeConvertScale(timestamp, FramesPerSecond, kCMTimeRoundingMethod_Default);
    return aligned.value >= 0 && aligned.value < (int64_t)frames && CMTimeCompare(timestamp, aligned) == 0
        ? (int)aligned.value : -1;
}

static int brightness(unsigned index) {
    return 24 + 8 * (int)(index % 24);
}

static void encodedOutput(void *state, void *source, OSStatus status, VTEncodeInfoFlags flags,
                          CMSampleBufferRef sample) {
    (void)source;
    struct Probe *probe = state;
    if (status != noErr || (flags & kVTEncodeInfo_FrameDropped) || !sample ||
        !CMSampleBufferDataIsReady(sample) || CMSampleBufferGetTotalSampleSize(sample) == 0) {
        atomic_store(&probe->callbackError, status == noErr ? -1 : status);
        return;
    }
    CMFormatDescriptionRef type = CMSampleBufferGetFormatDescription(sample);
    int index = frameIndex(CMSampleBufferGetPresentationTimeStamp(sample), probe->frames);
    if (!type || CMFormatDescriptionGetMediaSubType(type) != kCMVideoCodecType_H264 ||
        index < 0 || atomic_exchange(&probe->encodedOnce[index], true)) {
        atomic_store(&probe->callbackError, -2);
        return;
    }
    atomic_store(&probe->encoded[index], (CMSampleBufferRef)CFRetain(sample));
}

static OSStatus submit(struct Probe *probe, VTCompressionSessionRef encoder, unsigned index) {
    if (probe->fixedInput) {
        probe->submittedAt[index] = nowNs();
        return VTCompressionSessionEncodeFrame(encoder, probe->fixedInput, CMTimeMake(index, FramesPerSecond),
                                               CMTimeMake(1, FramesPerSecond), NULL, NULL, NULL);
    }
    CVPixelBufferRef pixels = NULL;
    OSStatus status = CVPixelBufferCreate(NULL, (size_t)probe->width, (size_t)probe->height, NV12, NULL, &pixels);
    if (status != noErr) {
        return status;
    }
    status = CVPixelBufferLockBaseAddress(pixels, 0);
    if (status == noErr) {
        const size_t width = (size_t)probe->width;
        const size_t stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, 0);
        uint8_t *luma = CVPixelBufferGetBaseAddressOfPlane(pixels, 0);
        for (size_t row = 0; row < (size_t)probe->height; ++row) {
            memset(luma + row * stride, brightness(index), stride);
            memset(luma + row * stride + width * 3 / 4, 232 - brightness(index), width / 4);
        }
        memset(CVPixelBufferGetBaseAddressOfPlane(pixels, 1), 128,
               CVPixelBufferGetBytesPerRowOfPlane(pixels, 1) * CVPixelBufferGetHeightOfPlane(pixels, 1));
        status = CVPixelBufferUnlockBaseAddress(pixels, 0);
    }
    if (status == noErr) {
        probe->submittedAt[index] = nowNs();
        status = VTCompressionSessionEncodeFrame(encoder, pixels, CMTimeMake(index, FramesPerSecond),
                                                 CMTimeMake(1, FramesPerSecond), NULL, NULL, NULL);
    }
    CVPixelBufferRelease(pixels);
    return status;
}

static bool matchesFixture(CVImageBufferRef frame, struct Probe const *probe, unsigned index) {
    if (!frame || CVPixelBufferGetWidth(frame) != (size_t)probe->width ||
        CVPixelBufferGetHeight(frame) != (size_t)probe->height || CVPixelBufferGetPixelFormatType(frame) != NV12 ||
        CVPixelBufferLockBaseAddress(frame, kCVPixelBufferLock_ReadOnly) != noErr) {
        return false;
    }
    const size_t width = (size_t)probe->width;
    const size_t stride = CVPixelBufferGetBytesPerRowOfPlane(frame, 0);
    uint8_t const *luma = CVPixelBufferGetBaseAddressOfPlane(frame, 0);
    bool valid = false;
    if (luma && stride >= width && CVPixelBufferGetHeightOfPlane(frame, 0) >= (size_t)probe->height) {
        const uint8_t *row = luma + stride * (size_t)(probe->height / 2);
        const int expected = probe->fixedInput ? 126 : brightness(index);
        const int stripe = probe->fixedInput ? expected : 232 - expected;
        valid = abs(row[width / 2] - expected) <= 6 && abs(row[width * 7 / 8] - stripe) <= 6;
    }
    return CVPixelBufferUnlockBaseAddress(frame, kCVPixelBufferLock_ReadOnly) == noErr && valid;
}

static void decodedOutput(void *state, void *source, OSStatus status, VTDecodeInfoFlags flags,
                          CVImageBufferRef frame, CMTime pts, CMTime duration) {
    (void)source;
    (void)duration;
    uint64_t received = nowNs();
    struct Probe *probe = state;
    int index = frameIndex(pts, probe->frames);
    if (status != noErr || (flags & kVTDecodeInfo_FrameDropped) || index < 0 ||
        !matchesFixture(frame, probe, (unsigned)index) || atomic_exchange(&probe->decodedAt[index], received)) {
        atomic_store(&probe->callbackError, status == noErr ? -3 : status);
    }
}

static OSStatus startDecoder(struct Probe *probe, CMFormatDescriptionRef type,
                             VTDecompressionSessionRef *decoder) {
    VTDecompressionOutputCallbackRecord callback = {decodedOutput, probe};
    const int format = (int)NV12;
    CFNumberRef pixelFormat = CFNumberCreate(NULL, kCFNumberIntType, &format);
    if (!pixelFormat) {
        return -4;
    }
    const void *keys[] = {kCVPixelBufferPixelFormatTypeKey};
    const void *values[] = {pixelFormat};
    CFDictionaryRef attributes = CFDictionaryCreate(NULL, keys, values, 1,
                                                     &kCFTypeDictionaryKeyCallBacks,
                                                     &kCFTypeDictionaryValueCallBacks);
    CFRelease(pixelFormat);
    if (!attributes) {
        return -4;
    }
    OSStatus status = VTDecompressionSessionCreate(NULL, type, NULL, attributes, &callback, decoder);
    CFRelease(attributes);
    return status;
}

static OSStatus deliverSamples(struct Probe *probe, VTDecompressionSessionRef *decoder, unsigned *next) {
    while (*next < probe->frames) {
        CMSampleBufferRef encoded = atomic_exchange(&probe->encoded[*next], NULL);
        if (!encoded) {
            break;
        }
        OSStatus status = *decoder ? noErr : startDecoder(probe, CMSampleBufferGetFormatDescription(encoded), decoder);
        if (status == noErr) {
            status = VTDecompressionSessionDecodeFrame(*decoder, encoded, 0, NULL, NULL);
        }
        CFRelease(encoded);
        if (status != noErr) {
            return status;
        }
        ++*next;
    }
    return noErr;
}

static OSStatus deliverUntil(struct Probe *probe, VTDecompressionSessionRef *decoder, unsigned *next,
                             uint64_t deadline) {
    for (;;) {
        OSStatus status = deliverSamples(probe, decoder, next);
        uint64_t now = nowNs();
        if (status != noErr || now >= deadline || deadline == 0) {
            return status;
        }
        const uint64_t left = deadline - now;
        const struct timespec poll = {.tv_nsec = (long)(left < 1000000 ? left : 1000000)};
        nanosleep(&poll, NULL);
    }
}

static const char *hardwareMode(VTSessionRef session, CFStringRef key) {
    CFPropertyListRef value = NULL;
    OSStatus status = VTSessionCopyProperty(session, key, NULL, &value);
    const char *mode = "unknown";
    if (status == noErr && value && CFGetTypeID(value) == CFBooleanGetTypeID()) {
        mode = CFBooleanGetValue((CFBooleanRef)value) ? "hardware" : "software";
    }
    if (value) {
        CFRelease(value);
    }
    return mode;
}

static int compareLatency(const void *left, const void *right) {
    const double a = *(const double *)left;
    const double b = *(const double *)right;
    return (a > b) - (a < b);
}

static bool report(struct Probe const *probe, bool paced, unsigned beforeDrain,
                   unsigned delivered, uint64_t elapsed, bool verbose) {
    if (delivered != probe->frames || elapsed == 0 || (!probe->fixedInput && probe->frames > 1 && beforeDrain == 0)) {
        return false;
    }
    double latency[MaxFrames];
    for (unsigned index = 0; index < probe->frames; ++index) {
        uint64_t received = atomic_load(&probe->decodedAt[index]);
        if (received <= probe->submittedAt[index] || !probe->submittedAt[index]) {
            return false;
        }
        latency[index] = (double)(received - probe->submittedAt[index]) / 1000000.0;
    }
    if (!verbose) {
        return true;
    }
    qsort(latency, probe->frames, sizeof(latency[0]), compareLatency);
    printf("VideoToolbox %dx%d (%s): %u decoded, %u decoded before drain, throughput %.1f frames/s.\n",
           probe->width, probe->height, paced ? "30 fps" : "unpaced", probe->frames, beforeDrain,
           probe->frames * 1000000000.0 / elapsed);
    printf("VideoToolbox submit-to-decode in ms: p50=%.3f p95=%.3f p99=%.3f max=%.3f.\n",
           latency[(probe->frames * 50 + 99) / 100 - 1], latency[(probe->frames * 95 + 99) / 100 - 1],
           latency[(probe->frames * 99 + 99) / 100 - 1], latency[probe->frames - 1]);
    return true;
}

static OSStatus startEncoder(struct Probe *probe, VTCompressionSessionRef *encoder) {
    OSStatus status = VTCompressionSessionCreate(NULL, probe->width, probe->height, kCMVideoCodecType_H264,
                                                 NULL, NULL, NULL, encodedOutput, probe, encoder);
    if (status == noErr) {
        status = VTSessionSetProperty(*encoder, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse);
    }
    if (status == noErr) {
        status = VTSessionSetProperty(*encoder, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
    }
    if (status != noErr && *encoder) {
        VTCompressionSessionInvalidate(*encoder);
        CFRelease(*encoder);
        *encoder = NULL;
    }
    return status;
}

static bool run(int width, int height, unsigned frames, bool paced, CVPixelBufferRef fixedInput, bool verbose) {
    struct Probe probe = {.width = width, .height = height, .frames = frames, .fixedInput = fixedInput};
    VTCompressionSessionRef encoder = NULL;
    VTDecompressionSessionRef decoder = NULL;
    OSStatus status = startEncoder(&probe, &encoder);
    if (status != noErr) {
        fprintf(stderr, "create H.264 encoder: %d\n", (int)status);
        return false;
    }
    unsigned delivered = 0;
    uint64_t started = nowNs();
    for (unsigned index = 0; index < frames && status == noErr; ++index) {
        status = submit(&probe, encoder, index);
        if (status == noErr) {
            const uint64_t deadline = paced ? started + (index + 1) * 1000000000ull / FramesPerSecond : 0;
            status = deliverUntil(&probe, &decoder, &delivered, deadline);
        }
    }
    unsigned beforeDrain = 0;
    for (unsigned index = 0; index < delivered; ++index) {
        beforeDrain += atomic_load(&probe.decodedAt[index]) != 0;
    }
    if (status == noErr) {
        status = VTCompressionSessionCompleteFrames(encoder, kCMTimeInvalid);
    }
    if (status == noErr) {
        status = deliverSamples(&probe, &decoder, &delivered);
    }
    if (status == noErr && decoder) {
        status = VTDecompressionSessionWaitForAsynchronousFrames(decoder);
    }
    const uint64_t elapsed = nowNs() - started;
    const char *encodeMode = hardwareMode(encoder, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder);
    const char *decodeMode = decoder
        ? hardwareMode(decoder, kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder) : "unknown";
    if (verbose) {
        printf("VideoToolbox encoder=%s decoder=%s.\n", encodeMode, decodeMode);
    }
    if (decoder) {
        VTDecompressionSessionInvalidate(decoder);
        CFRelease(decoder);
    }
    VTCompressionSessionInvalidate(encoder);
    CFRelease(encoder);
    for (unsigned index = delivered; index < frames; ++index) {
        CMSampleBufferRef unused = atomic_exchange(&probe.encoded[index], NULL);
        if (unused) {
            CFRelease(unused);
        }
    }
    OSStatus callbackError = atomic_load(&probe.callbackError);
    const bool modesKnown = fixedInput ? strcmp(encodeMode, "hardware") == 0 && strcmp(decodeMode, "hardware") == 0
                                      : strcmp(encodeMode, "unknown") != 0;
    if (status != noErr || callbackError != noErr || !modesKnown ||
        !report(&probe, paced, beforeDrain, delivered, elapsed, verbose)) {
        fprintf(stderr, "H.264 %dx%d result: status=%d callback=%d delivered=%u before_drain=%u paths=%s/%s\n",
                width, height, (int)status, (int)callbackError, delivered, beforeDrain, encodeMode, decodeMode);
        return false;
    }
    return true;
}

static bool fillMemoryPlane(CVPixelBufferRef pixels, size_t plane, int value) {
    void *bytes = CVPixelBufferGetBaseAddressOfPlane(pixels, plane);
    const size_t stride = CVPixelBufferGetBytesPerRowOfPlane(pixels, plane);
    const size_t rows = CVPixelBufferGetHeightOfPlane(pixels, plane);
    if (!bytes || !stride || !rows || stride > SIZE_MAX / rows) {
        return false;
    }
    memset(bytes, value, stride * rows);
    return true;
}

static CVPixelBufferRef makeMemoryFixture(int width, int height, OSType format) {
    CFDictionaryRef empty = CFDictionaryCreateMutable(NULL, 0, &kCFTypeDictionaryKeyCallBacks,
                                                        &kCFTypeDictionaryValueCallBacks);
    if (!empty) {
        return NULL;
    }
    const void *keys[] = {kCVPixelBufferIOSurfacePropertiesKey};
    const void *values[] = {empty};
    CFDictionaryRef attributes = CFDictionaryCreate(NULL, keys, values, 1,
                                                     &kCFTypeDictionaryKeyCallBacks,
                                                     &kCFTypeDictionaryValueCallBacks);
    CFRelease(empty);
    if (!attributes) {
        return NULL;
    }
    CVPixelBufferRef pixels = NULL;
    OSStatus status = CVPixelBufferCreate(NULL, (size_t)width, (size_t)height, format, attributes, &pixels);
    CFRelease(attributes);
    if (status != noErr) {
        return NULL;
    }
    bool initialized = false;
    if (CVPixelBufferLockBaseAddress(pixels, 0) == noErr) {
        if (format == NV12) {
            initialized = CVPixelBufferGetPlaneCount(pixels) == 2 &&
                          fillMemoryPlane(pixels, 0, 126) && fillMemoryPlane(pixels, 1, 128);
        } else {
            void *bytes = CVPixelBufferGetBaseAddress(pixels);
            const size_t length = CVPixelBufferGetDataSize(pixels);
            initialized = bytes && length >= (size_t)width * (size_t)height * 4;
            if (initialized) {
                memset(bytes, 0x80, length);
            }
        }
        initialized = CVPixelBufferUnlockBaseAddress(pixels, 0) == noErr && initialized;
    }
    if (!initialized) {
        CVPixelBufferRelease(pixels);
        return NULL;
    }
    return pixels;
}

static bool sampleMemory(unsigned cycle) {
    mach_task_basic_info_data_t memory = {0};
    mach_msg_type_number_t count = MACH_TASK_BASIC_INFO_COUNT;
    if (task_info(mach_task_self(), MACH_TASK_BASIC_INFO, (task_info_t)&memory, &count) != KERN_SUCCESS) {
        return false;
    }
    printf("VideoToolbox memory: stopped=%u rss_bytes=%" PRIu64 "\n", cycle, (uint64_t)memory.resident_size);
    return linko_rw04_sample_vm_regions(cycle == 10);
}

struct MemorySamples {
    CMSampleBufferRef frames[MemoryFrames];
};

static void releaseMemorySamples(struct MemorySamples *samples) {
    for (unsigned index = 0; index < MemoryFrames; ++index) {
        if (samples->frames[index]) {
            CFRelease(samples->frames[index]);
            samples->frames[index] = NULL;
        }
    }
}

static bool encodeMemoryFixture(int width, int height, CVPixelBufferRef input, struct MemorySamples *samples) {
    struct Probe probe = {.width = width, .height = height, .frames = MemoryFrames, .fixedInput = input};
    VTCompressionSessionRef encoder = NULL;
    OSStatus status = startEncoder(&probe, &encoder);
    if (status != noErr) {
        fprintf(stderr, "VideoToolbox memory: create isolated encoder failed: %d.\n", (int)status);
        return false;
    }
    const struct timespec interval = {.tv_nsec = 1000000000 / FramesPerSecond};
    for (unsigned index = 0; index < MemoryFrames && status == noErr; ++index) {
        status = submit(&probe, encoder, index);
        if (status == noErr) {
            nanosleep(&interval, NULL);
        }
    }
    if (status == noErr) {
        status = VTCompressionSessionCompleteFrames(encoder, kCMTimeInvalid);
    }
    const char *mode = hardwareMode(encoder, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder);
    VTCompressionSessionInvalidate(encoder);
    CFRelease(encoder);
    bool present = true;
    for (unsigned index = 0; index < MemoryFrames; ++index) {
        samples->frames[index] = atomic_exchange(&probe.encoded[index], NULL);
        present = present && samples->frames[index] != NULL;
    }
    OSStatus callbackError = atomic_load(&probe.callbackError);
    if (status != noErr || callbackError != noErr || !present || strcmp(mode, "hardware") != 0) {
        fprintf(stderr, "VideoToolbox memory: isolated encoder status=%d callback=%d mode=%s.\n",
                (int)status, (int)callbackError, mode);
        return false;
    }
    return true;
}

static bool decodeMemoryFixture(int width, int height, CVPixelBufferRef input, struct MemorySamples const *samples) {
    struct Probe probe = {.width = width, .height = height, .frames = MemoryFrames, .fixedInput = input};
    VTDecompressionSessionRef decoder = NULL;
    CMFormatDescriptionRef type = CMSampleBufferGetFormatDescription(samples->frames[0]);
    OSStatus status = startDecoder(&probe, type, &decoder);
    if (status != noErr) {
        fprintf(stderr, "VideoToolbox memory: create isolated decoder failed: %d.\n", (int)status);
        return false;
    }
    for (unsigned index = 0; index < MemoryFrames && status == noErr; ++index) {
        status = VTDecompressionSessionDecodeFrame(decoder, samples->frames[index], 0, NULL, NULL);
    }
    if (status == noErr) {
        status = VTDecompressionSessionWaitForAsynchronousFrames(decoder);
    }
    const char *mode = hardwareMode(decoder, kVTDecompressionPropertyKey_UsingHardwareAcceleratedVideoDecoder);
    VTDecompressionSessionInvalidate(decoder);
    CFRelease(decoder);
    bool present = true;
    for (unsigned index = 0; index < MemoryFrames; ++index) {
        present = present && atomic_load(&probe.decodedAt[index]) != 0;
    }
    OSStatus callbackError = atomic_load(&probe.callbackError);
    if (status != noErr || callbackError != noErr || !present || strcmp(mode, "hardware") != 0) {
        fprintf(stderr, "VideoToolbox memory: isolated decoder status=%d callback=%d mode=%s.\n",
                (int)status, (int)callbackError, mode);
        return false;
    }
    return true;
}

static bool checkMemory(int width, int height, enum MemoryStage stage, OSType format, const char *name) {
    const unsigned cycles = strcmp(name, "encode-soak") == 0 ? 500 : 100;
    printf("VideoToolbox memory: size=%dx%d, stage=%s, cycles=%u.\n", width, height, name, cycles);
    CVPixelBufferRef input = makeMemoryFixture(width, height, format);
    if (!input) {
        fputs("VideoToolbox memory: could not create the fixed input.\n", stderr);
        return false;
    }
    struct MemorySamples seed = {0};
    bool passed = stage != DecodeStage || encodeMemoryFixture(width, height, input, &seed);
    for (unsigned cycle = 1; passed && cycle <= cycles; ++cycle) {
        if (stage == EncodeStage) {
            struct MemorySamples samples = {0};
            passed = encodeMemoryFixture(width, height, input, &samples);
            releaseMemorySamples(&samples);
        } else if (stage == DecodeStage) {
            passed = decodeMemoryFixture(width, height, input, &seed);
        } else {
            passed = run(width, height, MemoryFrames, true, input, cycle == 1);
        }
        if (passed && (cycle == 10 || cycle == 100 || cycle == 250 || cycle == cycles)) {
            passed = sampleMemory(cycle);
        }
        if (!passed) {
            fprintf(stderr, "VideoToolbox memory: stopped at cycle %u.\n", cycle);
        }
    }
    releaseMemorySamples(&seed);
    CVPixelBufferRelease(input);
    if (passed) {
        printf("VideoToolbox memory: %u standalone hardware cycles passed, stage=%s.\n", cycles, name);
    }
    return passed;
}

static int evenDimension(const char *text, int limit) {
    char *end = NULL;
    const long value = strtol(text, &end, 10);
    return end != text && *end == '\0' && value >= 64 && value <= limit && value % 2 == 0
        ? (int)value : 0;
}

int main(int argc, char **argv) {
    setvbuf(stdout, NULL, _IONBF, 0);
    if ((argc == 4 || argc == 5) && strcmp(argv[1], "--vm") == 0) {
        const int width = evenDimension(argv[2], 1920);
        const int height = evenDimension(argv[3], 1080);
        if (!width || !height) {
            fputs("VideoToolbox memory: expected bounded even dimensions.\n", stderr);
            return 2;
        }
        enum MemoryStage stage = BothStages;
        OSType format = kCVPixelFormatType_32BGRA;
        const char *name = argc == 5 ? argv[4] : "both";
        if (argc == 5) {
            if (strcmp(name, "encode") == 0 || strcmp(name, "encode-soak") == 0) {
                stage = EncodeStage;
            } else if (strcmp(name, "encode-nv12") == 0) {
                stage = EncodeStage;
                format = NV12;
            } else if (strcmp(name, "decode") == 0) {
                stage = DecodeStage;
            } else {
                return 2;
            }
        }
        return checkMemory(width, height, stage, format, name) ? 0 : 1;
    }
    if (argc != 1) {
        return 2;
    }
    return run(128, 96, 1, false, NULL, true) && run(1920, 1080, MaxFrames, false, NULL, true) &&
           run(1920, 1080, MaxFrames, true, NULL, true) ? 0 : 1;
}
