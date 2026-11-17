// A copy-only AppKit drag from prepared local content.
#import <AppKit/AppKit.h>
#import <CoreGraphics/CoreGraphics.h>

#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>

using Clock = std::chrono::steady_clock;
using CanWritePromise = bool (*)(void*);
using WritePromise = bool (*)(void*, const char*, void*, CanWritePromise);
using DropContext = void (*)(void*);
using CheckContent = bool (*)(void*);

@interface LinkoNativeCopySource
    : NSObject <NSDraggingSource, NSPasteboardItemDataProvider, NSFilePromiseProviderDelegate> {
@public
    std::atomic<int32_t> outcome;
    std::atomic<bool> allowed;
    std::atomic<bool> started;
    std::atomic<bool> promise_requested;
    std::atomic<int32_t> promise_outcome;
    Clock::time_point deadline;
    NSString* text;
    NSData* png;
    NSString* file_url;
    NSString* promise_name;
    NSOperationQueue* promise_queue;
    void* promise_context;
    WritePromise write_promise;
    DropContext drop_promise;
    void* copy_context;
    CheckContent check_copy;
    DropContext drop_copy;
    std::atomic<bool> copy_read;
}
- (BOOL)offerAvailable;
@end

// AppKit owns the native drag. Keep its source alive on the main thread until
// the terminal callback, even if the Rust UI releases its observation handle.
static LinkoNativeCopySource* active_drag = nil;

static bool can_write_promise(void* owner) {
    auto* source = (__bridge LinkoNativeCopySource*)owner;
    return [source offerAvailable] && source->outcome.load(std::memory_order_acquire) <= 1;
}

@implementation LinkoNativeCopySource
- (instancetype)init {
    self = [super init];
    if (self) {
        outcome.store(0, std::memory_order_relaxed);
        allowed.store(false, std::memory_order_relaxed);
        started.store(false, std::memory_order_relaxed);
        promise_requested.store(false, std::memory_order_relaxed);
        promise_outcome.store(0, std::memory_order_relaxed);
        copy_read.store(false, std::memory_order_relaxed);
    }
    return self;
}

- (void)dealloc {
    if (promise_context) {
        drop_promise(promise_context);
    }
    if (copy_context) {
        drop_copy(copy_context);
    }
}

- (BOOL)offerAvailable {
    return allowed.load(std::memory_order_acquire) && Clock::now() < deadline;
}

- (void)pasteboard:(NSPasteboard*)pasteboard item:(NSPasteboardItem*)item
    provideDataForType:(NSPasteboardType)type {
    (void)pasteboard;
    if (![self offerAvailable]
        || (copy_context && (!check_copy(copy_context) || ![self offerAvailable]))) {
        return;
    }
    BOOL provided = NO;
    if (text && [type isEqualToString:NSPasteboardTypeString]) {
        provided = [item setString:text forType:type];
    } else if (png && [type isEqualToString:NSPasteboardTypePNG]) {
        provided = [item setData:png forType:type];
    } else if (file_url && [type isEqualToString:NSPasteboardTypeFileURL]) {
        [item setString:file_url forType:type];
    }
    if (copy_context && provided) {
        copy_read.store(true, std::memory_order_release);
    }
}

- (NSDragOperation)draggingSession:(NSDraggingSession*)session
    sourceOperationMaskForDraggingContext:(NSDraggingContext)context {
    (void)session;
    (void)context;
    return [self offerAvailable] ? NSDragOperationCopy : NSDragOperationNone;
}

- (void)draggingSession:(NSDraggingSession*)session willBeginAtPoint:(NSPoint)point {
    (void)session;
    (void)point;
    started.store(true, std::memory_order_release);
}

- (void)draggingSession:(NSDraggingSession*)session endedAtPoint:(NSPoint)point
        operation:(NSDragOperation)operation {
    (void)session;
    (void)point;
    const bool copy = operation == NSDragOperationCopy;
    const bool accepted = [self offerAvailable]
        && (!copy_context || copy_read.load(std::memory_order_acquire));
    if (copy_context) {
        allowed.store(false, std::memory_order_release);
    }
    const int32_t result = operation == NSDragOperationNone ? 2 : (copy && accepted ? 1 : 3);
    outcome.store(result, std::memory_order_release);
    if (active_drag == self) {
        active_drag = nil;
    }
}

- (NSString*)filePromiseProvider:(NSFilePromiseProvider*)provider fileNameForType:(NSString*)type {
    (void)provider;
    (void)type;
    return promise_name;
}

- (NSOperationQueue*)operationQueueForFilePromiseProvider:(NSFilePromiseProvider*)provider {
    (void)provider;
    return promise_queue;
}

