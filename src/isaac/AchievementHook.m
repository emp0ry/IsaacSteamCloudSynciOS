#import <Foundation/Foundation.h>
#import <GameKit/GameKit.h>
#import <objc/runtime.h>
#import "IsaacCloudCore.h"

typedef void (*ICSReportAchievementsIMP)(id, SEL, NSArray<GKAchievement *> *, void (^)(NSError *));
static ICSReportAchievementsIMP ICSOriginalReportAchievements;

static BOOL ICSObserveAchievement(GKAchievement *achievement) {
    if (![achievement isKindOfClass:GKAchievement.class] || achievement.percentComplete < 100.0) return NO;
    NSString *identifier = achievement.identifier;
    NSString *prefix = @"Achievement_";
    if (![identifier hasPrefix:prefix]) return NO;
    NSString *suffix = [identifier substringFromIndex:prefix.length];
    NSCharacterSet *nonDecimal = NSCharacterSet.decimalDigitCharacterSet.invertedSet;
    if (suffix.length == 0 || [suffix rangeOfCharacterFromSet:nonDecimal].location != NSNotFound) return NO;
    unsigned long long value = suffix.longLongValue;
    if (value == 0 || value > UINT32_MAX) return NO;
    if (!ICSCoreStageAchievementId((uint32_t)value)) return NO;
    NSLog(@"[IsaacCloud] achievement: staged GameKit %@", identifier);
    return YES;
}

static void ICSReportAchievements(
    id receiver,
    SEL selector,
    NSArray<GKAchievement *> *achievements,
    void (^completion)(NSError *)
) {
    BOOL stagedAny = NO;
    for (GKAchievement *achievement in achievements) {
        stagedAny |= ICSObserveAchievement(achievement);
    }
    if (stagedAny) ICSCoreSyncAchievements();
    ICSOriginalReportAchievements(receiver, selector, achievements, completion);
}

void ICSInstallAchievementObserver(void) {
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        SEL selector = @selector(reportAchievements:withCompletionHandler:);
        Method method = class_getClassMethod(GKAchievement.class, selector);
        if (method == NULL) {
            NSLog(@"[IsaacCloud] achievement: GameKit report method not found");
            return;
        }
        ICSOriginalReportAchievements = (ICSReportAchievementsIMP)method_getImplementation(method);
        method_setImplementation(method, (IMP)ICSReportAchievements);
        NSLog(@"[IsaacCloud] achievement: GameKit observer installed");

#pragma clang diagnostic push
#pragma clang diagnostic ignored "-Wdeprecated-declarations"
        [GKAchievement loadAchievementsWithCompletionHandler:^(NSArray<GKAchievement *> *items, NSError *error) {
            if (error != nil) {
                NSLog(@"[IsaacCloud] achievement: existing GameKit achievements unavailable: %@", error.localizedDescription);
                return;
            }
            BOOL stagedAny = NO;
            for (GKAchievement *achievement in items) {
                stagedAny |= ICSObserveAchievement(achievement);
            }
            if (stagedAny) ICSCoreSyncAchievements();
        }];
#pragma clang diagnostic pop
    });
}
