#pragma once

#include <cstddef>
#include <span>

struct WgcFixtureFrame {
    std::span<std::byte const> bgra;
    unsigned width;
    unsigned height;
};

int checkWgcCodec(WgcFixtureFrame first, WgcFixtureFrame resized);