- (void)filePromiseProvider:(NSFilePromiseProvider*)provider writePromiseToURL:(NSURL*)url
          completionHandler:(void (^)(NSError* error))completionHandler {
    (void)provider;
    if (promise_requested.exchange(true, std::memory_order_acq_rel)) {
        completionHandler([NSError errorWithDomain:NSCocoaErrorDomain code:NSFileWriteUnknownError
                                         userInfo:nil]);
        return;
    }
    void* owner = (__bridge void*)self;
    NSError* error = nil;
    __block bool copied = false;
    if (url.isFileURL && can_write_promise(owner)) {
        NSFileCoordinator* coordinator = [[NSFileCoordinator alloc] initWithFilePresenter:nil];
        [coordinator coordinateWritingItemAtURL:url options:0 error:&error
                                      byAccessor:^(NSURL* destination) {
            const char* path = destination.fileSystemRepresentation;
            if (path && can_write_promise(owner)) {
                copied = write_promise(promise_context, path, owner, can_write_promise);
            }
        }];
    }
    promise_outcome.store(copied && !error ? 1 : 3, std::memory_order_release);
    if (!copied && !error) {
        error = [NSError errorWithDomain:NSCocoaErrorDomain code:NSFileWriteUnknownError
                                userInfo:nil];
    }
    completionHandler(error);
}
@end

static int32_t start_drag(id<NSPasteboardWriting> data, LinkoNativeCopySource* source,
                          void** owner, NSEvent* origin = nil) {
    if (!owner || !data || !source || ![NSThread isMainThread]) {
        return -1;
    }
    *owner = nullptr;
    if (active_drag) {
        return -2;
    }
    NSWindow* window = NSApp.keyWindow;
    NSView* view = window.contentView;
    NSEvent* event = origin ?: NSApp.currentEvent;
    if (!view || !event || event.window != window || event.type != NSEventTypeLeftMouseDown) {
        return -3;
    }
    source->allowed.store(true, std::memory_order_release);
    NSDraggingItem* item = [[NSDraggingItem alloc] initWithPasteboardWriter:data];
    NSPoint point = [view convertPoint:event.locationInWindow fromView:nil];
    NSRect preview = NSMakeRect(point.x - 16, point.y - 16, 32, 32);
    [item setDraggingFrame:preview contents:[NSImage imageNamed:NSImageNameMultipleDocuments]];

    active_drag = source;
    NSDraggingSession* session = [view beginDraggingSessionWithItems:@[item] event:event source:source];
    if (!session) {
        source->allowed.store(false, std::memory_order_release);
        active_drag = nil;
        return -4;
    }
    *owner = (__bridge_retained void*)source;
    return 0;
}

extern "C" int32_t linko_macos_copy_drag_start(
    const uint8_t* path, size_t bytes, uint32_t millis, void** owner) {
    if (!path || bytes == 0 || bytes > 4096 || millis == 0 || millis > 60'000
        || ![NSThread isMainThread]) {
        return -1;
    }
    NSString* name = [[NSString alloc] initWithBytes:path length:bytes encoding:NSUTF8StringEncoding];
    if (!name.isAbsolutePath) {
        return -1;
    }
    NSURL* file = [NSURL fileURLWithPath:name isDirectory:NO];
    NSPasteboardItem* data = [NSPasteboardItem new];
    LinkoNativeCopySource* source = [LinkoNativeCopySource new];
    source->file_url = file.absoluteString;
    source->deadline = Clock::now() + std::chrono::milliseconds(millis);
    if (!source->file_url || ![data setDataProvider:source forTypes:@[NSPasteboardTypeFileURL]]) {
        return -1;
    }
    return start_drag(data, source, owner);
}

extern "C" int32_t linko_macos_file_promise_start(
    const uint8_t* name, size_t bytes, uint32_t millis, void* context, WritePromise write,
    DropContext destroy, void** owner, void* origin) {
    if (!name || bytes == 0 || bytes > 200 || millis == 0 || millis > 60'000 || !context || !write
        || !destroy || ![NSThread isMainThread]) {
        if (context && destroy) {
            destroy(context);
        }
        return -1;
    }
    LinkoNativeCopySource* source = [LinkoNativeCopySource new];
    source->promise_context = context;
    source->write_promise = write;
    source->drop_promise = destroy;
    source->promise_name = [[NSString alloc] initWithBytes:name length:bytes encoding:NSUTF8StringEncoding];
    if (!source->promise_name) {
        return -1;
    }
    source->deadline = Clock::now() + std::chrono::milliseconds(millis);
    source->promise_queue = [NSOperationQueue new];
    source->promise_queue.maxConcurrentOperationCount = 1;
    source->promise_queue.qualityOfService = NSQualityOfServiceUtility;
    NSFilePromiseProvider* data = [[NSFilePromiseProvider alloc] initWithFileType:@"public.data"
                                                                       delegate:source];
    // The provider keeps its delegate alive through the post-drop write request.
    data.userInfo = source;
    return start_drag(data, source, owner, (__bridge NSEvent*)origin);
}

