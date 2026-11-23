#pragma once

#include <cstddef>
#include <span>

struct WgcFixtureFrame {
    std::span<std::byte const> bgra;
    unsigned width;
    unsigned height;
};

struct WorkerControl;
struct WorkerFramePixels;

bool hasWgcFixtureColor(WgcFixtureFrame frame, bool resized);
void markWgcCodecStage(char const* stage) noexcept;
int runWgcCodecWorker(int (*run)(WorkerControl const*));
int runWgcPresenter(int (*run)(WorkerControl const*));
int checkWgcCodec(WgcFixtureFrame first, WgcFixtureFrame resized, WorkerControl const* worker);

struct WgcLiveCodec;
int runWgcLiveCodecs(int (*run)(WorkerControl const*, bool), bool display, bool pauseForExpiry);
WgcLiveCodec* startWgcLiveCodec();
void submitWgcLiveFrame(WgcLiveCodec& codec, WgcFixtureFrame frame);
void verifyWgcLiveFrame(WgcLiveCodec& codec, char const* source, WorkerFramePixels* displayOutput);
void finishWgcLiveCodec(WgcLiveCodec& codec);
void deleteWgcLiveCodec(WgcLiveCodec* codec) noexcept;
