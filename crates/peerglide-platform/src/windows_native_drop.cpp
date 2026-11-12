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

class DataObject final : public IDataObject {
public:
    DataObject(HGLOBAL data, CLIPFORMAT format, ULONGLONG deadline)
        : data_(data), format_(format), deadline_(deadline) {}

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
        if (expired(deadline_) || request->cfFormat != format_) {
            return DV_E_FORMATETC;
        }
        if (request->dwAspect != DVASPECT_CONTENT) {
            return DV_E_DVASPECT;
        }
        if (request->lindex != -1) {
            return DV_E_LINDEX;
        }
        return (request->tymed & TYMED_HGLOBAL) ? S_OK : DV_E_TYMED;
    }

    HRESULT STDMETHODCALLTYPE EnumFormatEtc(DWORD direction, IEnumFORMATETC** result) override {
        if (!result) {
            return E_POINTER;
        }
        *result = nullptr;
        if (direction != DATADIR_GET) {
            return E_NOTIMPL;
        }
        FORMATETC format{format_, nullptr, DVASPECT_CONTENT, -1, TYMED_HGLOBAL};
        return SHCreateStdEnumFmtEtc(1, &format, result);
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
    volatile LONG refs_ = 1;
    HGLOBAL data_;
    CLIPFORMAT format_;
    ULONGLONG deadline_;
};

class DropSource final : public IDropSource {
public:
    explicit DropSource(ULONGLONG deadline) : deadline_(deadline) {}

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
            return DRAGDROP_S_CANCEL;
        }
        return (keys & MK_LBUTTON) ? S_OK : DRAGDROP_S_DROP;
    }

    HRESULT STDMETHODCALLTYPE GiveFeedback(DWORD) override {
        return DRAGDROP_S_USEDEFAULTCURSORS;
    }

private:
    volatile LONG refs_ = 1;
    ULONGLONG deadline_;
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
int32_t start_drag(HGLOBAL allocation, CLIPFORMAT format, ULONGLONG deadline) {
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
    data.Attach(new (std::nothrow) DataObject(allocation, format, deadline));
    if (!data) {
        GlobalFree(allocation);
        return E_OUTOFMEMORY;
    }
    ComPtr<IDropSource> source;
    source.Attach(new (std::nothrow) DropSource(deadline));
    if (!source) {
        return E_OUTOFMEMORY;
    }

    DWORD effect = DROPEFFECT_NONE;
    const auto result = DoDragDrop(data.Get(), source.Get(), DROPEFFECT_COPY, &effect);
    if (result == DRAGDROP_S_CANCEL) {
        return 0;
    }
    if (result == DRAGDROP_S_DROP && effect == DROPEFFECT_COPY && !expired(deadline)) {
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
} // namespace

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
