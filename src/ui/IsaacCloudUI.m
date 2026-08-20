#import <UIKit/UIKit.h>
#import <CoreImage/CoreImage.h>
#import "../loaders/IsaacCloudCore.h"

static NSString *const ICSInvisibleButtonDefaultsKey = @"IsaacCloudSyncInvisibleMenuButton";
static BOOL ICSQRPresentationScheduled = NO;
static BOOL ICSQRDismissedByUser = NO;
static __weak UINavigationController *ICSActivePanelNavigation;

@interface ICSQRCodeViewController : UIViewController
@property(nonatomic) UIImageView *codeImageView;
@property(nonatomic) NSString *challengeURL;
- (void)updateChallengeURL:(NSString *)url;
- (void)openSteamMobile;
@end

static __weak ICSQRCodeViewController *ICSActiveQRController;

@implementation ICSQRCodeViewController

- (void)viewDidLoad {
    [super viewDidLoad];
    self.title = @"Approve with Steam Mobile";
    self.view.backgroundColor = UIColor.systemBackgroundColor;
    self.codeImageView = [UIImageView new];
    self.codeImageView.contentMode = UIViewContentModeScaleAspectFit;
    self.codeImageView.translatesAutoresizingMaskIntoConstraints = NO;
    [self.view addSubview:self.codeImageView];
    UIButton *openButton = [UIButton buttonWithType:UIButtonTypeSystem];
    openButton.translatesAutoresizingMaskIntoConstraints = NO;
    [openButton setTitle:@"Open in Steam Mobile" forState:UIControlStateNormal];
    [openButton addTarget:self action:@selector(openSteamMobile) forControlEvents:UIControlEventTouchUpInside];
    [self.view addSubview:openButton];
    [NSLayoutConstraint activateConstraints:@[
        [self.codeImageView.centerXAnchor constraintEqualToAnchor:self.view.centerXAnchor],
        [self.codeImageView.centerYAnchor constraintEqualToAnchor:self.view.centerYAnchor constant:-24],
        [self.codeImageView.widthAnchor constraintEqualToConstant:280],
        [self.codeImageView.heightAnchor constraintEqualToConstant:280],
        [openButton.topAnchor constraintEqualToAnchor:self.codeImageView.bottomAnchor constant:12],
        [openButton.centerXAnchor constraintEqualToAnchor:self.view.centerXAnchor],
    ]];
    [self updateChallengeURL:self.challengeURL];
}

- (void)openSteamMobile {
    NSURL *url = [NSURL URLWithString:self.challengeURL ?: @""];
    if (url != nil) {
        [UIApplication.sharedApplication openURL:url options:@{} completionHandler:nil];
    }
}

- (void)updateChallengeURL:(NSString *)url {
    if (![url isKindOfClass:NSString.class] || url.length == 0) return;
    self.challengeURL = url;
    if (!self.isViewLoaded) return;
    NSData *data = [url dataUsingEncoding:NSUTF8StringEncoding];
    CIFilter *filter = [CIFilter filterWithName:@"CIQRCodeGenerator"];
    [filter setValue:data forKey:@"inputMessage"];
    [filter setValue:@"M" forKey:@"inputCorrectionLevel"];
    CIImage *output = [filter.outputImage imageByApplyingTransform:CGAffineTransformMakeScale(8, 8)];
    self.codeImageView.image = [UIImage imageWithCIImage:output];
}

- (void)viewDidDisappear:(BOOL)animated {
    [super viewDidDisappear:animated];
    if (self.isMovingFromParentViewController && ICSActiveQRController == self) {
        ICSQRDismissedByUser = YES;
        ICSActiveQRController = nil;
    }
}

@end

static BOOL ICSButtonIsInvisible(void) {
    return [NSUserDefaults.standardUserDefaults boolForKey:ICSInvisibleButtonDefaultsKey];
}

static NSDictionary *ICSReadDictionary(char *(*copyFunction)(void)) {
    char *json = copyFunction();
    if (json == NULL) return @{};
    NSData *data = [NSData dataWithBytes:json length:strlen(json)];
    ICSCoreFreeString(json);
    id object = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
    return [object isKindOfClass:NSDictionary.class] ? object : @{};
}

static NSArray *ICSReadArray(char *(*copyFunction)(void)) {
    char *json = copyFunction();
    if (json == NULL) return @[];
    NSData *data = [NSData dataWithBytes:json length:strlen(json)];
    ICSCoreFreeString(json);
    id object = [NSJSONSerialization JSONObjectWithData:data options:0 error:nil];
    return [object isKindOfClass:NSArray.class] ? object : @[];
}

