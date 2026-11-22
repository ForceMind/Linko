#define NOMINMAX
#include <windows.h>
#ifdef LINKO_RW03_CI
#include <psapi.h>
#endif
#include <d3d11.h>
#include <dwmapi.h>
#include <windows.graphics.capture.interop.h>
#include <windows.graphics.directx.direct3d11.interop.h>
#include <winrt/Windows.Foundation.h>
#include <winrt/Windows.Graphics.Capture.h>
#include <winrt/Windows.Graphics.DirectX.Direct3D11.h>
#include <winrt/Windows.Graphics.DirectX.h>

#include <algorithm>
#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstring>
#include <cstdio>
#include <iterator>
#include <string_view>
#include <system_error>
#include <utility>
#include <memory>
#include <mutex>
#include <string>
#include <thread>
#include <vector>

using namespace winrt;
using namespace winrt::Windows::Graphics;
using namespace winrt::Windows::Graphics::Capture;
using namespace winrt::Windows::Graphics::DirectX;
using namespace winrt::Windows::Graphics::DirectX::Direct3D11;
using namespace std::chrono_literals;
using Clock = std::chrono::steady_clock;

namespace {
constexpr SizeInt32 captureLimit{4096, 2160};
constexpr int refreshId = 100;
constexpr int startId = 101;
constexpr int stopId = 102;
constexpr int listId = 103;
constexpr auto unavailableSource = L"窗口已关闭、隐藏或最小化。请重新选择。";

struct Source {
    HWND window{};
    DWORD process{};
    DWORD thread{};
    std::wstring name;
};

bool available(Source const& source) {
    DWORD process{};
    DWORD const thread = GetWindowThreadProcessId(source.window, &process);
    DWORD cloaked{};
    return thread && thread == source.thread && process == source.process &&
           IsWindowVisible(source.window) && !IsIconic(source.window) &&
           SUCCEEDED(DwmGetWindowAttribute(source.window, DWMWA_CLOAKED, &cloaked, sizeof(cloaked))) &&
           !cloaked;
}

std::wstring title(HWND window) {
    wchar_t buffer[256]{};
    GetWindowTextW(window, buffer, static_cast<int>(std::size(buffer)));
    return buffer;
}

struct Frame {
    int width{};
    int height{};
    std::vector<std::byte> pixels;
};

bool valid(SizeInt32 size) {
    return size.Width > 0 && size.Height > 0 &&
           size.Width <= captureLimit.Width && size.Height <= captureLimit.Height;
}

class Preview {
    std::mutex mutex_;
    std::shared_ptr<Frame const> frame_;
    wchar_t const* status_ = L"请选择一个窗口。";
    HWND viewWindow_{};
    std::jthread worker_;

    void publish(wchar_t const* status, std::shared_ptr<Frame const> frame = {}) {
        std::lock_guard guard(mutex_);
        if (status_ == status && frame_ == frame) {
            return;
        }
        status_ = status;
        frame_ = std::move(frame);
        if (viewWindow_) {
            InvalidateRect(viewWindow_, nullptr, FALSE);
        }
    }

