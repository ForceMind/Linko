#import <Foundation/Foundation.h>

#include <atomic>
#include <chrono>
#include <cstdio>
#include <future>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <utility>
#include <vector>

#include "api/audio/audio_device.h"
#include "api/audio_codecs/builtin_audio_decoder_factory.h"
#include "api/audio_codecs/builtin_audio_encoder_factory.h"
#include "api/candidate.h"
#include "api/create_modular_peer_connection_factory.h"
#include "api/enable_media.h"
#include "api/environment/environment_factory.h"
#include "api/jsep.h"
#include "api/make_ref_counted.h"
#include "api/media_stream_interface.h"
#include "api/peer_connection_interface.h"
#include "api/rtp_receiver_interface.h"
#include "api/rtp_transceiver_interface.h"
#include "api/set_local_description_observer_interface.h"
#include "api/set_remote_description_observer_interface.h"
#include "api/video/i420_buffer.h"
#include "api/video/video_broadcaster.h"
#include "api/video/video_frame.h"
#include "api/video/video_sink_interface.h"
#include "linko_probe/loopback_preview.h"
#include "modules/audio_device/audio_device_impl.h"
#include "p2p/base/basic_packet_socket_factory.h"
#include "p2p/client/basic_port_allocator.h"
#include "pc/video_track_source.h"
#include "rtc_base/ip_address.h"
#include "rtc_base/network.h"
#include "rtc_base/socket_server.h"
#include "rtc_base/ssl_adapter.h"
#include "rtc_base/thread.h"
#include "sdk/objc/components/video_codec/RTCVideoDecoderFactoryH264.h"
#include "sdk/objc/components/video_codec/RTCVideoEncoderFactoryH264.h"
#include "sdk/objc/components/video_frame_buffer/RTCCVPixelBuffer.h"
#include "sdk/objc/native/api/video_decoder_factory.h"
#include "sdk/objc/native/api/video_encoder_factory.h"
#include "sdk/objc/native/api/video_frame_buffer.h"

namespace {
using namespace std::chrono_literals;
using Clock = std::chrono::steady_clock;
using Peer = webrtc::PeerConnectionInterface;

constexpr int kWidth = 128;
constexpr int kHeight = 96;

class LoopbackNetwork final : public webrtc::NetworkManagerBase {
 public:
    LoopbackNetwork()
        : network_("lo0", "Linko loopback fixture", webrtc::IPAddress(0x7f000000u), 8,
                   webrtc::ADAPTER_TYPE_LOOPBACK) {
        const webrtc::IPAddress local(0x7f000001u);
        network_.AddIP(local);
        network_.set_default_local_address_provider(this);
        set_default_local_addresses(local, webrtc::IPAddress());
    }

    void StartUpdating() override {
        webrtc::Thread::Current()->PostTask([this] { NotifyNetworksChanged(); });
    }

    void StopUpdating() override {}
    std::vector<const webrtc::Network*> GetNetworks() const override { return {&network_}; }
    std::vector<const webrtc::Network*> GetAnyAddressNetworks() override { return {}; }

 private:
    webrtc::Network network_;
};

class FixtureTrack : public webrtc::VideoTrackSource {
 public:
    FixtureTrack() : VideoTrackSource(false) { SetState(kLive); }

    void Send(uint8_t luma) {
        const std::vector<uint8_t> y(kWidth * kHeight, luma);
        const std::vector<uint8_t> uv(kWidth * kHeight / 4, 128);
        auto buffer = webrtc::I420Buffer::Copy(kWidth, kHeight, y.data(), kWidth,
                                               uv.data(), kWidth / 2, uv.data(), kWidth / 2);
        SendBuffer(buffer);
    }

    void SendBuffer(webrtc::scoped_refptr<webrtc::VideoFrameBuffer> buffer) {
        const auto now = std::chrono::duration_cast<std::chrono::microseconds>(
            Clock::now().time_since_epoch()).count();
        const auto frame = webrtc::VideoFrame::Builder()
                               .set_video_frame_buffer(buffer)
                               .set_timestamp_us(now)
                               .build();
        broadcast_.OnFrame(frame);
    }

