// A copy-only AppKit drag from prepared local content.
#import <AppKit/AppKit.h>

#include <atomic>
#include <chrono>
#include <cstddef>
#include <cstdint>

using Clock = std::chrono::steady_clock;

@interface LinkoNativeCopySource : NSObject <NSDraggingSource, NSPasteboardItemDataProvider> {
@public
    std::atomic<int32_t> outcome;
    std::atomic<bool> allowed;
    Clock::time_point deadline;
    NSString* text;
    NSData* png;
}
- (BOOL)offerAvailable;
@end

// AppKit owns the native drag. Keep its source alive on the main thread until
// the terminal callback, even if the Rust UI releases its observation handle.
static LinkoNativeCopySource* active_drag = nil;

@implementation LinkoNativeCopySource
- (instancetype)init {
    self = [super init];
    if (self) {
        outcome.store(0, std::memory_order_relaxed);
        allowed.store(false, std::memory_order_relaxed);
    }
    return self;
}

- (BOOL)offerAvailable {
    return allowed.load(std::memory_order_acquire) && Clock::now() < deadline;
}

- (void)pasteboard:(NSPasteboard*)pasteboard item:(NSPasteboardItem*)item
    provideDataForType:(NSPasteboardType)type {
    (void)pasteboard;
    if (![self offerAvailable]) {
        return;
    }
    if (text && [type isEqualToString:NSPasteboardTypeString]) {
        [item setString:text forType:type];
    } else if (png && [type isEqualToString:NSPasteboardTypePNG]) {
        [item setData:png forType:type];
    }
}

- (NSDragOperation)draggingSession:(NSDraggingSession*)session
    sourceOperationMaskForDraggingContext:(NSDraggingContext)context {
    (void)session;
    (void)context;
    return [self offerAvailable] ? NSDragOperationCopy : NSDragOperationNone;
}

- (void)draggingSession:(NSDraggingSession*)session endedAtPoint:(NSPoint)point
        operation:(NSDragOperation)operation {
    (void)session;
    (void)point;
    const bool copy = operation == NSDragOperationCopy;
    const bool accepted = [self offerAvailable];
    const int32_t result = operation == NSDragOperationNone ? 2 : (copy && accepted ? 1 : 3);
    outcome.store(result, std::memory_order_release);
    if (active_drag == self) {
        active_drag = nil;
    }
}
@end

static int32_t start_drag(NSPasteboardItem* data, LinkoNativeCopySource* source, void** owner) {
    if (!owner || !data || !source || ![NSThread isMainThread]) {
        return -1;
    }
    *owner = nullptr;
    if (active_drag) {
        return -2;
    }
    NSWindow* window = NSApp.keyWindow;
    NSView* view = window.contentView;
    NSEvent* event = NSApp.currentEvent;
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

extern "C" int32_t linko_macos_copy_drag_start(const uint8_t* path, size_t bytes, void** owner) {
    if (!path || bytes == 0 || bytes > 4096 || ![NSThread isMainThread]) {
        return -1;
    }
    NSString* name = [[NSString alloc] initWithBytes:path length:bytes encoding:NSUTF8StringEncoding];
    if (!name.isAbsolutePath) {
        return -1;
    }
    NSURL* file = [NSURL fileURLWithPath:name isDirectory:NO];
    NSPasteboardItem* data = [NSPasteboardItem new];
    if (![data setString:file.absoluteString forType:NSPasteboardTypeFileURL]) {
        return -1;
    }
    LinkoNativeCopySource* source = [LinkoNativeCopySource new];
    source->deadline = Clock::time_point::max();
    return start_drag(data, source, owner);
}

static int32_t start_content_drag(
    LinkoNativeCopySource* source, NSPasteboardType type, uint32_t millis, void** owner) {
    if (millis == 0 || millis > 60'000) {
        return -1;
    }
    source->deadline = Clock::now() + std::chrono::milliseconds(millis);
    NSPasteboardItem* item = [NSPasteboardItem new];
    if (![item setDataProvider:source forTypes:@[type]]) {
        return -1;
    }
    return start_drag(item, source, owner);
}

extern "C" int32_t linko_macos_copy_drag_text(
    const uint8_t* text, size_t bytes, uint32_t millis, void** owner) {
    if (!text || bytes == 0 || bytes > 1024 * 1024 || ![NSThread isMainThread]) {
        return -1;
    }
    LinkoNativeCopySource* source = [LinkoNativeCopySource new];
    source->text = [[NSString alloc] initWithBytes:text length:bytes encoding:NSUTF8StringEncoding];
    if (!source->text) {
        return -1;
    }
    return start_content_drag(source, NSPasteboardTypeString, millis, owner);
}

extern "C" int32_t linko_macos_copy_drag_png(
    const uint8_t* png, size_t bytes, uint32_t millis, void** owner) {
    if (!png || bytes == 0 || bytes > 4 * 1024 * 1024 || ![NSThread isMainThread]) {
        return -1;
    }
    LinkoNativeCopySource* source = [LinkoNativeCopySource new];
    source->png = [NSData dataWithBytes:png length:bytes];
    return start_content_drag(source, NSPasteboardTypePNG, millis, owner);
}

extern "C" int32_t linko_macos_copy_drag_outcome(void* owner) {
    if (!owner) {
        return -1;
    }
    auto* source = (__bridge LinkoNativeCopySource*)owner;
    return source->outcome.load(std::memory_order_acquire);
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
