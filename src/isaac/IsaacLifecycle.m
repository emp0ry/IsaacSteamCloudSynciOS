#import <UIKit/UIKit.h>
#import "../loaders/IsaacCloudCore.h"

static UIBackgroundTaskIdentifier ICSBackgroundTask = (UIBackgroundTaskIdentifier)-1;
static dispatch_source_t ICSBackgroundPollTimer;

static NSDictionary *ICSStatus(void) {
    char *json = ICSCoreCopyStatusJSON();
    if (json == NULL) return @{};
    NSData *data = [[NSData alloc] initWithBytes:json length:strlen(json)];
    ICSCoreFreeString(json);
    id value = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
    return [value isKindOfClass:NSDictionary.class] ? value : @{};
}

static void ICSEndBackgroundTask(void) {
    if (ICSBackgroundPollTimer != nil) {
        dispatch_source_cancel(ICSBackgroundPollTimer);
        ICSBackgroundPollTimer = nil;
    }
    if (ICSBackgroundTask != UIBackgroundTaskInvalid) {
        [[UIApplication sharedApplication] endBackgroundTask:ICSBackgroundTask];
        ICSBackgroundTask = UIBackgroundTaskInvalid;
    }
}

static BOOL ICSOperationIsBusy(void) {
    NSString *phase = ICSStatus()[@"phase"];
    NSSet *busy = [NSSet setWithArray:@[@"syncing", @"connecting", @"forcing", @"resolving"]];
    return [busy containsObject:phase];
}

static void ICSProtectActiveSyncInBackground(void) {
    // Do not start a new upload merely because Isaac was backgrounded. The
    // user explicitly starts phone -> Steam publication with Sync Now. If
    // that operation is already running, request enough background time for
    // its verified upload/commit to finish.
    if (!ICSOperationIsBusy()) return;
    UIApplication *application = UIApplication.sharedApplication;
    ICSEndBackgroundTask();
    ICSBackgroundTask = [application beginBackgroundTaskWithName:@"IsaacCloudSync" expirationHandler:^{
        ICSEndBackgroundTask();
    }];
    dispatch_queue_t queue = dispatch_get_global_queue(QOS_CLASS_UTILITY, 0);
    ICSBackgroundPollTimer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, queue);
    dispatch_source_set_timer(ICSBackgroundPollTimer, dispatch_time(DISPATCH_TIME_NOW, NSEC_PER_SEC), NSEC_PER_SEC, NSEC_PER_MSEC * 100);
    dispatch_source_set_event_handler(ICSBackgroundPollTimer, ^{
        if (!ICSOperationIsBusy()) ICSEndBackgroundTask();
    });
    dispatch_resume(ICSBackgroundPollTimer);
}

void ICSInstallLifecycleAdapter(void) {
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        NSNotificationCenter *center = NSNotificationCenter.defaultCenter;
        [center addObserverForName:UIApplicationDidEnterBackgroundNotification object:nil queue:NSOperationQueue.mainQueue usingBlock:^(__unused NSNotification *note) {
            NSLog(@"[IsaacCloud] lifecycle: background");
            ICSCoreLog("lifecycle", "background");
            ICSCoreSetForeground(false);
            ICSProtectActiveSyncInBackground();
        }];
        [center addObserverForName:UIApplicationWillEnterForegroundNotification object:nil queue:NSOperationQueue.mainQueue usingBlock:^(__unused NSNotification *note) {
            NSLog(@"[IsaacCloud] lifecycle: foreground");
            ICSCoreLog("lifecycle", "foreground");
            ICSCoreSetForeground(true);
            ICSCoreSyncNow("foreground");
        }];
        [center addObserverForName:UIApplicationWillTerminateNotification object:nil queue:NSOperationQueue.mainQueue usingBlock:^(__unused NSNotification *note) {
            NSLog(@"[IsaacCloud] lifecycle: terminate");
            ICSCoreLog("lifecycle", "terminate");
            ICSCoreSetForeground(false);
        }];
        [center addObserverForName:UIApplicationProtectedDataDidBecomeAvailable object:nil queue:NSOperationQueue.mainQueue usingBlock:^(__unused NSNotification *note) {
            ICSCoreLog("lifecycle", "protected-data-available");
        }];
        ICSCoreLog("lifecycle", "adapter installed");
    });
}
