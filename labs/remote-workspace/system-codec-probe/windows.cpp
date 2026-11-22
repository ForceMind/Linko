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
#include <cstring>

using Microsoft::WRL::ComPtr;

namespace {
constexpr UINT32 width = 128;
constexpr UINT32 height = 96;
constexpr UINT32 luma = width * height;
constexpr UINT32 nv12Bytes = luma * 3 / 2;
constexpr LONGLONG frameDuration = 10'000'000 / 30;

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

void submitFrame(IMFTransform* encoder) {
    ComPtr<IMFMediaBuffer> buffer;
    auto sample = sampleWithBuffer(nv12Bytes, MF_1_BYTE_ALIGNMENT, buffer);
    BYTE* pixels{};
    check(buffer->Lock(&pixels, nullptr, nullptr), "lock NV12 buffer");
    std::memset(pixels, 16, luma);
    std::memset(pixels + luma, 128, nv12Bytes - luma);
    check(buffer->Unlock(), "unlock NV12 buffer");
    check(buffer->SetCurrentLength(nv12Bytes), "commit NV12 buffer");
    check(sample->SetSampleTime(0), "set sample timestamp");
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

bool decodeFrame(IMFMediaType* type, IMFSample* encoded) {
    ComPtr<IMFTransform> decoder;
    check(CoCreateInstance(CLSID_CMSH264DecoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                           IID_PPV_ARGS(&decoder)), "activate system H.264 decoder");
    check(decoder->SetInputType(0, type, 0), "configure H.264 decoder input");
    selectDecodedType(decoder.Get());
    check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "start decoder streaming");
    check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "start decoder stream");
    check(decoder->ProcessInput(0, encoded, 0), "submit H.264 sample");
    check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "mark decoder input complete");
    check(decoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain decoder");
    auto decoded = readOutput(decoder.Get());
    if (decoded.status == MF_E_TRANSFORM_STREAM_CHANGE) {
        selectDecodedType(decoder.Get());
        decoded = readOutput(decoder.Get());
    }
    check(decoded.status, "read decoded frame");
    if (!decoded.sample) {
        return false;
    }
    DWORD length{};
    check(decoded.sample->GetTotalLength(&length), "read decoded NV12 length");
    ComPtr<IMFMediaType> layout;
    check(decoder->GetOutputCurrentType(0, &layout), "read decoded video layout");
    UINT32 decodedWidth{}, decodedHeight{};
    check(MFGetAttributeSize(layout.Get(), MF_MT_FRAME_SIZE, &decodedWidth, &decodedHeight),
          "read decoded frame size");
    check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish decoder streaming");
    return length >= nv12Bytes && decodedWidth == width && decodedHeight == height;
}

int probe() {
    ComPtr<IMFTransform> encoder;
    check(CoCreateInstance(CLSID_CMSH264EncoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                           IID_PPV_ARGS(&encoder)), "activate system H.264 encoder");
    auto outputType = videoType(MFVideoFormat_H264);
    check(outputType->SetUINT32(MF_MT_AVG_BITRATE, 200'000), "set output bitrate");
    check(outputType->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base), "set H.264 profile");
    check(encoder->SetOutputType(0, outputType.Get(), 0), "configure H.264 output");
    auto inputType = videoType(MFVideoFormat_NV12);
    check(encoder->SetInputType(0, inputType.Get(), 0), "configure NV12 input");

    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "start media streaming");
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "start media stream");
    submitFrame(encoder.Get());
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "mark input complete");
    check(encoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain encoder");
    auto encoded = readOutput(encoder.Get());
    check(encoded.status, "read H.264 sample");
    DWORD size{};
    if (encoded.sample) {
        check(encoded.sample->GetTotalLength(&size), "read H.264 length");
    }
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish media streaming");

    ComPtr<IMFMediaType> actual;
    check(encoder->GetOutputCurrentType(0, &actual), "read negotiated output");
    GUID subtype{};
    check(actual->GetGUID(MF_MT_SUBTYPE, &subtype), "read negotiated codec");
    if (subtype != MFVideoFormat_H264 || size == 0) {
        return 2;
    }
    std::printf("Media Foundation system software H.264 encoder: %lu bytes\n", size);
    if (!decodeFrame(actual.Get(), encoded.sample.Get())) {
        return 3;
    }
    std::printf("Media Foundation decoded the %ux%u NV12 frame.\n", width, height);
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
        return probe();
    } catch (HRESULT) {
        return 1;
    }
}
