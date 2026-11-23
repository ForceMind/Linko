#ifndef LINKO_RW04_MACOS_CODEC_FIXTURE_H
#define LINKO_RW04_MACOS_CODEC_FIXTURE_H

#include "codec_worker_protocol.h"

struct SckWindowFrame {
    uint8_t bgra[WorkerFrameWidth * WorkerFrameHeight * 4];
};

bool withMacWorkerControl(bool (*run)(struct WorkerControl const *, void *), void *context);
bool encodeSckWindowFrames(struct WorkerControl const *control, struct SckWindowFrame const frames[2]);

#endif
