#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>
#import <CoreImage/CoreImage.h>
#import <CoreMedia/CoreMedia.h>
#import <CoreVideo/CoreVideo.h>
#import <ScreenCaptureKit/ScreenCaptureKit.h>
#import <QuartzCore/QuartzCore.h>
#include <math.h>
#include <string.h>

static const size_t MaxWidth = 1920;
static const size_t MaxHeight = 1080;
static const NSTimeInterval FrameLifetime = 2.0;

static CIContext *makeRenderingContext(void) {
    return [CIContext contextWithOptions:@{kCIContextCacheIntermediates: @NO}];
}

static NSTimeInterval uptime(void) {
    return NSProcessInfo.processInfo.systemUptime;
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
- (void)discard;
- (void)publish:(NSImage *)image;
- (void)stillCurrent;
- (NSImage *)currentAt:(NSTimeInterval)now;
@end

@implementation CaptureOutput {
    NSLock *_lock;
    CIContext *_context;
    NSImage *_image;
    NSTimeInterval _lastFrame;
    BOOL _ended;
}

- (instancetype)initWithRenderingContext:(CIContext *)context {
    self = [super init];
    if (self) {
        _lock = [NSLock new];
        _context = context;
    }
    return self;
}

- (void)discard {
    [_lock lock];
    _ended = YES;
    _image = nil;
    [_lock unlock];
}

- (BOOL)ended {
    [_lock lock];
    BOOL value = _ended;
    [_lock unlock];
    return value;
}

- (void)publish:(NSImage *)image {
    [_lock lock];
    if (!_ended) {
        _image = image;
        _lastFrame = uptime();
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
    [_lock lock];
    if (now - _lastFrame > FrameLifetime) {
        _image = nil;
    }
    NSImage *image = _image;
    [_lock unlock];
    return image;
}

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
        [self stillCurrent];
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
    CGImageRef result = [_context createCGImage:frame fromRect:pixels];
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
#ifdef LINKO_RW03_CI
- (void)beginCapture:(SCWindow *)source;
- (void)stopButton:(id)sender;
- (NSImage *)displayedFrame;
- (BOOL)hasStream;
- (BOOL)hasFrameTimer;
- (BOOL)captureStopAcknowledged;
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
#ifdef LINKO_RW03_CI
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

    if (!_renderingContext) {
        _renderingContext = makeRenderingContext();
    }
    CaptureOutput *output = [[CaptureOutput alloc] initWithRenderingContext:_renderingContext];
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
#ifdef LINKO_RW03_CI
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
#ifdef LINKO_RW03_CI
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
        dispatch_async(queue, ^{
            dispatch_async(dispatch_get_main_queue(), ^{
                [self captureDidStop:error];
            });
        });
    }];
}

- (void)updateFrame:(NSTimer *)timer {
    if (!_stream || _stopRequested) {
        return;
    }
    if (_output.ended || !CGPreflightScreenCaptureAccess() || !sourceExists(_windowID, _owner)) {
        [self stopCaptureWithMessage:@"窗口或权限已失效，预览已停止。"];
        return;
    }
    NSImage *frame = [_output currentAt:uptime()];
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
- (BOOL)hasStream {
    return _stream != nil;
}
- (BOOL)hasFrameTimer {
    return _timer.isValid;
}
- (BOOL)captureStopAcknowledged {
    return _captureStopAcknowledged;
}
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

static int checkNativeCapture(void) {
    if (!CGPreflightScreenCaptureAccess()) {
        puts("macOS hosted permission unavailable; native capture was not exercised.");
        return 0;
    }
    [NSApplication sharedApplication].activationPolicy = NSApplicationActivationPolicyRegular;
    [NSApp finishLaunching];
    PreviewApp *preview = [PreviewApp new];
    [preview applicationDidFinishLaunching:
        [NSNotification notificationWithName:NSApplicationDidFinishLaunchingNotification object:NSApp]];
    if (preview.hasFrameTimer) {
        fputs("macOS preview scheduled frame polling while idle.\n", stderr);
        return 9;
    }
    NSWindow *fixture = [[NSWindow alloc] initWithContentRect:NSMakeRect(50, 50, 220, 140)
                                                   styleMask:NSWindowStyleMaskTitled
                                                     backing:NSBackingStoreBuffered defer:NO];
    fixture.releasedWhenClosed = NO;
    fixture.title = @"Linko RW03 fixture";
    fixture.backgroundColor = NSColor.systemRedColor;
    fixture.contentView = [NSTextField labelWithString:@"RW03"];
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
    [preview beginCapture:source];
    if (!waitFor(^BOOL { return preview.displayedFrame != nil; })) {
        fputs("macOS ScreenCaptureKit fixture did not deliver a viewable frame.\n", stderr);
        return 6;
    }
    CGFloat width = preview.displayedFrame.size.width;
    [fixture setContentSize:NSMakeSize(350, 230)];
    [fixture displayIfNeeded];
    if (!waitFor(^BOOL { return preview.displayedFrame.size.width > width; })) {
        fputs("macOS ScreenCaptureKit fixture did not deliver the resized frame.\n", stderr);
        return 7;
    }
    [preview stopButton:nil];
    if (preview.displayedFrame || preview.hasFrameTimer ||
        !waitFor(^BOOL { return !preview.hasStream; }) || !preview.captureStopAcknowledged) {
        fputs("macOS preview did not clear and stop polling on explicit stop.\n", stderr);
        return 10;
    }
    [preview beginCapture:source];
    if (!waitFor(^BOOL { return preview.displayedFrame.size.width > width; })) {
        fputs("macOS preview did not render the explicitly selected window again.\n", stderr);
        return 11;
    }
    [fixture close];
    fixture = nil;
    if (!waitFor(^BOOL {
        return !preview.displayedFrame && !preview.hasStream && !preview.hasFrameTimer;
    })) {
        fputs("macOS preview did not clear and release the closed fixture.\n", stderr);
        return 8;
    }
    puts("ScreenCaptureKit fixture: start, resize, stop, reopen, source close and no idle polling passed.");
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
