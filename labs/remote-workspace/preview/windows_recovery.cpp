// An Actions-only owner for one self-created Win32 window.
#define WIN32_LEAN_AND_MEAN
#define NOMINMAX
#include <windows.h>
#include <bcrypt.h>

#include <array>
#include <cstdint>
#include <iostream>
#include <sstream>
#include <string>

namespace {
std::string nonce() {
    std::array<unsigned char, 16> bytes{};
    if (BCryptGenRandom(nullptr, bytes.data(), static_cast<ULONG>(bytes.size()),
                        BCRYPT_USE_SYSTEM_PREFERRED_RNG) != 0) {
        return {};
    }
    constexpr char digits[] = "0123456789abcdef";
    std::string text;
    for (const auto byte : bytes) {
        text += digits[byte >> 4];
        text += digits[byte & 15];
    }
    return text;
}

bool moveWindow(HWND window, RECT bounds) {
    return SetWindowPos(window, nullptr, bounds.left, bounds.top,
                        bounds.right - bounds.left, bounds.bottom - bounds.top,
                        SWP_NOZORDER | SWP_NOACTIVATE) != FALSE;
}

struct Owner {
    enum class Completion { None, Untouched, Restored };
    HWND window;
    std::string incarnation;
    RECT original;
    static constexpr std::uint64_t object = 1;
    std::uint64_t parked{};
    std::uint64_t resolved{};
    bool bound{};
    Completion completion{};
    unsigned restores{};

    bool snapshot() const {
        RECT bounds{};
        if (!GetWindowRect(window, &bounds)) {
            return false;
        }
        std::cout << "snapshot " << (IsWindowVisible(window) != FALSE) << ' '
                  << restores << ' ' << incarnation << ' ' << object << ' '
                  << bounds.left << ' ' << bounds.top << ' '
                  << bounds.right - bounds.left << ' ' << bounds.bottom - bounds.top
                  << std::endl;
        return true;
    }

    bool command(std::string const& line) {
        if (line == "inspect") {
            return snapshot();
        }
        std::istringstream input(line);
        std::string command, owner, trailing;
        std::uint64_t transaction{}, generation{};
        LONG x{}, y{}, width{}, height{};
        if (!(input >> command >> transaction >> owner >> generation >> x >> y >> width >> height) ||
            input >> trailing || owner != incarnation || generation != object ||
            x != original.left || y != original.top ||
            width != original.right - original.left || height != original.bottom - original.top) {
            std::cout << "rejected" << std::endl;
            return true;
        }
        if (command == "bind" && !bound) {
            resolved = transaction;
            bound = true;
            std::cout << "bound" << std::endl;
            return true;
        }
        if (!bound || !transaction) {
            std::cout << "rejected" << std::endl;
            return true;
        }
        const bool next = transaction > resolved && transaction - resolved == 1;
        if (command == "hide" && !parked && next && IsWindowVisible(window)) {
            // Record ownership of the intent before the first native effect.
            parked = transaction;
            ShowWindow(window, SW_HIDE);
            RECT shifted{original.left + 48, original.top + 32,
                         original.right + 48, original.bottom + 32};
            RECT measured{};
            if (IsWindowVisible(window) || !moveWindow(window, shifted) ||
                !GetWindowRect(window, &measured) || !EqualRect(&shifted, &measured)) {
                return false;
            }
            std::cout << "hidden " << transaction << std::endl;
            return true;
        }
        if (command == "restore" && parked == transaction && !IsWindowVisible(window)) {
            if (!moveWindow(window, original)) {
                return false;
            }
            ShowWindow(window, SW_SHOWNOACTIVATE);
            RECT measured{};
            if (!IsWindowVisible(window) || !GetWindowRect(window, &measured) ||
                !EqualRect(&original, &measured)) {
                return false;
            }
            parked = 0;
            resolved = transaction;
            completion = Completion::Restored;
            ++restores;
        } else if (command == "restore" && !parked && next) {
            // Fence the unused transaction before sending its receipt.
            resolved = transaction;
            completion = Completion::Untouched;
        }
        if (command == "restore" && !parked && resolved == transaction &&
            completion != Completion::None) {
            std::cout << (completion == Completion::Restored ? "restored " : "untouched ")
                      << transaction << std::endl;
        } else {
            std::cout << "rejected" << std::endl;
        }
        return true;
    }
};

bool run(Owner& owner) {
    const auto input = GetStdHandle(STD_INPUT_HANDLE);
    std::string line;
    for (;;) {
        MSG message{};
        while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
        DWORD available{};
        if (!PeekNamedPipe(input, nullptr, 0, nullptr, &available, nullptr)) {
            return false;
        }
        if (available == 0) {
            Sleep(10);
            continue;
        }
        char byte{};
        DWORD received{};
        if (!ReadFile(input, &byte, 1, &received, nullptr) || received != 1) {
            return false;
        }
        if (byte == '\n') {
            if (line == "exit") {
                return true;
            }
            if (!owner.command(line)) {
                return false;
            }
            line.clear();
        } else if (byte != '\r') {
            if (line.size() >= 256) {
                return false;
            }
            line += byte;
        }
    }
}
} // namespace

int main() {
    if (!SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2)) {
        std::cerr << "cannot set recovery fixture DPI context\n";
        return 1;
    }
    const auto instance = GetModuleHandleW(nullptr);
    WNDCLASSW type{};
    type.lpfnWndProc = DefWindowProcW;
    type.hInstance = instance;
    type.lpszClassName = L"LinkoRecoveryFixture";
    if (!RegisterClassW(&type)) {
        return 1;
    }
    const auto window = CreateWindowExW(
        0, type.lpszClassName, L"Linko recovery fixture", WS_OVERLAPPEDWINDOW,
        120, 120, 320, 220, nullptr, nullptr, instance, nullptr);
    if (!window) {
        return 1;
    }
    ShowWindow(window, SW_SHOWNOACTIVATE);
    Owner owner{window, nonce(), {}};
    const bool ready = !owner.incarnation.empty() && GetWindowRect(window, &owner.original) &&
                       IsWindowVisible(window);
    if (ready) {
        std::cout << "ready" << std::endl;
    }
    const bool passed = ready && run(owner);
    DestroyWindow(window);
    if (!passed) {
        std::cerr << "Win32 window owner failed, error " << GetLastError() << '\n';
    }
    return passed ? 0 : 1;
}
