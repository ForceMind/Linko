#include "rtc_base/win32_socket_init.h"
#include <mfapi.h>
#include "linko_probe/windows_h264_factory.h"

namespace {
constexpr char kVideoCodec[] = "H264";
auto CreateLoopbackEncoderFactory() { return CreateMfEncoderFactory(); }
auto CreateLoopbackDecoderFactory() { return CreateMfDecoderFactory(); }
}  // namespace

#include "linko_probe/local_transport_common.inc"

int main() {
    webrtc::WinsockInitializer winsock;
    if (FAILED(MFStartup(MF_VERSION))) return 8;
    const int result = Run();
    const bool passed = MfCodecCallbacksPassed();
    const HRESULT stopped = MFShutdown();
    return result == 0 && passed && SUCCEEDED(stopped) ? 0 : 9;
}
