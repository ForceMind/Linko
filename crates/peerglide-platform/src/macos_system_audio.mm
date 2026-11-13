// Audio-only adapters for separately authorized system and microphone sources.
#import <Foundation/Foundation.h>
#import <AVFoundation/AVFoundation.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreAudio/CoreAudioTypes.h>

#include <algorithm>
#include <array>
#include <atomic>
#include <cmath>
#include <chrono>
#include <cstddef>
#include <cstdint>
#include <cstring>
#include <memory>
#include <new>

namespace {
constexpr size_t queue_capacity = 9; // Eight usable sample references.
constexpr uint32_t channels = 2;
constexpr uint32_t max_frames = 24'000;
constexpr int32_t invalid = -1;
constexpr int32_t unavailable = -2;
constexpr int32_t timed_out = -3;
constexpr int32_t stopped = -4;

struct Ring {
    std::array<CMSampleBufferRef, queue_capacity> slots{};
    std::atomic<size_t> read{0};
    std::atomic<size_t> write{0};
    std::atomic<bool> accepting{false};
    std::atomic<bool> ended{false};

    void publish(CMSampleBufferRef sample) {
        if (!accepting.load(std::memory_order_acquire)) {
            return;
        }
        const auto tail = write.load(std::memory_order_relaxed);
        const auto next = (tail + 1) % queue_capacity;
        if (next == read.load(std::memory_order_acquire)) {
            return;
        }
        CFRetain(sample);
        slots[tail] = sample;
        write.store(next, std::memory_order_release);
    }

    CMSampleBufferRef take() {
        const auto head = read.load(std::memory_order_relaxed);
        if (head == write.load(std::memory_order_acquire)) {
            return nullptr;
        }
        const auto sample = slots[head];
        read.store((head + 1) % queue_capacity, std::memory_order_release);
        return sample;
    }

    ~Ring() {
        while (auto sample = take()) {
            CFRelease(sample);
        }
    }
};

int16_t from_float(float sample) {
    if (!std::isfinite(sample)) {
        return 0;
    }
    if (sample <= -1.0f) {
        return INT16_MIN;
    }
    return static_cast<int16_t>(std::round(std::min(sample, 1.0f) * INT16_MAX));
}

bool wait_for(dispatch_semaphore_t ready) {
    return dispatch_semaphore_wait(ready, dispatch_time(DISPATCH_TIME_NOW, 10 * NSEC_PER_SEC)) == 0;
}
} // namespace

API_AVAILABLE(macos(13.0))
@interface LinkoAudioOutput : NSObject <SCStreamOutput, SCStreamDelegate> {
@public
    std::shared_ptr<Ring> ring;
}
@end

@implementation LinkoAudioOutput
- (void)stream:(SCStream*)stream didOutputSampleBuffer:(CMSampleBufferRef)sample
        ofType:(SCStreamOutputType)type API_AVAILABLE(macos(13.0)) {
    (void)stream;
    if (sample && type == SCStreamOutputTypeAudio) {
        // A single serial callback queue only retains and publishes references.
        ring->publish(sample);
    }
}

- (void)stream:(SCStream*)stream didStopWithError:(NSError*)error {
    (void)stream;
    (void)error;
    ring->accepting.store(false, std::memory_order_release);
    ring->ended.store(true, std::memory_order_release);
}
@end

@interface LinkoMicrophoneOutput : NSObject <AVCaptureAudioDataOutputSampleBufferDelegate> {
@public
    std::shared_ptr<Ring> ring;
}
@end

@implementation LinkoMicrophoneOutput
- (void)captureOutput:(AVCaptureOutput*)output didOutputSampleBuffer:(CMSampleBufferRef)sample
       fromConnection:(AVCaptureConnection*)connection {
    (void)output;
    (void)connection;
    if (sample) {
        ring->publish(sample);
    }
}
@end

namespace {
struct Capture {
    SCStream* stream = nil;
    LinkoAudioOutput* output = nil;
    AVCaptureSession* microphone = nil;
    AVCaptureAudioDataOutput* microphone_output = nil;
    LinkoMicrophoneOutput* microphone_delegate = nil;
    dispatch_queue_t microphone_control = nil;
    id microphone_error = nil;
    id microphone_disconnected = nil;
    std::chrono::steady_clock::time_point last_microphone_check{};
    std::shared_ptr<Ring> ring = std::make_shared<Ring>();
    std::array<int16_t, max_frames * channels> packet{};
    uint32_t count = 0;
    uint32_t consumed = 0;
    bool requested_stop = false;

