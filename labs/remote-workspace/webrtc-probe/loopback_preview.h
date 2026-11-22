#ifndef LINKO_RW04_LOOPBACK_PREVIEW_H
#define LINKO_RW04_LOOPBACK_PREVIEW_H

#include <CoreVideo/CoreVideo.h>
#include <stdbool.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LinkoLoopbackPreview LinkoLoopbackPreview;

// Open before capture. The preview owns both endpoints and their threads.
LinkoLoopbackPreview* linko_rw04_loopback_open(void);
void linko_rw04_loopback_close(LinkoLoopbackPreview* preview);

// Revoke closes both endpoints and releases their threads, even when a
// capture callback still retains the preview. Calls after revocation fail closed.
void linko_rw04_loopback_revoke(LinkoLoopbackPreview* preview);
bool linko_rw04_loopback_was_closed(LinkoLoopbackPreview* preview);

// One serial producer and one consumer. The producer does not wait for decoding.
bool linko_rw04_loopback_submit(LinkoLoopbackPreview* preview, CVPixelBufferRef input);

// Both timestamps use the same process-local monotonic clock. Polling a decoded
// frame does not issue a new timestamp. received_at may be NULL.
double linko_rw04_loopback_now(void);
CVPixelBufferRef linko_rw04_loopback_take(LinkoLoopbackPreview* preview,
                                         double* received_at) CF_RETURNS_RETAINED;

#ifdef __cplusplus
}
#endif

#endif
