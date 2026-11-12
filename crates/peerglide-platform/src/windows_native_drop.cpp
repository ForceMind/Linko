// A local, copy-only OLE drag from a prepared regular file.
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
class DataObject final : public IDataObject {
public:
    explicit DataObject(HGLOBAL data) : data_(data) {}

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
        if (request->cfFormat != CF_HDROP) {
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
        FORMATETC format{CF_HDROP, nullptr, DVASPECT_CONTENT, -1, TYMED_HGLOBAL};
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
};

class DropSource final : public IDropSource {
public:
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
        if (escape) {
            return DRAGDROP_S_CANCEL;
        }
        return (keys & MK_LBUTTON) ? S_OK : DRAGDROP_S_DROP;
    }

    HRESULT STDMETHODCALLTYPE GiveFeedback(DWORD) override {
        return DRAGDROP_S_USEDEFAULTCURSORS;
    }

private:
    volatile LONG refs_ = 1;
};

struct OleScope {
    HRESULT result = OleInitialize(nullptr);
    ~OleScope() {
        if (SUCCEEDED(result)) {
            OleUninitialize();
        }
    }
};
} // namespace

extern "C" int32_t linko_windows_copy_drag(const uint16_t* path, size_t units) {
    if (!path || units == 0 || units > 32'767) {
        return E_INVALIDARG;
    }
    if (!(GetAsyncKeyState(VK_LBUTTON) & 0x8000)) {
        return 0;
    }
    OleScope ole;
    if (FAILED(ole.result)) {
        return ole.result;
    }
    // UTF-16 path followed by two NUL units. Allocation happens before the
    // native drag loop and is never owned by an input capture callback.
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
    ComPtr<IDataObject> data;
    data.Attach(new (std::nothrow) DataObject(allocation));
    if (!data) {
        GlobalFree(allocation);
        return E_OUTOFMEMORY;
    }
    ComPtr<IDropSource> source;
    source.Attach(new (std::nothrow) DropSource());
    if (!source) {
        return E_OUTOFMEMORY;
    }

    DWORD effect = DROPEFFECT_NONE;
    const auto result = DoDragDrop(data.Get(), source.Get(), DROPEFFECT_COPY, &effect);
    if (result == DRAGDROP_S_CANCEL) {
        return 0;
    }
    if (result == DRAGDROP_S_DROP && effect == DROPEFFECT_COPY) {
        return 1;
    }
    return FAILED(result) ? result : 2;
}
