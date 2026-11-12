#pragma once

#include <algorithm>
#include <array>
#include <limits>
#include <utility>

namespace {
constexpr ULONG STREAM_PIECE = 64 * 1024;
using ReadDropFile = bool (*)(void*, uint64_t, uint8_t*, size_t);
using ReleaseDropFile = void (*)(void*);

class Lock {
public:
    explicit Lock(SRWLOCK& lock) : lock_(&lock) { AcquireSRWLockExclusive(lock_); }
    ~Lock() { ReleaseSRWLockExclusive(lock_); }
    Lock(const Lock&) = delete;
    Lock& operator=(const Lock&) = delete;

private:
    SRWLOCK* lock_;
};

class VirtualFile final {
public:
    VirtualFile(uint64_t size, ULONGLONG deadline, void* owner, ReadDropFile read,
                ReleaseDropFile release)
        : size(size), deadline_(deadline), owner_(owner), read_(read), release_(release) {}

    ULONG AddRef() { return static_cast<ULONG>(InterlockedIncrement(&refs_)); }
    ULONG Release() {
        const auto refs = InterlockedDecrement(&refs_);
        if (refs == 0) {
            delete this;
        }
        return static_cast<ULONG>(refs);
    }

    void admit_drop() {
        Lock lock(lock_);
        released_ = owner_ && !expired(deadline_);
    }

    bool offering() {
        Lock lock(lock_);
        return owner_ && !failed_ && !expired(deadline_);
    }

    bool open_stream() {
        Lock lock(lock_);
        if (!readable()) {
            return false;
        }
        if (size == 0) {
            uint8_t empty = 0;
            if (!read_(owner_, 0, &empty, 0) || expired(deadline_)) {
                failed_ = true;
                return false;
            }
        }
        opened_ = true;
        return true;
    }

    bool close() {
        Lock lock(lock_);
        if (owner_) {
            release_(std::exchange(owner_, nullptr));
        }
        return opened_ && !failed_ && prefix_ == size;
    }

    HRESULT read_piece(uint64_t offset, uint8_t* output, ULONG count) {
        Lock lock(lock_);
        if (!readable()) {
            return STG_E_ACCESSDENIED;
        }
        if (offset > size || count > size - offset || count > STREAM_PIECE) {
            return STG_E_INVALIDFUNCTION;
        }
        std::array<uint8_t, STREAM_PIECE> piece;
        if (!read_(owner_, offset, piece.data(), count) || expired(deadline_)) {
            failed_ = true;
            return STG_E_READFAULT;
        }
        std::memcpy(output, piece.data(), count);
        if (offset <= prefix_) {
            prefix_ = std::max(prefix_, offset + count);
        }
        return S_OK;
    }

    const uint64_t size;

private:
    ~VirtualFile() { close(); }
    bool readable() const { return owner_ && released_ && !failed_ && !expired(deadline_); }

    volatile LONG refs_ = 1;
    SRWLOCK lock_ = SRWLOCK_INIT;
    const ULONGLONG deadline_;
    void* owner_;
    const ReadDropFile read_;
    const ReleaseDropFile release_;
    uint64_t prefix_ = 0;
    bool released_ = false;
    bool opened_ = false;
    bool failed_ = false;
};

class VerifiedFileStream final : public IStream {
public:
    explicit VerifiedFileStream(VirtualFile* file, uint64_t position = 0)
        : file_(file), position_(position) {}

    HRESULT STDMETHODCALLTYPE QueryInterface(REFIID id, void** result) override {
        if (!result) {
            return E_POINTER;
        }
        *result = nullptr;
        if (id != __uuidof(IUnknown) && id != __uuidof(ISequentialStream) &&
            id != __uuidof(IStream)) {
            return E_NOINTERFACE;
        }
        *result = static_cast<IStream*>(this);
        AddRef();
        return S_OK;
    }

    ULONG STDMETHODCALLTYPE AddRef() override {
        return static_cast<ULONG>(InterlockedIncrement(&refs_));
    }

    ULONG STDMETHODCALLTYPE Release() override {
        const auto refs = InterlockedDecrement(&refs_);
        if (refs == 0) {
            delete this;
        }
        return static_cast<ULONG>(refs);
    }