static UIViewController *ICSTopController(void) {
    UIWindow *window = nil;
    if (@available(iOS 13.0, *)) {
        for (UIScene *scene in UIApplication.sharedApplication.connectedScenes) {
            if (![scene isKindOfClass:UIWindowScene.class]) continue;
            for (UIWindow *candidate in ((UIWindowScene *)scene).windows) {
                if (candidate.isKeyWindow) { window = candidate; break; }
            }
            if (window != nil) break;
        }
    }
    UIViewController *controller = window.rootViewController;
    while (controller.presentedViewController != nil) controller = controller.presentedViewController;
    if ([controller isKindOfClass:UINavigationController.class]) controller = ((UINavigationController *)controller).topViewController;
    if ([controller isKindOfClass:UITabBarController.class]) controller = ((UITabBarController *)controller).selectedViewController;
    return controller;
}

static NSString *ICSShortHash(id value) {
    if (![value isKindOfClass:NSString.class]) return @"—";
    NSString *hash = value;
    return hash.length > 12 ? [hash substringToIndex:12] : hash;
}

static NSString *ICSDateForMilliseconds(id value) {
    if (![value respondsToSelector:@selector(doubleValue)] || value == NSNull.null) return @"unknown time";
    NSDate *date = [NSDate dateWithTimeIntervalSince1970:[value doubleValue] / 1000.0];
    return [NSDateFormatter localizedStringFromDate:date dateStyle:NSDateFormatterShortStyle timeStyle:NSDateFormatterMediumStyle];
}

static NSDictionary *ICSFindSlot(NSArray *items, NSUInteger slot) {
    for (NSDictionary *item in items) {
        if ([item[@"slot"] unsignedIntegerValue] == slot) return item;
    }
    return nil;
}

@interface ICSPanelViewController : UITableViewController
@property(nonatomic) NSDictionary *status;
@property(nonatomic) NSArray *backups;
@property(nonatomic) NSTimer *timer;
@property(nonatomic) BOOL qrPresented;
@property(nonatomic) BOOL guardPromptPresented;
@end

@implementation ICSPanelViewController

- (void)viewDidLoad {
    [super viewDidLoad];
    self.title = @"Isaac Steam Sync iOS";
    self.navigationItem.rightBarButtonItem = [[UIBarButtonItem alloc] initWithBarButtonSystemItem:UIBarButtonSystemItemClose target:self action:@selector(closePanel)];
    [self.tableView registerClass:UITableViewCell.class forCellReuseIdentifier:@"cell"];
    [self refresh];
    self.timer = [NSTimer scheduledTimerWithTimeInterval:0.75 target:self selector:@selector(refresh) userInfo:nil repeats:YES];
}

- (void)dealloc { [self.timer invalidate]; }
- (void)closePanel { [self dismissViewControllerAnimated:YES completion:nil]; }

- (NSArray<NSString *> *)actionRows {
    NSString *phase = [self.status[@"phase"] isKindOfClass:NSString.class] ? self.status[@"phase"] : nil;
    NSString *guard = [self.status[@"guard_kind"] isKindOfClass:NSString.class] ? self.status[@"guard_kind"] : nil;
    if ([phase isEqualToString:@"awaiting_steam_guard"] || [phase isEqualToString:@"connecting"]) {
        if ([guard isEqualToString:@"qr"]) return @[@"Continue QR Sign-In", @"Cancel Steam Sign-In"];
        if ([guard isEqualToString:@"email_code"] || [guard isEqualToString:@"device_code"])
            return @[@"Enter Steam Guard Code", @"Cancel Steam Sign-In"];
        if ([guard isEqualToString:@"device_confirmation"])
            return @[@"Steam Mobile Approval Help", @"Cancel Steam Sign-In"];
        return @[@"Cancel Steam Sign-In"];
    }
    if ([self.status[@"account_connected"] boolValue]) {
        return @[@"Disconnect Steam Account", @"Sync Now", @"Force Push…", @"Force Pull…"];
    }
    return @[@"Connect Steam Account"];
}

- (void)refresh {
    self.status = ICSReadDictionary(ICSCoreCopyStatusJSON);
    self.backups = ICSReadArray(ICSCoreCopyBackupsJSON);
    [self.tableView reloadData];
    NSString *qrURL = self.status[@"qr_url"];
    BOOL awaitingGuard = [self.status[@"phase"] isKindOfClass:NSString.class]
        && [self.status[@"phase"] isEqualToString:@"awaiting_steam_guard"];
    if ([qrURL isKindOfClass:NSString.class] && qrURL.length != 0) {
        dispatch_async(dispatch_get_main_queue(), ^{
            if (ICSActiveQRController != nil) {
                [ICSActiveQRController updateChallengeURL:qrURL];
            } else if (!ICSQRPresentationScheduled && !ICSQRDismissedByUser) {
                ICSQRPresentationScheduled = YES;
                [self showQR];
            }
        });
    } else if (!awaitingGuard) {
        self.qrPresented = NO;
        dispatch_async(dispatch_get_main_queue(), ^{
            ICSQRPresentationScheduled = NO;
            ICSQRDismissedByUser = NO;
            if (ICSActiveQRController != nil
                && ICSActiveQRController.navigationController.topViewController == ICSActiveQRController) {
                [ICSActiveQRController.navigationController popViewControllerAnimated:YES];
            }
            ICSActiveQRController = nil;
        });
    }
    id guardValue = self.status[@"guard_kind"];
    NSString *guardKind = [guardValue isKindOfClass:NSString.class] ? guardValue : nil;
    BOOL needsCode = [guardKind isEqualToString:@"email_code"] || [guardKind isEqualToString:@"device_code"];
    BOOL needsDeviceApproval = [guardKind isEqualToString:@"device_confirmation"];
    if (needsCode && !self.guardPromptPresented) {
        self.guardPromptPresented = YES;
        dispatch_async(dispatch_get_main_queue(), ^{ [self promptForSteamGuard:guardKind]; });
    } else if (needsDeviceApproval && !self.guardPromptPresented) {
        self.guardPromptPresented = YES;
        dispatch_async(dispatch_get_main_queue(), ^{
            [self alert:@"Approve with Steam Mobile" message:@"Open the Steam app on your trusted phone and approve this sign-in. IsaacSteamSynciOS will continue automatically."];
        });
    } else if (!awaitingGuard) {
        self.guardPromptPresented = NO;
    }
}

