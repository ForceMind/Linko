#import <Foundation/Foundation.h>
#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <limits>
#include <mutex>
#include <optional>
#include <vector>

#include "api/environment/environment_factory.h"
#include "api/video/encoded_image.h"
#include "api/video/i420_buffer.h"
#include "api/video/video_frame.h"
#include "api/video_codecs/video_codec.h"
#include "api/video_codecs/video_decoder.h"
#include "api/video_codecs/video_encoder.h"
#include "linko_probe/h264_probe.h"
#include "sdk/objc/components/video_codec/RTCVideoDecoderFactoryH264.h"
#include "sdk/objc/components/video_codec/RTCVideoEncoderFactoryH264.h"
#include "sdk/objc/native/api/video_decoder_factory.h"
#include "sdk/objc/native/api/video_encoder_factory.h"

namespace {

using namespace std::chrono_literals;

class EncodedFrame final : public webrtc::EncodedImageCallback {
 public:
    Result OnEncodedImage(const webrtc::EncodedImage& image,
                          const webrtc::CodecSpecificInfo*) override {
        {
            std::lock_guard lock(mutex_);
            image_ = image;
            finished_ = true;
        }
        ready_.notify_one();
        return Result(Result::OK);
    }

    void OnFrameDropped(uint32_t, int, bool) override {
        {
            std::lock_guard lock(mutex_);
            finished_ = true;
        }
        ready_.notify_one();
    }

    std::optional<webrtc::EncodedImage> Wait() {
        std::unique_lock lock(mutex_);
        return ready_.wait_for(lock, 5s, [&] { return finished_; }) ? image_ : std::nullopt;
    }

 private:
    std::mutex mutex_;
    std::condition_variable ready_;
    std::optional<webrtc::EncodedImage> image_;
    bool finished_ = false;
};

class DecodedFrame final : public webrtc::DecodedImageCallback {
 public:
    int32_t Decoded(webrtc::VideoFrame& frame) override {
        {
            std::lock_guard lock(mutex_);
            dimensions_ = LinkoH264Result{
                static_cast<uint32_t>(frame.width()), static_cast<uint32_t>(frame.height()),
                frame.rtp_timestamp(), 0};
        }
        ready_.notify_one();
        return 0;
    }

    std::optional<LinkoH264Result> Wait() {
        std::unique_lock lock(mutex_);
        return ready_.wait_for(lock, 5s, [&] { return dimensions_.has_value(); })
                   ? dimensions_
                   : std::nullopt;
    }

 private:
    std::mutex mutex_;
    std::condition_variable ready_;
    std::optional<LinkoH264Result> dimensions_;
};

std::optional<webrtc::EncodedImage> Encode(webrtc::VideoEncoder& encoder, EncodedFrame& callback,
                                            const LinkoH264Request& request) {
    webrtc::VideoCodec codec{};
    codec.codecType = webrtc::kVideoCodecH264;
    codec.width = static_cast<uint16_t>(request.width);
    codec.height = static_cast<uint16_t>(request.height);
    codec.startBitrate = 240;
    codec.maxBitrate = 300;
    codec.maxFramerate = 30;

    if (encoder.InitEncode(&codec, webrtc::VideoEncoder::Settings(1, 1200)) != 0) {
        return std::nullopt;
    }
    std::optional<webrtc::EncodedImage> encoded;
    if (encoder.RegisterEncodeCompleteCallback(&callback) == 0) {
        auto buffer = webrtc::I420Buffer::Create(static_cast<int>(request.width),
                                                  static_cast<int>(request.height));
        webrtc::I420Buffer::SetBlack(buffer.get());
        auto frame = webrtc::VideoFrame::Builder()
                         .set_video_frame_buffer(buffer)
                         .set_rtp_timestamp(request.rtp_timestamp)
                         .set_timestamp_us(1000)
                         .build();
        const std::vector<webrtc::VideoFrameType> frame_types{webrtc::VideoFrameType::kVideoFrameKey};
        if (encoder.Encode(frame, &frame_types) == 0) {
            encoded = callback.Wait();
        }
    }
    if (encoder.Release() != 0) {
        return std::nullopt;
    }
    return encoded;
}

std::optional<LinkoH264Result> Decode(webrtc::VideoDecoder& decoder, DecodedFrame& callback,
                                      const webrtc::EncodedImage& encoded) {
    webrtc::VideoDecoder::Settings settings;
    settings.set_codec_type(webrtc::kVideoCodecH264);
    settings.set_number_of_cores(1);
    if (!decoder.Configure(settings)) {
        return std::nullopt;
    }
    std::optional<LinkoH264Result> decoded;
    if (decoder.RegisterDecodeCompleteCallback(&callback) == 0 && decoder.Decode(encoded, 0) == 0) {
        decoded = callback.Wait();
    }
    if (decoder.Release() != 0) {
        return std::nullopt;
    }
    return decoded;
}

int Roundtrip(const LinkoH264Request& request, LinkoH264Result& result) {
    // Destroy native codecs before the objects used by their callbacks.
    EncodedFrame encoded_frame;
    DecodedFrame decoded_frame;
    auto environment = webrtc::CreateEnvironment();
    auto encoder_factory = webrtc::ObjCToNativeVideoEncoderFactory(
        [RTC_OBJC_TYPE(RTCVideoEncoderFactoryH264) new]);
    auto decoder_factory = webrtc::ObjCToNativeVideoDecoderFactory(
        [RTC_OBJC_TYPE(RTCVideoDecoderFactoryH264) new]);

    const auto encoders = encoder_factory->GetSupportedFormats();
    const auto decoders = decoder_factory->GetSupportedFormats();
    const auto format = std::find_if(encoders.begin(), encoders.end(), [&](const auto& candidate) {
        return std::find(decoders.begin(), decoders.end(), candidate) != decoders.end();
    });
    if (format == encoders.end()) {
        return LINKO_H264_UNAVAILABLE;
    }
    auto encoder = encoder_factory->Create(environment, *format);
    auto decoder = decoder_factory->Create(environment, *format);
    if (!encoder || !decoder) {
        return LINKO_H264_UNAVAILABLE;
    }

    auto encoded = Encode(*encoder, encoded_frame, request);
    if (!encoded || !encoded->size() || encoded->size() > std::numeric_limits<uint32_t>::max()) {
        return LINKO_H264_ENCODE_FAILED;
    }
    auto decoded = Decode(*decoder, decoded_frame, *encoded);
    if (!decoded) {
        return LINKO_H264_DECODE_FAILED;
    }
    if (decoded->width != request.width || decoded->height != request.height ||
        decoded->rtp_timestamp != request.rtp_timestamp) {
        return LINKO_H264_FRAME_MISMATCH;
    }
    decoded->encoded_bytes = static_cast<uint32_t>(encoded->size());
    result = *decoded;
    return LINKO_H264_OK;
}

}  // namespace

extern "C" int linko_rw04_h264_roundtrip(const LinkoH264Request* request, LinkoH264Result* result) {
    if (!request || !result || request->version != LINKO_H264_PROBE_V1 ||
        request->width < 64 || request->height < 64 || request->width > 1920 ||
        request->height > 1080 || (request->width & 1) != 0 || (request->height & 1) != 0) {
        return LINKO_H264_BAD_REQUEST;
    }
    *result = {};
    @autoreleasepool {
        @try {
            return Roundtrip(*request, *result);
        } @catch (NSException* exception) {
            (void)exception;
            return LINKO_H264_PLATFORM_ERROR;
        }
    }
}