    void capture(std::stop_token stop, Source const& source) {
        init_apartment(apartment_type::multi_threaded);
        struct Apartment {
            ~Apartment() { uninit_apartment(); }
        } apartment;
        if (!GraphicsCaptureSession::IsSupported()) {
            publish(L"当前 Windows 桌面不支持窗口采集。");
            return;
        }
        if (!available(source) || title(source.window) != source.name) {
            publish(L"窗口已变化，请刷新后重新选择。");
            return;
        }
        auto factory = get_activation_factory<GraphicsCaptureItem, IGraphicsCaptureItemInterop>();
        GraphicsCaptureItem item{nullptr};
        check_hresult(factory->CreateForWindow(source.window, guid_of<GraphicsCaptureItem>(), put_abi(item)));
        auto closed = std::make_shared<std::atomic_bool>(false);
        auto onClosed = item.Closed(auto_revoke, [closed](auto const&, auto const&) { closed->store(true); });
        if (!available(source) || title(source.window) != source.name) {
            publish(L"窗口已变化，请刷新后重新选择。");
            return;
        }
        auto size = item.Size();
        if (!valid(size)) {
            publish(L"窗口尺寸超出预览限制。");
            return;
        }

        com_ptr<ID3D11Device> device;
        com_ptr<ID3D11DeviceContext> context;
        check_hresult(D3D11CreateDevice(nullptr, D3D_DRIVER_TYPE_HARDWARE, nullptr, D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                                       nullptr, 0, D3D11_SDK_VERSION, device.put(), nullptr, context.put()));
        com_ptr<::IInspectable> inspectable;
        check_hresult(CreateDirect3D11DeviceFromDXGIDevice(device.as<IDXGIDevice>().get(), inspectable.put()));
        auto direct = inspectable.as<IDirect3DDevice>();
        constexpr auto format = DirectXPixelFormat::B8G8R8A8UIntNormalized;
        // Keep the capture surfaces stable. Recreating the pool can drop the only
        // frame emitted by a static window after resizing.
        auto pool = Direct3D11CaptureFramePool::CreateFreeThreaded(direct, format, 2, captureLimit);
        auto session = pool.CreateCaptureSession(item);
        com_ptr<ID3D11Texture2D> staging;
        UINT stagingWidth{};
        UINT stagingHeight{};
        session.StartCapture();
        auto lastFrame = Clock::now();
        while (!stop.stop_requested() && !closed->load() && available(source)) {
            auto captured = pool.TryGetNextFrame();
            if (!captured) {
                check_hresult(device->GetDeviceRemovedReason());
                if (Clock::now() - lastFrame > 2s) {
                    publish(L"暂无画面。");
                }
                std::this_thread::sleep_for(40ms);
                continue;
            }
            auto next = captured.ContentSize();
            if (!valid(next)) {
                publish(L"窗口尺寸超出预览限制。");
                return;
            }
            if (next.Width != size.Width || next.Height != size.Height) {
                publish(L"正在调整预览尺寸。");
                size = next;
            }
            auto access = captured.Surface().as<::Windows::Graphics::DirectX::Direct3D11::IDirect3DDxgiInterfaceAccess>();
            com_ptr<ID3D11Texture2D> texture;
            check_hresult(access->GetInterface(__uuidof(ID3D11Texture2D), texture.put_void()));
            D3D11_TEXTURE2D_DESC desc{};
            texture->GetDesc(&desc);
            if (desc.Width < static_cast<UINT>(size.Width) || desc.Height < static_cast<UINT>(size.Height)) {
                publish(L"正在等待新尺寸的画面。");
                continue;
            }
            if (!staging || desc.Width != stagingWidth || desc.Height != stagingHeight) {
                staging = nullptr;
                stagingWidth = desc.Width;
                stagingHeight = desc.Height;
                desc.Usage = D3D11_USAGE_STAGING;
                desc.BindFlags = 0;
                desc.CPUAccessFlags = D3D11_CPU_ACCESS_READ;
                desc.MiscFlags = 0;
                check_hresult(device->CreateTexture2D(&desc, nullptr, staging.put()));
            }
            auto frame = std::make_shared<Frame>();
            frame->width = size.Width;
            frame->height = size.Height;
            auto const row = static_cast<std::size_t>(size.Width) * 4;
            frame->pixels.resize(row * static_cast<std::size_t>(size.Height));
            context->CopyResource(staging.get(), texture.get());
            D3D11_MAPPED_SUBRESOURCE mapped{};
            check_hresult(context->Map(staging.get(), 0, D3D11_MAP_READ, 0, &mapped));
            if (!mapped.pData || mapped.RowPitch < row) {
                context->Unmap(staging.get(), 0);
                publish(L"无法读取窗口画面。");
                return;
            }
            for (int y = 0; y < size.Height; ++y) {
                auto offset = static_cast<std::size_t>(y);
                std::memcpy(frame->pixels.data() + offset * row,
                            static_cast<std::byte const*>(mapped.pData) + offset * mapped.RowPitch, row);
            }
            context->Unmap(staging.get(), 0);
            captured.Close();
            if (!stop.stop_requested() && !closed->load() && available(source)) {
                lastFrame = Clock::now();
                publish(L"正在本机预览。", std::move(frame));
            }
            std::this_thread::sleep_for(40ms);
        }
        onClosed.revoke();
        pool.Close();
        session.Close();
        publish(stop.stop_requested() ? L"预览已停止。" : unavailableSource);
    }

public:
    ~Preview() { stop(); }

