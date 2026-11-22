#import <Foundation/Foundation.h>
#include <algorithm>
#include <chrono>
#include <condition_variable>
#include <limits>
#include <memory>
#include <mutex>
#include <optional>
#include <utility>
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
#include "sdk/objc/components/video_frame_buffer/RTCCVPixelBuffer.h"
#include "sdk/objc/native/api/video_decoder_factory.h"
#include "sdk/objc/native/api/video_frame_buffer.h"
#include "sdk/objc/native/api/video_encoder_factory.h"

namespace {

using namespace std::chrono_literals;
constexpr uint32_t kRtpClockHz = 90'000;

class EncodedFrame final : public webrtc::EncodedImageCallback {
 public:
    void Expect(uint32_t timestamp) {
        std::lock_guard lock(mutex_);
        expected_ = timestamp;
        image_.reset();
        dropped_ = false;
    }

    Result OnEncodedImage(const webrtc::EncodedImage& image,
                          const webrtc::CodecSpecificInfo*) override {
        {
            std::lock_guard lock(mutex_);
            if (expected_ != image.RtpTimestamp() || image_ || dropped_) {
                return Result(Result::ERROR_SEND_FAILED);
            }
            image_ = image;
        }
        ready_.notify_one();
        return Result(Result::OK);
    }

    void OnFrameDropped(uint32_t timestamp, int, bool) override {
        {
            std::lock_guard lock(mutex_);
            if (expected_ != timestamp || image_) {
                return;
            }
            dropped_ = true;
        }
        ready_.notify_one();
    }

    std::optional<webrtc::EncodedImage> Wait() {
        std::unique_lock lock(mutex_);
        ready_.wait_for(lock, 5s, [&] { return image_.has_value() || dropped_; });
        expected_.reset();
        return std::exchange(image_, std::nullopt);
    }

 private:
    std::mutex mutex_;
    std::condition_variable ready_;
    std::optional<uint32_t> expected_;
    std::optional<webrtc::EncodedImage> image_;
    bool dropped_ = false;
};

struct DecodedOutput {
    LinkoH264StreamResult summary;
    webrtc::scoped_refptr<webrtc::VideoFrameBuffer> pixels;
};

class DecodedFrame final : public webrtc::DecodedImageCallback {
 public:
    void Expect(uint32_t timestamp, bool keep_pixels) {
        std::lock_guard lock(mutex_);
        expected_ = timestamp;
        keep_pixels_ = keep_pixels;
        decoded_.reset();
    }

    int32_t Decoded(webrtc::VideoFrame& frame) override {
        std::lock_guard lock(mutex_);
        if (expected_ != frame.rtp_timestamp() || decoded_) {
            return 0;
        }
        DecodedOutput output{
            {{static_cast<uint32_t>(frame.width()), static_cast<uint32_t>(frame.height()),
              frame.rtp_timestamp(), 0},
             0, 0},
            nullptr};
        if (keep_pixels_) {
            output.pixels = frame.video_frame_buffer();
        } else {
            auto planar = frame.video_frame_buffer()->ToI420ForInspection();
            if (!planar || !planar->DataY()) {
                return 0;
            }
            output.summary.decoded_luma = *planar->DataY();
        }
        decoded_ = std::move(output);
        ready_.notify_one();
        return 0;
    }

    std::optional<DecodedOutput> Wait() {
        std::unique_lock lock(mutex_);
        ready_.wait_for(lock, 5s, [&] { return decoded_.has_value(); });
        expected_.reset();
        return std::exchange(decoded_, std::nullopt);
    }

 private:
    std::mutex mutex_;
    std::condition_variable ready_;
    std::optional<uint32_t> expected_;
    std::optional<DecodedOutput> decoded_;
    bool keep_pixels_ = false;
};

template <typename Codec>
bool ReleaseCodec(std::unique_ptr<Codec> codec) {
    @try {
        return !codec || codec->Release() == 0;
    } @catch (NSException* exception) {
        (void)exception;
        return false;
    }
}

bool ValidRequest(const LinkoH264Request& request) {
    return request.version == LINKO_H264_PROBE_V1 && request.width >= 64 && request.height >= 64 &&
           request.width <= 1920 && request.height <= 1080 && (request.width & 1) == 0 &&
           (request.height & 1) == 0;
}

}  // namespace

struct LinkoH264Session {
    explicit LinkoH264Session(const LinkoH264Request& request) : request_(request) {}
    ~LinkoH264Session() { Stop(); }

