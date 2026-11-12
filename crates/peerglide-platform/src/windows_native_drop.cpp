// A local, copy-only OLE drag from prepared content.
#define NOMINMAX
#include <windows.h>
#include <ole2.h>
#include <shellapi.h>
#include <shlobj.h>
#include <wrl/client.h>

#include <cstdint>
#include <cstring>
#include <new>

using Microsoft::WRL::ComPtr;

namespace {
bool expired(ULONGLONG deadline) {
    return deadline != 0 && GetTickCount64() >= deadline;
}
} // namespace

#include "windows_file_drop_stream.h"

namespace {
class DataObject final : public IDataObject {
public:
    DataObject(HGLOBAL data, CLIPFORMAT format, ULONGLONG deadline,
               VirtualFile* file = nullptr, CLIPFORMAT stream_format = 0)
        : data_(data), format_(format), deadline_(deadline), file_(file),
          stream_format_(stream_format) {}

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** result) override {
        if (!result) {
            return E_POINTER;
        }
        *result = nullptr;
        if (id != __uuidof(IUnknown) && id != __uuidof(IDataObject)) {
            return E_NOINTERFACE;
        }
        *result = static_cast<IDataObject*>(this);
        AddRef();
        return S_OK;
    }

    ULONG STDMETHODCALLTYPE AddRef() override {
        return static_cast<ULONG>(InterlockedIncrement(&refs_));
    }

    ULONG STDMETHODCALLTYPE Release() override {
        const auto remaining = InterlockedDecrement(&refs_);
        if (remaining == 0) {
            delete this;
        }
        return static_cast<ULONG>(remaining);
    }

    HRESULT STDMETHODCALLTYPE GetData(FORMATETC* request, STGMEDIUM* medium) override {
        if (!medium) {
            return E_POINTER;
        }
        const auto result = QueryGetData(request);
        if (FAILED(result)) {
            return result;
        }
        if (stream_request(request)) {
            ComPtr<IStream> stream;
            stream.Attach(new (std::nothrow) VerifiedFileStream(file_.Get()));
            if (!stream) {
                return E_OUTOFMEMORY;
            }
            if (!file_->open_stream()) {
                return STG_E_ACCESSDENIED;
            }
            *medium = {};
            medium->tymed = TYMED_ISTREAM;
            medium->pstm = stream.Detach();
            return S_OK;
        }
        // The medium keeps this owner alive rather than transferring or copying
        // the same global handle. ReleaseStgMedium releases that owner.
        medium->tymed = TYMED_HGLOBAL;
        medium->hGlobal = data_;
        medium->pUnkForRelease = static_cast<IDataObject*>(this);
        AddRef();
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE QueryGetData(FORMATETC* request) override {
        if (!request) {
            return E_POINTER;
        }
        const bool stream = stream_request(request);
        if (expired(deadline_) || (file_ && !file_->offering()) ||
            (!stream && request->cfFormat != format_)) {
            return DV_E_FORMATETC;
        }
        if (request->dwAspect != DVASPECT_CONTENT) {
            return DV_E_DVASPECT;
        }
        if (request->lindex != (stream ? 0 : -1)) {
            return DV_E_LINDEX;
        }
        return (request->tymed & (stream ? TYMED_ISTREAM : TYMED_HGLOBAL)) ? S_OK : DV_E_TYMED;
    }

    HRESULT STDMETHODCALLTYPE EnumFormatEtc(DWORD direction, IEnumFORMATETC** result) override {
        if (!result) {
            return E_POINTER;
        }
        *result = nullptr;
        if (direction != DATADIR_GET) {
            return E_NOTIMPL;
        }
        FORMATETC formats[] = {
            {format_, nullptr, DVASPECT_CONTENT, -1, TYMED_HGLOBAL},
            {stream_format_, nullptr, DVASPECT_CONTENT, 0, TYMED_ISTREAM},
        };
        return SHCreateStdEnumFmtEtc(file_ ? 2 : 1, formats, result);
    }

    HRESULT STDMETHODCALLTYPE GetDataHere(FORMATETC*, STGMEDIUM*) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE GetCanonicalFormatEtc(FORMATETC*, FORMATETC* result) override {
        if (!result) {
            return E_POINTER;
        }
        result->ptd = nullptr;
        return E_NOTIMPL;
    }
    HRESULT STDMETHODCALLTYPE SetData(FORMATETC*, STGMEDIUM*, BOOL) override { return E_NOTIMPL; }
    HRESULT STDMETHODCALLTYPE DAdvise(FORMATETC*, DWORD, IAdviseSink*, DWORD*) override {
        return OLE_E_ADVISENOTSUPPORTED;
    }
    HRESULT STDMETHODCALLTYPE DUnadvise(DWORD) override { return OLE_E_ADVISENOTSUPPORTED; }
    HRESULT STDMETHODCALLTYPE EnumDAdvise(IEnumSTATDATA**) override { return OLE_E_ADVISENOTSUPPORTED; }

private:
    ~DataObject() { GlobalFree(data_); }
    bool stream_request(const FORMATETC* request) const {
        return file_ && request->cfFormat == stream_format_;
    }
    volatile LONG refs_ = 1;
    HGLOBAL data_;
    CLIPFORMAT format_;
    ULONGLONG deadline_;
    ComPtr<VirtualFile> file_;
    CLIPFORMAT stream_format_;
};

class DropSource final : public IDropSource {
public:
    explicit DropSource(ULONGLONG deadline, VirtualFile* file = nullptr)
        : deadline_(deadline), file_(file) {}

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** result) override {
        if (!result) {
            return E_POINTER;
        }
        *result = nullptr;
        if (id != __uuidof(IUnknown) && id != __uuidof(IDropSource)) {
            return E_NOINTERFACE;
        }
        *result = static_cast<IDropSource*>(this);
        AddRef();
        return S_OK;
    }

