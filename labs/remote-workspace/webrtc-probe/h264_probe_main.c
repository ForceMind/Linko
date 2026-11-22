#include "h264_probe.h"

#include <stddef.h>
#include <stdio.h>
#include <stdlib.h>

_Static_assert(sizeof(LinkoH264Request) == 16, "request ABI");
_Static_assert(sizeof(LinkoH264Result) == 16, "result ABI");
_Static_assert(sizeof(LinkoH264StreamResult) == 24, "stream result ABI");
_Static_assert(offsetof(LinkoH264Request, rtp_timestamp) == 12, "request timestamp offset");
_Static_assert(offsetof(LinkoH264Result, rtp_timestamp) == 8, "result timestamp offset");

static int check_frame(LinkoH264Request request, uint32_t timestamp) {
    LinkoH264Result frame = {0};
    request.rtp_timestamp = timestamp;
    int status = linko_rw04_h264_roundtrip(&request, &frame);
    if (status != LINKO_H264_OK || frame.width != request.width || frame.height != request.height ||
        frame.rtp_timestamp != request.rtp_timestamp || frame.encoded_bytes == 0) {
        fprintf(stderr, "H.264 bridge result: status=%d timestamp=%u\n", status, (unsigned)timestamp);
        return 3;
    }
    return 0;
}

static int check_stream_frames(LinkoH264Session* session, LinkoH264Request request) {
    uint32_t timestamp = request.rtp_timestamp;
    unsigned delta_frames = 0;
    for (unsigned i = 0; i < 18; ++i) {
        LinkoH264StreamResult result = {0};
        uint8_t level = (uint8_t)(48 + i * 6);
        int status = linko_rw04_h264_submit(session, timestamp, level, &result);
        if (status != LINKO_H264_OK || result.frame.rtp_timestamp != timestamp ||
            result.frame.width != request.width || result.frame.height != request.height ||
            result.frame.encoded_bytes == 0 || (i == 0 && !result.key_frame) ||
            abs((int)result.decoded_luma - level) > 12) {
            fprintf(stderr, "H.264 stream frame %u: status=%d expected=%u decoded=%u key=%u\n",
                    i, status, (unsigned)level, (unsigned)result.decoded_luma, (unsigned)result.key_frame);
            return 4;
        }
        delta_frames += result.key_frame == 0;
        if (i == 1 &&
            (linko_rw04_h264_submit(session, timestamp, level, &result) != LINKO_H264_BAD_REQUEST ||
             linko_rw04_h264_submit(session, timestamp - 3000, level, &result) != LINKO_H264_BAD_REQUEST)) {
            return 5;
        }
        timestamp += 3000;
    }
    if (!delta_frames || timestamp >= request.rtp_timestamp) {
        fprintf(stderr, "H.264 stream: delta_frames=%u final_rtp=%u\n", delta_frames, (unsigned)timestamp);
        return 6;
    }
    printf("Continuous H.264: 18 decoded frames, %u delta frames, RTP wrap and brightness verified.\n",
           delta_frames);
    return 0;
}

static int check_stream(void) {
    LinkoH264Request request = {LINKO_H264_PROBE_V1, 128, 96, UINT32_MAX - 5000};
    LinkoH264Session* session = NULL;
    int status = linko_rw04_h264_open(&request, &session);
    if (status != LINKO_H264_OK || !session) {
        fprintf(stderr, "H.264 stream open: status=%d\n", status);
        return 7;
    }
    int result = check_stream_frames(session, request);
    int close_status = linko_rw04_h264_close(&session);
    LinkoH264StreamResult frame = {0};
    if (close_status != LINKO_H264_OK || session ||
        linko_rw04_h264_close(&session) != LINKO_H264_OK ||
        linko_rw04_h264_submit(session, request.rtp_timestamp, 48, &frame) != LINKO_H264_BAD_REQUEST) {
        return 8;
    }
    return result;
}

int main(void) {
    LinkoH264Request request = {LINKO_H264_PROBE_V1, 128, 96, UINT32_MAX - 20};
    LinkoH264Result frame = {0};
    LinkoH264Request invalid = request;
    invalid.version++;
    if (linko_rw04_h264_roundtrip(&invalid, &frame) != LINKO_H264_BAD_REQUEST ||
        linko_rw04_h264_roundtrip(&request, NULL) != LINKO_H264_BAD_REQUEST ||
        linko_rw04_h264_roundtrip(NULL, &frame) != LINKO_H264_BAD_REQUEST) {
        return 1;
    }
    invalid = request;
    invalid.width = 1930;
    if (linko_rw04_h264_roundtrip(&invalid, &frame) != LINKO_H264_BAD_REQUEST) {
        return 2;
    }
    if (check_frame(request, UINT32_MAX - 20) || check_frame(request, 12)) {
        return 3;
    }
    puts("C ABI -> WebRTC VideoToolbox H.264 -> decoded dimensions and RTP timestamps passed.");
    return 0;
}
