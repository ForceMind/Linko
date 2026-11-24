#ifndef NOMINMAX
#define NOMINMAX
#endif
#include <windows.h>
#include <codecapi.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <wmcodecdsp.h>
#include <wrl/client.h>

#include <atomic>
#include <algorithm>
#include <cstdio>
#include <cstring>
#include <map>
#include <span>
#include <vector>

#include "linko_probe/windows_h264_factory.h"
#include "api/video/i420_buffer.h"
#include "api/video_codecs/video_codec.h"
#include "api/video_codecs/video_decoder.h"
#include "api/video_codecs/video_encoder.h"
#include "api/video_codecs/h264_profile_level_id.h"
#include "common_video/h264/h264_common.h"
#include "modules/video_coding/include/video_error_codes.h"
#include "modules/video_coding/include/video_codec_interface.h"
#include "third_party/libyuv/include/libyuv/convert.h"
#include "third_party/libyuv/include/libyuv/convert_from.h"

using Microsoft::WRL::ComPtr;
namespace {
constexpr int kWidth = 128;
constexpr int kHeight = 96;
constexpr DWORD kPixels = kWidth * kHeight * 3 / 2;
constexpr LONGLONG kDuration = 10'000'000 / 30;
std::atomic<unsigned> encodedFrames{0};
std::atomic<unsigned> decodedFrames{0};
std::atomic<unsigned> releasedEncoders{0};
std::atomic<unsigned> releasedDecoders{0};

#define MF_TRY(operation) do { const HRESULT mfStatus = (operation); if (FAILED(mfStatus)) { \
    std::fprintf(stderr, "MFT adapter %s: 0x%08X\n", #operation, unsigned(mfStatus)); return mfStatus; } } while (false)

class ComScope {
 public:
    ComScope() : status_(CoInitializeEx(nullptr, COINIT_MULTITHREADED)) {}
    ~ComScope() { if (SUCCEEDED(status_)) CoUninitialize(); }
    bool valid() const { return SUCCEEDED(status_); }
 private:
    HRESULT status_;
};

webrtc::SdpVideoFormat Format() {
    return {"H264", {{"profile-level-id", "42c00d"}, {"packetization-mode", "1"},
                     {"level-asymmetry-allowed", "1"}}};
}

bool Supports(const webrtc::SdpVideoFormat& format) {
    const auto profile = webrtc::ParseSdpForH264ProfileLevelId(format.parameters);
    const auto mode = format.parameters.find("packetization-mode");
    return format.name == "H264" && profile &&
           profile->profile == webrtc::H264Profile::kProfileConstrainedBaseline &&
           profile->level == webrtc::H264Level::kLevel1_3 &&
           mode != format.parameters.end() && mode->second == "1";
}

HRESULT MediaType(const GUID& subtype, ComPtr<IMFMediaType>& type) {
    MF_TRY(MFCreateMediaType(&type));
    MF_TRY(type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video));
    MF_TRY(type->SetGUID(MF_MT_SUBTYPE, subtype));
    MF_TRY(MFSetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, kWidth, kHeight));
    MF_TRY(MFSetAttributeRatio(type.Get(), MF_MT_FRAME_RATE, 30, 1));
    MF_TRY(MFSetAttributeRatio(type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1));
    MF_TRY(type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive));
    return S_OK;
}

HRESULT Sample(DWORD size, DWORD alignment, ComPtr<IMFSample>& sample, ComPtr<IMFMediaBuffer>& buffer) {
    MF_TRY(MFCreateSample(&sample));
    MF_TRY(MFCreateAlignedMemoryBuffer(size, alignment, &buffer));
    MF_TRY(sample->AddBuffer(buffer.Get()));
    return S_OK;
}