- (NSInteger)numberOfSectionsInTableView:(UITableView *)tableView { return 6; }

- (NSInteger)tableView:(UITableView *)tableView numberOfRowsInSection:(NSInteger)section {
    switch (section) {
        case 0: return 5;
        case 1: return self.actionRows.count;
        case 2: return 3;
        case 3: return [self.status[@"pending_choices"] count];
        case 4: return MIN((NSUInteger)20, self.backups.count);
        case 5: return 2;
        default: return 0;
    }
}

- (NSString *)tableView:(UITableView *)tableView titleForHeaderInSection:(NSInteger)section {
    switch (section) {
        case 0: return @"Status";
        case 1: return @"Actions";
        case 2: return @"Save Identities";
        case 3: return @"Conflicts / First Sync";
        case 4: return @"Verified Backups";
        case 5: return @"Interface & Diagnostics";
        default: return nil;
    }
}

- (UITableViewCell *)tableView:(UITableView *)tableView cellForRowAtIndexPath:(NSIndexPath *)indexPath {
    UITableViewCell *cell = [tableView dequeueReusableCellWithIdentifier:@"cell" forIndexPath:indexPath];
    cell.textLabel.numberOfLines = 2;
    cell.detailTextLabel.text = nil;
    cell.accessoryType = UITableViewCellAccessoryNone;
    cell.textLabel.textColor = UIColor.labelColor;

    if (indexPath.section == 0) {
        NSArray *labels = @[@"State", @"Detail", @"Steam account", @"Steam presence", @"Last successful check"];
        id value = @"—";
        if (indexPath.row == 0) value = self.status[@"phase"] ?: @"—";
        if (indexPath.row == 1) value = self.status[@"detail"] ?: @"—";
        if (indexPath.row == 2) {
            if ([self.status[@"authenticated"] boolValue]) {
                value = [NSString stringWithFormat:@"SteamID %@", self.status[@"steam_id"] ?: @"unknown"];
            } else if ([self.status[@"account_connected"] boolValue]) {
                value = @"Connected account saved • currently offline";
            } else {
                value = @"Not connected";
            }
        }
        if (indexPath.row == 3) value = [self.status[@"steam_playing"] boolValue]
            ? @"Playing The Binding of Isaac: Rebirth" : @"Not advertised";
        if (indexPath.row == 4 && self.status[@"last_sync_unix_ms"] != NSNull.null) {
            NSTimeInterval seconds = [self.status[@"last_sync_unix_ms"] doubleValue] / 1000.0;
            value = [NSDateFormatter localizedStringFromDate:[NSDate dateWithTimeIntervalSince1970:seconds] dateStyle:NSDateFormatterShortStyle timeStyle:NSDateFormatterMediumStyle];
        }
        cell.textLabel.text = [NSString stringWithFormat:@"%@\n%@", labels[indexPath.row], value];
        return cell;
    }

    if (indexPath.section == 1) {
        NSString *action = self.actionRows[indexPath.row];
        cell.textLabel.text = action;
        cell.textLabel.textColor = [action isEqualToString:@"Disconnect Steam Account"] || [action isEqualToString:@"Force Pull…"]
            ? UIColor.systemRedColor : UIColor.labelColor;
        cell.accessoryType = UITableViewCellAccessoryDisclosureIndicator;
        return cell;
    }

    if (indexPath.section == 2) {
        NSUInteger slot = indexPath.row + 1;
        NSDictionary *local = ICSFindSlot(self.status[@"local_saves"], slot);
        NSDictionary *remote = ICSFindSlot(self.status[@"remote_saves"], slot);
        cell.textLabel.text = [NSString stringWithFormat:@"Slot %lu\niPhone %@ • Steam %@",
            (unsigned long)slot, ICSShortHash(local[@"sha256"]), ICSShortHash(remote[@"sha256"] ?: remote[@"steam_sha1"])];
        return cell;
    }

    if (indexPath.section == 3) {
        NSDictionary *choice = self.status[@"pending_choices"][indexPath.row];
        NSDictionary *local = choice[@"local"];
        NSDictionary *remote = choice[@"remote"];
        cell.textLabel.text = [NSString stringWithFormat:@"Slot %@ — %@\niPhone %@  •  Steam %@",
            choice[@"slot"], choice[@"kind"], ICSShortHash(local[@"sha256"]), ICSShortHash(remote[@"sha256"])];
        cell.accessoryType = UITableViewCellAccessoryDisclosureIndicator;
        return cell;
    }

    if (indexPath.section == 4) {
        NSDictionary *backup = self.backups[indexPath.row];
        NSDate *date = [NSDate dateWithTimeIntervalSince1970:[backup[@"created_unix_ms"] doubleValue] / 1000.0];
        cell.textLabel.text = [NSString stringWithFormat:@"Slot %@ — %@\n%@ • %@ bytes • %@",
            backup[@"slot"], backup[@"source"],
            [NSDateFormatter localizedStringFromDate:date dateStyle:NSDateFormatterShortStyle timeStyle:NSDateFormatterShortStyle],
            backup[@"size"], ICSShortHash(backup[@"sha256"])];
        cell.accessoryType = UITableViewCellAccessoryDisclosureIndicator;
        return cell;
    }

    cell.textLabel.text = indexPath.row == 0 ? @"Export Structured Log"
        : (ICSButtonIsInvisible() ? @"Menu Button: Invisible (tap area remains)" : @"Menu Button: Visible");
    cell.accessoryType = UITableViewCellAccessoryDisclosureIndicator;
    return cell;
}