    int Start() {
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
        encoder_ = encoder_factory->Create(environment_, *format);
        decoder_ = decoder_factory->Create(environment_, *format);
        if (!encoder_ || !decoder_) {
            return LINKO_H264_UNAVAILABLE;
        }

        webrtc::VideoCodec codec{};
        codec.codecType = webrtc::kVideoCodecH264;
        codec.width = static_cast<uint16_t>(request_.width);
        codec.height = static_cast<uint16_t>(request_.height);
        codec.startBitrate = 240;
        codec.maxBitrate = 300;
        codec.maxFramerate = 30;
        if (encoder_->InitEncode(&codec, webrtc::VideoEncoder::Settings(1, 1200)) != 0 ||
            encoder_->RegisterEncodeCompleteCallback(&encoded_) != 0) {
            return LINKO_H264_ENCODE_FAILED;
        }
        webrtc::VideoDecoder::Settings settings;
        settings.set_codec_type(webrtc::kVideoCodecH264);
        settings.set_number_of_cores(1);
        if (!decoder_->Configure(settings) || decoder_->RegisterDecodeCompleteCallback(&decoded_) != 0) {
            return LINKO_H264_DECODE_FAILED;
        }
        return LINKO_H264_OK;
    }

    int Submit(uint32_t timestamp, uint8_t luma, LinkoH264StreamResult& result,
               CVPixelBufferRef source = nullptr, CVPixelBufferRef* preview = nullptr) {
        if (failed_) {
            return LINKO_H264_SESSION_FAILED;
        }
        if (source && (CVPixelBufferGetWidth(source) != request_.width ||
                       CVPixelBufferGetHeight(source) != request_.height ||
                       CVPixelBufferGetPixelFormatType(source) != kCVPixelFormatType_32BGRA)) {
            return LINKO_H264_BAD_REQUEST;
        }
        const bool first = !last_timestamp_;
        const uint32_t advance = timestamp - last_timestamp_.value_or(request_.rtp_timestamp);
        if ((first && advance != 0) || (!first && (advance == 0 || advance > kRtpClockHz))) {
            return LINKO_H264_BAD_REQUEST;
        }
        const int64_t time_delta_us = static_cast<int64_t>(advance) * 1'000'000 / kRtpClockHz;
        if (timestamp_us_ > std::numeric_limits<int64_t>::max() - time_delta_us) {
            return LINKO_H264_BAD_REQUEST;
        }
        timestamp_us_ += time_delta_us;

        // One frame may be in flight. A codec failure poisons the stream until
        // close, so an eventual late callback cannot be used in another submit.
        failed_ = true;
        const int width = static_cast<int>(request_.width);
        const int height = static_cast<int>(request_.height);
        webrtc::scoped_refptr<webrtc::VideoFrameBuffer> buffer;
        if (source) {
            buffer = webrtc::ObjCToNativeVideoFrameBuffer(
                [[RTC_OBJC_TYPE(RTCCVPixelBuffer) alloc] initWithPixelBuffer:source]);
        } else {
            const std::vector<uint8_t> luminance(static_cast<size_t>(width * height), luma);
            const std::vector<uint8_t> chroma(static_cast<size_t>((width / 2) * (height / 2)), 128);
            buffer = webrtc::I420Buffer::Copy(width, height, luminance.data(), width,
                                             chroma.data(), width / 2, chroma.data(), width / 2);
        }
        if (!buffer) {
            return LINKO_H264_ENCODE_FAILED;
        }
        auto frame = webrtc::VideoFrame::Builder()
                         .set_video_frame_buffer(buffer)
                         .set_rtp_timestamp(timestamp)
                         .set_timestamp_us(timestamp_us_)
                         .build();
        const std::vector<webrtc::VideoFrameType> frame_types{
            first ? webrtc::VideoFrameType::kVideoFrameKey : webrtc::VideoFrameType::kVideoFrameDelta};

        encoded_.Expect(timestamp);
        if (encoder_->Encode(frame, &frame_types) != 0) {
            return LINKO_H264_ENCODE_FAILED;
        }
        auto encoded = encoded_.Wait();
        if (!encoded || !encoded->size() || encoded->size() > std::numeric_limits<uint32_t>::max()) {
            return LINKO_H264_ENCODE_FAILED;
        }
        decoded_.Expect(timestamp, preview != nullptr);
        if (decoder_->Decode(*encoded, 0) != 0) {
            return LINKO_H264_DECODE_FAILED;
        }
        auto decoded = decoded_.Wait();
        if (!decoded) {
            return LINKO_H264_DECODE_FAILED;
        }
        if (decoded->summary.frame.width != request_.width || decoded->summary.frame.height != request_.height ||
            decoded->summary.frame.rtp_timestamp != timestamp) {
            return LINKO_H264_FRAME_MISMATCH;
        }
        decoded->summary.frame.encoded_bytes = static_cast<uint32_t>(encoded->size());
        decoded->summary.key_frame = encoded->IsKey();
        if (preview) {
            id image = webrtc::NativeToObjCVideoFrameBuffer(decoded->pixels);
            if (![image isKindOfClass:[RTC_OBJC_TYPE(RTCCVPixelBuffer) class]]) {
                return LINKO_H264_DECODE_FAILED;
            }
            auto* native = (RTC_OBJC_TYPE(RTCCVPixelBuffer)*)image;
            CVPixelBufferRef pixels = native.pixelBuffer;
            if (!pixels) {
                return LINKO_H264_DECODE_FAILED;
            }
            *preview = CVPixelBufferRetain(pixels);
        }
        result = decoded->summary;
        last_timestamp_ = timestamp;
        failed_ = false;
        return LINKO_H264_OK;
    }

