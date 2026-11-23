#define NOMINMAX
#include <windows.h>
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
#include <cstdint>
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

struct WorkerControl;

#ifdef LINKO_RW04_WGC_CODEC_CI
#include <dbghelp.h>

#include "../system-codec-probe/codec_worker_protocol.h"
#include "../system-codec-probe/windows_fixture.h"

namespace {
std::atomic<char const*> wgcCodecStage{"opening"};
std::atomic_flag reportingException{};
DWORD smokeThread{};
HANDLE debugProcess{};

bool reportCodecFrame(unsigned depth, DWORD64 address) {
    HMODULE module{};
    if (GetModuleHandleExW(GET_MODULE_HANDLE_EX_FLAG_FROM_ADDRESS | GET_MODULE_HANDLE_EX_FLAG_UNCHANGED_REFCOUNT,
                           reinterpret_cast<LPCWSTR>(address), &module)) {
        wchar_t path[MAX_PATH]{};
        const auto length = GetModuleFileNameW(module, path, static_cast<DWORD>(std::size(path)));
        if (length && length < std::size(path)) {
            const std::wstring_view name(path, length);
            const auto lastSeparator = name.find_last_of(L'\\');
            const auto offset = address - reinterpret_cast<std::uintptr_t>(module);
            std::fprintf(stderr, "WGC codec frame=%u module=%ls offset=0x%llX.\n",
                         depth, path + (lastSeparator == std::wstring_view::npos ? 0 : lastSeparator + 1),
                         static_cast<unsigned long long>(offset));
            return true;
        }
    }
    std::fprintf(stderr, "WGC codec frame=%u module=unknown.\n", depth);
    return false;
}

LONG WINAPI reportWgcCodecException(EXCEPTION_POINTERS* exception) {
    if (reportingException.test_and_set(std::memory_order_relaxed)) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    auto const* record = exception->ExceptionRecord;
    std::fprintf(stderr, "WGC codec exception=0x%08lX, last_stage=%s, thread=%lu, smoke_thread=%lu.\n",
                 record->ExceptionCode, wgcCodecStage.load(std::memory_order_relaxed),
                 GetCurrentThreadId(), smokeThread);
    CONTEXT context = *exception->ContextRecord;
    unsigned depth = 0;
    const bool mapped = reportCodecFrame(depth++, context.Rip);
    if (!debugProcess) {
        return EXCEPTION_CONTINUE_SEARCH;
    }
    if (!mapped && record->ExceptionCode == EXCEPTION_ACCESS_VIOLATION &&
        record->NumberParameters >= 2 && record->ExceptionInformation[0] == 8) {
        // A failed indirect call can leave RIP outside a loaded image and its return address at RSP.
        DWORD64 possibleCaller{};
        SIZE_T bytesRead{};
        if (ReadProcessMemory(debugProcess, reinterpret_cast<void const*>(context.Rsp),
                              &possibleCaller, sizeof(possibleCaller), &bytesRead) &&
            bytesRead == sizeof(possibleCaller) && reportCodecFrame(depth++, possibleCaller)) {
            std::fputs("WGC codec unwinding from possible caller.\n", stderr);
            context.Rip = possibleCaller;
            context.Rsp += sizeof(possibleCaller);
        }
    }
    SymRefreshModuleList(debugProcess);
    STACKFRAME64 frame{};
    frame.AddrPC = {context.Rip, 0, AddrModeFlat};
    frame.AddrStack = {context.Rsp, 0, AddrModeFlat};
    frame.AddrFrame = {context.Rbp, 0, AddrModeFlat};
    auto previous = context.Rip;
    for (unsigned step = 0; step < 12 && depth < 12; ++step) {
        if (!StackWalk64(IMAGE_FILE_MACHINE_AMD64, debugProcess, GetCurrentThread(), &frame, &context,
                         nullptr, SymFunctionTableAccess64, SymGetModuleBase64, nullptr) ||
            !frame.AddrPC.Offset) {
            break;
        }
        if (frame.AddrPC.Offset != previous) {
            reportCodecFrame(depth++, frame.AddrPC.Offset);
            previous = frame.AddrPC.Offset;
        }
    }
    return EXCEPTION_CONTINUE_SEARCH;
}
}

void markWgcCodecStage(char const* stage) noexcept {
    wgcCodecStage.store(stage, std::memory_order_relaxed);
}
#endif

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
    std::jthread worker_;

    void publish(wchar_t const* status, std::shared_ptr<Frame const> frame = {}) {
        std::lock_guard guard(mutex_);
        status_ = status;
        frame_ = std::move(frame);
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
        publish(stop.stop_requested() ? L"预览已停止。" : L"窗口已关闭、隐藏或最小化。请重新选择。");
    }

public:
    ~Preview() { stop(); }

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