- (void)tableView:(UITableView *)tableView didSelectRowAtIndexPath:(NSIndexPath *)indexPath {
    [tableView deselectRowAtIndexPath:indexPath animated:YES];
    if (indexPath.section == 1) {
        NSString *action = self.actionRows[indexPath.row];
        if ([action isEqualToString:@"Connect Steam Account"]) {
            [self presentConnectOptions];
        } else if ([action isEqualToString:@"Continue QR Sign-In"]) {
            ICSQRDismissedByUser = NO;
            [self showQR];
        } else if ([action isEqualToString:@"Cancel Steam Sign-In"]) {
            ICSQRDismissedByUser = YES;
            ICSCoreCancelSteamLogin();
        } else if ([action isEqualToString:@"Enter Steam Guard Code"]) {
            self.guardPromptPresented = NO;
            [self promptForSteamGuard:self.status[@"guard_kind"]];
        } else if ([action isEqualToString:@"Steam Mobile Approval Help"]) {
            [self alert:@"Approve with Steam Mobile" message:@"Open Steam on the trusted phone for this account, open the pending sign-in request, and tap Approve. No QR is used for Login + Password."];
        } else if ([action isEqualToString:@"Disconnect Steam Account"]) {
            [self confirm:@"Disconnect Steam?" message:@"The refresh token will be removed from Keychain. Local saves and backups remain." destructive:YES action:^{ ICSCoreDisconnectSteam(); }];
        } else if ([action isEqualToString:@"Sync Now"]) {
            [self confirm:@"Sync iPhone and Steam now?" message:@"IsaacSteamSynciOS compares iPhone, Steam, and the last common BASE. iPhone LZ4 data is converted to the normal Windows format before upload, the current Steam copy is backed up, and a true conflict is never overwritten automatically. It then compares native Isaac save achievements with Steam and adds only missing unlocks. Existing Steam achievements are never cleared, and Game Center is not used." destructive:NO action:^{ ICSCoreSyncNow("manual"); }];
        } else if ([action isEqualToString:@"Force Push…"]) {
            [self chooseSlotForLocal:YES];
        } else if ([action isEqualToString:@"Force Pull…"]) {
            [self chooseSlotForLocal:NO];
        }
    } else if (indexPath.section == 3) {
        [self resolveChoice:self.status[@"pending_choices"][indexPath.row]];
    } else if (indexPath.section == 4) {
        NSDictionary *backup = self.backups[indexPath.row];
        [self confirm:@"Restore this backup?" message:@"The backup will be verified now, then restored before game initialization on your next launch. The current local save is backed up before replacement." destructive:YES action:^{
            ICSCoreRestoreBackup([backup[@"backup_id"] UTF8String]);
        }];
    } else if (indexPath.section == 5 && indexPath.row == 0) {
        NSString *path = [NSHomeDirectory() stringByAppendingPathComponent:@"Library/Application Support/IsaacCloudSync/logs/isaaccloud.ndjson"];
        NSURL *url = [NSURL fileURLWithPath:path];
        UIActivityViewController *activity = [[UIActivityViewController alloc] initWithActivityItems:@[url] applicationActivities:nil];
        activity.popoverPresentationController.sourceView = self.view;
        activity.popoverPresentationController.sourceRect = CGRectMake(CGRectGetMidX(self.view.bounds), CGRectGetMidY(self.view.bounds), 1, 1);
        [self presentViewController:activity animated:YES completion:nil];
    } else if (indexPath.section == 5 && indexPath.row == 1) {
        BOOL invisible = !ICSButtonIsInvisible();
        [NSUserDefaults.standardUserDefaults setBool:invisible forKey:ICSInvisibleButtonDefaultsKey];
        UIButton *button = (UIButton *)[ICSTopController().view.window viewWithTag:0x495343];
        button.backgroundColor = invisible ? UIColor.clearColor : [UIColor.systemBackgroundColor colorWithAlphaComponent:0.82];
        button.layer.borderWidth = invisible ? 0 : 1;
        [button setTitle:invisible ? @"" : @"☁︎" forState:UIControlStateNormal];
        [self.tableView reloadData];
    }
}

