#include <CoreMedia/CoreMedia.h>
#include <CoreVideo/CoreVideo.h>
#include <VideoToolbox/VideoToolbox.h>

#include <stdatomic.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>

enum { Width = 128, Height = 96 };

struct Encoded {
    _Atomic size_t bytes;
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
        atomic_fetch_add(&encoded->bytes, CMSampleBufferGetTotalSampleSize(sample));
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

    size_t bytes = atomic_load(&result.bytes);
    OSStatus asynchronous = atomic_load(&result.error);
    if (status != noErr || asynchronous != noErr || bytes == 0 || !modeKnown) {
        fprintf(stderr, "H.264 result: status=%d callback=%d mode=%d bytes=%zu\n",
                (int)status, (int)asynchronous, (int)property, bytes);
        return 2;
    }
    printf("VideoToolbox %s H.264 encoder: %zu bytes\n", hardware ? "hardware" : "software", bytes);
    return 0;
}
