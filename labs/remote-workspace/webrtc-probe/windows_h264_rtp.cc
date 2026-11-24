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
#include <span>
#include <vector>

#include "common_video/h264/h264_common.h"
#include "modules/rtp_rtcp/source/rtp_format_h264.h"
#include "modules/rtp_rtcp/source/rtp_packet_received.h"
#include "modules/rtp_rtcp/source/video_rtp_depacketizer_h264.h"
#include "modules/video_coding/h264_sps_pps_tracker.h"

using Microsoft::WRL::ComPtr;
namespace {
constexpr UINT32 width = 128;
constexpr UINT32 height = 96;
constexpr UINT32 frameCount = 12;
constexpr LONGLONG frameDuration = 10'000'000 / 30;
struct Fixture {
    UINT32 width;
    UINT32 height;
    UINT32 bitrate;
};
constexpr Fixture fixture{width, height, 500'000};
void require(bool condition, const char* operation) {
    if (!condition) {
        std::fprintf(stderr, "H.264 RTP check failed: %s\n", operation);
        std::abort();
    }
}
void check(HRESULT result, char const* operation) {
    if (FAILED(result)) {
        std::fprintf(stderr, "%s failed: HRESULT 0x%08X\n", operation, static_cast<unsigned>(result));
        std::abort();
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

ComPtr<IMFTransform> startEncoder(Fixture const& fixture, bool lowLatency, MFT_INPUT_STREAM_INFO& inputStream) {
    ComPtr<IMFTransform> encoder;
    check(CoCreateInstance(CLSID_CMSH264EncoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                           IID_PPV_ARGS(&encoder)), "activate system H.264 encoder");
    if (lowLatency) {
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
    auto outputType = videoType(MFVideoFormat_H264, fixture);
    check(outputType->SetUINT32(MF_MT_AVG_BITRATE, fixture.bitrate), "set output bitrate");
    check(outputType->SetUINT32(MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Base), "set H.264 profile");
    check(encoder->SetOutputType(0, outputType.Get(), 0), "configure H.264 output");
    auto inputType = videoType(MFVideoFormat_NV12, fixture);
    check(encoder->SetInputType(0, inputType.Get(), 0), "configure NV12 input");
    check(encoder->GetInputStreamInfo(0, &inputStream), "read encoder input buffer requirements");

    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "start media streaming");
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "start media stream");
    return encoder;
}

std::vector<BYTE> bytesOf(IMFSample* sample) {
    ComPtr<IMFMediaBuffer> buffer;
    check(sample->ConvertToContiguousBuffer(&buffer), "get contiguous sample");
    BYTE* data{};
    DWORD length{};
    check(buffer->Lock(&data, nullptr, &length), "lock sample");
    require(length > 0 && length <= 128 * 1024, "bounded sample");
    std::vector<BYTE> bytes(data, data + length);
    check(buffer->Unlock(), "unlock sample");
    return bytes;
}

int luma(UINT32 index) { return 40 + 12 * index; }

ComPtr<IMFSample> inputFrame(UINT32 index, MFT_INPUT_STREAM_INFO stream) {
    ComPtr<IMFMediaBuffer> buffer;
    auto sample = sampleWithBuffer(std::max<DWORD>(width * height * 3 / 2, stream.cbSize),
                                  std::max<DWORD>(stream.cbAlignment, MF_16_BYTE_ALIGNMENT), buffer);
    BYTE* pixels{};
    check(buffer->Lock(&pixels, nullptr, nullptr), "lock input");
    std::memset(pixels, luma(index), width * height);
    for (UINT32 row = 0; row < height; ++row) {
        std::memset(pixels + row * width + width * 3 / 4, 232 - luma(index), width / 4);
    }
    std::memset(pixels + width * height, 128, width * height / 2);
    check(buffer->Unlock(), "unlock input");
    check(buffer->SetCurrentLength(width * height * 3 / 2), "commit input");
    check(sample->SetSampleTime(frameDuration * (index + 1)), "input timestamp");
    check(sample->SetSampleDuration(frameDuration), "input duration");
    return sample;
}

bool hasNalu(std::span<const BYTE> data, webrtc::H264::NaluType type) {
    const auto nalus = webrtc::H264::FindNaluIndices(data);
    return std::any_of(nalus.begin(), nalus.end(), [&](const auto& nalu) {
        return nalu.payload_size && webrtc::H264::ParseNaluType(data[nalu.payload_start_offset]) == type;
    });
}

struct Receiver {
    ComPtr<IMFTransform> decoder;
    webrtc::VideoRtpDepacketizerH264 depacketizer;
    webrtc::video_coding::H264SpsPpsTracker tracker;
    UINT32 decoded = 0;
    UINT32 packets = 0;
    UINT32 fragments = 0;
    uint16_t nextSequence = 65530;

    Receiver() {
        check(CoCreateInstance(CLSID_CMSH264DecoderMFT, nullptr, CLSCTX_INPROC_SERVER,
                               IID_PPV_ARGS(&decoder)), "activate independent decoder");
        auto type = videoType(MFVideoFormat_H264, fixture);
        // No sender media type or sequence header crosses this boundary.
        check(decoder->SetInputType(0, type.Get(), 0), "configure independent receiver");
        selectDecodedType(decoder.Get());
        check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0), "begin receiver");
        check(decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0), "start receiver");
    }

