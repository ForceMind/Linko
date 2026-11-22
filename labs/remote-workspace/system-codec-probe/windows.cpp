#define NOMINMAX
#include <windows.h>
#include <codecapi.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <wmcodecdsp.h>
#include <wrl/client.h>

#include <algorithm>
#include <cstdio>
#include <cstdlib>
#include <cstring>

using Microsoft::WRL::ComPtr;

namespace {
constexpr UINT32 width = 128;
constexpr UINT32 height = 96;
constexpr UINT32 luma = width * height;
constexpr UINT32 nv12Bytes = luma * 3 / 2;
constexpr LONGLONG frameDuration = 10'000'000 / 30;
constexpr UINT32 frameCount = 24;
constexpr UINT32 maxLowLatencyBacklog = 2;

enum class LatencyMode { Default, DecoderOnly, EncoderAndDecoder };

int fixtureLuma(UINT32 index) {
    return 24 + 8 * static_cast<int>(index);
}

void check(HRESULT result, char const* operation) {
    if (FAILED(result)) {
        std::fprintf(stderr, "%s failed: HRESULT 0x%08X\n", operation, static_cast<unsigned>(result));
        throw result;
    }
}

ComPtr<IMFMediaType> videoType(GUID const& codec) {
    ComPtr<IMFMediaType> type;
    check(MFCreateMediaType(&type), "create video type");
    check(type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video), "set video major type");
    check(type->SetGUID(MF_MT_SUBTYPE, codec), "set video subtype");
    check(MFSetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, width, height), "set frame size");
    check(MFSetAttributeRatio(type.Get(), MF_MT_FRAME_RATE, 30, 1), "set frame rate");
    check(MFSetAttributeRatio(type.Get(), MF_MT_PIXEL_ASPECT_RATIO, 1, 1), "set aspect ratio");
    check(type->SetUINT32(MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive), "set scan type");
    return type;
}

ComPtr<IMFSample> sampleWithBuffer(DWORD size, DWORD alignment, ComPtr<IMFMediaBuffer>& buffer) {
    ComPtr<IMFSample> sample;
    check(MFCreateSample(&sample), "create sample");
    check(MFCreateAlignedMemoryBuffer(size, alignment, &buffer), "create sample buffer");
    check(sample->AddBuffer(buffer.Get()), "attach buffer");
    return sample;
}

void submitFrame(IMFTransform* encoder, UINT32 index) {
    ComPtr<IMFMediaBuffer> buffer;
    auto sample = sampleWithBuffer(nv12Bytes, MF_1_BYTE_ALIGNMENT, buffer);
    BYTE* pixels{};
    check(buffer->Lock(&pixels, nullptr, nullptr), "lock NV12 buffer");
    std::memset(pixels, fixtureLuma(index), luma);
    std::memset(pixels + luma, 128, nv12Bytes - luma);
    check(buffer->Unlock(), "unlock NV12 buffer");
    check(buffer->SetCurrentLength(nv12Bytes), "commit NV12 buffer");
    check(sample->SetSampleTime(frameDuration * (index + 1)), "set sample timestamp");
    check(sample->SetSampleDuration(frameDuration), "set sample duration");
    check(encoder->ProcessInput(0, sample.Get(), 0), "encode NV12 sample");
}

struct Output {
    HRESULT status{};
    ComPtr<IMFSample> sample;
};

