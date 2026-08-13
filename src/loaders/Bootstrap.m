#import <Foundation/Foundation.h>
#import "IsaacCloudCore.h"

extern void ICSInstallLifecycleAdapter(void);
extern void ICSInstallUI(void);
extern void ICSInstallGameStateDetector(void);

static const uint64_t ICSPrelaunchTimeoutMilliseconds = 25000;
static NSString *const ICSPreflightFinishedNotification = @"IsaacCloudSyncPreflightFinished";

__attribute__((constructor))
static void IsaacCloudSyncBootstrap(void) {
    @autoreleasepool {
        NSString *home = NSHomeDirectory();
        if (ICSCoreStart(home.fileSystemRepresentation) != 0) {
            NSLog(@"[IsaacCloud] bootstrap: core initialization failed");
            return;
        }

        // Start the portable pre-load operation before UIApplicationMain, but do
        // not block the launch thread: iOS enforces a 20-second launch watchdog.
        // The UIKit adapter gates user input until this bounded worker finishes.
        dispatch_async(dispatch_get_global_queue(QOS_CLASS_USER_INITIATED, 0), ^{
            BOOL completed = ICSCorePreflight(ICSPrelaunchTimeoutMilliseconds);
            ICSCoreLog(
                "bootstrap",
                completed ? "preflight completed" : "preflight timed out; local play allowed"
            );
            dispatch_async(dispatch_get_main_queue(), ^{
                [NSNotificationCenter.defaultCenter
                    postNotificationName:ICSPreflightFinishedNotification
                    object:nil];
            });
        });

        dispatch_async(dispatch_get_main_queue(), ^{
            ICSInstallLifecycleAdapter();
            ICSInstallGameStateDetector();
            ICSInstallUI();
        });
    }
}
