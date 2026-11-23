#ifndef LINKO_RW04_CODEC_WORKER_PROTOCOL_H
#define LINKO_RW04_CODEC_WORKER_PROTOCOL_H

#include <stdbool.h>
#include <stdint.h>
#include <stdio.h>
#include <string.h>

// Fixed records on the child process's inherited control pipes.
enum WorkerMessage { WorkerStart = 1, WorkerReady, WorkerStop, WorkerStopped };
enum { WorkerVersion = 1, WorkerMessageSize = 16, WorkerGenerationOffset = 8 };

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
