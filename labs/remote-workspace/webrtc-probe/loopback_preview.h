#ifndef LINKO_RW04_LOOPBACK_PREVIEW_H
#define LINKO_RW04_LOOPBACK_PREVIEW_H

#include <CoreVideo/CoreVideo.h>

#ifdef __cplusplus
extern "C" {
#endif

typedef struct LinkoLoopbackPreview LinkoLoopbackPreview;

// Open before capture. The preview owns both endpoints and their threads.
LinkoLoopbackPreview* linko_rw04_loopback_open(void);
void linko_rw04_loopback_close(LinkoLoopbackPreview* preview);

// One serial producer and one consumer. The producer does not wait for decoding.
void linko_rw04_loopback_submit(LinkoLoopbackPreview* preview, CVPixelBufferRef input);
CVPixelBufferRef linko_rw04_loopback_take(LinkoLoopbackPreview* preview) CF_RETURNS_RETAINED;

#ifdef __cplusplus
}
#endif

#endif
