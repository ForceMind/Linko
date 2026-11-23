#ifndef LINKO_RW04_CODEC_WORKER_PROTOCOL_H
#define LINKO_RW04_CODEC_WORKER_PROTOCOL_H

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

// Fixed records on the child process's inherited control pipes.
enum WorkerMessage { WorkerStart = 1, WorkerReady, WorkerStop, WorkerStopped, WorkerFrame };
enum { WorkerVersion = 1, WorkerMessageSize = 16, WorkerGenerationOffset = 8 };
enum { WorkerFrameWidth = 128, WorkerFrameHeight = 96, WorkerFrameCount = 2 };
enum { WorkerFrameBytes = WorkerFrameWidth * WorkerFrameHeight * 3 / 2 };

struct WorkerFramePixels {
    uint8_t data[WorkerFrameBytes];
};

struct WorkerControl {
    FILE *events;
    uint64_t generation;
    const char *backend;
};

static inline bool readWorkerMessage(struct WorkerControl const *control, enum WorkerMessage expected, uint64_t *generation) {
    unsigned char message[WorkerMessageSize];
    if (fread(message, 1, sizeof(message), stdin) != sizeof(message)) {
        fprintf(stderr, "%s worker control: incomplete message.\n", control->backend);
        return false;
    }
    if (memcmp(message, "LRW4", 4) != 0 || message[4] != WorkerVersion ||
        message[5] != (unsigned char)expected || message[6] != 0 || message[7] != 0) {
        fprintf(stderr, "%s worker control: invalid header.\n", control->backend);
        return false;
    }
    uint64_t value = 0;
    for (size_t index = WorkerGenerationOffset; index < sizeof(message); ++index) {
        value = (value << 8) | message[index];
    }
    if (!value) {
        fprintf(stderr, "%s worker control: missing generation.\n", control->backend);
        return false;
    }
    *generation = value;
    return true;
}

static inline bool sendWorkerMessage(struct WorkerControl const *control, enum WorkerMessage kind) {
    unsigned char message[WorkerMessageSize] = {'L', 'R', 'W', '4', WorkerVersion, (unsigned char)kind};
    uint64_t generation = control->generation;
    for (size_t index = sizeof(message); index > WorkerGenerationOffset;) {
        message[--index] = (unsigned char)(generation & 0xff);
        generation >>= 8;
    }
    return fwrite(message, 1, sizeof(message), control->events) == sizeof(message) &&
           fflush(control->events) == 0;
}

// Callers check the native plane heights and readable buffer extents first.
static inline bool copyWorkerNv12(struct WorkerFramePixels *frame, uint8_t const *y, size_t yStride,
                                 uint8_t const *uv, size_t uvStride) {
    if (!y || !uv || yStride < WorkerFrameWidth || uvStride < WorkerFrameWidth) {
        return false;
    }
    for (size_t row = 0; row < WorkerFrameHeight; ++row) {
        memcpy(frame->data + row * WorkerFrameWidth, y + row * yStride, WorkerFrameWidth);
    }
    uint8_t *chroma = frame->data + WorkerFrameWidth * WorkerFrameHeight;
    for (size_t row = 0; row < WorkerFrameHeight / 2; ++row) {
        memcpy(chroma + row * WorkerFrameWidth, uv + row * uvStride, WorkerFrameWidth);
    }
    return true;
}

static inline bool sendWorkerFrame(struct WorkerControl const *control,
                                   struct WorkerFramePixels const *frame, unsigned char sequence) {
    const uint8_t geometry[12] = {
        0, 0, 0, sequence, 0, WorkerFrameWidth, 0, WorkerFrameHeight,
        0, 0, WorkerFrameBytes >> 8, WorkerFrameBytes & 0xff,
    };
    return sendWorkerMessage(control, WorkerFrame) &&
           fwrite(geometry, 1, sizeof(geometry), control->events) == sizeof(geometry) &&
           fwrite(frame->data, 1, sizeof(frame->data), control->events) == sizeof(frame->data) &&
           fflush(control->events) == 0;
}

static inline bool waitForWorkerStop(struct WorkerControl const *control) {
    uint64_t generation;
    if (!sendWorkerMessage(control, WorkerReady) || !readWorkerMessage(control, WorkerStop, &generation)) {
        return false;
    }
    if (generation != control->generation) {
        fprintf(stderr, "%s worker control: mismatched generation.\n", control->backend);
        return false;
    }
    return true;
}

#endif