 protected:
    webrtc::VideoSourceInterface<webrtc::VideoFrame>* source() override { return &broadcast_; }

 private:
    webrtc::VideoBroadcaster broadcast_;
};

class FrameSink final : public webrtc::VideoSinkInterface<webrtc::VideoFrame> {
 public:
    FrameSink(uint8_t expected, bool keep_pixels)
        : expected_(expected), keep_pixels_(keep_pixels) {}

    void OnFrame(const webrtc::VideoFrame& frame) override {
        if (keep_pixels_) {
            if (frame.width() < 64 || frame.height() < 64 ||
                frame.width() > 1920 || frame.height() > 1080) {
                return;
            }
            std::lock_guard lock(mutex_);
            latest_ = frame.video_frame_buffer();
            arrived_ = Clock::now();
            return;
        }
        auto pixels = frame.video_frame_buffer()->ToI420ForInspection();
        if (!pixels || !pixels->DataY() || frame.width() != kWidth || frame.height() != kHeight) {
            return;
        }
        const int difference = static_cast<int>(*pixels->DataY()) - expected_;
        if (difference >= -16 && difference <= 16) {
            matches_.fetch_add(1);
        }
    }

    unsigned matches() const { return matches_.load(); }

    webrtc::scoped_refptr<webrtc::VideoFrameBuffer> Take() {
        std::lock_guard lock(mutex_);
        if (latest_ && Clock::now() - arrived_ >= 2s) {
            latest_ = nullptr;
        }
        return std::exchange(latest_, nullptr);
    }

 private:
    uint8_t expected_;
    bool keep_pixels_;
    std::atomic<unsigned> matches_{0};
    std::mutex mutex_;
    webrtc::scoped_refptr<webrtc::VideoFrameBuffer> latest_;
    Clock::time_point arrived_;
};

class Observer final : public webrtc::PeerConnectionObserver {
 public:
    Observer(uint8_t expected, bool keep_pixels) : sink(expected, keep_pixels) {}
    void OnSignalingChange(Peer::SignalingState) override {}
    void OnDataChannel(webrtc::scoped_refptr<webrtc::DataChannelInterface>) override {
        invalid_.store(true);
    }
    void OnIceGatheringChange(Peer::IceGatheringState state) override {
        if (state == Peer::kIceGatheringComplete) {
            gathered.store(true);
        }
    }
    void OnIceCandidate(const webrtc::IceCandidate* candidate) override {
        const auto& details = candidate->candidate();
        if (!details.address().IsLoopbackIP() || !details.is_local() || details.protocol() != "udp") {
            invalid_.store(true);
        }
        candidates.fetch_add(1);
    }
    void OnTrack(webrtc::scoped_refptr<webrtc::RtpTransceiverInterface> transceiver) override {
        auto track = transceiver->receiver()->track();
        if (!track || track->kind() != webrtc::MediaStreamTrackInterface::kVideoKind || track_) {
            invalid_.store(true);
            return;
        }
        track_ = webrtc::scoped_refptr<webrtc::VideoTrackInterface>(
            static_cast<webrtc::VideoTrackInterface*>(track.get()));
        track_->AddOrUpdateSink(&sink, webrtc::VideoSinkWants());
    }

    void Detach() {
        if (track_) {
            track_->RemoveSink(&sink);
            track_ = nullptr;
        }
    }

    bool invalid() const { return invalid_.load(); }

    FrameSink sink;
    std::atomic<bool> gathered{false};
    std::atomic<unsigned> candidates{0};