    int32_t stop(bool wait) {
        ring->accepting.store(false, std::memory_order_release);
        if (requested_stop || (!stream && !microphone)) {
            return 0;
        }
        requested_stop = true;
        if (microphone) {
            NSNotificationCenter* center = [NSNotificationCenter defaultCenter];
            if (microphone_error) {
                [center removeObserver:microphone_error];
            }
            if (microphone_disconnected) {
                [center removeObserver:microphone_disconnected];
            }
            AVCaptureSession* live_session = microphone;
            AVCaptureAudioDataOutput* live_output = microphone_output;
            LinkoMicrophoneOutput* live_delegate = microphone_delegate;
            dispatch_semaphore_t ready = dispatch_semaphore_create(0);
            __block int32_t result = unavailable;
            dispatch_async(microphone_control, ^{
                [live_output setSampleBufferDelegate:nil queue:nil];
                [live_session stopRunning];
                result = !live_delegate || live_session.isRunning ? unavailable : 0;
                dispatch_semaphore_signal(ready);
            });
            return !wait ? 0 : wait_for(ready) ? result : timed_out;
        }
        // Keep both Objective-C owners alive even if the asynchronous stop
        // reply arrives after the caller's deadline.
        SCStream* live_stream = stream;
        LinkoAudioOutput* live_output = output;
        dispatch_semaphore_t ready = dispatch_semaphore_create(0);
        __block int32_t result = unavailable;
        [live_stream stopCaptureWithCompletionHandler:^(NSError* error) {
            result = (error || !live_stream || !live_output) ? unavailable : 0;
            dispatch_semaphore_signal(ready);
        }];
        return !wait ? 0 : wait_for(ready) ? result : timed_out;
    }

    ~Capture() {
        stop(false);
    }
};

using AllowMicrophone = uint8_t (*)(const void*);

bool wait_for_microphone(dispatch_semaphore_t ready, AllowMicrophone allow, const void* token) {
    for (int attempt = 0; attempt < 100 && allow(token); ++attempt) {
        if (dispatch_semaphore_wait(ready, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_MSEC * 100)) == 0) {
            return allow(token) != 0;
        }
    }
    return false;
}

bool microphone_authorized(AllowMicrophone allow, const void* token) {
    id purpose = [[NSBundle mainBundle] objectForInfoDictionaryKey:@"NSMicrophoneUsageDescription"];
    if (!allow(token) || ![purpose isKindOfClass:[NSString class]] || [(NSString*)purpose length] == 0) {
        return false;
    }
    const auto status = [AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio];
    if (status != AVAuthorizationStatusNotDetermined) {
        return status == AVAuthorizationStatusAuthorized;
    }
    dispatch_semaphore_t ready = dispatch_semaphore_create(0);
    __block BOOL granted = NO;
    [AVCaptureDevice requestAccessForMediaType:AVMediaTypeAudio completionHandler:^(BOOL allowed) {
        granted = allowed;
        dispatch_semaphore_signal(ready);
    }];
    return wait_for_microphone(ready, allow, token) && granted;
}

int32_t read_packet(Capture* capture) {
    CMSampleBufferRef sample = capture->ring->take();
    if (!sample) {
        return 0;
    }
    const auto description = CMSampleBufferGetFormatDescription(sample);
    const auto* format = description ? CMAudioFormatDescriptionGetStreamBasicDescription(description) : nullptr;
    const auto frames = CMSampleBufferGetNumSamples(sample);
    if (!format || format->mFormatID != kAudioFormatLinearPCM || format->mSampleRate != 48'000
        || format->mChannelsPerFrame != channels || frames <= 0 || frames > max_frames) {
        CFRelease(sample);
        return invalid;
    }
    const bool floating = (format->mFormatFlags & kAudioFormatFlagIsFloat) != 0;
    const bool planar = (format->mFormatFlags & kAudioFormatFlagIsNonInterleaved) != 0;
    const bool signed_pcm = (format->mFormatFlags & kAudioFormatFlagIsSignedInteger) != 0;
    const size_t width = floating ? sizeof(float) : sizeof(int16_t);
    if ((format->mFormatFlags & kAudioFormatFlagIsBigEndian)
        || (!floating && !signed_pcm)
        || format->mBitsPerChannel != width * 8
        || format->mBytesPerFrame != width * (planar ? 1 : channels)) {
        CFRelease(sample);
        return invalid;
    }

    // An AudioBufferList is a count followed by a flexible AudioBuffer array.
    struct TwoBuffers {
        UInt32 count;
        AudioBuffer buffers[channels];
    } storage{};
    static_assert(offsetof(TwoBuffers, buffers) == offsetof(AudioBufferList, mBuffers));
    CMBlockBufferRef retained = nullptr;
    size_t required = 0;
    const OSStatus result = CMSampleBufferGetAudioBufferListWithRetainedBlockBuffer(
        sample, &required, reinterpret_cast<AudioBufferList*>(&storage), sizeof(storage),
        kCFAllocatorDefault, kCFAllocatorDefault, 0, &retained
    );
    int32_t status = invalid;
    if (result == noErr && retained && required <= sizeof(storage)
        && storage.count == (planar ? channels : 1)) {
        bool valid = true;
        for (uint32_t index = 0; index < storage.count; ++index) {
            const auto& buffer = storage.buffers[index];
            const auto in_channels = planar ? 1 : channels;
            valid &= buffer.mData && buffer.mNumberChannels == in_channels
                && buffer.mDataByteSize >= static_cast<size_t>(frames) * width * in_channels;
        }
        if (valid) {
            for (uint32_t frame = 0; frame < static_cast<uint32_t>(frames); ++frame) {
                for (uint32_t channel = 0; channel < channels; ++channel) {
                    const auto* data = static_cast<const uint8_t*>(
                        storage.buffers[planar ? channel : 0].mData
                    );
                    const auto index = planar ? frame : frame * channels + channel;
                    auto& output = capture->packet[frame * channels + channel];
                    if (floating) {
                        float value;
                        std::memcpy(&value, data + index * width, width);
                        output = from_float(value);
                    } else {
                        std::memcpy(&output, data + index * width, width);
                    }
                }
            }
            capture->count = static_cast<uint32_t>(frames);
            capture->consumed = 0;
            status = 0;
        }
    }
    if (retained) {
        CFRelease(retained);
    }
    CFRelease(sample);
    return status;
}
} // namespace

