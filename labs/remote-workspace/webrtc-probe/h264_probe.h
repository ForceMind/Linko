#ifndef LINKO_H264_PROBE_H
#define LINKO_H264_PROBE_H

#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum {
    LINKO_H264_PROBE_V1 = 1,
    LINKO_H264_OK = 0,
    LINKO_H264_BAD_REQUEST = 1,
    LINKO_H264_UNAVAILABLE = 2,
    LINKO_H264_ENCODE_FAILED = 3,
    LINKO_H264_DECODE_FAILED = 4,
    LINKO_H264_FRAME_MISMATCH = 5,
    LINKO_H264_PLATFORM_ERROR = 6
};

typedef struct {
    uint32_t version;
    uint32_t width;
    uint32_t height;
    uint32_t rtp_timestamp;
} LinkoH264Request;

typedef struct {
    uint32_t width;
    uint32_t height;
    uint32_t rtp_timestamp;
    uint32_t encoded_bytes;
} LinkoH264Result;

// Runs one synthetic key frame. Both pointers are caller-owned for the call.
int linko_rw04_h264_roundtrip(const LinkoH264Request* request, LinkoH264Result* result);

#ifdef __cplusplus
}
#endif
#endif
