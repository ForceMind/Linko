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
    LINKO_H264_PLATFORM_ERROR = 6,
    LINKO_H264_SESSION_FAILED = 7
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

typedef struct LinkoH264Session LinkoH264Session;

typedef struct {
    LinkoH264Result frame;
    uint32_t decoded_luma;
    uint32_t key_frame;
} LinkoH264StreamResult;

// Single owner, serialized calls. Open requires an empty output slot.
int linko_rw04_h264_open(const LinkoH264Request* request, LinkoH264Session** session);

// Submits one synthetic I420 frame with constant luma in [16, 235].
// The first timestamp must match the request. Each subsequent advance may
// wrap the 32-bit clock and must be between 1 and 90000 ticks.
int linko_rw04_h264_submit(LinkoH264Session* session, uint32_t timestamp, uint8_t luma,
                           LinkoH264StreamResult* result);

// Releases the handle and clears its caller-owned slot. An empty slot is safe.
int linko_rw04_h264_close(LinkoH264Session** session);

int linko_rw04_h264_roundtrip(const LinkoH264Request* request, LinkoH264Result* result);

#ifdef __cplusplus
}
#endif
#endif
