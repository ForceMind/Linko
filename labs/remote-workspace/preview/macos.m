#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreImage/CoreImage.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <QuartzCore/QuartzCore.h>
#include <math.h>
#include <string.h>

#ifdef LINKO_RW04_CODEC_PREVIEW
#include "h264_probe.h"
#endif
#ifdef LINKO_RW04_NETWORK_PREVIEW
#include <libproc.h>
#include <mach/mach.h>
#include <malloc/malloc.h>
#include <unistd.h>
#include "loopback_preview.h"

@interface RW04NetworkPreview : NSObject
- (BOOL)submit:(CVPixelBufferRef)pixels;
- (CVPixelBufferRef)copyFrameReceivedAt:(NSTimeInterval *)receivedAt CF_RETURNS_RETAINED;
- (void)revoke;
- (BOOL)wasClosed;
@end

@implementation RW04NetworkPreview {
    LinkoLoopbackPreview *_session;
}

- (instancetype)init {
    self = [super init];
    if (self) {
        _session = linko_rw04_loopback_open();
        if (!_session) {
            return nil;
        }
    }
    return self;
}

- (void)dealloc {
    linko_rw04_loopback_close(_session);
}

- (BOOL)submit:(CVPixelBufferRef)pixels {
    return linko_rw04_loopback_submit(_session, pixels);
}

- (CVPixelBufferRef)copyFrameReceivedAt:(NSTimeInterval *)receivedAt {
    return linko_rw04_loopback_take(_session, receivedAt);
}

- (void)revoke {
    linko_rw04_loopback_revoke(_session);
}

- (BOOL)wasClosed {
    return linko_rw04_loopback_was_closed(_session);
}
@end
#endif
#if defined(LINKO_RW04_CODEC_PREVIEW) || defined(LINKO_RW04_NETWORK_PREVIEW)
#define LINKO_RW04_COLOR_PREVIEW 1
#endif

static const size_t MaxWidth = 1920;
static const size_t MaxHeight = 1080;
static const NSTimeInterval FrameLifetime = 2.0;
#ifdef LINKO_RW04_NETWORK_PREVIEW
static const NSUInteger InputPoolLimit = 6;
#endif

static CIContext *makeRenderingContext(void) {
    return [CIContext contextWithOptions:@{kCIContextCacheIntermediates: @NO}];
}

static NSTimeInterval uptime(void) {
#ifdef LINKO_RW04_NETWORK_PREVIEW
    return linko_rw04_loopback_now();
#else
    return NSProcessInfo.processInfo.systemUptime;
#endif
}

static CGRect visiblePixels(id value, CGSize frameSize) {
    CGRect rect = CGRectNull;
    if ([value isKindOfClass:NSDictionary.class]) {
        if (!CGRectMakeWithDictionaryRepresentation((__bridge CFDictionaryRef)value, &rect)) {
            return CGRectNull;
        }
    } else if ([value isKindOfClass:NSValue.class] && strcmp([value objCType], @encode(CGRect)) == 0) {
        [value getValue:&rect size:sizeof(rect)];
    }
    if (!isfinite(rect.origin.x) || !isfinite(rect.origin.y) ||
        !isfinite(rect.size.width) || !isfinite(rect.size.height) ||
        rect.size.width <= 0 || rect.size.height <= 0) {
        return CGRectNull;
    }
    return CGRectIntersection(CGRectIntegral(rect), CGRectMake(0, 0, frameSize.width, frameSize.height));
}

static BOOL sourceExists(CGWindowID window, pid_t owner) {
    NSArray<NSDictionary *> *info = CFBridgingRelease(
        CGWindowListCopyWindowInfo(kCGWindowListOptionIncludingWindow, window));
    for (NSDictionary *entry in info) {
        if ([entry[(__bridge NSString *)kCGWindowNumber] unsignedIntValue] == window &&
            [entry[(__bridge NSString *)kCGWindowOwnerPID] intValue] == owner) {
            return YES;
        }
    }
    return NO;
}

@interface CaptureOutput : NSObject <SCStreamOutput, SCStreamDelegate>
@property(nonatomic, readonly) BOOL ended;
- (instancetype)initWithRenderingContext:(CIContext *)context;
#ifdef LINKO_RW04_NETWORK_PREVIEW
@property(strong) RW04NetworkPreview *networkPreview;
- (BOOL)usesLoopback;
- (BOOL)consumeDecodedFrameWithoutRendering;
- (CVPixelBufferRef)copyInputWithWidth:(size_t)width height:(size_t)height CF_RETURNS_RETAINED;
- (CVPixelBufferRef)copyCroppedFrame:(CIImage *)frame pixels:(CGRect)pixels;
#endif
- (void)discard;
- (void)publish:(NSImage *)image;
- (void)publish:(NSImage *)image receivedAt:(NSTimeInterval)receivedAt;
- (void)stillCurrent;
- (NSImage *)currentAt:(NSTimeInterval)now;
@end

@implementation CaptureOutput {
    NSLock *_lock;
    CIContext *_context;
    NSImage *_image;
    NSTimeInterval _lastFrame;
    BOOL _ended;
#ifdef LINKO_RW04_NETWORK_PREVIEW
    CVPixelBufferPoolRef _inputPool;
    size_t _inputWidth;
    size_t _inputHeight;
    NSDictionary *_inputAllocationLimits;
#endif
#ifdef LINKO_RW04_CODEC_PREVIEW
    LinkoH264Session *_codec;
    size_t _codecWidth;
    size_t _codecHeight;
    uint32_t _codecTimestamp;
#endif
}

#ifdef LINKO_RW04_NETWORK_PREVIEW
@synthesize networkPreview = _networkPreview;

- (BOOL)usesLoopback {
    return YES;
}

- (BOOL)consumeDecodedFrameWithoutRendering {
    CVPixelBufferRef pixels = [self.networkPreview copyFrameReceivedAt:NULL];
    if (!pixels) {
        return NO;
    }
    BOOL valid = CVPixelBufferGetWidth(pixels) >= 64 && CVPixelBufferGetWidth(pixels) <= MaxWidth &&
                 CVPixelBufferGetHeight(pixels) >= 64 && CVPixelBufferGetHeight(pixels) <= MaxHeight;
    CVPixelBufferRelease(pixels);
    return valid;
}
#endif