    void attachView(HWND window) {
        std::lock_guard guard(mutex_);
        viewWindow_ = window;
    }

    void start(Source source) {
        stop();
        publish(L"正在打开预览。");
        try {
            worker_ = std::jthread([this, source = std::move(source)](std::stop_token stopToken) {
                try {
                    capture(stopToken, source);
                } catch (hresult_error const& error) {
#ifdef LINKO_RW03_CI
                    std::fprintf(stderr, "WGC HRESULT 0x%08X\n", static_cast<unsigned>(error.code().value));
#else
                    static_cast<void>(error);
#endif
                    publish(L"系统拒绝或中断了窗口采集。");
                } catch (std::exception const&) {
                    publish(L"无法采集此窗口。请检查系统权限或窗口状态。");
                } catch (...) {
                    publish(L"无法继续预览此窗口。");
                }
            });
        } catch (std::system_error const&) {
            publish(L"无法启动窗口预览。");
        }
    }

    void stop() {
        if (!worker_.joinable()) {
            return;
        }
        worker_.request_stop();
        worker_.join();
        publish(L"预览已停止。");
    }

    auto view() {
        std::lock_guard guard(mutex_);
        return std::pair{status_, frame_};
    }
};

struct App {
    HWND list{};
    HWND status{};
    std::vector<Source> sources;
    Preview preview;