 private:
    std::atomic<bool> invalid_{false};
    webrtc::scoped_refptr<webrtc::VideoTrackInterface> track_;
};

class NewDescription : public webrtc::CreateSessionDescriptionObserver {
 public:
    void OnSuccess(webrtc::SessionDescriptionInterface* description) override {
        result.set_value(std::unique_ptr<webrtc::SessionDescriptionInterface>(description));
    }
    void OnFailure(webrtc::RTCError) override { result.set_value(nullptr); }
    std::promise<std::unique_ptr<webrtc::SessionDescriptionInterface>> result;
};

class LocalDescription : public webrtc::SetLocalDescriptionObserverInterface {
 public:
    void OnSetLocalDescriptionComplete(webrtc::RTCError error) override { done.set_value(error.ok()); }
    std::promise<bool> done;
};

class RemoteDescription : public webrtc::SetRemoteDescriptionObserverInterface {
 public:
    void OnSetRemoteDescriptionComplete(webrtc::RTCError error) override { done.set_value(error.ok()); }
    std::promise<bool> done;
};

template <typename Predicate>
bool Until(Predicate predicate, Clock::duration limit) {
    const auto deadline = Clock::now() + limit;
    do {
        if (predicate()) {
            return true;
        }
        std::this_thread::sleep_for(25ms);
    } while (Clock::now() < deadline);
    return false;
}

class Endpoint {
 public:
    Endpoint(webrtc::Thread& signaling, uint8_t color, bool keep_pixels = false)
        : observer(color, keep_pixels), signaling_(signaling) {}
    ~Endpoint() { Close(); }

    bool Open(webrtc::PeerConnectionFactoryInterface& factory,
              const webrtc::Environment& environment,
              LoopbackNetwork& network, webrtc::BasicPacketSocketFactory& sockets) {
        auto allocator = std::make_unique<webrtc::BasicPortAllocator>(environment, &network, &sockets);
        allocator->set_flags(webrtc::PORTALLOCATOR_DISABLE_STUN | webrtc::PORTALLOCATOR_DISABLE_RELAY |
                             webrtc::PORTALLOCATOR_DISABLE_TCP);
        webrtc::PeerConnectionDependencies dependencies(&observer);
        dependencies.allocator = std::move(allocator);
        Peer::RTCConfiguration config;
        auto result = factory.CreatePeerConnectionOrError(config, std::move(dependencies));
        if (!result.ok()) {
            return false;
        }
        peer = result.MoveValue();
        return true;
    }

    bool Close() {
        if (!peer) {
            return true;
        }
        signaling_.BlockingCall([&] { observer.Detach(); });
        peer->Close();
        bool closed = peer->signaling_state() == Peer::kClosed;
        peer = nullptr;
        return closed;
    }

    Observer observer;
    webrtc::scoped_refptr<Peer> peer;