HRESULT Read(IMFTransform* transform, ComPtr<IMFSample>& sample) {
    MFT_OUTPUT_STREAM_INFO info{};
    MF_TRY(transform->GetOutputStreamInfo(0, &info));
    ComPtr<IMFMediaBuffer> buffer;
    if (!(info.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
        MF_TRY(Sample(std::max<DWORD>(info.cbSize, kPixels), info.cbAlignment, sample, buffer));
    }
    MFT_OUTPUT_DATA_BUFFER output{};
    output.pSample = sample.Get();
    DWORD flags{};
    const HRESULT status = transform->ProcessOutput(0, 1, &output, &flags);
    ComPtr<IMFCollection> events;
    events.Attach(output.pEvents);
    if (sample.Get() != output.pSample) {
        sample.Reset();
        sample.Attach(output.pSample);
    }
    return status;
}

HRESULT CopyBytes(IMFSample* sample, std::vector<BYTE>& bytes) {
    ComPtr<IMFMediaBuffer> buffer;
    MF_TRY(sample->ConvertToContiguousBuffer(&buffer));
    BYTE* data{};
    DWORD size{};
    MF_TRY(buffer->Lock(&data, nullptr, &size));
    if (size > 0 && size <= 128 * 1024) {
        const std::span<const BYTE> source(data, size);
        bytes.assign(source.begin(), source.end());
    }
    MF_TRY(buffer->Unlock());
    return bytes.empty() ? E_INVALIDARG : S_OK;
}

HRESULT SelectNv12(IMFTransform* decoder) {
    for (DWORD index = 0; index < 32; ++index) {
        ComPtr<IMFMediaType> type;
        MF_TRY(decoder->GetOutputAvailableType(0, index, &type));
        GUID subtype{};
        MF_TRY(type->GetGUID(MF_MT_SUBTYPE, &subtype));
        if (subtype == MFVideoFormat_NV12) {
            return decoder->SetOutputType(0, type.Get(), 0);
        }
    }
    return MF_E_INVALIDMEDIATYPE;
}

HRESULT Start(IMFTransform* transform) {
    MF_TRY(transform->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0));
    return transform->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0);
}

HRESULT ForceKeyframe(IMFTransform* encoder) {
    ComPtr<ICodecAPI> controls;
    MF_TRY(encoder->QueryInterface(IID_PPV_ARGS(&controls)));
    VARIANT enabled{};
    enabled.vt = VT_UI4;
    enabled.ulVal = 1;
    return controls->SetValue(&CODECAPI_AVEncVideoForceKeyFrame, &enabled);
}

struct PendingFrame { uint32_t rtp; int64_t capture; };