    HRESULT STDMETHODCALLTYPE Read(void* output, ULONG count, ULONG* read) override {
        if (read) {
            *read = 0;
        }
        if (count == 0) {
            return S_OK;
        }
        if (!output) {
            return STG_E_INVALIDPOINTER;
        }
        Lock lock(lock_);
        ULONG copied = 0;
        while (copied < count && position_ < file_->size) {
            const auto piece = static_cast<ULONG>(std::min<uint64_t>(
                file_->size - position_, std::min(STREAM_PIECE, count - copied)));
            const auto status = file_->read_piece(position_, static_cast<uint8_t*>(output) + copied,
                                                  piece);
            if (FAILED(status)) {
                return status;
            }
            position_ += piece;
            copied += piece;
            if (read) {
                *read = copied;
            }
        }
        return copied == count ? S_OK : S_FALSE;
    }

    HRESULT STDMETHODCALLTYPE Seek(LARGE_INTEGER delta, DWORD origin,
                                   ULARGE_INTEGER* result) override {
        Lock lock(lock_);
        uint64_t base = 0;
        switch (origin) {
        case STREAM_SEEK_SET: break;
        case STREAM_SEEK_CUR: base = position_; break;
        case STREAM_SEEK_END: base = file_->size; break;
        default: return STG_E_INVALIDFUNCTION;
        }
        if (delta.QuadPart < 0) {
            const auto distance = static_cast<uint64_t>(-(delta.QuadPart + 1)) + 1;
            if (distance > base) {
                return STG_E_INVALIDFUNCTION;
            }
            position_ = base - distance;
        } else {
            const auto distance = static_cast<uint64_t>(delta.QuadPart);
            if (distance > std::numeric_limits<uint64_t>::max() - base) {
                return STG_E_INVALIDFUNCTION;
            }
            position_ = base + distance;
        }
        if (result) {
            result->QuadPart = position_;
        }
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE CopyTo(IStream* target, ULARGE_INTEGER count, ULARGE_INTEGER* read,
                                     ULARGE_INTEGER* written) override {
        if (read) {
            read->QuadPart = 0;
        }
        if (written) {
            written->QuadPart = 0;
        }
        if (!target) {
            return STG_E_INVALIDPOINTER;
        }
        std::array<uint8_t, STREAM_PIECE> piece;
        while (count.QuadPart != 0) {
            const auto requested = static_cast<ULONG>(std::min<uint64_t>(count.QuadPart, piece.size()));
            ULONG actual = 0;
            const auto status = Read(piece.data(), requested, &actual);
            if (FAILED(status)) {
                return status;
            }
            if (actual == 0) {
                return S_FALSE;
            }
            if (read) {
                read->QuadPart += actual;
            }
            ULONG saved = 0;
            const auto result = target->Write(piece.data(), actual, &saved);
            if (written) {
                written->QuadPart += saved;
            }
            if (FAILED(result) || saved != actual) {
                return FAILED(result) ? result : STG_E_WRITEFAULT;
            }
            count.QuadPart -= actual;
        }
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE Stat(STATSTG* result, DWORD) override {
        if (!result) {
            return STG_E_INVALIDPOINTER;
        }
        *result = {};
        result->type = STGTY_STREAM;
        result->cbSize.QuadPart = file_->size;
        result->grfMode = STGM_READ;
        return S_OK;
    }

    HRESULT STDMETHODCALLTYPE Clone(IStream** result) override {
        if (!result) {
            return STG_E_INVALIDPOINTER;
        }
        Lock lock(lock_);
        *result = new (std::nothrow) VerifiedFileStream(file_.Get(), position_);
        return *result ? S_OK : E_OUTOFMEMORY;
    }

    HRESULT STDMETHODCALLTYPE Write(const void*, ULONG, ULONG*) override { return STG_E_ACCESSDENIED; }
    HRESULT STDMETHODCALLTYPE SetSize(ULARGE_INTEGER) override { return STG_E_ACCESSDENIED; }
    HRESULT STDMETHODCALLTYPE Commit(DWORD) override { return STG_E_ACCESSDENIED; }
    HRESULT STDMETHODCALLTYPE Revert() override { return STG_E_INVALIDFUNCTION; }
    HRESULT STDMETHODCALLTYPE LockRegion(ULARGE_INTEGER, ULARGE_INTEGER, DWORD) override {
        return STG_E_INVALIDFUNCTION;
    }
    HRESULT STDMETHODCALLTYPE UnlockRegion(ULARGE_INTEGER, ULARGE_INTEGER, DWORD) override {
        return STG_E_INVALIDFUNCTION;
    }

private:
    volatile LONG refs_ = 1;
    ComPtr<VirtualFile> file_;
    SRWLOCK lock_ = SRWLOCK_INIT;
    uint64_t position_;
};
} // namespace