    int Stop() {
        const bool decoder_ok = ReleaseCodec(std::move(decoder_));
        const bool encoder_ok = ReleaseCodec(std::move(encoder_));
        return decoder_ok && encoder_ok ? LINKO_H264_OK : LINKO_H264_PLATFORM_ERROR;
    }

 private:
    LinkoH264Request request_;
    int64_t timestamp_us_ = 1000;
    std::optional<uint32_t> last_timestamp_;
    bool failed_ = false;
    // Keep callback storage and environment alive until after codec release.
    EncodedFrame encoded_;
    DecodedFrame decoded_;
    webrtc::Environment environment_ = webrtc::CreateEnvironment();
    std::unique_ptr<webrtc::VideoEncoder> encoder_;
    std::unique_ptr<webrtc::VideoDecoder> decoder_;
};

extern "C" int linko_rw04_h264_open(const LinkoH264Request* request, LinkoH264Session** session) {
    if (!request || !session || *session || !ValidRequest(*request)) {
        return LINKO_H264_BAD_REQUEST;
    }
    @autoreleasepool {
        @try {
            auto pending = std::make_unique<LinkoH264Session>(*request);
            int status = pending->Start();
            if (status == LINKO_H264_OK) {
                *session = pending.release();
            }
            return status;
        } @catch (NSException* exception) {
            (void)exception;
            return LINKO_H264_PLATFORM_ERROR;
        }
    }
}

extern "C" int linko_rw04_h264_submit(LinkoH264Session* session, uint32_t timestamp, uint8_t luma,
                                       LinkoH264StreamResult* result) {
    if (!session || !result || luma < 16 || luma > 235) {
        return LINKO_H264_BAD_REQUEST;
    }
    *result = {};
    @autoreleasepool {
        @try {
            return session->Submit(timestamp, luma, *result);
        } @catch (NSException* exception) {
            (void)exception;
            return LINKO_H264_PLATFORM_ERROR;
        }
    }
}

extern "C" int linko_rw04_h264_submit_pixels(LinkoH264Session* session, uint32_t timestamp,
                                              CVPixelBufferRef source, CVPixelBufferRef* decoded) {
    if (!session || !source || !decoded || *decoded) {
        return LINKO_H264_BAD_REQUEST;
    }
    LinkoH264StreamResult result{};
    @autoreleasepool {
        @try {
            return session->Submit(timestamp, 0, result, source, decoded);
        } @catch (NSException* exception) {
            (void)exception;
            return LINKO_H264_PLATFORM_ERROR;
        }
    }
}

extern "C" int linko_rw04_h264_close(LinkoH264Session** session) {
    if (!session) {
        return LINKO_H264_BAD_REQUEST;
    }
    @autoreleasepool {
        std::unique_ptr<LinkoH264Session> stopping(std::exchange(*session, nullptr));
        return stopping ? stopping->Stop() : LINKO_H264_OK;
    }
}

extern "C" int linko_rw04_h264_roundtrip(const LinkoH264Request* request, LinkoH264Result* result) {
    if (!result) {
        return LINKO_H264_BAD_REQUEST;
    }
    LinkoH264Session* session = nullptr;
    int status = linko_rw04_h264_open(request, &session);
    if (status != LINKO_H264_OK) {
        return status;
    }
    LinkoH264StreamResult decoded{};
    status = linko_rw04_h264_submit(session, request->rtp_timestamp, 16, &decoded);
    int close_status = linko_rw04_h264_close(&session);
    if (status == LINKO_H264_OK && close_status == LINKO_H264_OK) {
        *result = decoded.frame;
    }
    return status != LINKO_H264_OK ? status : close_status;
}