    ULONG STDMETHODCALLTYPE AddRef() override {
        return static_cast<ULONG>(InterlockedIncrement(&refs_));
    }

    ULONG STDMETHODCALLTYPE Release() override {
        const auto remaining = InterlockedDecrement(&refs_);
        if (remaining == 0) {
            delete this;
        }
        return static_cast<ULONG>(remaining);
    }

    HRESULT STDMETHODCALLTYPE QueryContinueDrag(BOOL escape, DWORD keys) override {
        if (escape || expired(deadline_)) {
            if (file_) {
                file_->close();
            }
            return DRAGDROP_S_CANCEL;
        }
        if (keys & MK_LBUTTON) {
            return S_OK;
        }
        if (file_) {
            file_->admit_drop();
        }
        return DRAGDROP_S_DROP;
    }

    HRESULT STDMETHODCALLTYPE GiveFeedback(DWORD) override {
        return DRAGDROP_S_USEDEFAULTCURSORS;
    }

private:
    volatile LONG refs_ = 1;
    ULONGLONG deadline_;
    ComPtr<VirtualFile> file_;
};

struct OleScope {
    HRESULT result = OleInitialize(nullptr);
    ~OleScope() {
        if (SUCCEEDED(result)) {
            OleUninitialize();
        }
    }
};
// Takes ownership of allocation, including on cancellation or initialization failure.
int32_t start_drag(HGLOBAL allocation, CLIPFORMAT format, ULONGLONG deadline,
                   VirtualFile* file = nullptr, CLIPFORMAT stream_format = 0) {
    if (expired(deadline) || !(GetAsyncKeyState(VK_LBUTTON) & 0x8000)) {
        GlobalFree(allocation);
        return 0;
    }
    OleScope ole;
    if (FAILED(ole.result)) {
        GlobalFree(allocation);
        return ole.result;
    }
    ComPtr<IDataObject> data;
    data.Attach(new (std::nothrow) DataObject(allocation, format, deadline, file, stream_format));
    if (!data) {
        GlobalFree(allocation);
        return E_OUTOFMEMORY;
    }
    ComPtr<IDropSource> source;
    source.Attach(new (std::nothrow) DropSource(deadline, file));
    if (!source) {
        return E_OUTOFMEMORY;
    }

    DWORD effect = DROPEFFECT_NONE;
    const auto result = DoDragDrop(data.Get(), source.Get(), DROPEFFECT_COPY, &effect);
    const bool verified = !file || file->close();
    if (result == DRAGDROP_S_CANCEL) {
        return 0;
    }
    if (result == DRAGDROP_S_DROP && effect == DROPEFFECT_COPY && verified && !expired(deadline)) {
        return 1;
    }
    return FAILED(result) ? result : 2;
}