class MfEncoder final : public webrtc::VideoEncoder {
 public:
    ~MfEncoder() override { Release(); }
    int InitEncode(const webrtc::VideoCodec* codec, const Settings&) override {
        if (!codec || codec->width != kWidth || codec->height != kHeight) return WEBRTC_VIDEO_CODEC_ERR_PARAMETER;
        Release();
        ComScope com;
        if (!com.valid()) return WEBRTC_VIDEO_CODEC_ERROR;
        return SUCCEEDED(Open()) ? WEBRTC_VIDEO_CODEC_OK : WEBRTC_VIDEO_CODEC_ERROR;
    }
    int32_t RegisterEncodeCompleteCallback(webrtc::EncodedImageCallback* callback) override {
        callback_ = callback;
        return WEBRTC_VIDEO_CODEC_OK;
    }
    int32_t Encode(const webrtc::VideoFrame& frame,
                   const std::vector<webrtc::VideoFrameType>* types) override {
        ComScope com;
        if (!com.valid() || !transform_ || !callback_) return WEBRTC_VIDEO_CODEC_UNINITIALIZED;
        if (paused_) return WEBRTC_VIDEO_CODEC_OK;
        if (frame.width() != kWidth || frame.height() != kHeight || pending_.size() >= 4)
            return WEBRTC_VIDEO_CODEC_ERR_PARAMETER;
        const bool key = needsKey_ || (types && std::find(types->begin(), types->end(),
                                     webrtc::VideoFrameType::kVideoFrameKey) != types->end());
        return SUCCEEDED(Submit(frame, key)) ? WEBRTC_VIDEO_CODEC_OK : WEBRTC_VIDEO_CODEC_ERROR;
    }
    void SetRates(const RateControlParameters& rates) override {
        const auto bitrate = rates.bitrate.get_sum_bps();
        paused_ = bitrate == 0;
        if (paused_) { needsKey_ = true; return; }
        ComScope com;
        if (!com.valid() || !transform_) return;
        ComPtr<ICodecAPI> controls;
        if (FAILED(transform_.As(&controls))) { rateFailed_ = true; return; }
        VARIANT value{};
        value.vt = VT_UI4;
        value.ulVal = bitrate;
        rateFailed_ = FAILED(controls->SetValue(&CODECAPI_AVEncCommonMeanBitRate, &value));
    }
    int32_t Release() override {
        ComScope com;
        if (transform_) {
            if (!com.valid()) return WEBRTC_VIDEO_CODEC_ERROR;
            transform_->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            transform_.Reset();
            ++releasedEncoders;
        }
        pending_.clear();
        needsKey_ = true;
        return WEBRTC_VIDEO_CODEC_OK;
    }
    EncoderInfo GetEncoderInfo() const override {
        EncoderInfo info;
        info.implementation_name = "Linko CI Media Foundation H264";
        info.is_hardware_accelerated = false;
        return info;
    }
 private:
    HRESULT Open() {
        MF_TRY(CoCreateInstance(CLSID_CMSH264EncoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                                IID_PPV_ARGS(&transform_)));
        ComPtr<ICodecAPI> controls;
        MF_TRY(transform_.As(&controls));
        VARIANT value{};
        value.vt = VT_BOOL;
        value.boolVal = VARIANT_TRUE;
        MF_TRY(controls->SetValue(&CODECAPI_AVLowLatencyMode, &value));
        value.vt = VT_UI4;
        value.ulVal = 1;
        MF_TRY(controls->SetValue(&CODECAPI_AVEncNumWorkerThreads, &value));
        ComPtr<IMFMediaType> output;
        MF_TRY(MediaType(MFVideoFormat_H264, output));
        MF_TRY(output->SetUINT32(MF_MT_AVG_BITRATE, 500'000));
        MF_TRY(output->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base));
        MF_TRY(output->SetUINT32(MF_MT_MPEG2_LEVEL, 13));
        MF_TRY(transform_->SetOutputType(0, output.Get(), 0));
        ComPtr<IMFMediaType> input;
        MF_TRY(MediaType(MFVideoFormat_NV12, input));
        MF_TRY(transform_->SetInputType(0, input.Get(), 0));
        MF_TRY(transform_->GetInputStreamInfo(0, &stream_));
        rateFailed_ = false;
        paused_ = false;
        return Start(transform_.Get());
    }
    HRESULT Submit(const webrtc::VideoFrame& frame, bool key) {
        if (rateFailed_) return E_FAIL;
        if (key) MF_TRY(ForceKeyframe(transform_.Get()));
        const auto pixels = frame.video_frame_buffer()->ToI420();
        if (!pixels) return E_INVALIDARG;
        ComPtr<IMFSample> sample;
        ComPtr<IMFMediaBuffer> buffer;
        MF_TRY(Sample(std::max<DWORD>(stream_.cbSize, kPixels),
                      std::max<DWORD>(stream_.cbAlignment, MF_16_BYTE_ALIGNMENT), sample, buffer));
        BYTE* data{};
        MF_TRY(buffer->Lock(&data, nullptr, nullptr));
        std::span<BYTE> nv12(data, kPixels);
        const int converted = libyuv::I420ToNV12(pixels->DataY(), pixels->StrideY(),
            pixels->DataU(), pixels->StrideU(), pixels->DataV(), pixels->StrideV(),
            nv12.data(), kWidth, nv12.subspan(kWidth * kHeight).data(), kWidth, kWidth, kHeight);
        MF_TRY(buffer->Unlock());
        if (converted) return E_INVALIDARG;
        MF_TRY(buffer->SetCurrentLength(kPixels));
        const auto time = ++sequence_ * kDuration;
        MF_TRY(sample->SetSampleTime(time));
        MF_TRY(sample->SetSampleDuration(kDuration));
        MF_TRY(transform_->ProcessInput(0, sample.Get(), 0));
        pending_.emplace(time, PendingFrame{frame.rtp_timestamp(), frame.timestamp_us() / 1000});
        needsKey_ = false;
        for (;;) {
            ComPtr<IMFSample> output;
            const HRESULT status = Read(transform_.Get(), output);
            if (status == MF_E_TRANSFORM_NEED_MORE_INPUT) return S_OK;
            MF_TRY(status);
            LONGLONG outputTime{};
            MF_TRY(output->GetSampleTime(&outputTime));
            const auto pending = pending_.find(outputTime);
            if (pending == pending_.end()) return E_UNEXPECTED;
            std::vector<BYTE> bytes;
            MF_TRY(CopyBytes(output.Get(), bytes));
            bool idr = false, sps = false, pps = false;
            for (const auto& nalu : webrtc::H264::FindNaluIndices(bytes)) {
                const auto type = webrtc::H264::ParseNaluType(bytes[nalu.payload_start_offset]);
                idr |= type == webrtc::H264::kIdr;
                sps |= type == webrtc::H264::kSps;
                pps |= type == webrtc::H264::kPps;
            }
            if (idr && (!sps || !pps)) {
                ComPtr<IMFMediaType> type;
                MF_TRY(transform_->GetOutputCurrentType(0, &type));
                UINT32 size{};
                MF_TRY(type->GetBlobSize(MF_MT_MPEG_SEQUENCE_HEADER, &size));
                if (!size || size > 4096) return E_INVALIDARG;
                std::vector<BYTE> header(size);
                MF_TRY(type->GetBlob(MF_MT_MPEG_SEQUENCE_HEADER, header.data(), size, nullptr));
                header.insert(header.end(), bytes.begin(), bytes.end());
                bytes = std::move(header);
            }
            for (const auto& nalu : webrtc::H264::FindNaluIndices(bytes)) {
                if (webrtc::H264::ParseNaluType(bytes[nalu.payload_start_offset]) == webrtc::H264::kSps &&
                    (nalu.payload_size < 4 || bytes[nalu.payload_start_offset + 1] != 66 ||
                     (bytes[nalu.payload_start_offset + 2] & 0xc0) != 0xc0 ||
                     bytes[nalu.payload_start_offset + 3] > 13)) return E_INVALIDARG;
            }
            webrtc::EncodedImage image;
            image.SetEncodedData(webrtc::EncodedImageBuffer::Create(bytes.data(), bytes.size()));
            image.SetRtpTimestamp(pending->second.rtp);
            image.capture_time_ms_ = pending->second.capture;
            image._encodedWidth = kWidth;
            image._encodedHeight = kHeight;
            image.SetFrameType(idr ? webrtc::VideoFrameType::kVideoFrameKey :
                                     webrtc::VideoFrameType::kVideoFrameDelta);
            webrtc::CodecSpecificInfo info{};
            info.codecType = webrtc::kVideoCodecH264;
            info.codecSpecific.H264.packetization_mode = webrtc::H264PacketizationMode::NonInterleaved;
            info.codecSpecific.H264.idr_frame = idr;
            pending_.erase(pending);
            if (callback_->OnEncodedImage(image, &info).error != webrtc::EncodedImageCallback::Result::OK)
                return E_FAIL;
            ++encodedFrames;
        }
    }
    ComPtr<IMFTransform> transform_;
    MFT_INPUT_STREAM_INFO stream_{};
    webrtc::EncodedImageCallback* callback_ = nullptr;
    std::map<LONGLONG, PendingFrame> pending_;
    LONGLONG sequence_ = 0;
    bool paused_ = false, needsKey_ = true, rateFailed_ = false;
};

class MfDecoder final : public webrtc::VideoDecoder {
 public:
    ~MfDecoder() override { Release(); }
    bool Configure(const Settings&) override {
        Release();
        ComScope com;
        return com.valid() && SUCCEEDED(Open());
    }
    int32_t RegisterDecodeCompleteCallback(webrtc::DecodedImageCallback* callback) override {
        callback_ = callback;
        return WEBRTC_VIDEO_CODEC_OK;
    }
    int32_t Decode(const webrtc::EncodedImage& image, int64_t) override {
        ComScope com;
        if (!com.valid() || !transform_ || !callback_) return WEBRTC_VIDEO_CODEC_UNINITIALIZED;
        if (!image.size() || image.size() > 128 * 1024 || pending_.size() >= 4)
            return WEBRTC_VIDEO_CODEC_ERR_PARAMETER;
        return SUCCEEDED(Submit(image)) ? WEBRTC_VIDEO_CODEC_OK : WEBRTC_VIDEO_CODEC_ERROR;
    }
    int32_t Release() override {
        ComScope com;
        if (transform_) {
            if (!com.valid()) return WEBRTC_VIDEO_CODEC_ERROR;
            transform_->ProcessMessage(MFT_MESSAGE_COMMAND_FLUSH, 0);
            transform_->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0);
            transform_.Reset();
            ++releasedDecoders;
        }
        pending_.clear();
        return WEBRTC_VIDEO_CODEC_OK;
    }
 private:
    HRESULT Open() {
        MF_TRY(CoCreateInstance(CLSID_CMSH264DecoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                                IID_PPV_ARGS(&transform_)));
        ComPtr<IMFAttributes> attributes;
        MF_TRY(transform_->GetAttributes(&attributes));
        MF_TRY(attributes->SetUINT32(MF_LOW_LATENCY, TRUE));
        ComPtr<IMFMediaType> input;
        MF_TRY(MediaType(MFVideoFormat_H264, input));
        MF_TRY(transform_->SetInputType(0, input.Get(), 0));
        MF_TRY(SelectNv12(transform_.Get()));
        return Start(transform_.Get());
    }
    HRESULT Submit(const webrtc::EncodedImage& image) {
        ComPtr<IMFSample> input;
        ComPtr<IMFMediaBuffer> buffer;
        MF_TRY(Sample(static_cast<DWORD>(image.size()), 0, input, buffer));
        BYTE* data{};
        MF_TRY(buffer->Lock(&data, nullptr, nullptr));
        std::memcpy(data, image.data(), image.size());
        MF_TRY(buffer->Unlock());
        MF_TRY(buffer->SetCurrentLength(static_cast<DWORD>(image.size())));
        const auto time = ++sequence_ * kDuration;
        MF_TRY(input->SetSampleTime(time));
        MF_TRY(input->SetSampleDuration(kDuration));
        MF_TRY(transform_->ProcessInput(0, input.Get(), 0));
        pending_.emplace(time, image.RtpTimestamp());
        for (;;) {
            ComPtr<IMFSample> output;
            const HRESULT status = Read(transform_.Get(), output);
            if (status == MF_E_TRANSFORM_NEED_MORE_INPUT) return S_OK;
            if (status == MF_E_TRANSFORM_STREAM_CHANGE) {
                MF_TRY(SelectNv12(transform_.Get()));
                continue;
            }
            MF_TRY(status);
            LONGLONG outputTime{};
            MF_TRY(output->GetSampleTime(&outputTime));
            const auto pending = pending_.find(outputTime);
            if (pending == pending_.end()) return E_UNEXPECTED;
            ComPtr<IMFMediaType> type;
            MF_TRY(transform_->GetOutputCurrentType(0, &type));
            UINT32 actualWidth{}, actualHeight{};
            MF_TRY(MFGetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, &actualWidth, &actualHeight));
            if (actualWidth != kWidth || actualHeight != kHeight) return E_INVALIDARG;
            const UINT32 stride = MFGetAttributeUINT32(type.Get(), MF_MT_DEFAULT_STRIDE, kWidth);
            std::vector<BYTE> bytes;
            MF_TRY(CopyBytes(output.Get(), bytes));
            if (stride < kWidth || stride > 4096 || bytes.size() < stride * kHeight * 3 / 2)
                return E_INVALIDARG;
            const std::span<const BYTE> nv12(bytes);
            auto pixels = webrtc::I420Buffer::Create(kWidth, kHeight);
            if (libyuv::NV12ToI420(nv12.data(), stride,
                  nv12.subspan(stride * kHeight).data(), stride, pixels->MutableDataY(),
                  pixels->StrideY(), pixels->MutableDataU(), pixels->StrideU(),
                  pixels->MutableDataV(), pixels->StrideV(), kWidth, kHeight)) return E_FAIL;
            auto frame = webrtc::VideoFrame::Builder().set_video_frame_buffer(pixels)
                             .set_rtp_timestamp(pending->second).build();
            pending_.erase(pending);
            if (callback_->Decoded(frame) < 0) return E_FAIL;
            ++decodedFrames;
        }
    }
    ComPtr<IMFTransform> transform_;
    webrtc::DecodedImageCallback* callback_ = nullptr;
    std::map<LONGLONG, uint32_t> pending_;
    LONGLONG sequence_ = 0;
};