- (void)presentConnectOptions {
    UIAlertController *sheet = [UIAlertController alertControllerWithTitle:@"Connect Steam Account" message:@"Choose a secure sign-in method. Passwords and Steam Guard codes are used in memory only and are never saved or logged." preferredStyle:UIAlertControllerStyleActionSheet];
    [sheet addAction:[UIAlertAction actionWithTitle:@"QR Code / Steam Mobile" style:UIAlertActionStyleDefault handler:^(__unused UIAlertAction *action) {
        self.qrPresented = NO;
        ICSQRDismissedByUser = NO;
        ICSCoreConnectSteam();
    }]];
    [sheet addAction:[UIAlertAction actionWithTitle:@"Login + Password" style:UIAlertActionStyleDefault handler:^(__unused UIAlertAction *action) {
        [self promptForCredentials];
    }]];
    [sheet addAction:[UIAlertAction actionWithTitle:@"Cancel" style:UIAlertActionStyleCancel handler:nil]];
    sheet.popoverPresentationController.sourceView = self.view;
    sheet.popoverPresentationController.sourceRect = CGRectMake(CGRectGetMidX(self.view.bounds), CGRectGetMidY(self.view.bounds), 1, 1);
    [self presentViewController:sheet animated:YES completion:nil];
}

- (void)promptForCredentials {
    UIAlertController *alert = [UIAlertController alertControllerWithTitle:@"Steam Login" message:@"Steam may ask for Mobile approval or a Steam Guard code next." preferredStyle:UIAlertControllerStyleAlert];
    [alert addTextFieldWithConfigurationHandler:^(UITextField *field) {
        field.placeholder = @"Steam account name";
        field.textContentType = UITextContentTypeUsername;
        field.autocapitalizationType = UITextAutocapitalizationTypeNone;
        field.autocorrectionType = UITextAutocorrectionTypeNo;
    }];
    [alert addTextFieldWithConfigurationHandler:^(UITextField *field) {
        field.placeholder = @"Steam password";
        field.secureTextEntry = YES;
        field.textContentType = UITextContentTypePassword;
    }];
    [alert addAction:[UIAlertAction actionWithTitle:@"Cancel" style:UIAlertActionStyleCancel handler:nil]];
    [alert addAction:[UIAlertAction actionWithTitle:@"Sign In" style:UIAlertActionStyleDefault handler:^(__unused UIAlertAction *action) {
        UITextField *accountField = alert.textFields.firstObject;
        UITextField *passwordField = alert.textFields.lastObject;
        NSString *account = accountField.text ?: @"";
        NSString *password = passwordField.text ?: @"";
        if (account.length == 0 || password.length == 0) {
            [self alert:@"Missing login" message:@"Enter both your Steam account name and password."];
            return;
        }
        ICSCoreConnectSteamWithPassword(account.UTF8String, password.UTF8String);
        accountField.text = nil;
        passwordField.text = nil;
    }]];
    [self presentViewController:alert animated:YES completion:nil];
}

- (void)promptForSteamGuard:(NSString *)kind {
    NSString *message = [kind isEqualToString:@"email_code"]
        ? @"Enter the code Steam sent to your email." : @"Enter your current Steam Guard code.";
    UIAlertController *alert = [UIAlertController alertControllerWithTitle:@"Steam Guard" message:message preferredStyle:UIAlertControllerStyleAlert];
    [alert addTextFieldWithConfigurationHandler:^(UITextField *field) {
        field.placeholder = @"Steam Guard code";
        field.autocapitalizationType = UITextAutocapitalizationTypeAllCharacters;
        field.autocorrectionType = UITextAutocorrectionTypeNo;
        if (@available(iOS 12.0, *)) field.textContentType = UITextContentTypeOneTimeCode;
    }];
    [alert addAction:[UIAlertAction actionWithTitle:@"Cancel" style:UIAlertActionStyleCancel handler:^(__unused UIAlertAction *action) {
        self.guardPromptPresented = NO;
    }]];
    [alert addAction:[UIAlertAction actionWithTitle:@"Continue" style:UIAlertActionStyleDefault handler:^(__unused UIAlertAction *action) {
        NSString *code = alert.textFields.firstObject.text ?: @"";
        if (!ICSCoreSubmitSteamGuardCode(code.UTF8String)) {
            self.guardPromptPresented = NO;
            [self alert:@"Code not accepted" message:@"The authentication session is no longer waiting for a code. Start sign-in again."];
        }
        alert.textFields.firstObject.text = nil;
    }]];
    [self presentViewController:alert animated:YES completion:nil];
}

