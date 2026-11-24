#include "rtc_base/win32_socket_init.h"

#include <memory>

#include "api/video_codecs/video_decoder_factory_template.h"
#include "api/video_codecs/video_decoder_factory_template_libvpx_vp8_adapter.h"
#include "api/video_codecs/video_encoder_factory_template.h"
#include "api/video_codecs/video_encoder_factory_template_libvpx_vp8_adapter.h"

namespace {
constexpr char kVideoCodec[] = "VP8";

auto CreateLoopbackEncoderFactory() {
    return std::make_unique<webrtc::VideoEncoderFactoryTemplate<webrtc::LibvpxVp8EncoderTemplateAdapter>>();
}

auto CreateLoopbackDecoderFactory() {
    return std::make_unique<webrtc::VideoDecoderFactoryTemplate<webrtc::LibvpxVp8DecoderTemplateAdapter>>();
}
}  // namespace

#include "linko_probe/local_transport_common.inc"

int main() {
    webrtc::WinsockInitializer winsock;
    return Run();
}
