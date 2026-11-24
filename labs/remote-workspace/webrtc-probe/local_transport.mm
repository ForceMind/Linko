#import <Foundation/Foundation.h>

#include "linko_probe/loopback_preview.h"
#include "sdk/objc/components/video_codec/RTCVideoDecoderFactoryH264.h"
#include "sdk/objc/components/video_codec/RTCVideoEncoderFactoryH264.h"
#include "sdk/objc/components/video_frame_buffer/RTCCVPixelBuffer.h"
#include "sdk/objc/native/api/video_decoder_factory.h"
#include "sdk/objc/native/api/video_encoder_factory.h"
#include "sdk/objc/native/api/video_frame_buffer.h"

namespace {
constexpr char kVideoCodec[] = "H264";

auto CreateLoopbackEncoderFactory() {
    return webrtc::ObjCToNativeVideoEncoderFactory([RTC_OBJC_TYPE(RTCVideoEncoderFactoryH264) new]);
}

auto CreateLoopbackDecoderFactory() {
    return webrtc::ObjCToNativeVideoDecoderFactory([RTC_OBJC_TYPE(RTCVideoDecoderFactoryH264) new]);
}
}  // namespace

#include "linko_probe/local_transport_common.inc"

#ifdef LINKO_RW04_NETWORK_PREVIEW
struct LinkoLoopbackPreview {
    std::mutex mutex;
    std::unique_ptr<Runtime> runtime = std::make_unique<Runtime>();
    std::unique_ptr<Endpoint> sending;
    std::unique_ptr<Endpoint> receiving;
    webrtc::scoped_refptr<FixtureTrack> source;
    bool closed = false;
};

extern "C" LinkoLoopbackPreview* linko_rw04_loopback_open(void) {
    auto preview = std::make_unique<LinkoLoopbackPreview>();
    if (!preview->runtime->Start()) {
        return nullptr;
    }
    auto& signaling = *preview->runtime->signaling;
    preview->sending = std::make_unique<Endpoint>(signaling, 0);
    preview->receiving = std::make_unique<Endpoint>(signaling, 0, true);
    preview->source = signaling.BlockingCall([] { return webrtc::make_ref_counted<FixtureTrack>(); });
    std::string offer;
    if (!preview->runtime->Connect(*preview->sending, *preview->receiving, preview->source, offer)) {
        return nullptr;
    }
    return preview.release();
}

extern "C" void linko_rw04_loopback_close(LinkoLoopbackPreview* preview) {
    delete preview;
}

extern "C" void linko_rw04_loopback_revoke(LinkoLoopbackPreview* preview) {
    std::lock_guard lock(preview->mutex);
    if (!preview->runtime) {
        return;
    }
    const bool receiver_closed = preview->receiving->Close();
    const bool sender_closed = preview->sending->Close();
    preview->receiving.reset();
    preview->sending.reset();
    preview->source = nullptr;
    preview->runtime.reset();
    preview->closed = receiver_closed && sender_closed;
}

extern "C" bool linko_rw04_loopback_was_closed(LinkoLoopbackPreview* preview) {
    std::lock_guard lock(preview->mutex);
    return preview->closed;
}

extern "C" bool linko_rw04_loopback_submit(LinkoLoopbackPreview* preview, CVPixelBufferRef input) {
    std::lock_guard lock(preview->mutex);
    if (!preview->runtime || !input || preview->receiving->observer.invalid()) {
        return false;
    }
    auto buffer = webrtc::ObjCToNativeVideoFrameBuffer(
        [[RTC_OBJC_TYPE(RTCCVPixelBuffer) alloc] initWithPixelBuffer:input]);
    if (!buffer) {
        return false;
    }
    preview->source->SendBuffer(buffer);
    return true;
}

static double ClockSeconds(Clock::time_point time) {
    return std::chrono::duration<double>(time.time_since_epoch()).count();
}

extern "C" double linko_rw04_loopback_now(void) {
    return ClockSeconds(Clock::now());
}

extern "C" CVPixelBufferRef linko_rw04_loopback_take(LinkoLoopbackPreview* preview,
                                                      double* received_at) {
    std::lock_guard lock(preview->mutex);
    if (!preview->runtime) {
        return nullptr;
    }
    auto& observer = preview->receiving->observer;
    Clock::time_point receipt;
    auto frame = observer.sink.Take(receipt);
    if (!frame || observer.invalid() || preview->sending->observer.invalid()) {
        return nullptr;
    }
    id image = webrtc::NativeToObjCVideoFrameBuffer(frame);
    if (![image isKindOfClass:[RTC_OBJC_TYPE(RTCCVPixelBuffer) class]]) {
        return nullptr;
    }
    CVPixelBufferRef pixels = ((RTC_OBJC_TYPE(RTCCVPixelBuffer)*)image).pixelBuffer;
    if (pixels && received_at) {
        *received_at = ClockSeconds(receipt);
    }
    return pixels ? CVPixelBufferRetain(pixels) : nullptr;
}
#else
int main() {
    @autoreleasepool {
        return Run();
    }
}
#endif