    void refresh() {
        sources.clear();
        SendMessageW(list, LB_RESETCONTENT, 0, 0);
        EnumWindows([](HWND window, LPARAM data) -> BOOL {
            auto& app = *reinterpret_cast<App*>(data);
            if (app.sources.size() == 128) {
                return FALSE;
            }
            Source source{window, 0, 0, {}};
            source.thread = GetWindowThreadProcessId(window, &source.process);
            if (source.process == GetCurrentProcessId() || GetWindow(window, GW_OWNER) || !available(source)) {
                return TRUE;
            }
            source.name = title(window);
            if (!source.name.empty() &&
                SendMessageW(app.list, LB_ADDSTRING, 0, reinterpret_cast<LPARAM>(source.name.c_str())) >= 0) {
                app.sources.push_back(std::move(source));
            }
            return TRUE;
        }, reinterpret_cast<LPARAM>(this));
    }
};

HWND control(HWND owner, wchar_t const* type, wchar_t const* text, DWORD style, int id) {
    auto child = CreateWindowExW(0, type, text, WS_VISIBLE | WS_CHILD | style, 0, 0, 0, 0,
                                 owner, reinterpret_cast<HMENU>(static_cast<INT_PTR>(id)), nullptr, nullptr);
    SendMessageW(child, WM_SETFONT, reinterpret_cast<WPARAM>(GetStockObject(DEFAULT_GUI_FONT)), TRUE);
    return child;
}

LRESULT CALLBACK windowProc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == WM_NCCREATE) {
        auto create = reinterpret_cast<CREATESTRUCTW const*>(lparam);
        SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(create->lpCreateParams));
    }
    auto app = reinterpret_cast<App*>(GetWindowLongPtrW(window, GWLP_USERDATA));
    if (!app) {
        return DefWindowProcW(window, message, wparam, lparam);
    }
    switch (message) {
    case WM_CREATE:
        app->preview.attachView(window);
        app->list = control(window, L"LISTBOX", nullptr, WS_BORDER | WS_VSCROLL | WS_TABSTOP | LBS_NOINTEGRALHEIGHT, listId);
        app->status = control(window, L"STATIC", L"请选择一个窗口。", 0, 0);
        control(window, L"BUTTON", L"刷新列表", WS_TABSTOP, refreshId);
        control(window, L"BUTTON", L"开始预览", WS_TABSTOP, startId);
        control(window, L"BUTTON", L"停止", WS_TABSTOP, stopId);
        app->refresh();
        return 0;
    case WM_GETMINMAXINFO: {
        auto limits = reinterpret_cast<MINMAXINFO*>(lparam);
        limits->ptMinTrackSize = {360, 300};
        return 0;
    }
    case WM_SIZE: {
        int const width = LOWORD(lparam);
        MoveWindow(app->list, 12, 12, std::max(20, width - 24), 74, TRUE);
        MoveWindow(GetDlgItem(window, refreshId), 12, 94, 90, 28, TRUE);
        MoveWindow(GetDlgItem(window, startId), 110, 94, 90, 28, TRUE);
        MoveWindow(GetDlgItem(window, stopId), 208, 94, 78, 28, TRUE);
        MoveWindow(app->status, 12, 130, std::max(20, width - 24), 22, TRUE);
        return 0;
    }
    case WM_COMMAND:
        switch (LOWORD(wparam)) {
        case refreshId:
            app->refresh();
            break;
        case startId: {
            auto const index = SendMessageW(app->list, LB_GETCURSEL, 0, 0);
            if (index >= 0 && static_cast<std::size_t>(index) < app->sources.size()) {
                app->preview.start(app->sources[static_cast<std::size_t>(index)]);
            } else {
                MessageBeep(MB_ICONINFORMATION);
            }
            break;
        }
        case stopId:
            app->preview.stop();
            break;
        }
        return 0;
    case WM_PAINT: {
        PAINTSTRUCT paint{};
        auto dc = BeginPaint(window, &paint);
        auto [status, frame] = app->preview.view();
        wchar_t previous[120]{};
        GetWindowTextW(app->status, previous, static_cast<int>(std::size(previous)));
        if (std::wstring_view(previous) != status) {
            SetWindowTextW(app->status, status);
        }
        RECT area{};
        GetClientRect(window, &area);
        area.top = 156;
        FillRect(dc, &area, static_cast<HBRUSH>(GetStockObject(BLACK_BRUSH)));
        if (frame && area.bottom > area.top && area.right > 0) {
            auto scale = std::min(static_cast<double>(area.right) / frame->width,
                                  static_cast<double>(area.bottom - area.top) / frame->height);
            auto width = static_cast<int>(frame->width * scale);
            auto height = static_cast<int>(frame->height * scale);
            BITMAPINFO info{};
            info.bmiHeader = {sizeof(BITMAPINFOHEADER), frame->width, -frame->height, 1, 32, BI_RGB};
            StretchDIBits(dc, (area.right - width) / 2, area.top + (area.bottom - area.top - height) / 2,
                          width, height, 0, 0, frame->width, frame->height, frame->pixels.data(),
                          &info, DIB_RGB_COLORS, SRCCOPY);
        }
        EndPaint(window, &paint);
        return 0;
    }
    case WM_DESTROY:
        app->preview.attachView(nullptr);
        app->preview.stop();
        PostQuitMessage(0);
        return 0;
    default:
        return DefWindowProcW(window, message, wparam, lparam);
    }
}