extern "C" void* linko_macos_file_drag_press() {
    if (![NSThread isMainThread]) {
        return nullptr;
    }
    NSEvent* event = NSApp.currentEvent;
    if (!event || !event.window || event.window != NSApp.keyWindow
        || event.type != NSEventTypeLeftMouseDown || !event.CGEvent
        || CGEventGetIntegerValueField(event.CGEvent, kCGEventSourceUserData) != 0x50474c44) {
        return nullptr;
    }
    return (__bridge_retained void*)event;
}

extern "C" void linko_macos_file_drag_press_release(void* press) {
    (void)CFBridgingRelease(press);
}

extern "C" int32_t linko_macos_copy_drag_content(
    const uint8_t* content, size_t bytes, bool is_png, uint32_t millis, void** owner,
    void* origin, void* context, CheckContent check, DropContext destroy) {
    if (!content || bytes == 0 || bytes > (is_png ? 4 : 1) * 1024 * 1024
        || millis == 0 || millis > 60'000 || ![NSThread isMainThread]
        || (context && (!origin || !check || !destroy))) {
        if (context && destroy) {
            destroy(context);
        }
        return -1;
    }
    LinkoNativeCopySource* source = [LinkoNativeCopySource new];
    source->copy_context = context;
    source->check_copy = check;
    source->drop_copy = destroy;
    const auto type = is_png ? NSPasteboardTypePNG : NSPasteboardTypeString;
    if (is_png) {
        source->png = [NSData dataWithBytes:content length:bytes];
    } else {
        source->text = [[NSString alloc] initWithBytes:content length:bytes
                                              encoding:NSUTF8StringEncoding];
        if (!source->text) {
            return -1;
        }
    }

    source->deadline = Clock::now() + std::chrono::milliseconds(millis);
    NSPasteboardItem* item = [NSPasteboardItem new];
    if (![item setDataProvider:source forTypes:@[type]]) {
        return -1;
    }
    return start_drag(item, source, owner, (__bridge NSEvent*)origin);
}

extern "C" int32_t linko_macos_guarded_copy_regression() {
    @autoreleasepool {
        bool released = false;
        LinkoNativeCopySource* source = [LinkoNativeCopySource new];
        source->copy_context = &released;
        source->check_copy = [](void* context) { return *static_cast<bool*>(context); };
        source->drop_copy = [](void*) {};
        source->text = @"copy";
        source->deadline = Clock::now() + std::chrono::seconds(10);
        source->allowed.store(true);
        NSPasteboardItem* early = [NSPasteboardItem new];
        [source pasteboard:nil item:early provideDataForType:NSPasteboardTypeString];
        if ([early stringForType:NSPasteboardTypeString] || source->copy_read.load()) {
            return 1;
        }
        released = true;
        NSPasteboardItem* confirmed = [NSPasteboardItem new];
        [source pasteboard:nil item:confirmed provideDataForType:NSPasteboardTypeString];
        if (![[confirmed stringForType:NSPasteboardTypeString] isEqualToString:@"copy"]
            || !source->copy_read.load()) {
            return 2;
        }
        [source draggingSession:nil endedAtPoint:NSZeroPoint operation:NSDragOperationCopy];
        if (source->outcome.load() != 1) {
            return 3;
        }
        NSPasteboardItem* retired = [NSPasteboardItem new];
        [source pasteboard:nil item:retired provideDataForType:NSPasteboardTypeString];
        if ([retired stringForType:NSPasteboardTypeString]) {
            return 4;
        }
        source->allowed.store(true);
        source->deadline = Clock::now();
        [source pasteboard:nil item:retired provideDataForType:NSPasteboardTypeString];
        return [retired stringForType:NSPasteboardTypeString] ? 5 : 0;
    }
}

extern "C" int32_t linko_macos_copy_drag_outcome(void* owner) {
    if (!owner) {
        return -1;
    }
    auto* source = (__bridge LinkoNativeCopySource*)owner;
    const int32_t operation = source->outcome.load(std::memory_order_acquire);
    if (operation == 1 && source->promise_context) {
        return source->promise_outcome.load(std::memory_order_acquire);
    }
    return operation;
}

extern "C" bool linko_macos_copy_drag_started(void* owner) {
    auto* source = (__bridge LinkoNativeCopySource*)owner;
    return source && source->started.load(std::memory_order_acquire);
}

extern "C" void linko_macos_copy_drag_cancel(void* owner) {
    if (owner) {
        auto* source = (__bridge LinkoNativeCopySource*)owner;
        source->allowed.store(false, std::memory_order_release);
    }
}

extern "C" void linko_macos_copy_drag_release(void* owner) {
    if (owner) {
        LinkoNativeCopySource* source = (__bridge_transfer LinkoNativeCopySource*)owner;
        source->allowed.store(false, std::memory_order_release);
    }
}
