#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>

#include <stdatomic.h>
#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include <time.h>

enum { MaxFrames = 90, FramesPerSecond = 30 };
static const OSType NV12 = kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange;

struct Probe {
    int width;
    int height;
    unsigned frames;
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
        valid = abs(row[width / 2] - brightness(index)) <= 6 &&
                abs(row[width * 7 / 8] - (232 - brightness(index))) <= 6;
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

static bool report(struct Probe const *probe, bool paced, unsigned beforeDrain, unsigned delivered, uint64_t elapsed) {
    if (delivered != probe->frames || elapsed == 0 || (probe->frames > 1 && beforeDrain == 0)) {
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
    qsort(latency, probe->frames, sizeof(latency[0]), compareLatency);
    printf("VideoToolbox %dx%d (%s): %u decoded, %u decoded before drain, throughput %.1f frames/s.\n",
           probe->width, probe->height, paced ? "30 fps" : "unpaced", probe->frames, beforeDrain,
           probe->frames * 1000000000.0 / elapsed);
    printf("VideoToolbox submit-to-decode in ms: p50=%.3f p95=%.3f p99=%.3f max=%.3f.\n",
           latency[(probe->frames * 50 + 99) / 100 - 1], latency[(probe->frames * 95 + 99) / 100 - 1],
           latency[(probe->frames * 99 + 99) / 100 - 1], latency[probe->frames - 1]);
    return true;
}

static bool run(int width, int height, unsigned frames, bool paced) {
    struct Probe probe = {.width = width, .height = height, .frames = frames};
    VTCompressionSessionRef encoder = NULL;
    VTDecompressionSessionRef decoder = NULL;
    OSStatus status = VTCompressionSessionCreate(NULL, width, height, kCMVideoCodecType_H264, NULL, NULL, NULL,
                                                 encodedOutput, &probe, &encoder);
    if (status != noErr) {
        fprintf(stderr, "create H.264 encoder: %d\n", (int)status);
        return false;
    }
    status = VTSessionSetProperty(encoder, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse);
    if (status == noErr) {
        status = VTSessionSetProperty(encoder, kVTCompressionPropertyKey_RealTime, kCFBooleanTrue);
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
    printf("VideoToolbox encoder=%s decoder=%s.\n", encodeMode, decodeMode);
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
    if (status != noErr || callbackError != noErr || strcmp(encodeMode, "unknown") == 0 ||
        !report(&probe, paced, beforeDrain, delivered, elapsed)) {
        fprintf(stderr, "H.264 %dx%d result: status=%d callback=%d delivered=%u before_drain=%u\n",
                width, height, (int)status, (int)callbackError, delivered, beforeDrain);
        return false;
    }
    return true;
}

int main(void) {
    return run(128, 96, 1, false) && run(1920, 1080, MaxFrames, false) &&
           run(1920, 1080, MaxFrames, true) ? 0 : 1;
}