- (instancetype)initWithRenderingContext:(CIContext *)context {
    self = [super init];
    if (self) {
        _lock = [NSLock new];
        _context = context;
#ifdef LINKO_RW04_NETWORK_PREVIEW
        _inputAllocationLimits = @{
            (__bridge NSString *)kCVPixelBufferPoolAllocationThresholdKey: @(InputPoolLimit)
        };
#endif
    }
    return self;
}

- (void)discard {
    [_lock lock];
    _ended = YES;
    _image = nil;
#ifdef LINKO_RW04_NETWORK_PREVIEW
    if (_inputPool) {
        CFRelease(_inputPool);
        _inputPool = NULL;
    }
#endif
    [_lock unlock];
#ifdef LINKO_RW04_NETWORK_PREVIEW
    RW04NetworkPreview *network = self.networkPreview;
    [network revoke];
    self.networkPreview = nil;
#endif
}

- (BOOL)ended {
    [_lock lock];
    BOOL value = _ended;
    [_lock unlock];
    return value;
}

- (void)publish:(NSImage *)image {
    [self publish:image receivedAt:uptime()];
}

- (void)publish:(NSImage *)image receivedAt:(NSTimeInterval)receivedAt {
    [_lock lock];
    if (!_ended) {
        _image = image;
        _lastFrame = receivedAt;
    }
    [_lock unlock];
}

- (void)stillCurrent {
    [_lock lock];
    if (uptime() - _lastFrame > FrameLifetime) {
        _image = nil;
    }
    if (_image && !_ended) {
        _lastFrame = uptime();
    }
    [_lock unlock];
}

- (NSImage *)currentAt:(NSTimeInterval)now {
#ifdef LINKO_RW04_NETWORK_PREVIEW
    NSTimeInterval receipt = 0;
    CVPixelBufferRef pixels = [self.networkPreview copyFrameReceivedAt:&receipt];
    if (pixels) {
        CIImage *frame = [CIImage imageWithCVPixelBuffer:pixels];
        CGImageRef result = [_context createCGImage:frame fromRect:frame.extent];
        if (result) {
            [self publish:[[NSImage alloc] initWithCGImage:result size:NSZeroSize] receivedAt:receipt];
            CGImageRelease(result);
        }
        CVPixelBufferRelease(pixels);
    }
    // Include time spent converting a native frame to an AppKit image.
    now = fmax(now, uptime());
#endif
    [_lock lock];
    if (now - _lastFrame >= FrameLifetime) {
        _image = nil;
    }
    NSImage *image = _image;
    [_lock unlock];
    return image;
}

#ifdef LINKO_RW04_NETWORK_PREVIEW
- (void)dealloc {
    if (_inputPool) {
        CFRelease(_inputPool);
    }
}