- (void)chooseSlotForLocal:(BOOL)useLocal {
    NSArray *saves = self.status[@"local_saves"] ?: @[];
    NSString *message = useLocal
        ? @"This bypasses normal conflict protection. Existing versions are backed up, native iOS LZ4 data is converted to Windows format, the upload is verified, and only then does BASE change."
        : @"This bypasses normal conflict protection. Existing versions are backed up now; the selected Steam version is hash-pinned and applied before game initialization on your next launch.";
    UIAlertController *sheet = [UIAlertController alertControllerWithTitle:useLocal ? @"Force Push" : @"Force Pull" message:message preferredStyle:UIAlertControllerStyleActionSheet];
    for (NSDictionary *save in saves) {
        uint8_t slot = [save[@"slot"] unsignedCharValue];
        [sheet addAction:[UIAlertAction actionWithTitle:[NSString stringWithFormat:@"Slot %u", slot] style:UIAlertActionStyleDestructive handler:^(__unused UIAlertAction *action) {
            ICSCoreForce(slot, useLocal);
        }]];
    }
    [sheet addAction:[UIAlertAction actionWithTitle:@"Cancel" style:UIAlertActionStyleCancel handler:nil]];
    sheet.popoverPresentationController.sourceView = self.view;
    sheet.popoverPresentationController.sourceRect = CGRectMake(CGRectGetMidX(self.view.bounds), CGRectGetMidY(self.view.bounds), 1, 1);
    [self presentViewController:sheet animated:YES completion:nil];
}

- (void)resolveChoice:(NSDictionary *)choice {
    uint8_t slot = [choice[@"slot"] unsignedCharValue];
    NSDictionary *local = choice[@"local"];
    NSDictionary *remote = choice[@"remote"];
    BOOL remoteExists = [remote[@"size"] unsignedLongLongValue] != 0;
    NSString *message = [NSString stringWithFormat:@"iPhone: %@\n%@ bytes • %@\nSHA-256 %@\n\nSteam: %@\n%@ bytes • %@\nSHA-256 %@\n\n%@ Never choose based on timestamps alone.",
        local[@"filename"] ?: @"unknown", local[@"size"], ICSDateForMilliseconds(local[@"modified_unix_ms"]), local[@"sha256"] ?: @"—",
        remote[@"filename"] ?: @"unknown", remote[@"size"], ICSDateForMilliseconds(remote[@"modified_unix_ms"]), remote[@"sha256"] ?: @"—",
        remoteExists ? @"Both copies have been backed up." : @"The iPhone copy has been backed up; Steam has no corresponding file."];
    UIAlertController *sheet = [UIAlertController alertControllerWithTitle:[NSString stringWithFormat:@"Choose Slot %u", slot] message:message preferredStyle:UIAlertControllerStyleActionSheet];
    [sheet addAction:[UIAlertAction actionWithTitle:@"Use iPhone Save" style:UIAlertActionStyleDestructive handler:^(__unused UIAlertAction *action) { ICSCoreResolve(slot, true); }]];
    if (remoteExists) {
        [sheet addAction:[UIAlertAction actionWithTitle:@"Use Steam Save" style:UIAlertActionStyleDestructive handler:^(__unused UIAlertAction *action) { ICSCoreResolve(slot, false); }]];
    }
    [sheet addAction:[UIAlertAction actionWithTitle:@"Cancel" style:UIAlertActionStyleCancel handler:nil]];
    sheet.popoverPresentationController.sourceView = self.view;
    sheet.popoverPresentationController.sourceRect = CGRectMake(CGRectGetMidX(self.view.bounds), CGRectGetMidY(self.view.bounds), 1, 1);
    [self presentViewController:sheet animated:YES completion:nil];
}

- (void)showQR {
    NSString *url = self.status[@"qr_url"];
    if (![url isKindOfClass:NSString.class] || url.length == 0) {
        ICSQRPresentationScheduled = NO;
        return;
    }
    self.qrPresented = YES;
    if (ICSActiveQRController != nil) {
        [ICSActiveQRController updateChallengeURL:url];
        ICSQRPresentationScheduled = NO;
        return;
    }
    ICSQRCodeViewController *controller = [ICSQRCodeViewController new];
    ICSActiveQRController = controller;
    [controller updateChallengeURL:url];
    ICSQRPresentationScheduled = NO;
    [self.navigationController pushViewController:controller animated:YES];
}

- (void)confirm:(NSString *)title message:(NSString *)message destructive:(BOOL)destructive action:(dispatch_block_t)action {
    UIAlertController *alert = [UIAlertController alertControllerWithTitle:title message:message preferredStyle:UIAlertControllerStyleAlert];
    [alert addAction:[UIAlertAction actionWithTitle:@"Cancel" style:UIAlertActionStyleCancel handler:nil]];
    [alert addAction:[UIAlertAction actionWithTitle:@"Continue" style:destructive ? UIAlertActionStyleDestructive : UIAlertActionStyleDefault handler:^(__unused UIAlertAction *selected) { action(); }]];
    [self presentViewController:alert animated:YES completion:nil];
}