    static uint32_t timestamp(UINT32 index) { return UINT32_MAX - 4500u + index * 3000u; }

    void read() {
        for (;;) {
            auto output = readOutput(decoder.Get(), width * height * 3 / 2);
            if (output.status == MF_E_TRANSFORM_STREAM_CHANGE) {
                selectDecodedType(decoder.Get());
                continue;
            }
            if (output.status == MF_E_TRANSFORM_NEED_MORE_INPUT) {
                return;
            }
            check(output.status, "read independently decoded frame");
            require(decoded < frameCount, "bounded decoded frame count");
            LONGLONG time{};
            check(output.sample->GetSampleTime(&time), "decoded timestamp");
            require(time == frameDuration * (decoded + 1), "decoded frame timestamp");
            ComPtr<IMFMediaType> type;
            check(decoder->GetOutputCurrentType(0, &type), "decoded type");
            UINT32 actualWidth{}, actualHeight{};
            check(MFGetAttributeSize(type.Get(), MF_MT_FRAME_SIZE, &actualWidth, &actualHeight),
                  "decoded dimensions");
            require(actualWidth == width && actualHeight == height, "decoded dimensions match");
            const auto bytes = bytesOf(output.sample.Get());
            require(bytes.size() >= width * height * 3 / 2, "decoded NV12 size");
            require(std::abs(int(bytes[width / 2]) - luma(decoded)) <= 8 &&
                    std::abs(int(bytes[width * 7 / 8]) - (232 - luma(decoded))) <= 8,
                    "decoded changing brightness and stripe");
            ++decoded;
        }
    }

    void receive(const std::vector<webrtc::CopyOnWriteBuffer>& wire, UINT32 index) {
        require(!wire.empty(), "nonempty RTP frame");
        std::vector<BYTE> accessUnit;
        for (size_t i = 0; i < wire.size(); ++i) {
            webrtc::RtpPacketReceived packet;
            require(packet.Parse(wire[i]), "parse actual RTP bytes");
            require(packet.SequenceNumber() == nextSequence++, "RTP sequence including wrap");
            require(packet.Timestamp() == timestamp(index) && packet.PayloadType() == 96 &&
                    packet.Ssrc() == 1234 && packet.Marker() == (i + 1 == wire.size()),
                    "RTP frame identity and final marker");
            require(!packet.payload().empty() && packet.payload().size() <= 64, "bounded RTP payload");
            if ((packet.payload()[0] & 31) == webrtc::H264::kFuA) {
                ++fragments;
            }
            auto parsed = depacketizer.Parse(packet.PayloadBuffer());
            require(parsed.has_value(), "WebRTC H.264 depacketization");
            auto fixed = tracker.CopyAndFixBitstream(parsed->video_payload, &parsed->video_header);
            require(fixed.action == webrtc::video_coding::H264SpsPpsTracker::kInsert,
                    "WebRTC receiver reconstructs Annex B");
            accessUnit.insert(accessUnit.end(), fixed.bitstream.data(),
                              fixed.bitstream.data() + fixed.bitstream.size());
            ++packets;
        }
        require(accessUnit.size() <= 128 * 1024, "bounded received access unit");
        if (index == 0) {
            require(hasNalu(accessUnit, webrtc::H264::kIdr) &&
                    hasNalu(accessUnit, webrtc::H264::kSps) &&
                    hasNalu(accessUnit, webrtc::H264::kPps), "received SPS PPS and IDR");
        } else {
            require(hasNalu(accessUnit, webrtc::H264::kSlice), "received later non-IDR slice");
        }
        ComPtr<IMFMediaBuffer> buffer;
        auto sample = sampleWithBuffer(static_cast<DWORD>(accessUnit.size()), 0, buffer);
        BYTE* data{};
        check(buffer->Lock(&data, nullptr, nullptr), "lock received sample");
        std::memcpy(data, accessUnit.data(), accessUnit.size());
        check(buffer->Unlock(), "unlock received sample");
        check(buffer->SetCurrentLength(static_cast<DWORD>(accessUnit.size())), "commit received sample");
        // Reconstruct time from received RTP ticks, including 32-bit wrap.
        const uint32_t ticks = timestamp(index) - timestamp(0);
        check(sample->SetSampleTime(frameDuration * (ticks / 3000u + 1)), "received RTP time");
        check(sample->SetSampleDuration(frameDuration), "received duration");
        check(decoder->ProcessInput(0, sample.Get(), 0), "decode received access unit");
        read();
    }
};

