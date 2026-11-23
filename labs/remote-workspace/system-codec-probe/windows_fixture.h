#pragma once

#include <cstddef>
#include <span>

struct WgcFixtureFrame {
    std::span<std::byte const> bgra;
    unsigned width;
    unsigned height;
};

struct WorkerControl;

bool hasWgcFixtureColor(WgcFixtureFrame frame, bool resized);
void markWgcCodecStage(char const* stage) noexcept;
int runWgcCodecWorker(int (*run)(WorkerControl const*));
int checkWgcCodec(WgcFixtureFrame first, WgcFixtureFrame resized, WorkerControl const* worker);