- (void)alert:(NSString *)title message:(NSString *)message {
    UIAlertController *alert = [UIAlertController alertControllerWithTitle:title message:message preferredStyle:UIAlertControllerStyleAlert];
    [alert addAction:[UIAlertAction actionWithTitle:@"OK" style:UIAlertActionStyleDefault handler:nil]];
    [self presentViewController:alert animated:YES completion:nil];
}

@end

static void ICSPresentPanel(void) {
    if (!ICSGameMenuIsActive()) return;
    UIViewController *top = ICSTopController();
    if (top == nil || [top isKindOfClass:ICSPanelViewController.class]) return;
    ICSPanelViewController *panel = [[ICSPanelViewController alloc] initWithStyle:UITableViewStyleInsetGrouped];
    UINavigationController *navigation = [[UINavigationController alloc] initWithRootViewController:panel];
    navigation.modalPresentationStyle = UIModalPresentationFormSheet;
    ICSActivePanelNavigation = navigation;
    [top presentViewController:navigation animated:YES completion:nil];
}

@interface ICSButtonTarget : NSObject
+ (instancetype)shared;
- (void)openPanel;
@end

@implementation ICSButtonTarget
+ (instancetype)shared {
    static ICSButtonTarget *target;
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{ target = [ICSButtonTarget new]; });
    return target;
}
- (void)openPanel { ICSPresentPanel(); }
@end

static BOOL ICSAttentionPresented = NO;
static BOOL ICSStartupPreflightPending = YES;
static const NSInteger ICSStartupGateTag = 0x495347;

static BOOL ICSPhaseIsBusy(NSString *phase) {
    return [@[@"syncing", @"connecting", @"forcing", @"resolving", @"restoring"] containsObject:phase];
}

static void ICSUpdateSettingsVisibility(void) {
    UIViewController *top = ICSTopController();
    UIWindow *window = top.view.window;
    BOOL menuActive = ICSGameMenuIsActive();
    UIButton *button = (UIButton *)[window viewWithTag:0x495343];
    button.hidden = !menuActive;
    if (menuActive) return;

    UINavigationController *navigation = ICSActivePanelNavigation;
    if (navigation.presentingViewController != nil) {
        [navigation dismissViewControllerAnimated:YES completion:^{
            if (ICSActivePanelNavigation == navigation) ICSActivePanelNavigation = nil;
        }];
    }
}

static void ICSRemoveStartupGate(void) {
    UIWindow *window = ICSTopController().view.window;
    [[window viewWithTag:ICSStartupGateTag] removeFromSuperview];
}

static void ICSUpdateStartupGate(void) {
    UIWindow *window = ICSTopController().view.window;
    if (window == nil) return;
    NSDictionary *status = ICSReadDictionary(ICSCoreCopyStatusJSON);
    if (!ICSStartupPreflightPending || !ICSPhaseIsBusy(status[@"phase"])) {
        ICSStartupPreflightPending = NO;
        ICSRemoveStartupGate();
        return;
    }
    if ([window viewWithTag:ICSStartupGateTag] != nil) return;

    UIView *gate = [UIView new];
    gate.tag = ICSStartupGateTag;
    gate.translatesAutoresizingMaskIntoConstraints = NO;
    gate.backgroundColor = [UIColor.systemBackgroundColor colorWithAlphaComponent:0.94];

    UIActivityIndicatorView *spinner = [[UIActivityIndicatorView alloc]
        initWithActivityIndicatorStyle:UIActivityIndicatorViewStyleLarge];
    spinner.translatesAutoresizingMaskIntoConstraints = NO;
    [spinner startAnimating];

    UILabel *title = [UILabel new];
    title.translatesAutoresizingMaskIntoConstraints = NO;
    title.text = @"Checking Steam Cloud…";
    title.font = [UIFont preferredFontForTextStyle:UIFontTextStyleHeadline];
    title.textAlignment = NSTextAlignmentCenter;

    UILabel *detail = [UILabel new];
    detail.translatesAutoresizingMaskIntoConstraints = NO;
    detail.text = @"Isaac will continue with the valid local save if Steam is unavailable.";
    detail.font = [UIFont preferredFontForTextStyle:UIFontTextStyleSubheadline];
    detail.textColor = UIColor.secondaryLabelColor;
    detail.textAlignment = NSTextAlignmentCenter;
    detail.numberOfLines = 0;

    UIStackView *stack = [[UIStackView alloc] initWithArrangedSubviews:@[spinner, title, detail]];
    stack.translatesAutoresizingMaskIntoConstraints = NO;
    stack.axis = UILayoutConstraintAxisVertical;
    stack.alignment = UIStackViewAlignmentCenter;
    stack.spacing = 12;
    [gate addSubview:stack];
    [window addSubview:gate];
    [NSLayoutConstraint activateConstraints:@[
        [gate.leadingAnchor constraintEqualToAnchor:window.leadingAnchor],
        [gate.trailingAnchor constraintEqualToAnchor:window.trailingAnchor],
        [gate.topAnchor constraintEqualToAnchor:window.topAnchor],
        [gate.bottomAnchor constraintEqualToAnchor:window.bottomAnchor],
        [stack.centerXAnchor constraintEqualToAnchor:gate.centerXAnchor],
        [stack.centerYAnchor constraintEqualToAnchor:gate.centerYAnchor],
        [stack.leadingAnchor constraintGreaterThanOrEqualToAnchor:gate.safeAreaLayoutGuide.leadingAnchor constant:24],
        [stack.trailingAnchor constraintLessThanOrEqualToAnchor:gate.safeAreaLayoutGuide.trailingAnchor constant:-24],
    ]];
    ICSCoreLog("ui", "startup preflight gate installed");
}

