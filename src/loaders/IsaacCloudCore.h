#ifndef ISAAC_CLOUD_CORE_H
#define ISAAC_CLOUD_CORE_H

#include <stdbool.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

int32_t ICSCoreStart(const char *home);
bool ICSCoreConnectSteam(void);
bool ICSCoreConnectSteamWithPassword(const char *account, const char *password);
bool ICSCoreCancelSteamLogin(void);
bool ICSCoreSubmitSteamGuardCode(const char *code);
bool ICSCoreDisconnectSteam(void);
bool ICSCoreSetForeground(bool foreground);
bool ICSCoreSyncNow(const char *trigger);
void ICSCoreLog(const char *category, const char *message);
bool ICSCoreResolve(uint8_t slot, bool useLocal);
bool ICSCoreForce(uint8_t slot, bool useLocal);
bool ICSCoreRestoreBackup(const char *backupID);
bool ICSCorePreflight(uint64_t timeoutMilliseconds);
char *ICSCoreCopyStatusJSON(void);
char *ICSCoreCopyBackupsJSON(void);
void ICSCoreFreeString(char *value);

void ICSInstallGameStateDetector(void);
bool ICSGameMenuIsActive(void);

#ifdef __cplusplus
}
#endif

#endif