Output readOutput(IMFTransform* transform) {
    MFT_OUTPUT_STREAM_INFO stream{};
    check(transform->GetOutputStreamInfo(0, &stream), "read output stream requirements");
    ComPtr<IMFMediaBuffer> buffer;
    ComPtr<IMFSample> callerSample;
    if (!(stream.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
        callerSample = sampleWithBuffer(std::max<DWORD>(stream.cbSize, nv12Bytes), stream.cbAlignment, buffer);
    }
    MFT_OUTPUT_DATA_BUFFER output{};
    output.pSample = callerSample.Get();
    DWORD outputStatus{};
    HRESULT result = transform->ProcessOutput(0, 1, &output, &outputStatus);
    ComPtr<IMFCollection> events;
    events.Attach(output.pEvents);
    if (output.pSample != callerSample.Get()) {
        callerSample.Reset();
        callerSample.Attach(output.pSample);
    }
    return {result, callerSample};
}

void selectDecodedType(IMFTransform* decoder) {
    for (DWORD index = 0; index < 32; ++index) {
        ComPtr<IMFMediaType> type;
        HRESULT status = decoder->GetOutputAvailableType(0, index, &type);
        if (status == MF_E_NO_MORE_TYPES) {
            break;
        }
        check(status, "enumerate decoder output");
        GUID subtype{};
        check(type->GetGUID(MF_MT_SUBTYPE, &subtype), "read decoder output format");
        if (subtype == MFVideoFormat_NV12) {
            check(decoder->SetOutputType(0, type.Get(), 0), "select NV12 decoder output");
            return;
        }
    }
    check(MF_E_INVALIDMEDIATYPE, "find NV12 decoder output");
}

ComPtr<IMFTransform> startDecoder(IMFMediaType* type, bool lowLatency) {
    ComPtr<IMFTransform> decoder;
    check(CoCreateInstance(CLSID_CMSH264DecoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                           IID_PPV_ARGS(&decoder)), "activate system H.264 decoder");
    if (lowLatency) {
        ComPtr<IMFAttributes> attributes;
        check(decoder->GetAttributes(&attributes), "read H.264 decoder attributes");
        check(attributes->SetUINT32(MF_LOW_LATENCY, TRUE), "enable H.264 low-latency decoding");
    }
    check(decoder->SetInputType(0, type, 0), "configure H.264 decoder input");
    selectDecodedType(decoder.Get());
    check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "start decoder streaming");
    check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "start decoder stream");
    return decoder;
}

void verifyDecodedFrame(IMFTransform* decoder, IMFSample* sample, UINT32 index) {
    check(sample && index < frameCount ? S_OK : E_UNEXPECTED, "check decoded sample count");
    DWORD length{};
    check(sample->GetTotalLength(&length), "read decoded NV12 length");
    ComPtr<IMFMediaType> layout;
    check(decoder->GetOutputCurrentType(0, &layout), "read decoded video layout");
    GUID subtype{};
    check(layout->GetGUID(MF_MT_SUBTYPE, &subtype), "read decoded video format");
    UINT32 decodedWidth{}, decodedHeight{};
    check(MFGetAttributeSize(layout.Get(), MF_MT_FRAME_SIZE, &decodedWidth, &decodedHeight),
          "read decoded frame size");
    const bool layoutMatches = subtype == MFVideoFormat_NV12 && length >= nv12Bytes &&
                               decodedWidth == width && decodedHeight == height;
    check(layoutMatches ? S_OK : E_UNEXPECTED, "check decoded sample layout");

    LONGLONG timestamp{};
    check(sample->GetSampleTime(&timestamp), "read decoded timestamp");
    ComPtr<IMFMediaBuffer> buffer;
    check(sample->ConvertToContiguousBuffer(&buffer), "read contiguous decoded buffer");
    BYTE* pixels{};
    DWORD currentLength{};
    check(buffer->Lock(&pixels, nullptr, &currentLength), "lock decoded buffer");
    const int brightness = currentLength >= nv12Bytes ? pixels[width / 2] : -1;
    check(buffer->Unlock(), "unlock decoded buffer");
    if (timestamp != frameDuration * (index + 1) ||
        std::abs(brightness - fixtureLuma(index)) > 6) {
        std::fprintf(stderr, "Decoded frame %u: timestamp %lld, luma %d\n", index, timestamp, brightness);
        check(E_UNEXPECTED, "check decoded frame order and content");
    }
}

void readDecodedFrames(IMFTransform* decoder, UINT32& decodedCount) {
    for (;;) {
        auto decoded = readOutput(decoder);
        if (decoded.status == MF_E_TRANSFORM_STREAM_CHANGE) {
            selectDecodedType(decoder);
            decoded = readOutput(decoder);
        }
        if (decoded.status == MF_E_TRANSFORM_NEED_MORE_INPUT) {
            return;
        }
        check(decoded.status, "read decoded frame");
        verifyDecodedFrame(decoder, decoded.sample.Get(), decodedCount);
        ++decodedCount;
    }
}

void readEncodedFrames(IMFTransform* encoder, ComPtr<IMFTransform>& decoder,
                       UINT32& encodedCount, UINT32& decodedCount, bool lowLatency) {
    for (;;) {
        auto encoded = readOutput(encoder);
        if (encoded.status == MF_E_TRANSFORM_NEED_MORE_INPUT) {
            return;
        }
        check(encoded.status, "read H.264 sample");
        check(encoded.sample && encodedCount < frameCount ? S_OK : E_UNEXPECTED,
              "check H.264 sample count");
        DWORD size{};
        check(encoded.sample->GetTotalLength(&size), "read H.264 length");
        check(size != 0 ? S_OK : E_UNEXPECTED, "check H.264 sample length");
        if (!decoder) {
            ComPtr<IMFMediaType> actual;
            check(encoder->GetOutputCurrentType(0, &actual), "read negotiated output");
            GUID subtype{};
            check(actual->GetGUID(MF_MT_SUBTYPE, &subtype), "read negotiated codec");
            check(subtype == MFVideoFormat_H264 ? S_OK : E_UNEXPECTED, "check negotiated codec");
            decoder = startDecoder(actual.Get(), lowLatency);
        }
        check(decoder->ProcessInput(0, encoded.sample.Get(), 0), "submit H.264 sample");
        ++encodedCount;
        readDecodedFrames(decoder.Get(), decodedCount);
    }
}