extern "C" int32_t linko_mac_audio_open(void** owner) {
    if (!owner) {
        return invalid;
    }
    *owner = nullptr;
    @autoreleasepool {
        if (@available(macOS 13.0, *)) {
            std::unique_ptr<Capture> capture;
            try {
                capture = std::make_unique<Capture>();
            } catch (const std::bad_alloc&) {
                return unavailable;
            }
            if (!capture) {
                return unavailable;
            }
            dispatch_semaphore_t ready = dispatch_semaphore_create(0);
            __block SCShareableContent* content = nil;
            [SCShareableContent getShareableContentExcludingDesktopWindows:YES
                onScreenWindowsOnly:YES completionHandler:^(SCShareableContent* value, NSError* error) {
                    if (!error) {
                        content = value;
                    }
                    dispatch_semaphore_signal(ready);
                }];
            if (!wait_for(ready)) {
                return timed_out;
            }
            SCDisplay* display = content.displays.firstObject;
            if (!display) {
                return unavailable;
            }
            SCContentFilter* filter = [[SCContentFilter alloc] initWithDisplay:display excludingWindows:@[]];
            SCStreamConfiguration* config = [SCStreamConfiguration new];
            config.capturesAudio = YES;
            config.excludesCurrentProcessAudio = YES;
            config.sampleRate = 48'000;
            config.channelCount = channels;
            config.width = 64;
            config.height = 64;
            config.minimumFrameInterval = CMTimeMake(1, 1);
            capture->output = [LinkoAudioOutput new];
            capture->output->ring = capture->ring;
            capture->stream = [[SCStream alloc] initWithFilter:filter configuration:config
                                                     delegate:capture->output];
            dispatch_queue_t audio_queue = dispatch_queue_create("linko.audio.samples", DISPATCH_QUEUE_SERIAL);
            NSError* add_error = nil;
            if (![capture->stream addStreamOutput:capture->output type:SCStreamOutputTypeAudio
                                  sampleHandlerQueue:audio_queue error:&add_error]) {
                return unavailable;
            }
            dispatch_semaphore_t started = dispatch_semaphore_create(0);
            __block int32_t result = unavailable;
            capture->ring->accepting.store(true, std::memory_order_release);
            SCStream* live_stream = capture->stream;
            LinkoAudioOutput* live_output = capture->output;
            const auto lifetime = capture->ring;
            [live_stream startCaptureWithCompletionHandler:^(NSError* error) {
                if (!error && !lifetime->accepting.load(std::memory_order_acquire)) {
                    [live_stream stopCaptureWithCompletionHandler:^(NSError* stop_error) {
                        if (stop_error && live_output) {
                            lifetime->ended.store(true, std::memory_order_release);
                        }
                    }];
                }
                result = error ? unavailable : 0;
                dispatch_semaphore_signal(started);
            }];
            if (!wait_for(started)) {
                return timed_out;
            }
            if (result != 0) {
                return result;
            }
            *owner = capture.release();
            return 0;
        }
        return unavailable;
    }
}