void transmit(IMFTransform* encoder, IMFSample* sample, Receiver& receiver, UINT32 index) {
    auto encoded = bytesOf(sample);
    if (index == 0 && (!hasNalu(encoded, webrtc::H264::kSps) || !hasNalu(encoded, webrtc::H264::kPps))) {
        ComPtr<IMFMediaType> type;
        check(encoder->GetOutputCurrentType(0, &type), "read sender sequence header");
        UINT32 length{};
        check(type->GetBlobSize(MF_MT_MPEG_SEQUENCE_HEADER, &length), "sender sequence header size");
        require(length > 0 && length < 4096, "bounded sender sequence header");
        std::vector<BYTE> header(length);
        check(type->GetBlob(MF_MT_MPEG_SEQUENCE_HEADER, header.data(), length, nullptr),
              "get sender sequence header");
        header.insert(header.end(), encoded.begin(), encoded.end());
        encoded = std::move(header);
    }
    if (index == 0) {
        require(hasNalu(encoded, webrtc::H264::kSps) && hasNalu(encoded, webrtc::H264::kPps) &&
                hasNalu(encoded, webrtc::H264::kIdr), "send actual SPS PPS and IDR");
        for (const auto& nalu : webrtc::H264::FindNaluIndices(encoded)) {
            if (webrtc::H264::ParseNaluType(encoded[nalu.payload_start_offset]) == webrtc::H264::kSps) {
                require(nalu.payload_size >= 4 && encoded[nalu.payload_start_offset + 1] == 66,
                        "actual SPS reports baseline profile");
                std::printf("Sender SPS profile_idc=%u constraints=%u level_idc=%u.\n",
                            encoded[nalu.payload_start_offset + 1],
                            encoded[nalu.payload_start_offset + 2],
                            encoded[nalu.payload_start_offset + 3]);
            }
        }
    }
    webrtc::RtpPacketizer::PayloadSizeLimits limits;
    limits.max_payload_len = 64;
    webrtc::RtpPacketizerH264 packetizer(encoded, limits, webrtc::H264PacketizationMode::NonInterleaved);
    const auto count = packetizer.NumPackets();
    require(count > 0 && count < 4096, "bounded packet count");
    std::vector<webrtc::CopyOnWriteBuffer> wire;
    for (size_t i = 0; i < count; ++i) {
        webrtc::RtpPacketToSend packet(nullptr, 1500);
        packet.SetPayloadType(96);
        packet.SetSsrc(1234);
        packet.SetTimestamp(Receiver::timestamp(index));
        packet.SetSequenceNumber(static_cast<uint16_t>(receiver.nextSequence + i));
        require(packetizer.NextPacket(&packet), "WebRTC H.264 packetization");
        wire.push_back(packet.Buffer());
    }
    receiver.receive(wire, index);
}

int run() {
    Receiver receiver;
    MFT_INPUT_STREAM_INFO stream{};
    auto encoder = startEncoder(fixture, true, stream);
    UINT32 encoded = 0;
    auto read = [&] {
        for (;;) {
            auto output = readOutput(encoder.Get(), width * height * 3 / 2);
            if (output.status == MF_E_TRANSFORM_NEED_MORE_INPUT) {
                return;
            }
            check(output.status, "read encoded frame");
            require(encoded < frameCount, "bounded encoded count");
            transmit(encoder.Get(), output.sample.Get(), receiver, encoded++);
        }
    };
    for (UINT32 index = 0; index < frameCount; ++index) {
        auto sample = inputFrame(index, stream);
        check(encoder->ProcessInput(0, sample.Get(), 0), "encode changing NV12 frame");
        read();
    }
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "end encoder stream");
    check(encoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain encoder");
    read();
    check(receiver.decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_OF_STREAM, 0), "end receiver stream");
    check(receiver.decoder->ProcessMessage(MFT_MESSAGE_COMMAND_DRAIN, 0), "drain receiver");
    receiver.read();
    require(encoded == frameCount && receiver.decoded == frameCount && receiver.fragments > 1,
            "all key and later frames decoded with FU-A fragmentation");
    check(encoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "stop encoder");
    check(receiver.decoder->ProcessMessage(MFT_MESSAGE_NOTIFY_END_STREAMING, 0), "stop receiver");
    std::printf("Windows system H.264 RTP: %u encoded, %u decoded, %u packets, %u FU-A fragments.\n",
                encoded, receiver.decoded, receiver.packets, receiver.fragments);
    std::puts("Verified received SPS/PPS/IDR, later slices, dimensions, changing brightness, RTP wrap.");
    return 0;
}
}  // namespace

int main() {
    check(CoInitializeEx(nullptr, COINIT_MULTITHREADED), "initialize COM");
    check(MFStartup(MF_VERSION), "initialize Media Foundation");
    const int result = run();
    check(MFShutdown(), "shutdown Media Foundation");
    CoUninitialize();
    return result;
}

