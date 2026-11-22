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

- (instancetype)init {
    self = [super init];
    if (self) {
        _lock = [NSLock new];
        _context = [CIContext contextWithOptions:@{kCIContextCacheIntermediates: @NO}];
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

- (void)stream:(SCStream *)stream didOutputSampleBuffer:(CMSampleBufferRef)sample
        ofType:(SCStreamOutputType)type {
    if (self.ended || type != SCStreamOutputTypeScreen) {
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
@end

@interface PreviewApp : NSObject <NSApplicationDelegate, NSWindowDelegate>
@end

@implementation PreviewApp {
    NSWindow *_window;
    NSButton *_pick;
    NSButton *_stop;
    NSTextField *_status;
    NSImageView *_image;
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
    _timer = [NSTimer scheduledTimerWithTimeInterval:0.1 target:self
                                           selector:@selector(updateFrame:) userInfo:nil repeats:YES];
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

    CaptureOutput *output = [CaptureOutput new];
    SCStream *stream = [[SCStream alloc] initWithFilter:filter configuration:settings delegate:output];
    dispatch_queue_t queue = dispatch_queue_create("app.linko.rw03.frames", DISPATCH_QUEUE_SERIAL);
    NSError *error = nil;
    if (![stream addStreamOutput:output type:SCStreamOutputTypeScreen sampleHandlerQueue:queue error:&error]) {
        _status.stringValue = @"无法初始化窗口预览。";
        return;
    }

    _windowID = source.windowID;
    _owner = source.owningApplication.processID;
    _output = output;
    _stream = stream;
    _starting = YES;
    _pick.enabled = NO;
    _stop.enabled = YES;
    _status.stringValue = @"正在打开预览。";
    [stream startCaptureWithCompletionHandler:^(NSError *startError) {
        dispatch_async(dispatch_get_main_queue(), ^{
            self->_starting = NO;
            if (startError) {
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
    [_output discard];
    _image.image = nil;
    _status.stringValue = message;
    _stop.enabled = NO;
    if (!_starting) {
        [self finishStop];
    }
}

- (void)finishStop {
    _stopping = YES;
    SCStream *stream = _stream;
    CaptureOutput *output = _output;
    [stream removeStreamOutput:output type:SCStreamOutputTypeScreen error:NULL];
    [stream stopCaptureWithCompletionHandler:^(NSError *error) {
        dispatch_async(dispatch_get_main_queue(), ^{
            self->_stream = nil;
            self->_output = nil;
            self->_stopRequested = NO;
            self->_stopping = NO;
            self->_pick.enabled = YES;
            if (self->_quitting) {
                [NSApp replyToApplicationShouldTerminate:YES];
            }
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

- (void)windowWillClose:(NSNotification *)notification {
    [_timer invalidate];
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
int main(void) {
    @autoreleasepool {
        CaptureOutput *output = [CaptureOutput new];
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
        return 0;
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