void drawBgra(HDC dc, Frame const& frame, int x, int y, int width, int height) {
    BITMAPINFO info{};
    info.bmiHeader = {sizeof(BITMAPINFOHEADER), frame.width, -frame.height, 1, 32, BI_RGB};
    StretchDIBits(dc, x, y, width, height, 0, 0, frame.width, frame.height, frame.pixels.data(),
                  &info, DIB_RGB_COLORS, SRCCOPY);
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
        app->list = control(window, L"LISTBOX", nullptr, WS_BORDER | WS_VSCROLL | WS_TABSTOP | LBS_NOINTEGRALHEIGHT, listId);
        app->status = control(window, L"STATIC", L"请选择一个窗口。", 0, 0);
        control(window, L"BUTTON", L"刷新列表", WS_TABSTOP, refreshId);
        control(window, L"BUTTON", L"开始预览", WS_TABSTOP, startId);
        control(window, L"BUTTON", L"停止", WS_TABSTOP, stopId);
        app->refresh();
        SetTimer(window, 1, 60, nullptr);
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
    case WM_TIMER: {
        auto status = app->preview.view().first;
        wchar_t previous[120]{};
        GetWindowTextW(app->status, previous, static_cast<int>(std::size(previous)));
        if (std::wstring_view(previous) != status) {
            SetWindowTextW(app->status, status);
        }
        RECT area{};
        GetClientRect(window, &area);
        area.top = 156;
        InvalidateRect(window, &area, FALSE);
        return 0;
    }
    case WM_PAINT: {
        PAINTSTRUCT paint{};
        auto dc = BeginPaint(window, &paint);
        RECT area{};
        GetClientRect(window, &area);
        area.top = 156;
        FillRect(dc, &area, static_cast<HBRUSH>(GetStockObject(BLACK_BRUSH)));
        auto frame = app->preview.view().second;
        if (frame && area.bottom > area.top && area.right > 0) {
            auto scale = std::min(static_cast<double>(area.right) / frame->width,
                                  static_cast<double>(area.bottom - area.top) / frame->height);
            auto width = static_cast<int>(frame->width * scale);
            auto height = static_cast<int>(frame->height * scale);
            drawBgra(dc, *frame, (area.right - width) / 2, area.top + (area.bottom - area.top - height) / 2,
                     width, height);
        }
        EndPaint(window, &paint);
        return 0;
    }
    case WM_DESTROY:
        KillTimer(window, 1);
        app->preview.stop();
        PostQuitMessage(0);
        return 0;
    default:
        return DefWindowProcW(window, message, wparam, lparam);
    }
}

#ifdef LINKO_RW03_CI
bool waitForFixture(auto predicate) {
    for (int count = 0; count < 120; ++count) {
        MSG message{};
        while (PeekMessageW(&message, nullptr, 0, 0, PM_REMOVE)) {
            DispatchMessageW(&message);
        }
        if (predicate()) {
            return true;
        }
        std::this_thread::sleep_for(40ms);
    }
    return false;
}

#ifdef LINKO_RW04_WGC_CODEC_CI
WgcFixtureFrame capturedFixtureFrame(Frame const& frame) {
    return {frame.pixels, static_cast<unsigned>(frame.width), static_cast<unsigned>(frame.height)};
}
#endif

