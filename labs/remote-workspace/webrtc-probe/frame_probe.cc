#include <cstdint>
#include <cstdio>

#include "api/video/i420_buffer.h"
#include "api/video/video_frame.h"

extern "C" int linko_rw04_check_frame(std::uint32_t timestamp) {
    constexpr int width = 64;
    constexpr int height = 48;
    auto pixels = webrtc::I420Buffer::Create(width, height);
    webrtc::I420Buffer::SetBlack(pixels.get());
    auto frame = webrtc::VideoFrame::Builder()
                     .set_video_frame_buffer(pixels)
                     .set_rtp_timestamp(timestamp)
                     .set_timestamp_us(1000)
                     .build();
    if (frame.width() != width || frame.height() != height ||
        frame.rtp_timestamp() != timestamp || frame.timestamp_us() != 1000) {
        return 1;
    }
    return 0;
}

int main() {
    if (linko_rw04_check_frame(UINT32_MAX - 20) || linko_rw04_check_frame(12)) {
        return 1;
    }
    std::puts("WebRTC VideoFrame: I420 dimensions and both wrapped RTP values preserved.");
    return 0;
}