- (CVPixelBufferRef)copyInputWithWidth:(size_t)width height:(size_t)height {
    [_lock lock];
    if (_ended) {
        [_lock unlock];
        return NULL;
    }
    if (!_inputPool || _inputWidth != width || _inputHeight != height) {
        NSDictionary *attributes = @{
            (__bridge NSString *)kCVPixelBufferWidthKey: @(width),
            (__bridge NSString *)kCVPixelBufferHeightKey: @(height),
            (__bridge NSString *)kCVPixelBufferPixelFormatTypeKey: @(kCVPixelFormatType_32BGRA),
            (__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey: @{}
        };
        CVPixelBufferPoolRef replacement = NULL;
        if (CVPixelBufferPoolCreate(kCFAllocatorDefault, NULL,
                                    (__bridge CFDictionaryRef)attributes, &replacement) != kCVReturnSuccess) {
            [_lock unlock];
            return NULL;
        }
        if (_inputPool) {
            CFRelease(_inputPool);
        }
        _inputPool = replacement;
        _inputWidth = width;
        _inputHeight = height;
    }
    CVPixelBufferRef input = NULL;
    CVReturn status = CVPixelBufferPoolCreatePixelBufferWithAuxAttributes(
        kCFAllocatorDefault, _inputPool, (__bridge CFDictionaryRef)_inputAllocationLimits, &input);
    [_lock unlock];
    return status == kCVReturnSuccess ? input : NULL;
}
#endif

#ifdef LINKO_RW04_COLOR_PREVIEW
- (CVPixelBufferRef)copyCroppedFrame:(CIImage *)frame pixels:(CGRect)pixels {
    const size_t width = ((size_t)CGRectGetWidth(pixels)) & ~(size_t)1;
    const size_t height = ((size_t)CGRectGetHeight(pixels)) & ~(size_t)1;
    if (width < 64 || height < 64 || width > MaxWidth || height > MaxHeight) {
        return NULL;
    }
#ifdef LINKO_RW04_NETWORK_PREVIEW
    CVPixelBufferRef input = [self copyInputWithWidth:width height:height];
    if (!input) {
        return NULL;
    }
#else
    CVPixelBufferRef input = NULL;
    NSDictionary *attributes = @{(__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey: @{}};
    if (CVPixelBufferCreate(NULL, width, height, kCVPixelFormatType_32BGRA,
                            (__bridge CFDictionaryRef)attributes, &input) != kCVReturnSuccess) {
        return NULL;
    }
#endif
    CGRect crop = CGRectMake(pixels.origin.x, pixels.origin.y, width, height);
    CGAffineTransform origin = CGAffineTransformMakeTranslation(-crop.origin.x, -crop.origin.y);
    CIImage *cropped = [[frame imageByCroppingToRect:crop] imageByApplyingTransform:origin];
    [_context render:cropped toCVPixelBuffer:input];
    return input;
}
#endif

#ifdef LINKO_RW04_CODEC_PREVIEW
- (void)dealloc {
    linko_rw04_h264_close(&_codec);
}

// The serial sample queue recreates the codec when the cropped size changes.
- (CVPixelBufferRef)copyDecodedFrame:(CIImage *)frame pixels:(CGRect)pixels {
    CVPixelBufferRef input = [self copyCroppedFrame:frame pixels:pixels];
    if (!input) {
        return NULL;
    }
    const size_t width = CVPixelBufferGetWidth(input);
    const size_t height = CVPixelBufferGetHeight(input);
    if (!_codec || _codecWidth != width || _codecHeight != height) {
        LinkoH264Request request = {LINKO_H264_PROBE_V1, (uint32_t)width, (uint32_t)height, 0};
        if (linko_rw04_h264_close(&_codec) != LINKO_H264_OK ||
            linko_rw04_h264_open(&request, &_codec) != LINKO_H264_OK) {
            CVPixelBufferRelease(input);
            return NULL;
        }
        _codecWidth = width;
        _codecHeight = height;
        _codecTimestamp = 0;
    }
    CVPixelBufferRef decoded = NULL;
    int status = linko_rw04_h264_submit_pixels(_codec, _codecTimestamp, input, &decoded);
    CVPixelBufferRelease(input);
    if (status != LINKO_H264_OK) {
        linko_rw04_h264_close(&_codec);
        return NULL;
    }
    _codecTimestamp += 3000;
    return decoded;
}
#endif

- (void)stream:(SCStream *)stream didStopWithError:(NSError *)error {
    [self discard];
}

- (void)handleScreenSample:(CMSampleBufferRef)sample {
    if (self.ended) {
        return;
    }
    NSArray<NSDictionary *> *values = (__bridge NSArray *)CMSampleBufferGetSampleAttachmentsArray(sample, false);
    NSNumber *status = values.firstObject[SCStreamFrameInfoStatus];
    if (status && status.integerValue == SCFrameStatusStopped) {
        [self discard];
        return;
    }
    if (status && status.integerValue == SCFrameStatusIdle) {
#ifdef LINKO_RW04_NETWORK_PREVIEW
        if (![self usesLoopback]) {
            [self stillCurrent];
        }
#else
        [self stillCurrent];
#endif
        return;
    }
    CVPixelBufferRef buffer = CMSampleBufferGetImageBuffer(sample);
    if (!status || (status.integerValue != SCFrameStatusComplete && status.integerValue != SCFrameStatusStarted) ||
        !buffer || CVPixelBufferGetWidth(buffer) > MaxWidth || CVPixelBufferGetHeight(buffer) > MaxHeight) {
        [self publish:nil];
        return;
    }

    CIImage *frame = [CIImage imageWithCVPixelBuffer:buffer];
    CGRect pixels = visiblePixels(values.firstObject[SCStreamFrameInfoContentRect], frame.extent.size);
    if (CGRectIsEmpty(pixels)) {
        [self publish:nil];
        return;
    }
    // ScreenCaptureKit reports a top-left content rectangle. Core Image uses bottom-left coordinates.
    pixels.origin.y = CGRectGetHeight(frame.extent) - CGRectGetMaxY(pixels);
#ifdef LINKO_RW04_NETWORK_PREVIEW
    if ([self usesLoopback]) {
        RW04NetworkPreview *network = self.networkPreview;
        if (network) {
            CVPixelBufferRef input = [self copyCroppedFrame:frame pixels:pixels];
            if (input) {
                [network submit:input];
                CVPixelBufferRelease(input);
            }
        }
        return;
    }
#endif
#ifdef LINKO_RW04_CODEC_PREVIEW
    CVPixelBufferRef decoded = [self copyDecodedFrame:frame pixels:pixels];
    if (!decoded) {
        [self publish:nil];
        return;
    }
    frame = [CIImage imageWithCVPixelBuffer:decoded];
    pixels = frame.extent;
    CGImageRef result = [_context createCGImage:frame fromRect:pixels];
    CVPixelBufferRelease(decoded);
#else
    CGImageRef result = [_context createCGImage:frame fromRect:pixels];
#endif
    NSImage *image = result ? [[NSImage alloc] initWithCGImage:result size:NSZeroSize] : nil;
    if (result) {
        CGImageRelease(result);
    }
    [self publish:image];
}

- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sample
        ofType:(SCStreamOutputType)type {
    if (type == SCStreamOutputTypeScreen) {
        @autoreleasepool {
            [self handleScreenSample:sample];
        }
    }
}
@end

@interface PreviewApp : NSObject <NSApplicationDelegate, NSWindowDelegate>
- (CIContext *)contextForCapture;
- (CaptureOutput *)makeCaptureOutput;
- (NSImage *)frameForDisplay;
#ifdef LINKO_RW03_CI
- (void)beginCapture:(SCWindow *)source;
- (NSImage *)displayedFrame;
- (BOOL)hasStream;
- (BOOL)hasFrameTimer;
#ifdef LINKO_RW04_NETWORK_PREVIEW
- (CaptureOutput *)activeCaptureOutput;
- (void)stopButton:(id)sender;
- (BOOL)captureStopAcknowledged;
- (BOOL)receivedFixtureFrame;
- (dispatch_queue_t)captureQueueForFixture;
#endif
#ifdef LINKO_RW04_COLOR_PREVIEW
- (BOOL)showsFixtureColorRed:(BOOL)red;
#endif
#endif
@end

@implementation PreviewApp {
    NSWindow *_window;
    NSButton *_pick;
    NSButton *_stop;
    NSTextField *_status;
    NSImageView *_image;
    CIContext *_renderingContext;
    dispatch_queue_t _captureQueue;
    NSTimer *_timer;
    SCStream *_stream;
    CaptureOutput *_output;
    CGWindowID _windowID;
    pid_t _owner;
    BOOL _picking;
    BOOL _starting;
    BOOL _stopRequested;
    BOOL _stopping;
    BOOL _quitting;
#ifdef LINKO_RW04_NETWORK_PREVIEW
    BOOL _captureStopAcknowledged;
#endif
}

- (void)applicationDidFinishLaunching:(NSNotification *)notification {
    _window = [[NSWindow alloc] initWithContentRect:NSMakeRect(0, 0, 780, 580)
                                         styleMask:NSWindowStyleMaskTitled | NSWindowStyleMaskClosable |
                                                   NSWindowStyleMaskMiniaturizable | NSWindowStyleMaskResizable
                                           backing:NSBackingStoreBuffered defer:NO];
    _window.title = @"屏跃 · 本机窗口预览";
    _window.minSize = NSMakeSize(390, 300);
    _window.delegate = self;
    NSView *content = _window.contentView;
    _pick = [NSButton buttonWithTitle:@"选择窗口…" target:self action:@selector(pickWindow:)];
    _pick.frame = NSMakeRect(12, 534, 114, 30);
    _pick.autoresizingMask = NSViewMinYMargin;
    [content addSubview:_pick];
    _stop = [NSButton buttonWithTitle:@"停止" target:self action:@selector(stopButton:)];
    _stop.frame = NSMakeRect(130, 534, 78, 30);
    _stop.autoresizingMask = NSViewMinYMargin;
    _stop.enabled = NO;
    [content addSubview:_stop];
    _status = [NSTextField labelWithString:@"选择一个窗口开始预览。"];
    _status.frame = NSMakeRect(16, 507, 748, 20);
    _status.autoresizingMask = NSViewMinYMargin | NSViewWidthSizable;
    [content addSubview:_status];
    _image = [[NSImageView alloc] initWithFrame:NSMakeRect(16, 16, 748, 482)];
    _image.imageScaling = NSImageScaleProportionallyUpOrDown;
    _image.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    _image.wantsLayer = YES;
    _image.layer.backgroundColor = NSColor.blackColor.CGColor;
    [content addSubview:_image];

    [_window center];
    [_window makeKeyAndOrderFront:nil];
    [NSApp activate];
}

- (void)stopFrameTimer {
    [_timer invalidate];
    _timer = nil;
}

- (void)pickWindow:(id)sender {
    if (_picking || _stream || _stopping) {
        return;
    }
    if (!CGPreflightScreenCaptureAccess()) {
        CGRequestScreenCaptureAccess();
        _status.stringValue = @"请在系统设置允许屏幕录制，然后重新打开预览。";
        return;
    }
    _picking = YES;
    _pick.enabled = NO;
    [SCShareableContent getShareableContentExcludingDesktopWindows:YES onScreenWindowsOnly:YES
                                               completionHandler:^(SCShareableContent *content, NSError *error) {
        dispatch_async(dispatch_get_main_queue(), ^{
            self->_picking = NO;
            self->_pick.enabled = YES;
            if (error || !content) {
                self->_status.stringValue = @"读取窗口失败，请检查屏幕录制权限。";
                return;
            }
            [self chooseFrom:content.windows];
        });
    }];
}

- (void)chooseFrom:(NSArray<SCWindow *> *)windows {
    NSPopUpButton *options = [[NSPopUpButton alloc] initWithFrame:NSMakeRect(0, 0, 350, 26) pullsDown:NO];
    NSMutableArray<SCWindow *> *sources = [NSMutableArray array];
    for (SCWindow *candidate in windows) {
        SCRunningApplication *owner = candidate.owningApplication;
        if (!owner || owner.processID == NSProcessInfo.processInfo.processIdentifier ||
            !candidate.title.length || candidate.windowLayer != 0) {
            continue;
        }
        NSString *title = [NSString stringWithFormat:@"%@ — %@", owner.applicationName, candidate.title];
        [options addItemWithTitle:@""];
        options.lastItem.title = title;
        [sources addObject:candidate];
        if (sources.count == 128) {
            break;
        }
    }
    if (!sources.count) {
        _status.stringValue = @"当前没有可选窗口。";
        return;
    }
    NSAlert *selection = [NSAlert new];
    selection.messageText = @"选择一个窗口";
    selection.accessoryView = options;
    [selection addButtonWithTitle:@"开始预览"];
    [selection addButtonWithTitle:@"取消"];
    [selection beginSheetModalForWindow:_window completionHandler:^(NSModalResponse result) {
        if (result == NSAlertFirstButtonReturn && options.indexOfSelectedItem >= 0) {
            [self beginCapture:sources[(NSUInteger)options.indexOfSelectedItem]];
        }
    }];
}

- (CIContext *)contextForCapture {
    if (!_renderingContext) {
        _renderingContext = makeRenderingContext();
    }
    return _renderingContext;
}

- (CaptureOutput *)makeCaptureOutput {
    CaptureOutput *output = [[CaptureOutput alloc] initWithRenderingContext:[self contextForCapture]];
#ifdef LINKO_RW04_NETWORK_PREVIEW
    output.networkPreview = [RW04NetworkPreview new];
    if (!output.networkPreview) {
        return nil;
    }
#endif
    return output;
}

- (void)beginCapture:(SCWindow *)source {
    if (_stream || _stopping || !CGPreflightScreenCaptureAccess() ||
        !sourceExists(source.windowID, source.owningApplication.processID)) {
        _status.stringValue = @"窗口或权限已变化，请重新选择。";
        return;
    }
    SCContentFilter *filter = [[SCContentFilter alloc] initWithDesktopIndependentWindow:source];
    SCStreamConfiguration *settings = [SCStreamConfiguration new];
    settings.width = MaxWidth;
    settings.height = MaxHeight;
    settings.minimumFrameInterval = CMTimeMake(1, 24);
    settings.queueDepth = 3;
    settings.pixelFormat = kCVPixelFormatType_32BGRA;
    settings.showsCursor = NO;
    settings.capturesAudio = NO;
    settings.ignoreShadowsSingleWindow = YES;
    settings.preservesAspectRatio = YES;

    CaptureOutput *output = [self makeCaptureOutput];
    if (!output) {
        _status.stringValue = @"无法建立预览。";
        return;
    }
    SCStream *stream = [[SCStream alloc] initWithFilter:filter configuration:settings delegate:output];
    if (!_captureQueue) {
        dispatch_queue_attr_t attributes = dispatch_queue_attr_make_with_autorelease_frequency(
            DISPATCH_QUEUE_SERIAL, DISPATCH_AUTORELEASE_FREQUENCY_WORK_ITEM);
        _captureQueue = dispatch_queue_create("app.linko.rw03.frames", attributes);
    }
    NSError *error = nil;
    if (![stream addStreamOutput:output type:SCStreamOutputTypeScreen sampleHandlerQueue:_captureQueue error:&error]) {
        _status.stringValue = @"无法初始化窗口预览。";
        return;
    }

    _windowID = source.windowID;
    _owner = source.owningApplication.processID;
    _output = output;
    _stream = stream;
    _starting = YES;
#ifdef LINKO_RW04_NETWORK_PREVIEW
    _captureStopAcknowledged = NO;
#endif
    _pick.enabled = NO;
    _stop.enabled = YES;
    _status.stringValue = @"正在打开预览。";
    _timer = [NSTimer scheduledTimerWithTimeInterval:0.1 target:self
                                           selector:@selector(updateFrame:) userInfo:nil repeats:YES];
    [stream startCaptureWithCompletionHandler:^(NSError *startError) {
        dispatch_async(dispatch_get_main_queue(), ^{
            self->_starting = NO;
            if (startError) {
                [self stopFrameTimer];
                self->_image.image = nil;
                [output discard];
                [stream removeStreamOutput:output type:SCStreamOutputTypeScreen error:NULL];
                self->_stream = nil;
                self->_output = nil;
                self->_stopRequested = NO;
                self->_pick.enabled = YES;
                self->_stop.enabled = NO;
                self->_status.stringValue = @"无法开始采集，请检查窗口和权限。";
                if (self->_quitting) {
                    [NSApp replyToApplicationShouldTerminate:YES];
                }
            } else if (self->_stopRequested) {
                [self finishStop];
            }
        });
    }];
}

- (void)stopButton:(id)sender {
    [self stopCaptureWithMessage:@"预览已停止。"];
}

- (void)stopCaptureWithMessage:(NSString *)message {
    if (!_stream || _stopRequested) {
        return;
    }
    _stopRequested = YES;
    [self stopFrameTimer];
    _image.image = nil;
    [_output discard];
    _status.stringValue = message;
    _stop.enabled = NO;
    if (!_starting) {
        [self finishStop];
    }
}

- (void)captureDidStop:(NSError *)error {
#ifdef LINKO_RW04_NETWORK_PREVIEW
    _captureStopAcknowledged = error == nil;
#else
    (void)error;
#endif
    _stream = nil;
    _output = nil;
    _stopRequested = NO;
    _stopping = NO;
    _pick.enabled = YES;
    if (_quitting) {
        [NSApp replyToApplicationShouldTerminate:YES];
    }
}

- (void)finishStop {
    _stopping = YES;
    SCStream *stream = _stream;
    CaptureOutput *output = _output;
    dispatch_queue_t queue = _captureQueue;
    [stream removeStreamOutput:output type:SCStreamOutputTypeScreen error:NULL];
    [stream stopCaptureWithCompletionHandler:^(NSError *error) {
        // Retire queued samples before the next window can start.
        dispatch_async(queue, ^{
            dispatch_async(dispatch_get_main_queue(), ^{
                [self captureDidStop:error];
            });
        });
    }];
}

- (NSImage *)frameForDisplay {
    return [_output currentAt:uptime()];
}

- (void)updateFrame:(NSTimer *)timer {
    if (!_stream || _stopRequested) {
        return;
    }
    if (_output.ended || !CGPreflightScreenCaptureAccess() || !sourceExists(_windowID, _owner)) {
        [self stopCaptureWithMessage:@"窗口或权限已失效，预览已停止。"];
        return;
    }
    NSImage *frame = [self frameForDisplay];
    if (_image.image != frame) {
        _image.image = frame;
    }
    if (!_starting) {
        _status.stringValue = frame ? @"正在本机预览。" : @"暂无画面。";
    }
}

#ifdef LINKO_RW03_CI
- (NSImage *)displayedFrame {
    return _image.image;
}
#ifdef LINKO_RW04_COLOR_PREVIEW
- (BOOL)showsFixtureColorRed:(BOOL)red {
    if (!_image.image) {
        return NO;
    }
    NSRect bounds = _image.bounds;
    NSBitmapImageRep *bitmap = [_image bitmapImageRepForCachingDisplayInRect:bounds];
    if (!bitmap || bitmap.pixelsWide < 1 || bitmap.pixelsHigh < 1) {
        return NO;
    }
    [_image cacheDisplayInRect:bounds toBitmapImageRep:bitmap];
    NSColor *color = [[bitmap colorAtX:bitmap.pixelsWide / 2 y:bitmap.pixelsHigh / 2]
        colorUsingColorSpace:NSColorSpace.sRGBColorSpace];
    if (!color) {
        return NO;
    }
    const CGFloat expected = red ? color.redComponent : color.greenComponent;
    const CGFloat other = red ? color.greenComponent : color.redComponent;
    return expected > other + 0.25 && expected > color.blueComponent + 0.25;
}
#endif
- (BOOL)hasStream {
    return _stream != nil;
}
- (BOOL)hasFrameTimer {
    return _timer.isValid;
}
#ifdef LINKO_RW04_NETWORK_PREVIEW
- (CaptureOutput *)activeCaptureOutput {
    return _output;
}
- (BOOL)captureStopAcknowledged {
    return _captureStopAcknowledged;
}
- (BOOL)receivedFixtureFrame {
    return self.displayedFrame != nil;
}
- (dispatch_queue_t)captureQueueForFixture {
    return _captureQueue;
}
#endif
#endif

- (void)windowWillClose:(NSNotification *)notification {
    [self stopFrameTimer];
    [NSApp terminate:nil];
}

- (NSApplicationTerminateReply)applicationShouldTerminate:(NSApplication *)sender {
    if (!_stream) {
        return NSTerminateNow;
    }
    _quitting = YES;
    [self stopCaptureWithMessage:@"预览已停止。"];
    dispatch_after(dispatch_time(DISPATCH_TIME_NOW, 2 * NSEC_PER_SEC), dispatch_get_main_queue(), ^{
        if (self->_quitting && self->_stream) {
            [NSApp replyToApplicationShouldTerminate:YES];
        }
    });
    return NSTerminateLater;
}
@end

#ifdef LINKO_RW03_CI
static BOOL waitFor(BOOL (^predicate)(void)) {
    NSTimeInterval deadline = uptime() + 5;
    do {
        if (predicate()) {
            return YES;
        }
        [NSRunLoop.currentRunLoop runMode:NSDefaultRunLoopMode
                              beforeDate:[NSDate dateWithTimeIntervalSinceNow:0.02]];
    } while (uptime() < deadline);
    return NO;
}

#ifdef LINKO_RW04_NETWORK_PREVIEW
static BOOL checkInputPool(void) {
    @autoreleasepool {
        CaptureOutput *output = [[CaptureOutput alloc] initWithRenderingContext:makeRenderingContext()];
        NSMutableArray *held = [NSMutableArray arrayWithCapacity:InputPoolLimit];
        for (NSUInteger index = 0; index < InputPoolLimit; ++index) {
            CVPixelBufferRef pixel = [output copyInputWithWidth:128 height:96];
            if (!pixel) {
                return NO;
            }
            [held addObject:(__bridge id)pixel];
            CVPixelBufferRelease(pixel);
        }
        CVPixelBufferRef overflow = [output copyInputWithWidth:128 height:96];
        if (overflow) {
            CVPixelBufferRelease(overflow);
            return NO;
        }
        [held removeLastObject];
        CVPixelBufferRef recycled = [output copyInputWithWidth:128 height:96];
        CVPixelBufferRef resized = [output copyInputWithWidth:144 height:112];
        BOOL passed = recycled && resized && CVPixelBufferGetWidth(recycled) == 128 &&
                      CVPixelBufferGetWidth(resized) == 144 &&
                      CVPixelBufferGetHeight(resized) == 112;
        if (recycled) {
            CVPixelBufferRelease(recycled);
        }
        if (resized) {
            CVPixelBufferRelease(resized);
        }
        [output discard];
        CVPixelBufferRef revoked = [output copyInputWithWidth:144 height:112];
        if (revoked) {
            CVPixelBufferRelease(revoked);
            return NO;
        }
        return passed;
    }
}

@interface CaptureOnlyOutput : CaptureOutput
@end

@implementation CaptureOnlyOutput
- (BOOL)usesLoopback {
    return NO;
}
@end

@interface CaptureOnlyFixtureApp : PreviewApp
@end

@implementation CaptureOnlyFixtureApp
- (CaptureOutput *)makeCaptureOutput {
    return [[CaptureOnlyOutput alloc] initWithRenderingContext:[self contextForCapture]];
}
@end

@interface DrainOnlyFixtureApp : PreviewApp
@end

@implementation DrainOnlyFixtureApp {
    BOOL _received;
}

- (void)beginCapture:(SCWindow *)source {
    _received = NO;
    [super beginCapture:source];
}

- (NSImage *)frameForDisplay {
    if ([[self activeCaptureOutput] consumeDecodedFrameWithoutRendering]) {
        _received = YES;
    }
    return nil;
}

- (BOOL)receivedFixtureFrame {
    return _received;
}
@end

static int sampleStoppedProcess(unsigned cycle) {
    struct proc_taskinfo task = {0};
    struct proc_fdinfo descriptors[512];
    task_vm_info_data_t vm = {0};
    mach_msg_type_number_t vmCount = TASK_VM_INFO_COUNT;
    int taskBytes = proc_pidinfo(getpid(), PROC_PIDTASKINFO, 0, &task, (int)sizeof(task));
    int fdBytes = proc_pidinfo(getpid(), PROC_PIDLISTFDS, 0, descriptors, (int)sizeof(descriptors));
    kern_return_t vmStatus = task_info(mach_task_self(), TASK_VM_INFO, (task_info_t)&vm, &vmCount);
    if (taskBytes != (int)sizeof(task) || fdBytes <= 0 || fdBytes >= (int)sizeof(descriptors) ||
        fdBytes % (int)sizeof(struct proc_fdinfo) != 0 ||
        vmStatus != KERN_SUCCESS || vmCount < TASK_VM_INFO_REV1_COUNT) {
        fputs("Loopback cycles: could not measure the stopped process.\n", stderr);
        return -1;
    }
    malloc_statistics_t heap;
    malloc_zone_statistics(NULL, &heap);
    int fdCount = fdBytes / (int)sizeof(struct proc_fdinfo);
    printf("Loopback cycles: stopped=%u rss_bytes=%llu footprint_bytes=%llu "
           "heap_live_bytes=%zu heap_allocated_bytes=%zu threads=%d fds=%d\n",
           cycle, (unsigned long long)task.pti_resident_size, (unsigned long long)vm.phys_footprint,
           heap.size_in_use, heap.size_allocated, task.pti_threadnum, fdCount);
    return fdCount;
}

static BOOL checkStoppedSample(unsigned cycle, int *warmDescriptors) {
    if (cycle != 0 && cycle != 10 && cycle % 25 != 0) {
        return YES;
    }
    int descriptors = sampleStoppedProcess(cycle);
    if (descriptors < 0) {
        return NO;
    }
    if (cycle == 10) {
        *warmDescriptors = descriptors;
    }
    if (cycle == 100 && descriptors > *warmDescriptors) {
        fputs("Loopback cycles: open descriptors grew after warm-up.\n", stderr);
        return NO;
    }
    return YES;
}

static BOOL stopFixtureCycle(PreviewApp *preview, BOOL usesLoopback) {
    __weak CaptureOutput *retiredOutput;
    @autoreleasepool {
        CaptureOutput *output = [preview activeCaptureOutput];
        retiredOutput = output;
        RW04NetworkPreview *network = output.networkPreview;
        [preview stopButton:nil];
        BOOL mediaStopped = usesLoopback ? network.wasClosed : network == nil;
        if (preview.displayedFrame || preview.hasFrameTimer || !mediaStopped) {
            return NO;
        }
    }
    if (!waitFor(^BOOL { return !preview.hasStream; }) || !preview.captureStopAcknowledged) {
        return NO;
    }
    return waitFor(^BOOL { return !retiredOutput; });
}

static BOOL checkCaptureCycles(PreviewApp *preview, SCWindow *source, BOOL usesLoopback) {
    int warmDescriptors = -1;
    dispatch_queue_t queue = preview.captureQueueForFixture;
    if (!queue || !stopFixtureCycle(preview, usesLoopback) || !checkStoppedSample(0, &warmDescriptors)) {
        return NO;
    }
    for (unsigned cycle = 1; cycle <= 100; ++cycle) {
        @autoreleasepool {
            [preview beginCapture:source];
            if (preview.captureQueueForFixture != queue || !preview.hasFrameTimer ||
                !waitFor(^BOOL { return preview.receivedFixtureFrame; }) ||
                !stopFixtureCycle(preview, usesLoopback)) {
                fprintf(stderr, "Loopback cycles: open, render or cleanup failed at cycle %u.\n", cycle);
                return NO;
            }
        }
        if (!checkStoppedSample(cycle, &warmDescriptors)) {
            return NO;
        }
    }
    puts("Loopback cycles: one capture queue reused across all cycles.");
    puts("Loopback cycles: 100 native start/stop cycles passed.");
    return YES;
}

static BOOL exerciseMedia(CVPixelBufferRef input) {
    RW04NetworkPreview *network = [RW04NetworkPreview new];
    if (!network) {
        return NO;
    }
    BOOL received = NO;
    for (unsigned attempt = 0; attempt < 100 && !received; ++attempt) {
        @autoreleasepool {
            if (![network submit:input]) {
                break;
            }
            [NSThread sleepForTimeInterval:0.04];
            CVPixelBufferRef output = [network copyFrameReceivedAt:NULL];
            if (output) {
                received = CVPixelBufferGetWidth(output) == CVPixelBufferGetWidth(input) &&
                           CVPixelBufferGetHeight(output) == CVPixelBufferGetHeight(input);
                CVPixelBufferRelease(output);
            }
        }
    }
    [network revoke];
    return received && network.wasClosed;
}

static int checkMediaCycles(void) {
    puts("Loopback cycles: path=media-only");
    CVPixelBufferRef input = NULL;
    NSDictionary *attributes = @{(__bridge NSString *)kCVPixelBufferIOSurfacePropertiesKey: @{}};
    if (CVPixelBufferCreate(kCFAllocatorDefault, 224, 144, kCVPixelFormatType_32BGRA,
                            (__bridge CFDictionaryRef)attributes, &input) != kCVReturnSuccess) {
        return 16;
    }
    CIImage *pixels = [[CIImage imageWithColor:[CIColor colorWithRed:0.8 green:0.1 blue:0.1]]
        imageByCroppingToRect:CGRectMake(0, 0, 224, 144)];
    [makeRenderingContext() render:pixels toCVPixelBuffer:input];
    int warmDescriptors = -1;
    int result = checkStoppedSample(0, &warmDescriptors) ? 0 : 17;
    for (unsigned cycle = 1; cycle <= 100 && result == 0; ++cycle) {
        @autoreleasepool {
            if (!exerciseMedia(input)) {
                fprintf(stderr, "Loopback cycles: media-only frame or cleanup failed at cycle %u.\n", cycle);
                result = 17;
            }
        }
        if (result == 0 && !checkStoppedSample(cycle, &warmDescriptors)) {
            result = 17;
        }
    }
    CVPixelBufferRelease(input);
    if (result == 0) {
        puts("Loopback cycles: 100 media-only start/stop cycles passed.");
    }
    return result;
}
#endif

static int checkNativeCapture(void) {
#ifdef LINKO_RW04_NETWORK_PREVIEW
    NSArray<NSString *> *arguments = NSProcessInfo.processInfo.arguments;
    if ([arguments containsObject:@"--media-only"]) {
        return checkMediaCycles();
    }
#endif
    if (!CGPreflightScreenCaptureAccess()) {
        puts("macOS hosted permission unavailable; native capture was not exercised.");
#ifdef LINKO_RW04_COLOR_PREVIEW
        return 9;
#else
        return 0;
#endif
    }
    [NSApplication sharedApplication].activationPolicy = NSApplicationActivationPolicyRegular;
    [NSApp finishLaunching];
#ifdef LINKO_RW04_NETWORK_PREVIEW
    BOOL captureOnly = [arguments containsObject:@"--capture-only"];
    BOOL drainOnly = [arguments containsObject:@"--drain-only"];
    PreviewApp *preview = drainOnly ? [DrainOnlyFixtureApp new] :
                          captureOnly ? [CaptureOnlyFixtureApp new] : [PreviewApp new];
    printf("Loopback cycles: path=%s\n", drainOnly ? "drain-only" : captureOnly ? "capture-only" : "full");
    if (!captureOnly && !drainOnly) {
        if (!checkInputPool()) {
            fputs("Loopback input pool: allocation bound, recycling, resize, or stop failed.\n", stderr);
            return 18;
        }
        puts("Loopback input pool: six buffers, recycling, resize and stop passed.");
    }
#else
    PreviewApp *preview = [PreviewApp new];
#endif
    [preview applicationDidFinishLaunching:
        [NSNotification notificationWithName:NSApplicationDidFinishLaunchingNotification object:NSApp]];
    if (preview.hasFrameTimer) {
        fputs("macOS preview scheduled frame polling while idle.\n", stderr);
        return 13;
    }
    NSWindow *fixture = [[NSWindow alloc] initWithContentRect:NSMakeRect(50, 50, 220, 140)
                                                   styleMask:NSWindowStyleMaskTitled
                                                     backing:NSBackingStoreBuffered defer:NO];
    fixture.releasedWhenClosed = NO;
    fixture.title = @"Linko RW03 fixture";
    fixture.backgroundColor = NSColor.systemRedColor;
#ifdef LINKO_RW04_COLOR_PREVIEW
    NSView *solid = [[NSView alloc] initWithFrame:NSMakeRect(0, 0, 220, 140)];
    solid.wantsLayer = YES;
    solid.layer.backgroundColor = NSColor.redColor.CGColor;
    solid.autoresizingMask = NSViewWidthSizable | NSViewHeightSizable;
    fixture.contentView = solid;
#else
    fixture.contentView = [NSTextField labelWithString:@"RW03"];
#endif
    [fixture makeKeyAndOrderFront:nil];
    [fixture orderFrontRegardless];
    [fixture displayIfNeeded];
    CGWindowID window = (CGWindowID)fixture.windowNumber;
    BOOL visible = waitFor(^BOOL { return sourceExists(window, NSProcessInfo.processInfo.processIdentifier); });
    if (!visible) {
        fputs("macOS hosted WindowServer did not register the test window.\n", stderr);
        return 5;
    }
    __block SCWindow *source;
    __block NSError *queryError;
    __block NSUInteger count = 0;
    __block BOOL queried = NO;
    [SCShareableContent getShareableContentExcludingDesktopWindows:YES onScreenWindowsOnly:NO
                                               completionHandler:^(SCShareableContent *content, NSError *error) {
        dispatch_async(dispatch_get_main_queue(), ^{
            queryError = error;
            count = content.windows.count;
            for (SCWindow *candidate in content.windows) {
                if (candidate.windowID == window) {
                    source = candidate;
                    break;
                }
            }
            queried = YES;
        });
    }];
    if (!waitFor(^BOOL { return queried; }) || !source) {
        fprintf(stderr, "macOS hosted fixture missing from ScreenCaptureKit (query completed=%d windows=%lu error=%ld).\n",
                queried, (unsigned long)count, (long)queryError.code);
        return 5;
    }
#ifdef LINKO_RW04_NETWORK_PREVIEW
    __block BOOL red = YES;
    __block BOOL bright = NO;
    NSTimer *pulse = [NSTimer scheduledTimerWithTimeInterval:0.05 repeats:YES block:^(NSTimer *timer) {
        (void)timer;
        bright = !bright;
        CGFloat level = bright ? 1.0 : 0.75;
        solid.layer.backgroundColor =
            [NSColor colorWithSRGBRed:red ? level : 0 green:red ? 0 : level blue:0 alpha:1].CGColor;
    }];
    if (drainOnly) {
        [fixture setContentSize:NSMakeSize(350, 230)];
        [fixture displayIfNeeded];
        [preview beginCapture:source];
        BOOL passed = waitFor(^BOOL { return preview.receivedFixtureFrame; }) &&
                      checkCaptureCycles(preview, source, YES);
        [pulse invalidate];
        [fixture close];
        if (!passed) {
            fputs("macOS decoded-frame drain did not pass the native cycle check.\n", stderr);
        }
        return passed ? 0 : 19;
    }
#endif
    [preview beginCapture:source];
    if (!waitFor(^BOOL {
#ifdef LINKO_RW04_COLOR_PREVIEW
        return [preview showsFixtureColorRed:YES];
#else
        return preview.displayedFrame != nil;
#endif
    })) {
        fputs("macOS ScreenCaptureKit fixture did not deliver a viewable frame.\n", stderr);
        return 6;
    }
    CGFloat width = preview.displayedFrame.size.width;
    [fixture setContentSize:NSMakeSize(350, 230)];
#ifdef LINKO_RW04_NETWORK_PREVIEW
    red = NO;
#endif
#ifdef LINKO_RW04_COLOR_PREVIEW
    solid.layer.backgroundColor = NSColor.greenColor.CGColor;
    [solid setNeedsDisplay:YES];
#endif
    [fixture displayIfNeeded];
    if (!waitFor(^BOOL {
#ifdef LINKO_RW04_COLOR_PREVIEW
        return preview.displayedFrame.size.width > width && [preview showsFixtureColorRed:NO];
#else
        return preview.displayedFrame.size.width > width;
#endif
    })) {
        fputs("macOS ScreenCaptureKit fixture did not deliver the resized frame.\n", stderr);
        return 7;
    }
#ifdef LINKO_RW04_NETWORK_PREVIEW
    if (captureOnly) {
        BOOL passed = checkCaptureCycles(preview, source, NO);
        [pulse invalidate];
        [fixture close];
        return passed ? 0 : 14;
    }
    CaptureOutput *retiredOutput = [preview activeCaptureOutput];
    RW04NetworkPreview *retiredNetwork = retiredOutput.networkPreview;
    NSImage *retiredImage = preview.displayedFrame;
    CGImageRef retainedImage = [retiredImage CGImageForProposedRect:NULL context:nil hints:nil];
    CIImage *decoded = retainedImage ? [CIImage imageWithCGImage:retainedImage] : nil;
    CVPixelBufferRef delayedPixels = decoded
        ? [retiredOutput copyCroppedFrame:decoded pixels:decoded.extent]
        : NULL;
    if (!delayedPixels) {
        fputs("macOS loopback fixture could not retain a decoded frame for the revoke test.\n", stderr);
        [pulse invalidate];
        return 10;
    }

    [preview stopButton:nil];
    BOOL stopped = !preview.displayedFrame && [retiredNetwork wasClosed] &&
                   waitFor(^BOOL { return !preview.hasStream; });
    if (stopped) {
        red = YES;
        [preview beginCapture:source];
    }
    [retiredOutput publish:retiredImage];
    BOOL accepted = [retiredNetwork submit:delayedPixels];
    CVPixelBufferRelease(delayedPixels);
    CVPixelBufferRef residualPixels = [retiredNetwork copyFrameReceivedAt:NULL];
    BOOL discarded = !accepted && !residualPixels && ![retiredOutput currentAt:uptime()];
    if (residualPixels) {
        CVPixelBufferRelease(residualPixels);
    }
    if (!stopped || !discarded || preview.displayedFrame ||
        !waitFor(^BOOL { return [preview showsFixtureColorRed:YES]; })) {
        fputs("macOS loopback fixture failed to revoke, reject an old frame, or reopen.\n", stderr);
        [pulse invalidate];
        return 11;
    }
    puts("Loopback revoke: closed old peers, rejected delayed pixels, and rendered the reopened source.");
    if (!checkCaptureCycles(preview, source, YES)) {
        [pulse invalidate];
        return 14;
    }
    [preview beginCapture:source];
    if (!waitFor(^BOOL { return [preview showsFixtureColorRed:YES]; })) {
        fputs("macOS loopback fixture did not render after the repeated cycle check.\n", stderr);
        [pulse invalidate];
        return 15;
    }
    [pulse invalidate];
#endif
    [fixture close];
    fixture = nil;
    if (!waitFor(^BOOL {
        return !preview.displayedFrame && !preview.hasStream && !preview.hasFrameTimer;
    })) {
        fputs("macOS preview did not clear and release the closed fixture.\n", stderr);
        return 8;
    }
#ifdef LINKO_RW04_NETWORK_PREVIEW
    puts("ScreenCaptureKit -> loopback UDP/DTLS H.264 -> AppKit: rendered colors, resize and source close passed.");
#elif defined(LINKO_RW04_CODEC_PREVIEW)
    puts("ScreenCaptureKit -> WebRTC H.264 -> AppKit: rendered colors, resize and source close passed.");
#else
    puts("ScreenCaptureKit fixture start, resize and source close passed.");
#endif
    return 0;
}

int main(void) {
    @autoreleasepool {
        CaptureOutput *output = [[CaptureOutput alloc] initWithRenderingContext:makeRenderingContext()];
        NSImage *fixture = [[NSImage alloc] initWithSize:NSMakeSize(20, 10)];
        [output publish:fixture];
        if ([output currentAt:uptime()] != fixture || [output currentAt:uptime() + 3]) {
            return 1;
        }
#ifdef LINKO_RW04_NETWORK_PREVIEW
        const NSTimeInterval receipt = uptime() - 1.0;
        [output publish:fixture receivedAt:receipt];
        if ([output currentAt:receipt + 1.5] != fixture ||
            [output currentAt:receipt + FrameLifetime] ||
            [output currentAt:receipt + FrameLifetime + 0.1]) {
            fputs("macOS preview extended an already received image past its original deadline.\n", stderr);
            return 12;
        }
        puts("AppKit frame expiry: repeated presentation preserves the decoded receipt deadline.");
#endif
        [output publish:fixture];
        [output discard];
        [output publish:fixture];
        if (!output.ended || [output currentAt:uptime()]) {
            return 2;
        }
        CGRect cropped = visiblePixels([NSValue valueWithRect:NSMakeRect(0, 0, 24, 12)], CGSizeMake(20, 20));
        if (!CGRectEqualToRect(cropped, CGRectMake(0, 0, 20, 12)) ||
            !CGRectIsNull(visiblePixels(nil, CGSizeMake(20, 20)))) {
            return 3;
        }
        if (sourceExists(kCGNullWindowID, -1)) {
            return 4;
        }
        printf("macOS frame expiry, stop and source identity checks passed; TCC preflight: %s\n",
               CGPreflightScreenCaptureAccess() ? "allowed" : "unavailable");
        return checkNativeCapture();
    }
}
#else
int main(void) {
    @autoreleasepool {
        NSApplication *app = [NSApplication sharedApplication];
        app.activationPolicy = NSApplicationActivationPolicyRegular;
        PreviewApp *preview = [PreviewApp new];
        app.delegate = preview;
        [app run];
    }
    return 0;
}
#endif