int probe(LatencyMode mode, char const* modeName) {
    ComPtr<IMFTransform> encoder;
    check(CoCreateInstance(CLSID_CMSH264EncoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                           IID_PPV_ARGS(&encoder)), "activate system H.264 encoder");
    const bool lowLatencyEncoder = mode == LatencyMode::EncoderAndDecoder;
    const bool lowLatencyDecoder = mode != LatencyMode::Default;
    if (lowLatencyEncoder) {
        ComPtr<ICodecAPI> settings;
        check(encoder.As(&settings), "read H.264 encoder settings");
        VARIANT enabled{};
        enabled.vt = VT_BOOL;
        enabled.boolVal = VARIANT_TRUE;
        check(settings->SetValue(&CODECAPI_AVLowLatencyMode, &enabled), "enable H.264 low-latency encoding");
        VARIANT threads{};
        threads.vt = VT_UI4;
        threads.ulVal = 1;
        check(settings->SetValue(&CODECAPI_AVEncNumWorkerThreads, &threads), "configure one encoder worker");
    }
    auto outputType = videoType(MFVideoFormat_H264);
    check(outputType->SetUINT32(MF_MT_AVG_BITRATE, 200'000), "set output bitrate");
    check(outputType->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base), "set H.264 profile");
    check(encoder->SetOutputType(0, outputType.Get(), 0), "configure H.264 output");
    auto inputType = videoType(MFVideoFormat_NV12);
    check(encoder->SetInputType(0, inputType.Get(), 0), "configure NV12 input");

    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "start media streaming");
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "start media stream");
    ComPtr<IMFTransform> decoder;
    UINT32 encodedCount{};
    UINT32 decodedCount{};
    UINT32 maxBacklog{};
    for (UINT32 index = 0; index < frameCount; ++index) {
        submitFrame(encoder.Get(), index);
        readEncodedFrames(encoder.Get(), decoder, encodedCount, decodedCount, lowLatencyDecoder);
        maxBacklog = std::max(maxBacklog, index + 1 - decodedCount);
    }
    const UINT32 encodedBeforeDrain = encodedCount;
    const UINT32 framesBeforeDrain = decodedCount;
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "mark input complete");
    check(encoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain encoder");
    readEncodedFrames(encoder.Get(), decoder, encodedCount, decodedCount, lowLatencyDecoder);
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish media streaming");
    if (!decoder) {
        return 2;
    }
    check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "mark decoder input complete");
    check(decoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain decoder");
    readDecodedFrames(decoder.Get(), decodedCount);
    check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish decoder streaming");
    std::printf("Media Foundation system software H.264 (%s): %u encoded and %u decoded before drain, "
                "%u encoded and %u decoded total, maximum backlog %u.\n",
                modeName, encodedBeforeDrain,
                framesBeforeDrain, encodedCount, decodedCount, maxBacklog);
    if (encodedCount != frameCount || decodedCount != frameCount || !framesBeforeDrain ||
        (lowLatencyEncoder && maxBacklog > maxLowLatencyBacklog)) {
        return 3;
    }
    std::printf("Media Foundation verified %ux%u NV12 frames, timestamps and changing brightness.\n",
                width, height);
    return 0;
}
}

int main() {
    try {
        check(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "initialize COM");
        struct ComScope {
            ~ComScope() { CoUninitialize(); }
        } com;
        check(MFStartup(MF_VERSION), "start Media Foundation");
        struct MediaScope {
            ~MediaScope() { MFShutdown(); }
        } media;
        const int baseline = probe(LatencyMode::Default, "default");
        if (baseline != 0) {
            return baseline;
        }
        const int decoderOnly = probe(LatencyMode::DecoderOnly, "low-latency decoder");
        return decoderOnly == 0 ? probe(LatencyMode::EncoderAndDecoder, "low-latency encoder and decoder")
                                : decoderOnly;
    } catch (HRESULT) {
        return 1;
    }
}