 private:
    webrtc::Thread& signaling_;
};

bool Describe(webrtc::Thread& signaling, Endpoint& from, Endpoint& to,
              webrtc::SdpType type, std::string& sdp) {
    auto pending = webrtc::make_ref_counted<NewDescription>();
    auto created = pending->result.get_future();
    if (type == webrtc::SdpType::kOffer) {
        from.peer->CreateOffer(pending.get(), {});
    } else {
        from.peer->CreateAnswer(pending.get(), {});
    }
    if (created.wait_for(8s) != std::future_status::ready) {
        return false;
    }
    auto description = created.get();
    if (!description) {
        return false;
    }
    auto local = webrtc::make_ref_counted<LocalDescription>();
    auto applied = local->done.get_future();
    from.peer->SetLocalDescription(std::move(description), local);
    if (applied.wait_for(8s) != std::future_status::ready || !applied.get() ||
        !Until([&] { return from.observer.gathered.load(); }, 8s) ||
        from.observer.invalid() || from.observer.candidates.load() == 0) {
        return false;
    }
    const bool serialized = signaling.BlockingCall([&] {
        const auto* current = from.peer->local_description();
        return current && current->ToString(&sdp);
    });
    const char* direction = type == webrtc::SdpType::kOffer ? "a=sendonly" : "a=recvonly";
    if (!serialized || sdp.find(" H264/90000") == std::string::npos ||
        sdp.find("a=fingerprint:sha-256 ") == std::string::npos ||
        sdp.find(direction) == std::string::npos) {
        return false;
    }
    auto remote_description = webrtc::CreateSessionDescription(type, sdp);
    if (!remote_description) {
        return false;
    }
    auto remote = webrtc::make_ref_counted<RemoteDescription>();
    auto accepted = remote->done.get_future();
    to.peer->SetRemoteDescription(std::move(remote_description), remote);
    return accepted.wait_for(8s) == std::future_status::ready && accepted.get();
}

bool OpenPair(webrtc::Thread& signaling, webrtc::PeerConnectionFactoryInterface& factory,
              const webrtc::Environment& environment, LoopbackNetwork& network,
              webrtc::BasicPacketSocketFactory& sockets, Endpoint& sending,
              Endpoint& receiving, const webrtc::scoped_refptr<FixtureTrack>& source,
              std::string& offer) {
    if (!sending.Open(factory, environment, network, sockets) ||
        !receiving.Open(factory, environment, network, sockets)) {
        std::fputs("loopback: create peers failed\n", stderr);
        return false;
    }
    auto track = factory.CreateVideoTrack(source, "linko-fixture");
    webrtc::RtpTransceiverInit init;
    init.direction = webrtc::RtpTransceiverDirection::kSendOnly;
    init.stream_ids = {"linko-local"};
    if (!track || !sending.peer->AddTransceiver(track, init).ok()) {
        std::fputs("loopback: create sending track failed\n", stderr);
        return false;
    }
    std::string answer;
    if (!Describe(signaling, sending, receiving, webrtc::SdpType::kOffer, offer) ||
        !Describe(signaling, receiving, sending, webrtc::SdpType::kAnswer, answer)) {
        std::fprintf(stderr, "loopback: negotiate failed (host candidates %u/%u)\n",
                     sending.observer.candidates.load(), receiving.observer.candidates.load());
        return false;
    }
    if (!Until([&] {
            return sending.peer->peer_connection_state() == Peer::PeerConnectionState::kConnected &&
                   receiving.peer->peer_connection_state() == Peer::PeerConnectionState::kConnected;
        }, 10s)) {
        std::fputs("loopback: ICE/DTLS did not connect\n", stderr);
        return false;
    }
    return true;
}

class Runtime {
 public:
    bool Start() {
        ssl_ = webrtc::InitializeSSL();
        if (!ssl_ || !network_thread->Start() || !signaling->Start()) {
            return false;
        }
        sockets = std::make_unique<webrtc::BasicPacketSocketFactory>(network_thread->socketserver());
        webrtc::PeerConnectionFactoryDependencies dependencies;
        dependencies.network_thread = network_thread.get();
        dependencies.signaling_thread = signaling.get();
        dependencies.socket_factory = network_thread->socketserver();
        dependencies.env = env;
        dependencies.adm =
            webrtc::AudioDeviceModuleImpl::Create(env, webrtc::AudioDeviceModule::kDummyAudio);
        if (!dependencies.adm) {
            return false;
        }
        dependencies.audio_encoder_factory = webrtc::CreateBuiltinAudioEncoderFactory();
        dependencies.audio_decoder_factory = webrtc::CreateBuiltinAudioDecoderFactory();
        dependencies.video_encoder_factory = webrtc::ObjCToNativeVideoEncoderFactory(
            [RTC_OBJC_TYPE(RTCVideoEncoderFactoryH264) new]);
        dependencies.video_decoder_factory = webrtc::ObjCToNativeVideoDecoderFactory(
            [RTC_OBJC_TYPE(RTCVideoDecoderFactoryH264) new]);
        webrtc::EnableMedia(dependencies);
        factory = webrtc::CreateModularPeerConnectionFactory(std::move(dependencies));
        if (!factory) {
            return false;
        }
        webrtc::PeerConnectionFactoryInterface::Options options;
        options.network_ignore_mask = 0;
        factory->SetOptions(options);
        return true;
    }

    ~Runtime() {
        factory = nullptr;
        if (sockets) {
            network_thread->BlockingCall([] {});
        }
        signaling.reset();
        network_thread.reset();
        if (ssl_) {
            webrtc::CleanupSSL();
        }
    }

