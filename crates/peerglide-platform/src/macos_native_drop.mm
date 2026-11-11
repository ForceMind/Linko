// A copy-only AppKit drag from a prepared local regular file.
#import <AppKit/AppKit.h>

#include <atomic>
#include <cstddef>
#include <cstdint>

@interface LinkoNativeCopySource : NSObject <NSDraggingSource> {
@public
    std::atomic<int32_t> outcome;
    std::atomic<bool> allowed;
}
@end

// AppKit owns the native drag. Keep its source alive on the main thread until
// the terminal callback, even if the Rust UI releases its observation handle.
static LinkoNativeCopySource* active_drag = nil;

@implementation LinkoNativeCopySource
- (NSDragOperation)draggingSession:(NSDraggingSession*)session
    sourceOperationMaskForDraggingContext:(NSDraggingContext)context {
    (void)session;
    (void)context;
    return allowed.load(std::memory_order_acquire) ? NSDragOperationCopy : NSDragOperationNone;
}

- (void)draggingSession:(NSDraggingSession*)session endedAtPoint:(NSPoint)point
        operation:(NSDragOperation)operation {
    (void)session;
    (void)point;
    const bool copy = operation == NSDragOperationCopy;
    const bool accepted = allowed.load(std::memory_order_acquire);
    const int32_t result = operation == NSDragOperationNone ? 2 : (copy && accepted ? 1 : 3);
    outcome.store(result, std::memory_order_release);
    if (active_drag == self) {
        active_drag = nil;
    }
}
@end

extern "C" int32_t linko_macos_copy_drag_start(const uint8_t* path, size_t bytes, void** owner) {
    if (!owner || !path || bytes == 0 || bytes > 4096 || ![NSThread isMainThread]) {
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
    NSString* name = [[NSString alloc] initWithBytes:path length:bytes encoding:NSUTF8StringEncoding];
    if (!name.isAbsolutePath) {
        return -1;
    }
    NSURL* file = [NSURL fileURLWithPath:name isDirectory:NO];
    NSPasteboardItem* data = [NSPasteboardItem new];
    if (![data setString:file.absoluteString forType:NSPasteboardTypeFileURL]) {
        return -1;
    }
    NSDraggingItem* item = [[NSDraggingItem alloc] initWithPasteboardWriter:data];
    NSPoint point = [view convertPoint:event.locationInWindow fromView:nil];
    NSRect preview = NSMakeRect(point.x - 16, point.y - 16, 32, 32);
    [item setDraggingFrame:preview contents:[NSImage imageNamed:NSImageNameMultipleDocuments]];

    LinkoNativeCopySource* source = [LinkoNativeCopySource new];
    source->outcome.store(0, std::memory_order_relaxed);
    source->allowed.store(true, std::memory_order_relaxed);
    active_drag = source;
    NSDraggingSession* session = [view beginDraggingSessionWithItems:@[item] event:event source:source];
    if (!session) {
        active_drag = nil;
        return -4;
    }
    *owner = (__bridge_retained void*)source;
    return 0;
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