HWND createPreviewWindow(HINSTANCE instance, App& app) {
    WNDCLASSW type{};
    type.hInstance = instance;
    type.lpfnWndProc = windowProc;
    type.lpszClassName = L"LinkoLocalPreview";
    type.hCursor = LoadCursorW(nullptr, IDC_ARROW);
    type.hbrBackground = reinterpret_cast<HBRUSH>(COLOR_WINDOW + 1);
    if (!RegisterClassW(&type)) {
        return nullptr;
    }
    return CreateWindowExW(WS_EX_CONTROLPARENT, type.lpszClassName, L"屏跃 · 本机窗口预览",
                           WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                           CW_USEDEFAULT, CW_USEDEFAULT, 780, 580,
                           nullptr, nullptr, instance, &app);
}

#ifdef LINKO_RW03_CI
struct FixtureWindow {
    HWND handle{};
    ~FixtureWindow() { close(); }
    void close() {
        if (handle) {
            DestroyWindow(std::exchange(handle, nullptr));
        }
    }
};

bool showsFixtureColor(Frame const& frame, bool blue) {
    const auto width = static_cast<std::size_t>(frame.width);
    const auto height = static_cast<std::size_t>(frame.height);
    const auto center = (height / 2 * width + width / 2) * 4;
    if (center + 2 >= frame.pixels.size()) {
        return false;
    }
    auto const red = std::to_integer<unsigned>(frame.pixels[center + 2]);
    auto const blueValue = std::to_integer<unsigned>(frame.pixels[center]);
    return blue ? blueValue > red + 40 : red > blueValue + 40;
}

struct ProcessUsage {
    DWORD handles{};
    DWORD gdi{};
};

bool sampleStoppedProcess(unsigned cycle, ProcessUsage& usage) {
    auto process = GetCurrentProcess();
    PROCESS_MEMORY_COUNTERS memory{};
    memory.cb = static_cast<DWORD>(sizeof(memory));
    if (!GetProcessHandleCount(process, &usage.handles) ||
        !K32GetProcessMemoryInfo(process, &memory, memory.cb)) {
        std::fprintf(stderr, "WGC cycles: process measurement failed (%lu).\n", GetLastError());
        return false;
    }
    usage.gdi = GetGuiResources(process, GR_GDIOBJECTS);
    if (!usage.gdi) {
        std::fputs("WGC cycles: GDI object measurement unavailable.\n", stderr);
        return false;
    }
    std::printf("WGC cycles: stopped=%u working_set_bytes=%llu handles=%lu gdi=%lu\n",
                cycle, static_cast<unsigned long long>(memory.WorkingSetSize),
                usage.handles, usage.gdi);
    return true;
}