int32_t start_content_drag(
    const void* content, size_t bytes, size_t padding, CLIPFORMAT format, uint32_t millis) {
    if (!content || !format || bytes == 0 || millis == 0 || millis > 60'000) {
        return E_INVALIDARG;
    }
    const auto deadline = GetTickCount64() + millis;
    HGLOBAL allocation = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, bytes + padding);
    if (!allocation) {
        return E_OUTOFMEMORY;
    }
    void* data = GlobalLock(allocation);
    if (!data) {
        GlobalFree(allocation);
        return E_OUTOFMEMORY;
    }
    std::memcpy(data, content, bytes);
    GlobalUnlock(allocation);
    return start_drag(allocation, format, deadline);
}

HGLOBAL file_descriptor(const uint16_t* name, size_t units, uint64_t bytes) {
    HGLOBAL allocation = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, sizeof(FILEGROUPDESCRIPTORW));
    if (!allocation) {
        return nullptr;
    }
    auto* group = static_cast<FILEGROUPDESCRIPTORW*>(GlobalLock(allocation));
    if (!group) {
        GlobalFree(allocation);
        return nullptr;
    }
    group->cItems = 1;
    auto& descriptor = group->fgd[0];
    descriptor.dwFlags = FD_ATTRIBUTES | FD_FILESIZE | FD_UNICODE;
    descriptor.dwFileAttributes = FILE_ATTRIBUTE_NORMAL;
    descriptor.nFileSizeHigh = static_cast<DWORD>(bytes >> 32);
    descriptor.nFileSizeLow = static_cast<DWORD>(bytes);
    std::memcpy(descriptor.cFileName, name, units * sizeof(uint16_t));
    GlobalUnlock(allocation);
    return allocation;
}
} // namespace

extern "C" int32_t linko_windows_verified_file_drag(
    const uint16_t* name, size_t units, uint64_t bytes, uint32_t millis, void* owner,
    ReadDropFile read, ReleaseDropFile release) {
    if (!name || units == 0 || units >= MAX_PATH || millis == 0 || millis > 60'000 || !owner
        || !read || !release) {
        if (owner && release) {
            release(owner);
        }
        return E_INVALIDARG;
    }
    const auto deadline = GetTickCount64() + millis;
    ComPtr<VirtualFile> file;
    file.Attach(new (std::nothrow) VirtualFile(bytes, deadline, owner, read, release));
    if (!file) {
        release(owner);
        return E_OUTOFMEMORY;
    }
    const auto descriptor_format = static_cast<CLIPFORMAT>(RegisterClipboardFormatW(L"FileGroupDescriptorW"));
    const auto stream_format = static_cast<CLIPFORMAT>(RegisterClipboardFormatW(L"FileContents"));
    if (!descriptor_format || !stream_format) {
        return E_FAIL;
    }
    HGLOBAL allocation = file_descriptor(name, units, bytes);
    return allocation ? start_drag(allocation, descriptor_format, deadline, file.Get(), stream_format)
                      : E_OUTOFMEMORY;
}

