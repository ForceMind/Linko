#ifndef LINKO_RW04_CODEC_WORKER_PROTOCOL_H
#define LINKO_RW04_CODEC_WORKER_PROTOCOL_H

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

// Fixed records on the child process's inherited control pipes.
enum WorkerMessage {
    WorkerStart = 1, WorkerReady, WorkerStop, WorkerStopped, WorkerFrame, WorkerPresented, WorkerPresent
};
enum { WorkerVersion = 1, WorkerMessageSize = 16, WorkerGenerationOffset = 8 };
enum { WorkerFrameWidth = 128, WorkerFrameHeight = 96, WorkerFrameCount = 2 };
enum { WorkerFrameBytes = WorkerFrameWidth * WorkerFrameHeight * 3 / 2 };
enum { WorkerRgbaBytes = WorkerFrameWidth * WorkerFrameHeight * 4 };

struct WorkerFramePixels {
    uint8_t data[WorkerFrameBytes];
};

struct WorkerRgbaPixels {
    uint8_t data[WorkerRgbaBytes];
};

static inline bool readWorkerRgba(struct WorkerRgbaPixels *pixels) {
    return fread(pixels->data, 1, sizeof(pixels->data), stdin) == sizeof(pixels->data);
}

static inline bool matchesWorkerRgb(uint8_t const *capturedBgra, uint8_t const *expectedRgba) {
    for (size_t channel = 0; channel < 3; ++channel) {
        const int difference = (int)capturedBgra[2 - channel] - expectedRgba[channel];
        if (difference < -16 || difference > 16) {
            return false;
        }
    }
    return true;
}

static inline bool matchesWorkerPresentation(uint8_t const *capturedBgra, uint8_t const *expectedRgba) {
    for (size_t y = WorkerFrameHeight / 4; y < WorkerFrameHeight; y += WorkerFrameHeight / 4) {
        for (size_t x = WorkerFrameWidth / 4; x < WorkerFrameWidth; x += WorkerFrameWidth / 4) {
            const size_t offset = 4 * (y * WorkerFrameWidth + x);
            if (!matchesWorkerRgb(capturedBgra + offset, expectedRgba + offset)) {
                return false;
            }
        }
    }
    return true;
}

// The fixture provides a complete 128x96 BGRA crop with a verified row stride.
static inline void workerBgraToNv12(uint8_t const *bgra, size_t stride, uint8_t *nv12) {
    for (size_t y = 0; y < WorkerFrameHeight; y += 2) {
        for (size_t x = 0; x < WorkerFrameWidth; x += 2) {
            int u = 0;
            int v = 0;
            for (size_t dy = 0; dy < 2; ++dy) {
                for (size_t dx = 0; dx < 2; ++dx) {
                    const uint8_t *pixel = bgra + (y + dy) * stride + 4 * (x + dx);
                    const int blue = pixel[0], green = pixel[1], red = pixel[2];
                    nv12[(y + dy) * WorkerFrameWidth + x + dx] =
                        (uint8_t)((66 * red + 129 * green + 25 * blue + 128) / 256 + 16);
                    u += 112 * blue - 74 * green - 38 * red;
                    v += 112 * red - 94 * green - 18 * blue;
                }
            }
            const size_t chroma = WorkerFrameWidth * WorkerFrameHeight + (y / 2) * WorkerFrameWidth + x;
            nv12[chroma] = (uint8_t)((u + 128 * 1024 + 512) / 1024);
            nv12[chroma + 1] = (uint8_t)((v + 128 * 1024 + 512) / 1024);
        }
    }
}

struct WorkerControl {
    FILE *events;
    uint64_t generation;
    const char *backend;
    bool (*present)(struct WorkerControl const *);
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
    if (!sendWorkerMessage(control, WorkerReady) || (control->present && !control->present(control)) ||
        !readWorkerMessage(control, WorkerStop, &generation)) {
        return false;
    }
    if (generation != control->generation) {
        fprintf(stderr, "%s worker control: mismatched generation.\n", control->backend);
        return false;
    }
    return true;
}

#endif