LRESULT CALLBACK fixtureProc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == WM_PAINT) {
        PAINTSTRUCT paint{};
        RECT area{};
        auto dc = BeginPaint(window, &paint);
        GetClientRect(window, &area);
        const bool blue = GetWindowLongPtrW(window, GWLP_USERDATA) != 0;
        auto brush = CreateSolidBrush(blue ? RGB(30, 70, 190) : RGB(190, 30, 70));
        FillRect(dc, &area, brush);
        DeleteObject(brush);
        EndPaint(window, &paint);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

int smoke(HINSTANCE instance) {
    if (valid({0, 16}) || valid({4096, 4096}) || !valid({1920, 1080})) {
        return 10;
    }
    App app;
    FixtureWindow host{createPreviewWindow(instance, app)};
    if (!host.handle) {
        return 21;
    }
    ShowWindow(host.handle, SW_SHOWNOACTIVATE);
    auto& preview = app.preview;
    unsigned previewPaints = 0;
    unsigned previewTimers = 0;
    auto wait = [&](auto predicate) {
        for (int count = 0; count < 120; ++count) {
            MSG message{};
            while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
                if (message.hwnd == host.handle) {
                    previewPaints += message.message == WM_PAINT ? 1u : 0u;
                    previewTimers += message.message == WM_TIMER ? 1u : 0u;
                }
                DispatchMessageW(&message);
            }
            if (predicate()) {
                return true;
            }
            std::this_thread::sleep_for(40ms);
        }
        return false;
    };
    auto observeIdle = [&] {
        auto const until = Clock::now() + 300ms;
        return wait([&] { return Clock::now() >= until; }) && previewTimers == 0;
    };
    if (!observeIdle()) {
        std::fputs("WGC preview dispatched a timer while idle before selection.\n", stderr);
        return 21;
    }
    auto displaysStatus = [&](wchar_t const* expected) {
        wchar_t text[120]{};
        GetWindowTextW(app.status, text, static_cast<int>(std::size(text)));
        return std::wstring_view(text) == expected;
    };

    WNDCLASSW fixtureType{};
    fixtureType.hInstance = instance;
    fixtureType.lpfnWndProc = fixtureProc;
    fixtureType.style = CS_HREDRAW | CS_VREDRAW;
    fixtureType.lpszClassName = L"LinkoRW03Fixture";
    if (!RegisterClassW(&fixtureType)) {
        return 11;
    }
    constexpr auto caption = L"Linko RW03 fixture";
    FixtureWindow selected{CreateWindowExW(WS_EX_TOPMOST, fixtureType.lpszClassName, caption,
                                          WS_OVERLAPPEDWINDOW | WS_VISIBLE, 40, 40, 320, 200,
                                          nullptr, nullptr, instance, nullptr)};
    auto fixture = selected.handle;
    if (!fixture) {
        return 11;
    }
    UpdateWindow(fixture);
    DWORD process{};
    auto thread = GetWindowThreadProcessId(fixture, &process);
    Source firstSource{fixture, process, thread, caption};
    preview.start(firstSource);
    std::shared_ptr<Frame const> first;
    if (!wait([&] {
        first = preview.view().second;
        return first && showsFixtureColor(*first, false) && displaysStatus(L"正在本机预览。");
    })) {
        ::fwprintf(stderr, L"WGC fixture did not deliver a frame: %ls\n", preview.view().first);
        return 12;
    }
    auto previous = first->width;
    std::printf("WGC first fixture frame: %dx%d\n", first->width, first->height);
    first.reset();
    SetWindowLongPtrW(fixture, GWLP_USERDATA, 1);
    if (!SetWindowPos(fixture, nullptr, 0, 0, 450, 300, SWP_NOMOVE | SWP_NOZORDER)) {
        return 13;
    }
    RedrawWindow(fixture, nullptr, nullptr, RDW_INVALIDATE | RDW_UPDATENOW);
    if (!wait([&] {
        auto frame = preview.view().second;
        return frame && frame->width > previous && showsFixtureColor(*frame, true);
    })) {
        auto [state, frame] = preview.view();
        ::fwprintf(stderr, L"WGC fixture did not resize: %ls (%d)\n", state, frame ? frame->width : 0);
        return 13;
    }
    std::puts("WGC resized frame confirmed.");

    FixtureWindow alternate{CreateWindowExW(WS_EX_TOPMOST, fixtureType.lpszClassName, caption,
                                           WS_OVERLAPPEDWINDOW | WS_VISIBLE, 80, 80, 320, 200,
                                           nullptr, nullptr, instance, nullptr)};
    if (!alternate.handle) {
        return 14;
    }
    UpdateWindow(alternate.handle);
    ShowWindow(fixture, SW_MINIMIZE);
    auto stopped = [&] {
        auto [status, frame] = preview.view();
        return !frame && std::wstring_view(status) == unavailableSource;
    };
    if (!IsIconic(fixture) || !wait(stopped)) {
        std::fputs("WGC did not stop on minimize while the other fixture remained visible.\n", stderr);
        return 15;
    }
    ShowWindow(fixture, SW_RESTORE);
    RedrawWindow(fixture, nullptr, nullptr, RDW_INVALIDATE | RDW_UPDATENOW);
    auto until = Clock::now() + 400ms;
    bool resumed = false;
    if (!wait([&] {
        resumed = !stopped();
        return resumed || Clock::now() >= until;
    }) || resumed) {
        std::fputs("WGC resumed without an explicit selection.\n", stderr);
        return 16;
    }
    preview.start(firstSource);
    if (!wait([&] {
        auto frame = preview.view().second;
        return frame && showsFixtureColor(*frame, true);
    })) {
        std::fputs("WGC did not resume the reselected fixture.\n", stderr);
        return 17;
    }
    selected.close();
    if (!wait(stopped)) {
        std::fputs("WGC fixture remained visible after close.\n", stderr);
        return 18;
    }
    auto otherThread = GetWindowThreadProcessId(alternate.handle, &process);
    Source secondSource{alternate.handle, process, otherThread, caption};
    auto showsSecond = [&] {
        auto frame = preview.view().second;
        return frame && showsFixtureColor(*frame, false) && displaysStatus(L"正在本机预览。");
    };
    preview.start(secondSource);
    if (!wait(showsSecond)) {
        std::fputs("WGC did not switch to the explicitly selected same-title fixture.\n", stderr);
        return 19;
    }
    auto stopFromView = [&] {
        auto const paintsBeforeStop = previewPaints;
        SendMessageW(host.handle, WM_COMMAND, stopId, 0);
        return wait([&] {
            return !preview.view().second && previewPaints > paintsBeforeStop && displaysStatus(L"预览已停止。");
        });
    };
    if (!stopFromView() || !observeIdle()) {
        std::fputs("WGC preview did not repaint on stop or stopped with an active timer.\n", stderr);
        return 20;
    }
    std::puts("WGC preview UI: frame and stop repaints arrived without idle timer messages.");
    std::puts("WGC fixture: resize, minimize, explicit reselection, same-title isolation, and close passed.");

    ProcessUsage usage;
    if (!sampleStoppedProcess(0, usage)) {
        return 22;
    }
    ProcessUsage warm;
    for (unsigned cycle = 1; cycle <= 100; ++cycle) {
        preview.start(secondSource);
        RedrawWindow(alternate.handle, nullptr, nullptr, RDW_INVALIDATE | RDW_UPDATENOW);
        if (!wait(showsSecond) || !stopFromView()) {
            std::fprintf(stderr, "WGC cycles: open, render or stop failed at cycle %u.\n", cycle);
            return 23;
        }
        if (cycle == 10 || cycle % 25 == 0) {
            if (!observeIdle() || !sampleStoppedProcess(cycle, usage)) {
                return 24;
            }
            if (cycle == 10) {
                warm = usage;
            }
        }
    }
    if (usage.handles > warm.handles || usage.gdi > warm.gdi) {
        std::fputs("WGC cycles: outstanding handles or GDI objects grew after warm-up.\n", stderr);
        return 25;
    }
    std::puts("WGC cycles: 100 capture and stop cycles passed without handle or GDI growth.");
    return 0;
}
#endif
} // namespace

#ifdef LINKO_RW03_CI
int main() {
    std::setvbuf(stdout, nullptr, _IONBF, 0);
    std::setvbuf(stderr, nullptr, _IONBF, 0);
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    auto result = smoke(GetModuleHandleW(nullptr));
    std::printf("WGC smoke result: %d\n", result);
    return result;
}
#else
int WINAPI wWinMain(HINSTANCE instance, HINSTANCE, PWSTR, int show) {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    App app;
    auto window = createPreviewWindow(instance, app);
    if (!window) {
        return 1;
    }
    ShowWindow(window, show);
    MSG message{};
    int result{};
    while ((result = GetMessageW(&message, nullptr, 0, 0)) > 0) {
        if (!IsDialogMessageW(window, &message)) {
            TranslateMessage(&message);
            DispatchMessageW(&message);
        }
    }
    return result < 0 ? 1 : 0;
}
#endif
