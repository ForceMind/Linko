#include "h264_probe.h"

#include <stddef.h>
#include <stdio.h>

_Static_assert(sizeof(LinkoH264Request) == 16, "request ABI");
_Static_assert(sizeof(LinkoH264Result) == 16, "result ABI");
_Static_assert(offsetof(LinkoH264Request, rtp_timestamp) == 12, "request timestamp offset");
_Static_assert(offsetof(LinkoH264Result, rtp_timestamp) == 8, "result timestamp offset");

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
    const uint32_t timestamps[] = { UINT32_MAX - 20, 12 };
    for (size_t i = 0; i < sizeof(timestamps) / sizeof(timestamps[0]); ++i) {
        request.rtp_timestamp = timestamps[i];
        int status = linko_rw04_h264_roundtrip(&request, &frame);
        if (status != LINKO_H264_OK || frame.width != request.width || frame.height != request.height ||
            frame.rtp_timestamp != request.rtp_timestamp || frame.encoded_bytes == 0) {
            fprintf(stderr, "H.264 bridge result: status=%d frame=%zu\n", status, i);
            return 3;
        }
    }
    puts("C ABI -> WebRTC VideoToolbox H.264 -> decoded dimensions and RTP timestamps passed.");
    return 0;
}