static void ICSRefreshAttention(void) {
    ICSUpdateSettingsVisibility();
    NSDictionary *status = ICSReadDictionary(ICSCoreCopyStatusJSON);
    NSString *phase = status[@"phase"];
    UIButton *button = (UIButton *)[ICSTopController().view.window viewWithTag:0x495343];
    BOOL needsChoice = [phase isEqualToString:@"conflict"] || [phase isEqualToString:@"first_sync"];
    BOOL needsRestart = [phase isEqualToString:@"restart_required"];
    button.layer.borderColor = needsChoice ? UIColor.systemRedColor.CGColor
        : needsRestart ? UIColor.systemOrangeColor.CGColor : UIColor.systemBlueColor.CGColor;
    if (ICSGameMenuIsActive() && needsChoice && !ICSAttentionPresented &&
        ICSTopController() != nil) {
        ICSAttentionPresented = YES;
        ICSPresentPanel();
    }
}

static void ICSInstallButton(void) {
    UIWindow *window = ICSTopController().view.window;
    if (window == nil || [window viewWithTag:0x495343] != nil) return;
    UIButton *button = [UIButton buttonWithType:UIButtonTypeSystem];
    button.tag = 0x495343;
    button.translatesAutoresizingMaskIntoConstraints = NO;
    BOOL invisible = ICSButtonIsInvisible();
    button.backgroundColor = invisible ? UIColor.clearColor : [UIColor.systemBackgroundColor colorWithAlphaComponent:0.82];
    button.layer.cornerRadius = 20;
    button.layer.borderColor = UIColor.systemBlueColor.CGColor;
    button.layer.borderWidth = invisible ? 0 : 1;
    button.titleLabel.font = [UIFont systemFontOfSize:20 weight:UIFontWeightSemibold];
    [button setTitle:invisible ? @"" : @"☁︎" forState:UIControlStateNormal];
    button.accessibilityLabel = @"Open Isaac Steam Sync iOS";
    [button addTarget:ICSButtonTarget.shared action:@selector(openPanel) forControlEvents:UIControlEventTouchUpInside];
    [window addSubview:button];
    button.hidden = !ICSGameMenuIsActive();
    [NSLayoutConstraint activateConstraints:@[
        [button.trailingAnchor constraintEqualToAnchor:window.safeAreaLayoutGuide.trailingAnchor constant:-8],
        [button.topAnchor constraintEqualToAnchor:window.safeAreaLayoutGuide.topAnchor constant:8],
        [button.widthAnchor constraintEqualToConstant:40],
        [button.heightAnchor constraintEqualToConstant:40],
    ]];
    ICSCoreLog("ui", "overlay button installed");
}

void ICSInstallUI(void) {
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        [NSNotificationCenter.defaultCenter addObserverForName:UIApplicationDidBecomeActiveNotification object:nil queue:NSOperationQueue.mainQueue usingBlock:^(__unused NSNotification *note) {
            ICSInstallButton();
            ICSUpdateStartupGate();
            ICSRefreshAttention();
        }];
        [NSNotificationCenter.defaultCenter addObserverForName:@"IsaacCloudSyncPreflightFinished" object:nil queue:NSOperationQueue.mainQueue usingBlock:^(__unused NSNotification *note) {
            ICSStartupPreflightPending = NO;
            ICSRemoveStartupGate();
            ICSRefreshAttention();
        }];
        [NSNotificationCenter.defaultCenter addObserverForName:@"IsaacCloudSyncGameStateChanged" object:nil queue:NSOperationQueue.mainQueue usingBlock:^(__unused NSNotification *note) {
            ICSInstallButton();
            ICSUpdateSettingsVisibility();
            ICSRefreshAttention();
        }];
        [NSTimer scheduledTimerWithTimeInterval:1.0 repeats:YES block:^(__unused NSTimer *timer) {
            ICSInstallButton();
            ICSUpdateStartupGate();
            ICSRefreshAttention();
        }];
        dispatch_async(dispatch_get_main_queue(), ^{
            ICSInstallButton();
            ICSUpdateStartupGate();
            ICSRefreshAttention();
        });
        ICSCoreLog("ui", "adapter installed");
    });
}
