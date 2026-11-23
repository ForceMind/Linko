// An Actions-only owner for one self-created AppKit window.
#import <AppKit/AppKit.h>
#include <errno.h>
#include <fcntl.h>
#include <stdbool.h>
#include <stdio.h>
#include <string.h>
#include <unistd.h>

struct Owner {
    __unsafe_unretained NSWindow *window;
    char incarnation[33];
    NSRect original;
    unsigned long long parked;
    unsigned long long restored;
    unsigned restores;
};

static void snapshot(struct Owner const *owner) {
    const NSRect bounds = owner->window.frame;
    printf("snapshot %d %u %s 1 %.17g %.17g %.17g %.17g\n",
           owner->window.isVisible, owner->restores, owner->incarnation,
           bounds.origin.x, bounds.origin.y, bounds.size.width, bounds.size.height);
}

static bool command(struct Owner *owner, const char *line) {
    if (strcmp(line, "inspect") == 0) {
        snapshot(owner);
        return true;
    }
    char action[16], incarnation[33], trailing;
    unsigned long long transaction, generation;
    double x, y, width, height;
    if (sscanf(line, "%15s %llu %32s %llu %lf %lf %lf %lf %c", action, &transaction,
               incarnation, &generation, &x, &y, &width, &height, &trailing) != 8 ||
        transaction == 0 || strcmp(incarnation, owner->incarnation) != 0 ||
        generation != 1 || !NSEqualRects(NSMakeRect(x, y, width, height), owner->original)) {
        puts("rejected");
        return true;
    }
    NSWindow *window = owner->window;
    if (strcmp(action, "hide") == 0 && owner->parked == 0 &&
        transaction > owner->restored && window.isVisible) {
        [window orderOut:nil];
        const NSRect shifted = NSOffsetRect(owner->original, 48, 32);
        [window setFrame:shifted display:NO animate:NO];
        if (window.isVisible || !NSEqualRects(shifted, window.frame)) {
            return false;
        }
        owner->parked = transaction;
        printf("hidden %llu\n", transaction);
        return true;
    }
    if (strcmp(action, "restore") == 0 && owner->parked == transaction && !window.isVisible) {
        [window setFrame:owner->original display:NO animate:NO];
        [window orderFrontRegardless];
        if (!window.isVisible || !NSEqualRects(owner->original, window.frame)) {
            return false;
        }
        owner->parked = 0;
        owner->restored = transaction;
        ++owner->restores;
    }
    if (strcmp(action, "restore") == 0 && owner->parked == 0 && owner->restored == transaction) {
        printf("restored %llu\n", transaction);
    } else {
        puts("rejected");
    }
    return true;
}

static bool run(struct Owner *owner) {
    char line[257];
    size_t used = 0;
    for (;;) {
        @autoreleasepool {
            [NSRunLoop.currentRunLoop runUntilDate:[NSDate dateWithTimeIntervalSinceNow:0.01]];
        }
        char bytes[256];
        const ssize_t count = read(STDIN_FILENO, bytes, sizeof(bytes));
        if (count < 0 && (errno == EAGAIN || errno == EINTR)) {
            continue;
        }
        if (count <= 0) {
            return false;
        }
        for (ssize_t index = 0; index < count; ++index) {
            if (bytes[index] == '\n') {
                line[used] = '\0';
                if (strcmp(line, "exit") == 0) {
                    return true;
                }
                if (!command(owner, line)) {
                    return false;
                }
                used = 0;
            } else if (bytes[index] != '\r') {
                if (used >= sizeof(line) - 1) {
                    return false;
                }
                line[used++] = bytes[index];
            }
        }
    }
}

int main(void) {
    @autoreleasepool {
        const int flags = fcntl(STDIN_FILENO, F_GETFL);
        if (flags < 0 || fcntl(STDIN_FILENO, F_SETFL, flags | O_NONBLOCK) < 0 ||
            setvbuf(stdout, NULL, _IOLBF, 0) != 0) {
            return 1;
        }
        [NSApplication sharedApplication].activationPolicy = NSApplicationActivationPolicyRegular;
        [NSApp finishLaunching];
        NSScreen *screen = NSScreen.mainScreen;
        if (!screen) {
            fputs("AppKit recovery fixture requires a display.\n", stderr);
            return 1;
        }
        const NSRect display = screen.visibleFrame;
        const NSRect content = NSMakeRect(NSMinX(display) + 80, NSMinY(display) + 80, 320, 220);
        NSWindow *window = [[NSWindow alloc] initWithContentRect:content
                                                       styleMask:NSWindowStyleMaskTitled
                                                         backing:NSBackingStoreBuffered defer:NO];
        window.releasedWhenClosed = NO;
        window.title = @"Linko recovery fixture";
        [window orderFrontRegardless];
        NSString *incarnation = [[NSUUID.UUID.UUIDString stringByReplacingOccurrencesOfString:@"-"
                                                                                  withString:@""] lowercaseString];
        struct Owner owner = {.window = window, .original = window.frame};
        const bool ready = window.isVisible && incarnation.length == 32 &&
                           strlcpy(owner.incarnation, incarnation.UTF8String, sizeof(owner.incarnation)) == 32;
        if (ready) {
            puts("ready");
        }
        const bool passed = ready && run(&owner);
        [window close];
        if (!passed) {
            fputs("AppKit window owner did not complete the recovery fixture.\n", stderr);
        }
        return passed ? 0 : 1;
    }
}