extern "C" int32_t linko_mac_mic_open(void** owner, AllowMicrophone allow, const void* token) {
    if (!owner || !allow || !token) {
        return invalid;
    }
    *owner = nullptr;
    @autoreleasepool {
        if (!microphone_authorized(allow, token)) {
            return unavailable;
        }
        AVCaptureDevice* device = [AVCaptureDevice defaultDeviceWithMediaType:AVMediaTypeAudio];
        if (!device) {
            return unavailable;
        }
        AVCaptureDeviceInput* input = [AVCaptureDeviceInput deviceInputWithDevice:device error:nullptr];
        if (!input) {
            return unavailable;
        }
        std::unique_ptr<Capture> capture;
        try {
            capture = std::make_unique<Capture>();
        } catch (const std::bad_alloc&) {
            return unavailable;
        }
        capture->microphone_control = dispatch_queue_create("linko.microphone.lifecycle", DISPATCH_QUEUE_SERIAL);
        AVCaptureSession* session = [AVCaptureSession new];
        AVCaptureAudioDataOutput* output = [AVCaptureAudioDataOutput new];
        LinkoMicrophoneOutput* delegate = [LinkoMicrophoneOutput new];
        delegate->ring = capture->ring;
        output.audioSettings = @{
            AVFormatIDKey: @(kAudioFormatLinearPCM),
            AVSampleRateKey: @48000,
            AVNumberOfChannelsKey: @2,
            AVLinearPCMBitDepthKey: @16,
            AVLinearPCMIsFloatKey: @NO,
            AVLinearPCMIsBigEndianKey: @NO,
            AVLinearPCMIsNonInterleaved: @NO,
        };
        [output setSampleBufferDelegate:delegate queue:dispatch_queue_create("linko.microphone.samples", DISPATCH_QUEUE_SERIAL)];
        [session beginConfiguration];
        if (![session canAddInput:input]) {
            [session commitConfiguration];
            return unavailable;
        }
        [session addInput:input];
        if (![session canAddOutput:output]) {
            [session commitConfiguration];
            return unavailable;
        }
        [session addOutput:output];
        [session commitConfiguration];
        capture->microphone = session;
        capture->microphone_output = output;
        capture->microphone_delegate = delegate;
        const auto ring = capture->ring;
        void (^stop_on_failure)(NSNotification*) = ^(NSNotification* notice) {
            (void)notice;
            ring->accepting.store(false, std::memory_order_release);
            ring->ended.store(true, std::memory_order_release);
        };
        NSNotificationCenter* center = [NSNotificationCenter defaultCenter];
        capture->microphone_error = [center addObserverForName:AVCaptureSessionRuntimeErrorNotification
            object:session queue:nil usingBlock:stop_on_failure];
        capture->microphone_disconnected = [center addObserverForName:AVCaptureDeviceWasDisconnectedNotification
            object:device queue:nil usingBlock:stop_on_failure];
        if (!allow(token)) {
            return stopped;
        }
        ring->accepting.store(true, std::memory_order_release);
        dispatch_semaphore_t ready = dispatch_semaphore_create(0);
        __block BOOL started = NO;
        dispatch_async(capture->microphone_control, ^{
            if (ring->accepting.load(std::memory_order_acquire)) {
                [session startRunning];
                started = session.isRunning && ring->accepting.load(std::memory_order_acquire);
                if (!started && session.isRunning) {
                    [session stopRunning];
                }
            }
            dispatch_semaphore_signal(ready);
        });
        if (!wait_for_microphone(ready, allow, token)) {
            return timed_out;
        }
        if (!started) {
            return unavailable;
        }
        *owner = capture.release();
        return 0;
    }
}

extern "C" int32_t linko_mac_audio_read(void* owner, int16_t* output, uint32_t capacity,
                                       uint32_t* written) {
    if (!owner || !output || !written || capacity == 0) {
        return invalid;
    }
    auto* capture = static_cast<Capture*>(owner);
    *written = 0;
    if (capture->ring->ended.load(std::memory_order_acquire) || capture->requested_stop) {
        return stopped;
    }
    if (capture->microphone) {
        const auto now = std::chrono::steady_clock::now();
        if (now - capture->last_microphone_check >= std::chrono::milliseconds(500)) {
            capture->last_microphone_check = now;
            if ([AVCaptureDevice authorizationStatusForMediaType:AVMediaTypeAudio] != AVAuthorizationStatusAuthorized) {
                capture->ring->accepting.store(false, std::memory_order_release);
                return stopped;
            }
        }
    }
    if (capture->consumed == capture->count) {
        const auto result = read_packet(capture);
        if (result != 0 || capture->consumed == capture->count) {
            return result;
        }
    }
    const uint32_t frames = std::min(capacity, capture->count - capture->consumed);
    std::copy_n(capture->packet.data() + capture->consumed * channels, frames * channels, output);
    capture->consumed += frames;
    *written = frames;
    return 0;
}

extern "C" int32_t linko_mac_audio_stop(void* owner) {
    @autoreleasepool {
        return owner ? static_cast<Capture*>(owner)->stop(true) : invalid;
    }
}

extern "C" void linko_mac_audio_drop(void* owner) {
    @autoreleasepool {
        delete static_cast<Capture*>(owner);
    }
}
