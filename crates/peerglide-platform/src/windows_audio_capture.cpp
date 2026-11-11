// Thin Windows SDK adapter for the default render device's WASAPI loopback.
#define NOMINMAX
#include <windows.h>
#include <audioclient.h>
#include <mmdeviceapi.h>
#include <mmreg.h>
#include <wrl/client.h>

#include <algorithm>
#include <array>
#include <cmath>
#include <cstdint>
#include <cstring>
#include <memory>
#include <new>

using Microsoft::WRL::ComPtr;

namespace {
constexpr uint32_t sample_rate = 48'000;
constexpr uint32_t channels = 2;
constexpr uint32_t max_frames = 24'000;
constexpr GUID pcm_guid = {1, 0, 0x10, {0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71}};
constexpr GUID float_guid = {3, 0, 0x10, {0x80, 0, 0, 0xaa, 0, 0x38, 0x9b, 0x71}};

enum class Format { Pcm16, Float32 };

struct Capture {
    bool apartment = false;
    bool started = false;
    Format format = Format::Pcm16;
    ComPtr<IAudioClient> client;
    ComPtr<IAudioCaptureClient> stream;
    std::array<int16_t, max_frames * channels> packet{};
    uint32_t count = 0;
    uint32_t consumed = 0;

    HRESULT stop() {
        if (!started) {
            return S_OK;
        }
        const HRESULT result = client->Stop();
        if (SUCCEEDED(result)) {
            started = false;
        }
        return result;
    }

    ~Capture() {
        stop();
        stream.Reset();
        client.Reset();
        if (apartment) {
            CoUninitialize();
        }
    }
};

HRESULT input_format(const WAVEFORMATEX* input, Format* output) {
    if (input->nChannels != channels || input->nSamplesPerSec != sample_rate) {
        return AUDCLNT_E_UNSUPPORTED_FORMAT;
    }
    auto tag = input->wFormatTag;
    if (tag == WAVE_FORMAT_EXTENSIBLE) {
        if (input->cbSize < sizeof(WAVEFORMATEXTENSIBLE) - sizeof(WAVEFORMATEX)) {
            return AUDCLNT_E_UNSUPPORTED_FORMAT;
        }
        const auto* extended = reinterpret_cast<const WAVEFORMATEXTENSIBLE*>(input);
        if (extended->Samples.wValidBitsPerSample != input->wBitsPerSample) {
            return AUDCLNT_E_UNSUPPORTED_FORMAT;
        }
        if (IsEqualGUID(extended->SubFormat, pcm_guid)) {
            tag = WAVE_FORMAT_PCM;
        } else if (IsEqualGUID(extended->SubFormat, float_guid)) {
            tag = WAVE_FORMAT_IEEE_FLOAT;
        }
    }
    if (tag == WAVE_FORMAT_PCM && input->wBitsPerSample == 16 && input->nBlockAlign == 4) {
        *output = Format::Pcm16;
        return S_OK;
    }
    if (tag == WAVE_FORMAT_IEEE_FLOAT && input->wBitsPerSample == 32 && input->nBlockAlign == 8) {
        *output = Format::Float32;
        return S_OK;
    }
    return AUDCLNT_E_UNSUPPORTED_FORMAT;
}

int16_t from_float(float sample) {
    if (!std::isfinite(sample)) {
        return 0;
    }
    if (sample <= -1.0f) {
        return INT16_MIN;
    }
    const auto clamped = std::min(sample, 1.0f);
    return static_cast<int16_t>(std::round(clamped * INT16_MAX));
}

HRESULT read_packet(Capture* capture) {
    UINT32 available = 0;
    HRESULT result = capture->stream->GetNextPacketSize(&available);
    if (FAILED(result) || available == 0) {
        return result;
    }

    BYTE* bytes = nullptr;
    UINT32 frames = 0;
    DWORD flags = 0;
    result = capture->stream->GetBuffer(&bytes, &frames, &flags, nullptr, nullptr);
    if (FAILED(result) || frames == 0) {
        return result;
    }
    if (frames > max_frames || (bytes == nullptr && !(flags & AUDCLNT_BUFFERFLAGS_SILENT))) {
        capture->stream->ReleaseBuffer(0);
        return E_UNEXPECTED;
    }

    const auto samples = frames * channels;
    if (flags & (AUDCLNT_BUFFERFLAGS_SILENT | AUDCLNT_BUFFERFLAGS_DATA_DISCONTINUITY)) {
        std::fill_n(capture->packet.begin(), samples, 0);
    } else if (capture->format == Format::Pcm16) {
        std::memcpy(capture->packet.data(), bytes, samples * sizeof(int16_t));
    } else {
        for (uint32_t i = 0; i < samples; ++i) {
            float sample;
            std::memcpy(&sample, bytes + i * sizeof(sample), sizeof(sample));
            capture->packet[i] = from_float(sample);
        }
    }

    result = capture->stream->ReleaseBuffer(frames);
    if (SUCCEEDED(result)) {
        capture->count = frames;
        capture->consumed = 0;
    }
    return result;
}
} // namespace