    bool Connect(Endpoint& sending, Endpoint& receiving,
                 const webrtc::scoped_refptr<FixtureTrack>& source, std::string& offer) {
        return OpenPair(*signaling, *factory, env, loopback, *sockets, sending, receiving,
                        source, offer);
    }

    std::unique_ptr<webrtc::Thread> signaling = webrtc::Thread::Create();

 private:
    bool ssl_ = false;
    const webrtc::Environment env = webrtc::CreateEnvironment();
    std::unique_ptr<webrtc::Thread> network_thread = webrtc::Thread::CreateWithSocketServer();
    LoopbackNetwork loopback;
    std::unique_ptr<webrtc::BasicPacketSocketFactory> sockets;
    webrtc::scoped_refptr<webrtc::PeerConnectionFactoryInterface> factory;
};

#ifndef LINKO_RW04_NETWORK_PREVIEW
bool Exercise(Runtime& runtime, uint8_t color, std::string& offer) {
    Endpoint sending(*runtime.signaling, 0);
    Endpoint receiving(*runtime.signaling, color);
    auto source =
        runtime.signaling->BlockingCall([] { return webrtc::make_ref_counted<FixtureTrack>(); });
    if (!runtime.Connect(sending, receiving, source, offer)) {
        return false;
    }
    for (unsigned i = 0; i < 120 && receiving.observer.sink.matches() < 3; ++i) {
        source->Send(color);
        std::this_thread::sleep_for(40ms);
    }
    if (receiving.observer.sink.matches() < 3 || sending.observer.invalid() ||
        receiving.observer.invalid()) {
        std::fprintf(stderr, "loopback: received %u matching decoded frames\n",
                     receiving.observer.sink.matches());
        return false;
    }
    if (!receiving.Close() || !sending.Close()) {
        std::fputs("loopback: peers did not close\n", stderr);
        return false;
    }
    const auto stopped = receiving.observer.sink.matches();
    source->Send(color);
    std::this_thread::sleep_for(150ms);
    return receiving.observer.sink.matches() == stopped;
}

std::string IceUser(const std::string& sdp) {
    constexpr char leader[] = "a=ice-ufrag:";
    const auto at = sdp.find(leader);
    if (at == std::string::npos) {
        return {};
    }
    const auto start = at + sizeof(leader) - 1;
    return sdp.substr(start, sdp.find_first_of("\r\n", start) - start);
}

int Run() {
    Runtime runtime;
    if (!runtime.Start()) {
        return 1;
    }
    std::string first;
    std::string second;
    const bool passed = Exercise(runtime, 55, first) && Exercise(runtime, 185, second);
    if (!passed || IceUser(first).empty() || IceUser(second).empty() || IceUser(first) == IceUser(second)) {
        return 4;
    }
    std::puts("Loopback UDP/DTLS H.264: decoded video, closed peers, and fresh ICE reconnection passed.");
    return 0;
}
#endif

}  // namespace

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

extern "C" CVPixelBufferRef linko_rw04_loopback_take(LinkoLoopbackPreview* preview) {
    std::lock_guard lock(preview->mutex);
    if (!preview->runtime) {
        return nullptr;
    }
    auto& observer = preview->receiving->observer;
    auto frame = observer.sink.Take();
    if (!frame || observer.invalid() || preview->sending->observer.invalid()) {
        return nullptr;
    }
    id image = webrtc::NativeToObjCVideoFrameBuffer(frame);
    if (![image isKindOfClass:[RTC_OBJC_TYPE(RTCCVPixelBuffer) class]]) {
        return nullptr;
    }
    CVPixelBufferRef pixels = ((RTC_OBJC_TYPE(RTCCVPixelBuffer)*)image).pixelBuffer;
    return pixels ? CVPixelBufferRetain(pixels) : nullptr;
}
#else
int main() {
    @autoreleasepool {
        return Run();
    }
}
#endif