// Exercise the actual COM objects in CI without a native drag or input capture.
extern "C" int32_t linko_windows_file_stream_regression() {
    OleScope ole;
    if (FAILED(ole.result)) {
        return 1;
    }
    struct Fixture {
        unsigned reads = 0;
        unsigned closes = 0;
    } fixture;
    const auto read = [](void* context, uint64_t offset, uint8_t* output, size_t count) {
        ++static_cast<Fixture*>(context)->reads;
        constexpr uint8_t bytes[] = {14, 38, 92};
        if (offset > sizeof(bytes) || count > sizeof(bytes) - offset) {
            return false;
        }
        std::memcpy(output, bytes + offset, count);
        return true;
    };
    const auto release = [](void* context) { ++static_cast<Fixture*>(context)->closes; };
    const auto deadline = GetTickCount64() + 10'000;
    ComPtr<VirtualFile> file;
    file.Attach(new (std::nothrow) VirtualFile(3, deadline, &fixture, read, release));
    if (!file) {
        return 2;
    }
    const uint16_t name[] = {'o', 'n', 'e'};
    const auto descriptor_format = static_cast<CLIPFORMAT>(RegisterClipboardFormatW(L"FileGroupDescriptorW"));
    const auto stream_format = static_cast<CLIPFORMAT>(RegisterClipboardFormatW(L"FileContents"));
    HGLOBAL descriptor = file_descriptor(name, 3, file->size);
    if (!descriptor) {
        return 3;
    }
    ComPtr<IDataObject> data;
    data.Attach(new (std::nothrow) DataObject(descriptor, descriptor_format, deadline, file.Get(),
                                             stream_format));
    if (!data) {
        GlobalFree(descriptor);
        return 4;
    }
    ComPtr<IDropSource> source;
    source.Attach(new (std::nothrow) DropSource(deadline, file.Get()));
    if (!source) {
        return 5;
    }
    FORMATETC metadata{descriptor_format, nullptr, DVASPECT_CONTENT, -1, TYMED_HGLOBAL};
    STGMEDIUM medium{};
    if (FAILED(data->GetData(&metadata, &medium))) {
        return 6;
    }
    auto* group = static_cast<FILEGROUPDESCRIPTORW*>(GlobalLock(medium.hGlobal));
    const bool described = group && group->cItems == 1 && group->fgd[0].nFileSizeLow == 3
                           && group->fgd[0].cFileName[3] == 0;
    if (group) {
        GlobalUnlock(medium.hGlobal);
    }
    ReleaseStgMedium(&medium);
    FORMATETC content{stream_format, nullptr, DVASPECT_CONTENT, -1, TYMED_ISTREAM};
    if (!described || data->QueryGetData(&content) != DV_E_LINDEX) {
        return 7;
    }
    content.lindex = 0;
    if (source->QueryContinueDrag(FALSE, MK_LBUTTON) != S_OK ||
        data->QueryGetData(&content) != S_OK ||
        data->GetData(&content, &medium) != STG_E_ACCESSDENIED || fixture.reads != 0) {
        return 8;
    }
    if (source->QueryContinueDrag(FALSE, 0) != DRAGDROP_S_DROP ||
        FAILED(data->GetData(&content, &medium))) {
        return 9;
    }
    ComPtr<IStream> stream;
    stream.Attach(medium.pstm);
    uint8_t output[4]{};
    ULONG copied = 0;
    if (stream->Read(output, sizeof(output), &copied) != S_FALSE || copied != 3 ||
        output[0] != 14 || output[1] != 38 || output[2] != 92 || fixture.reads != 1) {
        return 10;
    }
    LARGE_INTEGER offset{};
    if (!file->close() || fixture.closes != 1 || FAILED(stream->Seek(offset, STREAM_SEEK_SET, nullptr)) ||
        stream->Read(output, 1, &copied) != STG_E_ACCESSDENIED || copied != 0 || fixture.reads != 1 ||
        SUCCEEDED(data->GetData(&content, &medium)) || SUCCEEDED(data->GetData(&metadata, &medium))) {
        return 11;
    }
    return 0;
}

extern "C" int32_t linko_windows_copy_drag(const uint16_t* path, size_t units) {
    if (!path || units == 0 || units > 32'767) {
        return E_INVALIDARG;
    }
    // UTF-16 path followed by two NUL units.
    const auto size = sizeof(DROPFILES) + (units + 2) * sizeof(uint16_t);
    HGLOBAL allocation = GlobalAlloc(GMEM_MOVEABLE | GMEM_ZEROINIT, size);
    if (!allocation) {
        return E_OUTOFMEMORY;
    }
    auto* header = static_cast<DROPFILES*>(GlobalLock(allocation));
    if (!header) {
        GlobalFree(allocation);
        return E_OUTOFMEMORY;
    }
    header->pFiles = sizeof(DROPFILES);
    header->fWide = TRUE;
    std::memcpy(header + 1, path, units * sizeof(uint16_t));
    GlobalUnlock(allocation);
    return start_drag(allocation, CF_HDROP, 0);
}

extern "C" int32_t linko_windows_copy_drag_text(const uint16_t* text, size_t units, uint32_t millis) {
    if (units > 1024 * 1024) {
        return E_INVALIDARG;
    }
    return start_content_drag(text, units * sizeof(uint16_t), sizeof(uint16_t), CF_UNICODETEXT, millis);
}

extern "C" int32_t linko_windows_copy_drag_png(const uint8_t* png, size_t bytes, uint32_t millis) {
    if (bytes > 4 * 1024 * 1024) {
        return E_INVALIDARG;
    }
    const auto format = static_cast<CLIPFORMAT>(RegisterClipboardFormatW(L"PNG"));
    return start_content_drag(png, bytes, 0, format, millis);
}
