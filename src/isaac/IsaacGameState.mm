#import <Foundation/Foundation.h>
#import <mach-o/dyld.h>
#import <mach-o/loader.h>
#import <mach/mach.h>

#import "../loaders/IsaacCloudCore.h"

#include <algorithm>
#include <array>
#include <atomic>
#include <cmath>
#include <cstring>
#include <vector>

namespace {
constexpr const char *kPlayerRTTIName = "N15IsaacRepentance13Entity_PlayerE";
constexpr const char *kSupportedUUID = "F4357753-A25F-30EE-BACF-63709F902895";
constexpr size_t kMaxVTables = 8;
constexpr size_t kEntityTypeOffset = 0x38;
constexpr size_t kEntityPositionOffset = 0x310;
constexpr vm_size_t kReadChunk = 2 * 1024 * 1024;
constexpr vm_size_t kGamePlayerVectorScanLimit = 512 * 1024;
constexpr uintptr_t kGameGlobalOffset = 0xac3b90;
constexpr unsigned kMenuDebounceObservations = 12;
NSString *const kGameStateChangedNotification = @"IsaacCloudSyncGameStateChanged";

struct ScanContext {
    std::array<uintptr_t, kMaxVTables> playerVTables{};
    size_t playerVTableCount = 0;
};

struct RegionResult {
    vm_address_t address = 0;
    vm_size_t size = 0;
    size_t playerVTableReferences = 0;
    size_t validPlayerCount = 0;
    vm_address_t firstValidPlayerAddress = 0;
};

std::atomic<int> gGameState{-1}; // -1 unknown, 0 menu, 1 gameplay
dispatch_source_t gScanTimer;
ScanContext gContext;
vm_address_t gPlayerRegionAddress = 0;
vm_size_t gPlayerRegionSize = 0;
size_t gPlayerVectorOffset = SIZE_MAX;
unsigned gMenuObservations = 0;

static NSString *UUIDForHeader(const mach_header_64 *header) {
    if (!header || header->magic != MH_MAGIC_64 || header->ncmds > 65536 ||
        header->sizeofcmds > 64 * 1024 * 1024) return @"UNKNOWN";
    const uint8_t *cursor = reinterpret_cast<const uint8_t *>(header + 1);
    const uint8_t *end = cursor + header->sizeofcmds;
    for (uint32_t index = 0; index < header->ncmds; ++index) {
        if (cursor > end || static_cast<size_t>(end - cursor) < sizeof(load_command)) break;
        const load_command *command = reinterpret_cast<const load_command *>(cursor);
        if (command->cmdsize < sizeof(load_command) ||
            static_cast<size_t>(end - cursor) < command->cmdsize) break;
        if (command->cmd == LC_UUID && command->cmdsize >= sizeof(uuid_command)) {
            const unsigned char *value =
                reinterpret_cast<const uuid_command *>(cursor)->uuid;
            return [NSString stringWithFormat:
                @"%02X%02X%02X%02X-%02X%02X-%02X%02X-%02X%02X-%02X%02X%02X%02X%02X%02X",
                value[0], value[1], value[2], value[3], value[4], value[5],
                value[6], value[7], value[8], value[9], value[10], value[11],
                value[12], value[13], value[14], value[15]];
        }
        cursor += command->cmdsize;
    }
    return @"UNKNOWN";
}

static const mach_header_64 *IsaacHeader(intptr_t *slideOut) {
    for (uint32_t index = 0; index < _dyld_image_count(); ++index) {
        const mach_header_64 *header = reinterpret_cast<const mach_header_64 *>(
            _dyld_get_image_header(index));
        if ([UUIDForHeader(header) caseInsensitiveCompare:
                [NSString stringWithUTF8String:kSupportedUUID]] == NSOrderedSame) {
            if (slideOut) *slideOut = _dyld_get_image_vmaddr_slide(index);
            return header;
        }
    }
    if (slideOut) *slideOut = 0;
    return nullptr;
}

static void ForEachIsaacSegment(
    void (^block)(const uint8_t *address, size_t size, vm_prot_t protection)) {
    intptr_t slide = 0;
    const mach_header_64 *header = IsaacHeader(&slide);
    if (!header) return;
    const uint8_t *cursor = reinterpret_cast<const uint8_t *>(header + 1);
    const uint8_t *end = cursor + header->sizeofcmds;
    for (uint32_t index = 0; index < header->ncmds; ++index) {
        if (cursor > end || static_cast<size_t>(end - cursor) < sizeof(load_command)) break;
        const load_command *command = reinterpret_cast<const load_command *>(cursor);
        if (command->cmdsize < sizeof(load_command) ||
            static_cast<size_t>(end - cursor) < command->cmdsize) break;
        if (command->cmd == LC_SEGMENT_64) {
            const segment_command_64 *segment =
                reinterpret_cast<const segment_command_64 *>(cursor);
            if (segment->vmsize && strcmp(segment->segname, "__LINKEDIT") != 0) {
                block(reinterpret_cast<const uint8_t *>(segment->vmaddr + slide),
                      static_cast<size_t>(segment->vmsize), segment->initprot);
            }
        }
        cursor += command->cmdsize;
    }
}

static void LocatePlayerVTables(ScanContext& context) {
    __block uintptr_t typeNameAddress = 0;
    const size_t nameLength = strlen(kPlayerRTTIName) + 1;
    ForEachIsaacSegment(^(const uint8_t *address, size_t size, vm_prot_t protection) {
        if (typeNameAddress || !(protection & VM_PROT_READ) ||
            (protection & VM_PROT_WRITE)) return;
        for (size_t offset = 0; offset + nameLength <= size; ++offset) {
            if (memcmp(address + offset, kPlayerRTTIName, nameLength) == 0) {
                typeNameAddress = reinterpret_cast<uintptr_t>(address + offset);
                return;
            }
        }
    });
    if (!typeNameAddress) return;

    __block std::array<uintptr_t, kMaxVTables> typeInfos{};
    __block size_t typeInfoCount = 0;
    ForEachIsaacSegment(^(const uint8_t *address, size_t size, vm_prot_t protection) {
        if (!(protection & VM_PROT_READ)) return;
        for (size_t offset = sizeof(uintptr_t); offset + sizeof(uintptr_t) <= size;
             offset += sizeof(uintptr_t)) {
            uintptr_t value = 0;
            memcpy(&value, address + offset, sizeof(value));
            if (value == typeNameAddress && typeInfoCount < typeInfos.size()) {
                typeInfos[typeInfoCount++] =
                    reinterpret_cast<uintptr_t>(address + offset - sizeof(uintptr_t));
            }
        }
    });

    ForEachIsaacSegment(^(const uint8_t *address, size_t size, vm_prot_t protection) {
        if (!(protection & VM_PROT_READ)) return;
        for (size_t offset = sizeof(uintptr_t); offset + 2 * sizeof(uintptr_t) <= size;
             offset += sizeof(uintptr_t)) {
            uintptr_t value = 0;
            memcpy(&value, address + offset, sizeof(value));
            for (size_t typeIndex = 0; typeIndex < typeInfoCount; ++typeIndex) {
                if (value != typeInfos[typeIndex] ||
                    context.playerVTableCount >= context.playerVTables.size()) continue;
                intptr_t offsetToTop = -1;
                uintptr_t firstMethod = 0;
                memcpy(&offsetToTop, address + offset - sizeof(uintptr_t), sizeof(offsetToTop));
                memcpy(&firstMethod, address + offset + sizeof(uintptr_t), sizeof(firstMethod));
                if (offsetToTop == 0 && firstMethod != 0) {
                    context.playerVTables[context.playerVTableCount++] =
                        reinterpret_cast<uintptr_t>(address + offset + sizeof(uintptr_t));
                }
            }
        }
    });
}

static bool IsPlayerVTable(uintptr_t value) {
    for (size_t index = 0; index < gContext.playerVTableCount; ++index) {
        if (gContext.playerVTables[index] == value) return true;
    }
    return false;
}

static bool PlausiblePosition(float x, float y) {
    return std::isfinite(x) && std::isfinite(y) && x > -4096 && x < 4096 &&
        y > -4096 && y < 4096;
}

static bool ReadOwnTaskMemory(vm_address_t address, void *destination, vm_size_t size) {
    if (!address || !destination || !size) return false;
    vm_size_t copied = 0;
    return vm_read_overwrite(mach_task_self(), address, size,
                             reinterpret_cast<vm_address_t>(destination), &copied) == KERN_SUCCESS &&
        copied == size;
}

static bool ReadGameObjectAddress(vm_address_t& gameAddress) {
    gameAddress = 0;
    const mach_header_64 *header = IsaacHeader(nullptr);
    uintptr_t game = 0;
    if (!header || !ReadOwnTaskMemory(
            reinterpret_cast<vm_address_t>(header) + kGameGlobalOffset,
            &game, sizeof(game)) || !game) return false;
    gameAddress = static_cast<vm_address_t>(game);
    return true;
}

struct RemotePointerVector {
    uintptr_t begin = 0;
    uintptr_t end = 0;
    uintptr_t capacity = 0;
};

static bool ReadPlayerObject(vm_address_t address) {
    uintptr_t vtable = 0;
    if (!ReadOwnTaskMemory(address, &vtable, sizeof(vtable)) ||
        !IsPlayerVTable(vtable)) return false;

    std::array<uint8_t, kEntityPositionOffset + 2 * sizeof(float)> object{};
    if (!ReadOwnTaskMemory(address, object.data(), object.size())) return false;
    int32_t identity[3]{};
    float x = 0;
    float y = 0;
    memcpy(identity, object.data() + kEntityTypeOffset, sizeof(identity));
    memcpy(&x, object.data() + kEntityPositionOffset, sizeof(x));
    memcpy(&y, object.data() + kEntityPositionOffset + sizeof(float), sizeof(y));
    return identity[0] == 1 && identity[1] == 0 && identity[2] >= 0 &&
        identity[2] < 100 && PlausiblePosition(x, y);
}

static bool ReadPlayerVector(vm_address_t vectorAddress, bool allowEmpty,
                             RegionResult& result) {
    RemotePointerVector remote;
    if (!ReadOwnTaskMemory(vectorAddress, &remote, sizeof(remote))) return false;
    if (!remote.begin && !remote.end && !remote.capacity) return allowEmpty;
    if (!remote.begin || remote.end < remote.begin || remote.capacity < remote.end ||
        (remote.end - remote.begin) % sizeof(uintptr_t) != 0 ||
        (remote.capacity - remote.begin) % sizeof(uintptr_t) != 0) return false;

    size_t count = (remote.end - remote.begin) / sizeof(uintptr_t);
    size_t capacity = (remote.capacity - remote.begin) / sizeof(uintptr_t);
    if (!count) return allowEmpty && capacity <= 64;
    if (count > 8 || capacity < count || capacity > 64) return false;

    std::array<uintptr_t, 8> addresses{};
    if (!ReadOwnTaskMemory(remote.begin, addresses.data(),
                           static_cast<vm_size_t>(count * sizeof(uintptr_t)))) return false;
    RegionResult players;
    for (size_t index = 0; index < count; ++index) {
        if (!addresses[index] ||
            !ReadPlayerObject(static_cast<vm_address_t>(addresses[index]))) return false;
        if (!players.firstValidPlayerAddress) {
            players.firstValidPlayerAddress = static_cast<vm_address_t>(addresses[index]);
        }
        players.validPlayerCount++;
    }
    result = players;
    return true;
}

static bool ResolvePlayersFromGame(RegionResult& result) {
    vm_address_t game = 0;
    if (!ReadGameObjectAddress(game)) return false;

    if (gPlayerVectorOffset != SIZE_MAX) {
        if (ReadPlayerVector(game + gPlayerVectorOffset, true, result)) return true;
        gPlayerVectorOffset = SIZE_MAX;
    }

    vm_address_t regionAddress = game;
    vm_size_t regionSize = 0;
    vm_region_basic_info_data_64_t info{};
    mach_msg_type_number_t infoCount = VM_REGION_BASIC_INFO_COUNT_64;
    mach_port_t objectName = MACH_PORT_NULL;
    kern_return_t status = vm_region_64(
        mach_task_self(), &regionAddress, &regionSize, VM_REGION_BASIC_INFO_64,
        reinterpret_cast<vm_region_info_t>(&info), &infoCount, &objectName);
    if (objectName != MACH_PORT_NULL) mach_port_deallocate(mach_task_self(), objectName);
    if (status != KERN_SUCCESS || regionAddress > game ||
        !(info.protection & VM_PROT_READ) || regionSize <= game - regionAddress) return false;

    vm_size_t available = regionSize - (game - regionAddress);
    vm_size_t scanSize = std::min(available, kGamePlayerVectorScanLimit);
    if (scanSize < sizeof(RemotePointerVector)) return false;
    std::vector<uint8_t> gameBytes(static_cast<size_t>(scanSize));
    if (!ReadOwnTaskMemory(game, gameBytes.data(), scanSize)) return false;

    for (size_t offset = 0; offset + sizeof(RemotePointerVector) <= gameBytes.size();
         offset += sizeof(uintptr_t)) {
        RemotePointerVector remote;
        memcpy(&remote, gameBytes.data() + offset, sizeof(remote));
        if (!remote.begin || remote.end <= remote.begin || remote.capacity < remote.end ||
            (remote.end - remote.begin) % sizeof(uintptr_t) != 0) continue;
        size_t count = (remote.end - remote.begin) / sizeof(uintptr_t);
        if (!count || count > 8) continue;
        RegionResult candidate;
        if (ReadPlayerVector(game + offset, false, candidate)) {
            gPlayerVectorOffset = offset;
            result = candidate;
            ICSCoreLog("ui", "native PlayerManager list resolved");
            return true;
        }
    }
    return false;
}

static void ScanCopy(const uint8_t *bytes, size_t size, size_t scanLimit,
                     vm_address_t sourceAddress, RegionResult& result) {
    scanLimit = std::min(scanLimit, size);
    for (size_t offset = 0;
         offset < scanLimit && offset + kEntityPositionOffset + 2 * sizeof(float) <= size;
         offset += sizeof(uintptr_t)) {
        uintptr_t vtable = 0;
        memcpy(&vtable, bytes + offset, sizeof(vtable));
        if (!IsPlayerVTable(vtable)) continue;
        result.playerVTableReferences++;

        int32_t identity[3]{};
        float x = 0;
        float y = 0;
        memcpy(identity, bytes + offset + kEntityTypeOffset, sizeof(identity));
        memcpy(&x, bytes + offset + kEntityPositionOffset, sizeof(x));
        memcpy(&y, bytes + offset + kEntityPositionOffset + sizeof(float), sizeof(y));
        if (identity[0] == 1 && identity[1] == 0 && identity[2] >= 0 &&
            identity[2] < 100 && PlausiblePosition(x, y)) {
            result.validPlayerCount++;
            if (!result.firstValidPlayerAddress) {
                result.firstValidPlayerAddress = sourceAddress + offset;
            }
        }
    }
}

static bool ScanRegion(vm_address_t address, vm_size_t size, RegionResult& result) {
    result.address = address;
    result.size = size;
    if (!address || !size || size > 512ull * 1024ull * 1024ull) return false;
    std::vector<uint8_t> buffer(static_cast<size_t>(std::min(size, kReadChunk)) + 4096);
    vm_size_t consumed = 0;
    while (consumed < size) {
        vm_size_t request = std::min(kReadChunk, size - consumed);
        vm_size_t overlap = consumed + request < size
            ? std::min(static_cast<vm_size_t>(4096), size - consumed - request) : 0;
        vm_size_t copied = 0;
        kern_return_t status = vm_read_overwrite(
            mach_task_self(), address + consumed, request + overlap,
            reinterpret_cast<vm_address_t>(buffer.data()), &copied);
        if (status != KERN_SUCCESS || copied < sizeof(uintptr_t)) return false;
        ScanCopy(buffer.data(), static_cast<size_t>(copied), static_cast<size_t>(request),
                 address + consumed, result);
        consumed += request;
    }
    return true;
}

static RegionResult DiscoverPlayerRegion(void) {
    RegionResult best;
    vm_address_t address = 0;
    natural_t depth = 0;
    while (true) {
        vm_size_t size = 0;
        vm_region_submap_info_data_64_t info{};
        mach_msg_type_number_t count = VM_REGION_SUBMAP_INFO_COUNT_64;
        kern_return_t status = vm_region_recurse_64(
            mach_task_self(), &address, &size, &depth,
            reinterpret_cast<vm_region_recurse_info_t>(&info), &count);
        if (status != KERN_SUCCESS) break;
        if (info.is_submap) {
            depth++;
            continue;
        }
        vm_address_t next = address + size;
        bool readableHeap = (info.protection & VM_PROT_READ) &&
            (info.protection & VM_PROT_WRITE) && size >= PAGE_SIZE &&
            size <= 512ull * 1024ull * 1024ull;
        if (readableHeap) {
            RegionResult candidate;
            if (ScanRegion(address, size, candidate) &&
                (candidate.validPlayerCount > best.validPlayerCount ||
                 (candidate.validPlayerCount == best.validPlayerCount &&
                  candidate.playerVTableReferences > best.playerVTableReferences))) {
                best = candidate;
            }
        }
        if (next <= address) break;
        address = next;
    }
    return best;
}

static void PublishObservation(bool gameplayActive) {
    int nextState = gameplayActive ? 1 : 0;
    if (gameplayActive) {
        gMenuObservations = 0;
    } else if (gMenuObservations < kMenuDebounceObservations) {
        gMenuObservations++;
        if (gMenuObservations < kMenuDebounceObservations) return;
    }
    int previous = gGameState.exchange(nextState);
    if (previous == nextState) return;
    ICSCoreLog("ui", gameplayActive
        ? "gameplay detected; Steam Cloud settings hidden"
        : "menu detected; Steam Cloud settings available");
    dispatch_async(dispatch_get_main_queue(), ^{
        [NSNotificationCenter.defaultCenter postNotificationName:kGameStateChangedNotification
                                                          object:nil];
    });
}

static void RefreshGameState(void) {
    RegionResult result;
    if (ResolvePlayersFromGame(result)) {
        PublishObservation(result.validPlayerCount > 0);
        return;
    }
    bool cached = gPlayerRegionAddress &&
        ScanRegion(gPlayerRegionAddress, gPlayerRegionSize, result) &&
        result.playerVTableReferences;
    if (!cached) {
        result = DiscoverPlayerRegion();
        if (result.playerVTableReferences) {
            if (result.firstValidPlayerAddress) {
                gPlayerRegionAddress = result.firstValidPlayerAddress;
                gPlayerRegionSize = 0x1000;
            } else {
                gPlayerRegionAddress = result.address;
                gPlayerRegionSize = result.size;
            }
        }
    }
    PublishObservation(result.validPlayerCount > 0);
}
} // namespace

extern "C" bool ICSGameMenuIsActive(void) {
    return gGameState.load() == 0;
}

extern "C" void ICSInstallGameStateDetector(void) {
    static dispatch_once_t onceToken;
    dispatch_once(&onceToken, ^{
        if (!IsaacHeader(nullptr)) {
            ICSCoreLog("ui", "game-state detector disabled for unsupported Isaac executable");
            return;
        }
        LocatePlayerVTables(gContext);
        if (!gContext.playerVTableCount) {
            ICSCoreLog("ui", "game-state detector could not resolve Entity_Player RTTI");
            return;
        }
        dispatch_queue_t queue = dispatch_queue_create(
            "com.isaaccloudsync.game-state", DISPATCH_QUEUE_SERIAL);
        gScanTimer = dispatch_source_create(DISPATCH_SOURCE_TYPE_TIMER, 0, 0, queue);
        dispatch_source_set_timer(gScanTimer, DISPATCH_TIME_NOW,
                                  NSEC_PER_MSEC * 250, NSEC_PER_MSEC * 50);
        dispatch_source_set_event_handler(gScanTimer, ^{ RefreshGameState(); });
        dispatch_resume(gScanTimer);
        ICSCoreLog("ui", "read-only native menu/game detector installed");
    });
}
