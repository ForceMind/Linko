#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>

#include <stdatomic.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

enum { Width = 128, Height = 96 };

struct Encoded {
    _Atomic(CMSampleBufferRef) sample;
    _Atomic OSStatus error;
};

static void output(void *state, void *source, OSStatus status, VTEncodeInfoFlags flags,
                   CMSampleBufferRef sample) {
    (void)source;
    struct Encoded *encoded = state;
    if (status != noErr) {
        atomic_store(&encoded->error, status);
        return;
    }
    if ((flags & kVTEncodeInfo_FrameDropped) || !sample || !CMSampleBufferDataIsReady(sample)) {
        return;
    }
    CMFormatDescriptionRef type = CMSampleBufferGetFormatDescription(sample);
    if (type && CMFormatDescriptionGetMediaSubType(type) == kCMVideoCodecType_H264) {
        CMSampleBufferRef previous = atomic_exchange(&encoded->sample, (CMSampleBufferRef)CFRetain(sample));
        if (previous) {
            CFRelease(previous);
        }
    }
}

static OSStatus submit(VTCompressionSessionRef session) {
    CVPixelBufferRef pixels = NULL;
    OSStatus status = CVPixelBufferCreate(NULL, Width, Height, kCVPixelFormatType_420YpCbCr8BiPlanarVideoRange,
                                         NULL, &pixels);
    if (status != noErr) {
        return status;
    }
    status = CVPixelBufferLockBaseAddress(pixels, 0);
    if (status == noErr) {
        for (size_t plane = 0; plane < 2; ++plane) {
            size_t bytes = CVPixelBufferGetBytesPerRowOfPlane(pixels, plane) *
                           CVPixelBufferGetHeightOfPlane(pixels, plane);
            memset(CVPixelBufferGetBaseAddressOfPlane(pixels, plane), plane == 0 ? 16 : 128, bytes);
        }
        status = CVPixelBufferUnlockBaseAddress(pixels, 0);
    }
    if (status == noErr) {
        status = VTCompressionSessionEncodeFrame(session, pixels, kCMTimeZero, CMTimeMake(1, 30),
                                                 NULL, NULL, NULL);
    }
    CVPixelBufferRelease(pixels);
    return status;
}

struct Decoded {
    _Atomic bool matchingSize;
    _Atomic OSStatus error;
};

static void decodedOutput(void *state, void *source, OSStatus status, VTDecodeInfoFlags flags,
                          CVImageBufferRef frame, CMTime pts, CMTime duration) {
    (void)source;
    (void)duration;
    struct Decoded *result = state;
    if (status != noErr) {
        atomic_store(&result->error, status);
    } else if (!(flags & kVTDecodeInfo_FrameDropped) && frame &&
               CVPixelBufferGetWidth(frame) == Width && CVPixelBufferGetHeight(frame) == Height &&
               CMTimeCompare(pts, kCMTimeZero) == 0) {
        atomic_store(&result->matchingSize, true);
    }
}

static bool decode(CMSampleBufferRef sample) {
    struct Decoded result = {0};
    VTDecompressionOutputCallbackRecord callback = { decodedOutput, &result };
    VTDecompressionSessionRef session = NULL;
    OSStatus status = VTDecompressionSessionCreate(NULL, CMSampleBufferGetFormatDescription(sample),
                                                   NULL, NULL, &callback, &session);
    if (status != noErr) {
        fprintf(stderr, "create H.264 decoder: %d\n", (int)status);
        return false;
    }
    status = VTDecompressionSessionDecodeFrame(session, sample, 0, NULL, NULL);
    if (status == noErr) {
        status = VTDecompressionSessionWaitForAsynchronousFrames(session);
    }
    VTDecompressionSessionInvalidate(session);
    CFRelease(session);

    OSStatus asynchronous = atomic_load(&result.error);
    bool matched = atomic_load(&result.matchingSize);
    if (status != noErr || asynchronous != noErr || !matched) {
        fprintf(stderr, "decode H.264 result: status=%d callback=%d matching_frame=%d\n",
                (int)status, (int)asynchronous, matched);
        return false;
    }
    printf("VideoToolbox decoded the %dx%d frame at the original timestamp.\n", Width, Height);
    return true;
}

int main(void) {
    struct Encoded result = {0};
    VTCompressionSessionRef session = NULL;
    OSStatus status = VTCompressionSessionCreate(NULL, Width, Height, kCMVideoCodecType_H264, NULL, NULL, NULL,
                                                 output, &result, &session);
    if (status != noErr) {
        fprintf(stderr, "create H.264 session: %d\n", (int)status);
        return 1;
    }
    status = VTSessionSetProperty(session, kVTCompressionPropertyKey_AllowFrameReordering, kCFBooleanFalse);
    if (status == noErr) {
        status = submit(session);
    }
    if (status == noErr) {
        status = VTCompressionSessionCompleteFrames(session, kCMTimeInvalid);
    }

    CFPropertyListRef accelerated = NULL;
    OSStatus property = VTSessionCopyProperty(session, kVTCompressionPropertyKey_UsingHardwareAcceleratedVideoEncoder,
                                              NULL, &accelerated);
    bool modeKnown = property == noErr && accelerated && CFGetTypeID(accelerated) == CFBooleanGetTypeID();
    bool hardware = modeKnown && CFBooleanGetValue((CFBooleanRef)accelerated);
    if (accelerated) {
        CFRelease(accelerated);
    }
    VTCompressionSessionInvalidate(session);
    CFRelease(session);

    CMSampleBufferRef encoded = atomic_exchange(&result.sample, NULL);
    size_t bytes = encoded ? CMSampleBufferGetTotalSampleSize(encoded) : 0;
    OSStatus asynchronous = atomic_load(&result.error);
    if (status != noErr || asynchronous != noErr || bytes == 0 || !modeKnown) {
        fprintf(stderr, "H.264 result: status=%d callback=%d mode=%d bytes=%zu\n",
                (int)status, (int)asynchronous, (int)property, bytes);
        if (encoded) {
            CFRelease(encoded);
        }
        return 2;
    }
    printf("VideoToolbox %s H.264 encoder: %zu bytes\n", hardware ? "hardware" : "software", bytes);
    bool decoded = decode(encoded);
    CFRelease(encoded);
    return decoded ? 0 : 3;
}