extern "C" int32_t linko_wasapi_open(void** output) {
    if (!output) {
        return E_POINTER;
    }
    *output = nullptr;
    std::unique_ptr<Capture> capture(new (std::nothrow) Capture());
    if (!capture) {
        return E_OUTOFMEMORY;
    }
    HRESULT result = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    if (FAILED(result)) {
        return result;
    }
    capture->apartment = true;
    ComPtr<IMMDeviceEnumerator> devices;
    result = CoCreateInstance(__uuidof(MMDeviceEnumerator), nullptr, CLSCTX_INPROC_SERVER,
                              IID_PPV_ARGS(devices.ReleaseAndGetAddressOf()));
    if (FAILED(result)) {
        return result;
    }
    ComPtr<IMMDevice> render;
    result = devices->GetDefaultAudioEndpoint(eRender, eConsole, render.ReleaseAndGetAddressOf());
    if (FAILED(result)) {
        return result;
    }
    result = render->Activate(__uuidof(IAudioClient), CLSCTX_INPROC_SERVER, nullptr,
                              reinterpret_cast<void**>(capture->client.ReleaseAndGetAddressOf()));
    if (FAILED(result)) {
        return result;
    }
    WAVEFORMATEX* raw_format = nullptr;
    result = capture->client->GetMixFormat(&raw_format);
    std::unique_ptr<WAVEFORMATEX, decltype(&CoTaskMemFree)> format(raw_format, CoTaskMemFree);
    if (FAILED(result) || !format) {
        return FAILED(result) ? result : E_UNEXPECTED;
    }
    result = input_format(format.get(), &capture->format);
    if (FAILED(result)) {
        return result;
    }
    constexpr REFERENCE_TIME buffer_duration = 2'000'000; // 200 ms.
    result = capture->client->Initialize(AUDCLNT_SHAREMODE_SHARED, AUDCLNT_STREAMFLAGS_LOOPBACK,
                                         buffer_duration, 0, format.get(), nullptr);
    if (FAILED(result)) {
        return result;
    }
    UINT32 frames = 0;
    result = capture->client->GetBufferSize(&frames);
    if (FAILED(result) || frames == 0 || frames > max_frames) {
        return FAILED(result) ? result : E_UNEXPECTED;
    }
    result = capture->client->GetService(IID_PPV_ARGS(capture->stream.ReleaseAndGetAddressOf()));
    if (FAILED(result)) {
        return result;
    }
    result = capture->client->Start();
    if (FAILED(result)) {
        return result;
    }
    capture->started = true;
    *output = capture.release();
    return S_OK;
}

extern "C" int32_t linko_wasapi_read(void* owner, int16_t* output, uint32_t capacity,
                                    uint32_t* written) {
    if (!owner || !output || !written || capacity == 0) {
        return E_POINTER;
    }
    auto* capture = static_cast<Capture*>(owner);
    *written = 0;
    if (!capture->started) {
        return E_UNEXPECTED;
    }
    if (capture->consumed == capture->count) {
        const HRESULT result = read_packet(capture);
        if (FAILED(result) || capture->consumed == capture->count) {
            return result;
        }
    }

    const uint32_t count = std::min(capacity, capture->count - capture->consumed);
    std::copy_n(capture->packet.data() + capture->consumed * channels,
                count * channels, output);
    capture->consumed += count;
    *written = count;
    return S_OK;
}

extern "C" int32_t linko_wasapi_stop(void* owner) {
    return owner ? static_cast<Capture*>(owner)->stop() : E_POINTER;
}

extern "C" void linko_wasapi_drop(void* owner) {
    delete static_cast<Capture*>(owner);
}