LRESULT CALLBACK fixtureProc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == WM_PAINT) {
        PAINTSTRUCT paint{};
        RECT area{};
        auto dc = BeginPaint(window, &paint);
        GetClientRect(window, &area);
        auto brush = CreateSolidBrush(area.right > 400 ? RGB(30, 70, 190) : RGB(190, 30, 70));
        FillRect(dc, &area, brush);
        DeleteObject(brush);
        EndPaint(window, &paint);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

bool registerFixture(HINSTANCE instance, wchar_t const* className = L"LinkoRW03Fixture",
                     WNDPROC proc = fixtureProc) {
    WNDCLASSW fixtureType{};
    fixtureType.hInstance = instance;
    fixtureType.lpfnWndProc = proc;
    fixtureType.style = CS_HREDRAW | CS_VREDRAW;
    fixtureType.lpszClassName = className;
    return RegisterClassW(&fixtureType) != 0;
}

#ifdef LINKO_RW04_WGC_CODEC_CI
LRESULT CALLBACK presenterProc(HWND window, UINT message, WPARAM wparam, LPARAM lparam) {
    if (message == WM_PAINT) {
        PAINTSTRUCT paint{};
        auto dc = BeginPaint(window, &paint);
        FillRect(dc, &paint.rcPaint, static_cast<HBRUSH>(GetStockObject(BLACK_BRUSH)));
        const auto* frame = reinterpret_cast<Frame const*>(GetWindowLongPtrW(window, GWLP_USERDATA));
        if (frame) {
            drawBgra(dc, *frame, 0, 0, frame->width, frame->height);
        }
        EndPaint(window, &paint);
        return 0;
    }
    return DefWindowProcW(window, message, wparam, lparam);
}

int presentFixture(WorkerControl const*) {
    WorkerRgbaPixels input{};
    const auto instance = GetModuleHandleW(nullptr);
    constexpr auto className = L"LinkoPresentationFixture";
    if (!readWorkerRgba(&input) || !registerFixture(instance, className, presenterProc)) {
        return 17;
    }
    Frame frame{WorkerFrameWidth, WorkerFrameHeight, std::vector<std::byte>(WorkerRgbaBytes)};
    for (std::size_t offset = 0; offset < frame.pixels.size(); offset += 4) {
        frame.pixels[offset] = static_cast<std::byte>(input.data[offset + 2]);
        frame.pixels[offset + 1] = static_cast<std::byte>(input.data[offset + 1]);
        frame.pixels[offset + 2] = static_cast<std::byte>(input.data[offset]);
        frame.pixels[offset + 3] = static_cast<std::byte>(input.data[offset + 3]);
    }
    const auto window = CreateWindowExW(WS_EX_TOPMOST, className, L"Linko presentation fixture",
                                       WS_POPUP, 40, 40, WorkerFrameWidth, WorkerFrameHeight,
                                       nullptr, nullptr, instance, nullptr);
    if (!window) {
        return 18;
    }
    SetWindowLongPtrW(window, GWLP_USERDATA, reinterpret_cast<LONG_PTR>(&frame));
    ShowWindow(window, SW_SHOWNOACTIVATE);
    RedrawWindow(window, nullptr, nullptr, RDW_INVALIDATE | RDW_UPDATENOW);
    DWORD process{};
    const auto thread = GetWindowThreadProcessId(window, &process);
    Preview preview;
    preview.start({window, process, thread, title(window)});
    const bool shown = waitForFixture([&] {
        auto captured = preview.view().second;
        if (!captured || captured->width != WorkerFrameWidth || captured->height != WorkerFrameHeight ||
            captured->pixels.size() != WorkerRgbaBytes) {
            return false;
        }
        auto const* bgra = reinterpret_cast<uint8_t const*>(captured->pixels.data());
        return matchesWorkerPresentation(bgra, input.data);
    });
    DestroyWindow(window);
    preview.stop();
    if (!shown) {
        std::fputs("WGC presenter did not recapture the drawn pixels.\n", stderr);
        return 19;
    }
    std::puts("WGC presenter: recaptured the drawn native window.");
    return 0;
}
#endif

int smoke(HINSTANCE instance, [[maybe_unused]] WorkerControl const* worker = nullptr) {
    if (valid({0, 16}) || valid({4096, 4096}) || !valid({1920, 1080})) {
        return 10;
    }
    if (!registerFixture(instance)) {
        return 11;
    }
    auto fixture = CreateWindowExW(WS_EX_TOPMOST, L"LinkoRW03Fixture", L"Linko RW03 fixture",
                                    WS_OVERLAPPEDWINDOW | WS_VISIBLE, 40, 40, 320, 200,
                                    nullptr, nullptr, instance, nullptr);
    if (!fixture) {
        return 11;
    }
    UpdateWindow(fixture);
    DWORD process{};
    auto thread = GetWindowThreadProcessId(fixture, &process);
    Preview preview;
    preview.start({fixture, process, thread, title(fixture)});
    std::shared_ptr<Frame const> first;
    if (!waitForFixture([&] {
        first = preview.view().second;
#ifdef LINKO_RW04_WGC_CODEC_CI
        if (!first || !hasWgcFixtureColor(capturedFixtureFrame(*first), false)) {
            InvalidateRect(fixture, nullptr, FALSE);
            return false;
        }
#endif
        return first != nullptr;
    })) {
        ::fwprintf(stderr, L"WGC fixture did not deliver a frame: %ls\n", preview.view().first);
        DestroyWindow(fixture);
        return 12;
    }
    auto previous = first->width;
    std::printf("WGC first fixture frame: %dx%d\n", first->width, first->height);
#ifdef LINKO_RW04_WGC_CODEC_CI
    std::shared_ptr<Frame const> resized;
#else
    first.reset();
#endif
    if (!SetWindowPos(fixture, nullptr, 0, 0, 450, 300, SWP_NOMOVE | SWP_NOZORDER)) {
        return 13;
    }
    RedrawWindow(fixture, nullptr, nullptr, RDW_INVALIDATE | RDW_UPDATENOW);
    if (!waitForFixture([&] {
        auto frame = preview.view().second;
#ifdef LINKO_RW04_WGC_CODEC_CI
        if (!frame || frame->width <= previous || !hasWgcFixtureColor(capturedFixtureFrame(*frame), true)) {
            InvalidateRect(fixture, nullptr, FALSE);
            return false;
        }
        resized = std::move(frame);
        return true;
#else
        return frame && frame->width > previous;
#endif
    })) {
        auto [state, frame] = preview.view();
        ::fwprintf(stderr, L"WGC fixture did not resize: %ls (%d)\n", state, frame ? frame->width : 0);
        DestroyWindow(fixture);
        return 13;
    }
    std::puts("WGC resized frame confirmed.");
#ifdef LINKO_RW04_WGC_CODEC_CI
    markWgcCodecStage("destroying-source");
#endif
    DestroyWindow(fixture);
    if (!waitForFixture([&] { return preview.view().second == nullptr; })) {
        std::fputs("WGC fixture remained visible after close.\n", stderr);
        return 14;
    }
#ifdef LINKO_RW04_WGC_CODEC_CI
    markWgcCodecStage("joining-capture");
#endif
    preview.stop();
    std::puts("WGC fixture start, resize, source close and stop passed.");
    if (preview.view().second) {
        return 15;
    }
#ifdef LINKO_RW04_WGC_CODEC_CI
    markWgcCodecStage("retained-fixture-frames");
    return checkWgcCodec(capturedFixtureFrame(*first), capturedFixtureFrame(*resized), worker);
#else
    return 0;
#endif
}
#endif
} // namespace

