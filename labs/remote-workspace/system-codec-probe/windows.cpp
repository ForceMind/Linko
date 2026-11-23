#define NOMINMAX
#include <windows.h>
#include <codecapi.h>
#include <fcntl.h>
#include <io.h>
#include <mfapi.h>
#include <mferror.h>
#include <mfidl.h>
#include <mftransform.h>
#include <wmcodecdsp.h>
#include <wrl/client.h>

#include <algorithm>
#include <array>
#include <chrono>
#include <cstdio>
#include <cstdlib>
#include <cstring>
#include <memory>
#include <optional>
#include <vector>
#include "codec_worker_protocol.h"
#include "windows_fixture.h"

using Microsoft::WRL::ComPtr;

namespace {
using Clock = std::chrono::steady_clock;
constexpr LONGLONG frameDuration = 10'000'000 / 30;
constexpr UINT32 maxLowLatencyBacklog = 2;

enum class LatencyMode { Default, DecoderOnly, EncoderAndDecoder };

struct Fixture {
    UINT32 width;
    UINT32 height;
    UINT32 frames;
    UINT32 bitrate;
    bool striped;

    UINT32 lumaBytes() const { return width * height; }
    UINT32 nv12Bytes() const { return lumaBytes() * 3 / 2; }
};

constexpr Fixture smallFixture{128, 96, 24, 200'000, false};
constexpr Fixture hdFixture{1920, 1080, 90, 4'000'000, true};
using CapturedSamples = std::array<std::vector<BYTE>, 2>;
using CaptureInputs = std::span<std::vector<BYTE> const>;

std::span<BYTE const> capturedSample(CaptureInputs samples, UINT32 index) {
    if (!samples.empty()) {
        return samples[index % samples.size()];
    }
    return {};
}

struct Timing {
    explicit Timing(UINT32 frames, bool exportPixels)
        : exported(exportPixels ? std::make_unique<WorkerFramePixels[]>(WorkerFrameCount) : nullptr) {
        submitted.reserve(frames);
        decodedMs.reserve(frames);
    }

    std::vector<Clock::time_point> submitted;
    std::vector<double> decodedMs;
    std::unique_ptr<WorkerFramePixels[]> exported;

    void report() {
        std::sort(decodedMs.begin(), decodedMs.end());
        auto percentile = [this](std::size_t percent) {
            return decodedMs[(decodedMs.size() * percent + 99) / 100 - 1];
        };
        std::printf("H.264 submit-to-decode in ms: p50=%.3f p95=%.3f p99=%.3f max=%.3f.\n",
                    percentile(50), percentile(95), percentile(99), decodedMs.back());
    }
};

int fixtureLuma(UINT32 index) {
    return 24 + 8 * static_cast<int>(index % smallFixture.frames);
}

void stage(char const* name) noexcept {
#ifdef LINKO_RW04_WGC_CODEC_CI
    markWgcCodecStage(name);
#else
    static_cast<void>(name);
#endif
}

void check(HRESULT result, char const* operation) {
    if (FAILED(result)) {
        std::fprintf(stderr, "%s failed: HRESULT 0x%08X\n", operation, static_cast<unsigned>(result));
        throw result;
    }
}

ComPtr<IMFMediaType> videoType(GUID const& codec, Fixture const& fixture) {
    ComPtr<IMFMediaType> type;
    check(MFCreateMediaType(&type), "create video type");
    check(type->SetGUID(MF_MT_MAJOR_TYPE, MFMediaType_Video), "set video major type");
    check(type->SetGUID(MF_MT_SUBTYPE, codec), "set video subtype");
    check(MFSetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, fixture.width, fixture.height), "set frame size");
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

Clock::time_point submitFrame(IMFTransform* encoder, Fixture const& fixture, MFT_INPUT_STREAM_INFO const& stream,
                              UINT32 index, std::span<BYTE const> captured) {
    ComPtr<IMFMediaBuffer> buffer;
    const auto capacity = std::max<DWORD>(fixture.nv12Bytes(), stream.cbSize);
    auto sample = sampleWithBuffer(capacity, std::max<DWORD>(stream.cbAlignment, MF_16_BYTE_ALIGNMENT), buffer);
    BYTE* pixels{};
    check(buffer->Lock(&pixels, nullptr, nullptr), "lock NV12 buffer");
    if (!captured.empty()) {
        std::memcpy(pixels, captured.data(), captured.size());
    } else {
        std::memset(pixels, fixtureLuma(index), fixture.lumaBytes());
        if (fixture.striped) {
            for (UINT32 row = 0; row < fixture.height; ++row) {
                std::memset(pixels + row * fixture.width + fixture.width * 3 / 4,
                            232 - fixtureLuma(index), fixture.width / 4);
            }
        }
        std::memset(pixels + fixture.lumaBytes(), 128, fixture.nv12Bytes() - fixture.lumaBytes());
    }
    if (capacity > fixture.nv12Bytes()) {
        std::memset(pixels + fixture.nv12Bytes(), 0, capacity - fixture.nv12Bytes());
    }
    check(buffer->Unlock(), "unlock NV12 buffer");
    check(buffer->SetCurrentLength(fixture.nv12Bytes()), "commit NV12 buffer");
    check(sample->SetSampleTime(frameDuration * (index + 1)), "set sample timestamp");
    check(sample->SetSampleDuration(frameDuration), "set sample duration");
    const auto submitted = Clock::now();
    stage("encoder-process-input");
    check(encoder->ProcessInput(0, sample.Get(), 0), "encode NV12 sample");
    return submitted;
}

struct Output {
    HRESULT status{};
    ComPtr<IMFSample> sample;
};

Output readOutput(IMFTransform* transform, DWORD bufferSize) {
    MFT_OUTPUT_STREAM_INFO stream{};
    check(transform->GetOutputStreamInfo(0, &stream), "read output stream requirements");
    ComPtr<IMFMediaBuffer> buffer;
    ComPtr<IMFSample> callerSample;
    if (!(stream.dwFlags & MFT_OUTPUT_STREAM_PROVIDES_SAMPLES)) {
        callerSample = sampleWithBuffer(std::max(stream.cbSize, bufferSize), stream.cbAlignment, buffer);
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

MFVideoArea displayArea(IMFMediaType* layout, UINT32 width, UINT32 height) {
    MFVideoArea area{};
    UINT32 bytes{};
    auto status = layout->GetBlob(MF_MT_MINIMUM_DISPLAY_APERTURE, reinterpret_cast<UINT8*>(&area),
                                  sizeof(area), &bytes);
    if (status == MF_E_ATTRIBUTENOTFOUND) {
        status = layout->GetBlob(MF_MT_GEOMETRIC_APERTURE, reinterpret_cast<UINT8*>(&area),
                                 sizeof(area), &bytes);
    }
    if (status == MF_E_ATTRIBUTENOTFOUND) {
        area.Area = {static_cast<LONG>(width), static_cast<LONG>(height)};
        return area;
    }
    check(status, "read decoder display area");
    check(bytes == sizeof(area) ? S_OK : E_UNEXPECTED, "check decoder display area length");
    return area;
}

void verifyDecodedFrame(IMFTransform* decoder, IMFSample* sample, Fixture const& fixture, UINT32 index,
                        std::span<BYTE const> captured, WorkerFramePixels* exported) {
    check(sample && index < fixture.frames ? S_OK : E_UNEXPECTED, "check decoded sample count");
    DWORD length{};
    check(sample->GetTotalLength(&length), "read decoded NV12 length");
    ComPtr<IMFMediaType> layout;
    check(decoder->GetOutputCurrentType(0, &layout), "read decoded video layout");
    GUID subtype{};
    check(layout->GetGUID(MF_MT_SUBTYPE, &subtype), "read decoded video format");
    UINT32 decodedWidth{}, decodedHeight{};
    check(MFGetAttributeSize(layout.Get(), MF_MT_FRAME_SIZE, &decodedWidth, &decodedHeight),
          "read decoded frame size");
    const auto area = displayArea(layout.Get(), decodedWidth, decodedHeight);
    const bool layoutMatches = subtype == MFVideoFormat_NV12 && length >= fixture.nv12Bytes() &&
                               decodedWidth >= fixture.width && decodedHeight >= fixture.height &&
                               area.OffsetX.value == 0 && area.OffsetX.fract == 0 &&
                               area.OffsetY.value == 0 && area.OffsetY.fract == 0 &&
                               area.Area.cx == static_cast<LONG>(fixture.width) &&
                               area.Area.cy == static_cast<LONG>(fixture.height);
    if (!layoutMatches) {
        std::fprintf(stderr, "Decoded layout: %ux%u, display area %ldx%ld, %lu bytes.\n",
                     decodedWidth, decodedHeight, area.Area.cx, area.Area.cy, length);
        check(E_UNEXPECTED, "check decoded sample layout");
    }
    if (index == 0) {
        std::printf("Decoded NV12 storage %ux%u, visible %ldx%ld.\n",
                    decodedWidth, decodedHeight, area.Area.cx, area.Area.cy);
    }

    LONGLONG timestamp{};
    check(sample->GetSampleTime(&timestamp), "read decoded timestamp");
    ComPtr<IMFMediaBuffer> buffer;
    check(sample->ConvertToContiguousBuffer(&buffer), "read contiguous decoded buffer");
    BYTE* pixels{};
    DWORD currentLength{};
    check(buffer->Lock(&pixels, nullptr, &currentLength), "lock decoded buffer");
    const bool complete = currentLength >= fixture.nv12Bytes();
    const int brightness = complete ? pixels[fixture.width / 2] : -1;
    const int stripe = complete ? pixels[fixture.width * 7 / 8] : -1;
    const int expectedBrightness = captured.empty() ? fixtureLuma(index) : captured[fixture.width / 2];
    bool chromaMatches = true;
    if (!captured.empty()) {
        const auto stride = MFGetAttributeUINT32(layout.Get(), MF_MT_DEFAULT_STRIDE, decodedWidth);
        const auto decodedOffset = static_cast<UINT64>(stride) * decodedHeight + fixture.width / 2;
        const auto inputOffset = fixture.lumaBytes() + fixture.width / 2;
        chromaMatches = stride >= fixture.width && decodedOffset + 1 < currentLength &&
            std::abs(pixels[decodedOffset] - captured[inputOffset]) <= 8 &&
            std::abs(pixels[decodedOffset + 1] - captured[inputOffset + 1]) <= 8;
    }
    bool copied = true;
    if (exported) {
        const auto stride = MFGetAttributeUINT32(layout.Get(), MF_MT_DEFAULT_STRIDE, decodedWidth);
        const auto uvStart = static_cast<UINT64>(stride) * decodedHeight;
        const auto uvBytes = static_cast<UINT64>(stride) * (WorkerFrameHeight / 2 - 1) + WorkerFrameWidth;
        copied = fixture.width == WorkerFrameWidth && fixture.height == WorkerFrameHeight &&
                 uvStart <= currentLength && uvBytes <= currentLength - uvStart &&
                 copyWorkerNv12(exported, pixels, stride, pixels + uvStart, stride);
    }
    check(buffer->Unlock(), "unlock decoded buffer");
    check(copied ? S_OK : E_UNEXPECTED, "copy the bounded decoded NV12 frame");
    if (timestamp != frameDuration * (index + 1) ||
        std::abs(brightness - expectedBrightness) > 6 || !chromaMatches ||
        (fixture.striped && std::abs(stripe - (232 - fixtureLuma(index))) > 6)) {
        std::fprintf(stderr, "Decoded frame %u: timestamp %lld, luma %d, stripe %d\n",
                     index, timestamp, brightness, stripe);
        check(E_UNEXPECTED, "check decoded frame order and content");
    }
}

void readDecodedFrames(IMFTransform* decoder, Fixture const& fixture, Timing& timing,
                       CaptureInputs samples, WorkerFramePixels* displayOutput = nullptr) {
    for (;;) {
        stage("decoder-read-output");
        auto decoded = readOutput(decoder, fixture.nv12Bytes());
        if (decoded.status == MF_E_TRANSFORM_STREAM_CHANGE) {
            selectDecodedType(decoder);
            decoded = readOutput(decoder, fixture.nv12Bytes());
        }
        if (decoded.status == MF_E_TRANSFORM_NEED_MORE_INPUT) {
            return;
        }
        check(decoded.status, "read decoded frame");
        const auto received = Clock::now();
        const auto index = timing.decodedMs.size();
        check(index < timing.submitted.size() ? S_OK : E_UNEXPECTED, "check sample had been submitted");
        const auto frame = static_cast<UINT32>(index);
        auto* exported = displayOutput;
        if (!exported && timing.exported && frame < WorkerFrameCount) {
            exported = &timing.exported[frame];
        }
        verifyDecodedFrame(decoder, decoded.sample.Get(), fixture, frame, capturedSample(samples, frame), exported);
        timing.decodedMs.push_back(std::chrono::duration<double, std::milli>(received - timing.submitted[index]).count());
    }
}

void readEncodedFrames(IMFTransform* encoder, ComPtr<IMFTransform>& decoder, Fixture const& fixture,
                       UINT32& encodedCount, Timing& timing, bool lowLatency, CaptureInputs samples,
                       WorkerFramePixels* displayOutput = nullptr) {
    for (;;) {
        stage("encoder-read-output");
        auto encoded = readOutput(encoder, fixture.nv12Bytes());
        if (encoded.status == MF_E_TRANSFORM_NEED_MORE_INPUT) {
            return;
        }
        check(encoded.status, "read H.264 sample");
        check(encoded.sample && encodedCount < fixture.frames ? S_OK : E_UNEXPECTED,
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
        stage("decoder-process-input");
        check(decoder->ProcessInput(0, encoded.sample.Get(), 0), "submit H.264 sample");
        ++encodedCount;
        readDecodedFrames(decoder.Get(), fixture, timing, samples, displayOutput);
    }
}

ComPtr<IMFTransform> startEncoder(Fixture const& fixture, bool lowLatency, MFT_INPUT_STREAM_INFO& inputStream) {
    stage("creating-encoder");
    ComPtr<IMFTransform> encoder;
    check(CoCreateInstance(CLSID_CMSH264EncoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                           IID_PPV_ARGS(&encoder)), "activate system H.264 encoder");
    if (lowLatency) {
        ComPtr<ICodecAPI> settings;
        check(encoder.As(&settings), "read H.264 encoder settings");
        VARIANT enabled{};
        enabled.vt = VT_BOOL;
        enabled.boolVal = VARIANT_TRUE;
        stage("enabling-encoder-low-latency");
        check(settings->SetValue(&CODECAPI_AVLowLatencyMode, &enabled), "enable H.264 low-latency encoding");
        VARIANT threads{};
        threads.vt = VT_UI4;
        threads.ulVal = 1;
        stage("configuring-encoder-worker");
        check(settings->SetValue(&CODECAPI_AVEncNumWorkerThreads, &threads), "configure one encoder worker");
    }
    auto outputType = videoType(MFVideoFormat_H264, fixture);
    check(outputType->SetUINT32(MF_MT_AVG_BITRATE, fixture.bitrate), "set output bitrate");
    check(outputType->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base), "set H.264 profile");
    stage("configuring-encoder-output");
    check(encoder->SetOutputType(0, outputType.Get(), 0), "configure H.264 output");
    auto inputType = videoType(MFVideoFormat_NV12, fixture);
    stage("configuring-encoder-input");
    check(encoder->SetInputType(0, inputType.Get(), 0), "configure NV12 input");
    check(encoder->GetInputStreamInfo(0, &inputStream), "read encoder input buffer requirements");

    stage("starting-encoder-stream");
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "start media streaming");
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "start media stream");
    return encoder;
}

int probe(Fixture const& fixture, LatencyMode mode, char const* modeName,
          CaptureInputs samples = {}, WorkerControl const* worker = nullptr, bool exportPixels = false) {
    const bool lowLatencyEncoder = mode == LatencyMode::EncoderAndDecoder;
    const bool lowLatencyDecoder = mode != LatencyMode::Default;
    MFT_INPUT_STREAM_INFO inputStream{};
    auto encoder = startEncoder(fixture, lowLatencyEncoder, inputStream);
    ComPtr<IMFTransform> decoder;
    UINT32 encodedCount{};
    UINT32 maxBacklog{};
    Timing timing(fixture.frames, exportPixels);
    const auto started = Clock::now();
    for (UINT32 index = 0; index < fixture.frames; ++index) {
        stage("allocating-encoder-input");
        timing.submitted.push_back(submitFrame(encoder.Get(), fixture, inputStream, index, capturedSample(samples, index)));
        readEncodedFrames(encoder.Get(), decoder, fixture, encodedCount, timing, lowLatencyDecoder, samples);
        maxBacklog = std::max(maxBacklog, index + 1 - static_cast<UINT32>(timing.decodedMs.size()));
    }
    const UINT32 encodedBeforeDrain = encodedCount;
    const auto framesBeforeDrain = timing.decodedMs.size();
    stage("draining-encoder");
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "mark input complete");
    check(encoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain encoder");
    readEncodedFrames(encoder.Get(), decoder, fixture, encodedCount, timing, lowLatencyDecoder, samples);
    if (!worker) {
        stage("ending-encoder-stream");
        check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish media streaming");
    }
    if (!decoder) {
        return 2;
    }
    check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "mark decoder input complete");
    check(decoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain decoder");
    readDecodedFrames(decoder.Get(), fixture, timing, samples);
    const auto decodedCount = timing.decodedMs.size();
    const double elapsed = std::chrono::duration<double>(Clock::now() - started).count();
    if (!worker) {
        check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish decoder streaming");
    }
    std::printf("Media Foundation system software H.264 (%s, %ux%u): "
                "%u encoded and %zu decoded before drain, "
                "%u encoded and %zu decoded total, maximum backlog %u.\n",
                modeName, fixture.width, fixture.height, encodedBeforeDrain,
                framesBeforeDrain, encodedCount, decodedCount, maxBacklog);
    if (encodedCount != fixture.frames || decodedCount != fixture.frames || !framesBeforeDrain ||
        (lowLatencyEncoder && maxBacklog > maxLowLatencyBacklog)) {
        return 3;
    }
    std::printf("Verified NV12 dimensions, timestamps and brightness. Unpaced throughput %.1f frames/s.\n",
                static_cast<double>(decodedCount) / elapsed);
    timing.report();
    if (worker) {
        bool sent = true;
        for (unsigned char index = 0; timing.exported && index < WorkerFrameCount && sent; ++index) {
            sent = sendWorkerFrame(worker, &timing.exported[index], static_cast<unsigned char>(index + 2));
        }
        const bool stopped = sent && waitForWorkerStop(worker);
        check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish worker encoder");
        check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish worker decoder");
        if (!stopped) {
            return 1;
        }
    }
    return 0;
}

int withMediaFoundation(auto run) {
    try {
#ifdef LINKO_RW04_WGC_CODEC_CI
        markWgcCodecStage("initializing-com");
#endif
        check(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "initialize COM");
        struct ComScope {
            ~ComScope() {
#ifdef LINKO_RW04_WGC_CODEC_CI
                markWgcCodecStage("uninitializing-com");
#endif
                CoUninitialize();
            }
        } com;
#ifdef LINKO_RW04_WGC_CODEC_CI
        markWgcCodecStage("starting-media-foundation");
#endif
        check(MFStartup(MF_VERSION), "start Media Foundation");
        struct MediaScope {
            ~MediaScope() {
#ifdef LINKO_RW04_WGC_CODEC_CI
                markWgcCodecStage("stopping-media-foundation");
#endif
                MFShutdown();
            }
        } media;
        return run();
    } catch (HRESULT) {
        return 1;
    }
}


int withWorkerControl(char const* backend, auto run,
                      std::optional<WorkerMessage> completion = WorkerStopped) {
    const int eventsFd = _dup(_fileno(stdout));
    if (eventsFd < 0) {
        std::perror("duplicate worker event pipe");
        return 1;
    }
    std::unique_ptr<FILE, decltype(&std::fclose)> events(_fdopen(eventsFd, "wb"), &std::fclose);
    if (!events) {
        std::perror("open worker event pipe");
        _close(eventsFd);
        return 1;
    }
    if (_setmode(_fileno(stdin), _O_BINARY) < 0 || _setmode(eventsFd, _O_BINARY) < 0 ||
        _dup2(_fileno(stderr), _fileno(stdout)) < 0) {
        std::perror("configure worker control pipes");
        return 1;
    }
    std::setvbuf(stdout, nullptr, _IONBF, 0);
    WorkerControl control{events.get(), 0, backend, nullptr, nullptr};
    if (!readWorkerMessage(&control, WorkerStart, &control.generation)) {
        return 1;
    }
    const int result = run(&control);
    if (result != 0) {
        return result;
    }
    return !completion || sendWorkerMessage(&control, *completion) ? 0 : 1;
}

#ifdef LINKO_RW04_WGC_CODEC_CI
bool validWgcFixtureFrame(WgcFixtureFrame frame) {
    return frame.width >= smallFixture.width && frame.height >= smallFixture.height &&
           frame.width <= 4096 && frame.height <= 2160 &&
           static_cast<std::size_t>(frame.width) * 4 * frame.height == frame.bgra.size();
}

std::vector<BYTE> cropWgcFixture(WgcFixtureFrame frame) {
    auto const& crop = smallFixture;
    const auto rowBytes = static_cast<std::size_t>(frame.width) * 4;
    const unsigned left = (frame.width - crop.width) / 2;
    const unsigned top = (frame.height - crop.height) / 2;
    auto const* bgra = reinterpret_cast<BYTE const*>(frame.bgra.data());

    std::vector<BYTE> nv12(crop.nv12Bytes());
    workerBgraToNv12(bgra + rowBytes * top + 4 * left, rowBytes, nv12.data());
    return nv12;
}
#endif
}

#ifdef LINKO_RW04_WGC_CODEC_CI
bool hasWgcFixtureColor(WgcFixtureFrame frame, bool resized) {
    if (!validWgcFixtureFrame(frame)) {
        return false;
    }
    const auto rowBytes = static_cast<std::size_t>(frame.width) * 4;
    const auto centerOffset = rowBytes * (frame.height / 2) + (frame.width / 2) * 4;
    auto const* center = reinterpret_cast<BYTE const*>(frame.bgra.data()) + centerOffset;
    const int blue = resized ? 190 : 70;
    const int green = resized ? 70 : 30;
    const int red = resized ? 30 : 190;
    return std::abs(center[0] - blue) <= 8 && std::abs(center[1] - green) <= 8 &&
           std::abs(center[2] - red) <= 8;
}

int runWgcCodecWorker(int (*run)(WorkerControl const*)) {
    return withWorkerControl("WGC/Media Foundation", run);
}

int runWgcPresenter(int (*run)(WorkerControl const*)) {
    return withWorkerControl("WGC presenter", run, WorkerPresented);
}

int checkWgcCodec(WgcFixtureFrame first, WgcFixtureFrame resized, WorkerControl const* worker) {
    return withMediaFoundation([&] {
        markWgcCodecStage("checking-fixture-colors");
        check(hasWgcFixtureColor(first, false) && hasWgcFixtureColor(resized, true)
                  ? S_OK : E_UNEXPECTED, "check WGC fixture colors and dimensions");
        markWgcCodecStage("converting-bgra");
        const CapturedSamples samples{cropWgcFixture(first), cropWgcFixture(resized)};
        markWgcCodecStage("running-h264-roundtrip");
        const auto result = probe(smallFixture, LatencyMode::EncoderAndDecoder, "WGC fixture",
                                  samples, worker, worker != nullptr);
        if (result == 0) {
            std::puts("WGC fixture H.264 roundtrip kept both source colors, luma and chroma.");
        }
        return result;
    });
}

struct WgcLiveCodec {
    MFT_INPUT_STREAM_INFO inputStream{};
    ComPtr<IMFTransform> encoder = startEncoder(smallFixture, true, inputStream);
    ComPtr<IMFTransform> decoder;
    UINT32 encoded{};
    Timing timing{smallFixture.frames, false};
    std::vector<BYTE> pending;
};

int runWgcLiveCodecs(int (*run)(WorkerControl const*, bool), bool display, bool pauseForExpiry) {
    return withMediaFoundation([&] {
        if (!display) {
            return run(nullptr, false);
        }
        if (std::setvbuf(stdin, nullptr, _IONBF, 0) != 0) {
            std::fputs("WGC live display: cannot configure the receipt pipe.\n", stderr);
            return 1;
        }
        return withWorkerControl(
            "WGC live display", [&](WorkerControl const* worker) { return run(worker, pauseForExpiry); }, std::nullopt);
    });
}

WgcLiveCodec* startWgcLiveCodec() {
    return new WgcLiveCodec;
}

void submitWgcLiveFrame(WgcLiveCodec& codec, WgcFixtureFrame frame) {
    const auto index = codec.timing.submitted.size();
    check(codec.pending.empty() && index < smallFixture.frames && validWgcFixtureFrame(frame)
              ? S_OK : E_UNEXPECTED, "bound the live WGC frame and in-flight sample");
    codec.pending = cropWgcFixture(frame);
    codec.timing.submitted.push_back(submitFrame(codec.encoder.Get(), smallFixture, codec.inputStream,
                                                 static_cast<UINT32>(index), codec.pending));
}

void verifyWgcLiveFrame(WgcLiveCodec& codec, char const* source, WorkerFramePixels* displayOutput) {
    check(codec.pending.empty() ? E_UNEXPECTED : S_OK, "require a submitted live WGC frame");
    readEncodedFrames(codec.encoder.Get(), codec.decoder, smallFixture, codec.encoded, codec.timing,
                      true, CaptureInputs(&codec.pending, 1), displayOutput);
    const auto expected = codec.timing.submitted.size();
    if (codec.encoded != expected || codec.timing.decodedMs.size() != expected) {
        std::fprintf(stderr,
                     "WGC live codec stall: source=%s submitted=%zu encoded=%u decoded=%zu.\n",
                     source, expected, codec.encoded, codec.timing.decodedMs.size());
    }
    check(codec.encoded == expected && codec.timing.decodedMs.size() == expected
              ? S_OK : E_UNEXPECTED, "encode and decode each live WGC frame before drain");
    codec.pending.clear();
}

void finishWgcLiveCodec(WgcLiveCodec& codec) {
    check(codec.pending.empty() && codec.decoder.Get() ? S_OK : E_UNEXPECTED, "finish a verified WGC stream");
    check(codec.encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "mark live encoder input complete");
    check(codec.encoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain live encoder");
    readEncodedFrames(codec.encoder.Get(), codec.decoder, smallFixture, codec.encoded, codec.timing, true, {});
    check(codec.decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "mark live decoder input complete");
    check(codec.decoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain live decoder");
    readDecodedFrames(codec.decoder.Get(), smallFixture, codec.timing, {});
    check(codec.encoded == codec.timing.submitted.size() ? S_OK : E_UNEXPECTED, "check live encoder has no late frames");
    check(codec.encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish live encoder streaming");
    check(codec.decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "finish live decoder streaming");
}

void deleteWgcLiveCodec(WgcLiveCodec* codec) noexcept {
    delete codec;
}
#else
namespace {
int runWorker(BYTE luma, bool exportPixels) {
    return withWorkerControl("Media Foundation", [&](WorkerControl const* control) {
        return withMediaFoundation([&] {
            if (exportPixels) {
                constexpr Fixture striped{WorkerFrameWidth, WorkerFrameHeight, 24, 200'000, true};
                return probe(striped, LatencyMode::EncoderAndDecoder, "worker pixels", {}, control, true);
            }
            std::vector<BYTE> pixels(smallFixture.nv12Bytes(), BYTE{128});
            std::fill_n(pixels.begin(), smallFixture.lumaBytes(), luma);
            const CapturedSamples samples{pixels, pixels};
            return probe(smallFixture, LatencyMode::EncoderAndDecoder, "worker fixture", samples, control);
        });
    });
}
}

int main(int argc, char** argv) {
    if (argc == 3 && std::strcmp(argv[1], "--worker-fixture") == 0) {
        char* end{};
        const long luma = std::strtol(argv[2], &end, 10);
        if (end == argv[2] || *end || luma < 16 || luma > 220 || luma % 2) {
            return 2;
        }
        return runWorker(static_cast<BYTE>(luma), false);
    }
    if (argc == 2 && std::strcmp(argv[1], "--worker-pixels") == 0) {
        return runWorker(0, true);
    }
    if (argc != 1) {
        return 2;
    }
    return withMediaFoundation([] {
        const int baseline = probe(smallFixture, LatencyMode::Default, "default");
        if (baseline != 0) {
            return baseline;
        }
        const int decoderOnly = probe(smallFixture, LatencyMode::DecoderOnly, "low-latency decoder");
        if (decoderOnly != 0) {
            return decoderOnly;
        }
        const int smallLowLatency = probe(smallFixture, LatencyMode::EncoderAndDecoder,
                                          "low-latency encoder and decoder");
        return smallLowLatency == 0
            ? probe(hdFixture, LatencyMode::EncoderAndDecoder, "low-latency encoder and decoder")
            : smallLowLatency;
    });
}
#endif