class EncoderFactory final : public webrtc::VideoEncoderFactory {
 public:
    std::vector<webrtc::SdpVideoFormat> GetSupportedFormats() const override { return {Format()}; }
    std::unique_ptr<webrtc::VideoEncoder> Create(const webrtc::Environment&,
                                               const webrtc::SdpVideoFormat& format) override {
        return Supports(format) ? std::make_unique<MfEncoder>() : nullptr;
    }
};
class DecoderFactory final : public webrtc::VideoDecoderFactory {
 public:
    std::vector<webrtc::SdpVideoFormat> GetSupportedFormats() const override { return {Format()}; }
    std::unique_ptr<webrtc::VideoDecoder> Create(const webrtc::Environment&,
                                               const webrtc::SdpVideoFormat& format) override {
        return Supports(format) ? std::make_unique<MfDecoder>() : nullptr;
    }
};
#undef MF_TRY
}  // namespace

std::unique_ptr<webrtc::VideoEncoderFactory> CreateMfEncoderFactory() {
    return std::make_unique<EncoderFactory>();
}
std::unique_ptr<webrtc::VideoDecoderFactory> CreateMfDecoderFactory() {
    return std::make_unique<DecoderFactory>();
}
bool MfCodecCallbacksPassed() {
    std::printf("MFT PeerConnection: encoded=%u decoded=%u released_encoders=%u released_decoders=%u.\n",
                encodedFrames.load(), decodedFrames.load(), releasedEncoders.load(), releasedDecoders.load());
    return encodedFrames >= 6 && decodedFrames >= 6 && releasedEncoders >= 2 && releasedDecoders >= 2;
}