#ifdef LINKO_RW03_CI
int main([[maybe_unused]] int argc, [[maybe_unused]] char** argv) {
    std::setvbuf(stdout, nullptr, _IONBF, 0);
    std::setvbuf(stderr, nullptr, _IONBF, 0);
#ifdef LINKO_RW04_WGC_CODEC_CI
    const bool exportPixels = argc == 2 && std::strcmp(argv[1], "--worker-pixels") == 0;
    const bool presentPixels = argc == 2 && std::strcmp(argv[1], "--present-pixels") == 0;
    if (argc != 1 && !exportPixels && !presentPixels) {
        return 2;
    }
    smokeThread = GetCurrentThreadId();
    SymSetOptions(SYMOPT_DEFERRED_LOADS | SYMOPT_NO_PROMPTS | SYMOPT_FAIL_CRITICAL_ERRORS);
    if (!DuplicateHandle(GetCurrentProcess(), GetCurrentProcess(), GetCurrentProcess(),
                         &debugProcess, 0, FALSE, DUPLICATE_SAME_ACCESS)) {
        std::fprintf(stderr, "WGC codec diagnostic process handle failed: %lu.\n", GetLastError());
    } else if (!SymInitialize(debugProcess, ".", TRUE)) {
        std::fprintf(stderr, "WGC codec diagnostic initialization failed: %lu.\n", GetLastError());
        CloseHandle(debugProcess);
        debugProcess = nullptr;
    }
    SetUnhandledExceptionFilter(reportWgcCodecException);
    // Keep the process MTA alive between joining the WGC thread and starting Media Foundation.
    const HRESULT apartment = CoInitializeEx(nullptr, COINIT_MULTITHREADED);
    if (FAILED(apartment)) {
        std::fprintf(stderr, "WGC codec shared MTA failed: 0x%08X.\n", static_cast<unsigned>(apartment));
        return 16;
    }
#endif
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    static_cast<void>(&windowProc);
#ifdef LINKO_RW04_WGC_CODEC_CI
    // The worker validates its start record before creating or capturing the fixture window.
    auto run = [](WorkerControl const* worker) { return smoke(GetModuleHandleW(nullptr), worker); };
    const int result = presentPixels ? runWgcPresenter(presentFixture)
                    : exportPixels ? runWgcCodecWorker(run) : run(nullptr);
    markWgcCodecStage("closing-shared-mta");
    CoUninitialize();
#else
    const int result = smoke(GetModuleHandleW(nullptr));
#endif
    std::printf("WGC smoke result: %d\n", result);
    return result;
}
#else
int WINAPI wWinMain(HINSTANCE instance, HINSTANCE, PWSTR, int show) {
    SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    App app;
    WNDCLASSW type{};
    type.hInstance = instance;
    type.lpfnWndProc = windowProc;
    type.lpszClassName = L"LinkoLocalPreview";
    type.hCursor = LoadCursorW(nullptr, IDC_ARROW);
    type.hbrBackground = reinterpret_cast<HBRUSH>(COLOR_WINDOW + 1);
    if (!RegisterClassW(&type)) {
        return 1;
    }
    auto window = CreateWindowExW(WS_EX_CONTROLPARENT, type.lpszClassName, L"屏跃 · 本机窗口预览",
                                  WS_OVERLAPPEDWINDOW | WS_CLIPCHILDREN,
                                  CW_USEDEFAULT, CW_USEDEFAULT, 780, 580,
                                  nullptr, nullptr, instance, &app);
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
