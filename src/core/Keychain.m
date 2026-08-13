#import <Foundation/Foundation.h>
#import <Security/Security.h>

static NSDictionary *ICSKeychainQuery(const char *service, const char *account) {
    NSString *serviceValue = [NSString stringWithUTF8String:service ?: ""];
    NSString *accountValue = [NSString stringWithUTF8String:account ?: ""];
    return @{
        (__bridge id)kSecClass: (__bridge id)kSecClassGenericPassword,
        (__bridge id)kSecAttrService: serviceValue,
        (__bridge id)kSecAttrAccount: accountValue,
    };
}

int32_t ICSKeychainStore(const char *service, const char *account, const uint8_t *bytes, size_t length) {
    @autoreleasepool {
        if (service == NULL || account == NULL || (bytes == NULL && length != 0)) {
            return errSecParam;
        }
        NSData *data = [NSData dataWithBytes:bytes length:length];
        NSDictionary *query = ICSKeychainQuery(service, account);
        OSStatus status = SecItemUpdate(
            (__bridge CFDictionaryRef)query,
            (__bridge CFDictionaryRef)@{(__bridge id)kSecValueData: data}
        );
        if (status == errSecItemNotFound) {
            NSMutableDictionary *insert = [query mutableCopy];
            insert[(__bridge id)kSecValueData] = data;
            insert[(__bridge id)kSecAttrAccessible] = (__bridge id)kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly;
            status = SecItemAdd((__bridge CFDictionaryRef)insert, NULL);
        }
        return status;
    }
}

uint8_t *ICSKeychainCopy(const char *service, const char *account, size_t *length) {
    @autoreleasepool {
        if (length == NULL || service == NULL || account == NULL) {
            return NULL;
        }
        *length = 0;
        NSMutableDictionary *query = [ICSKeychainQuery(service, account) mutableCopy];
        query[(__bridge id)kSecReturnData] = @YES;
        query[(__bridge id)kSecMatchLimit] = (__bridge id)kSecMatchLimitOne;
        CFTypeRef result = NULL;
        OSStatus status = SecItemCopyMatching((__bridge CFDictionaryRef)query, &result);
        if (status != errSecSuccess || result == NULL) {
            if (result != NULL) CFRelease(result);
            return NULL;
        }
        NSData *data = CFBridgingRelease(result);
        uint8_t *copy = malloc(MAX((NSUInteger)1, data.length));
        if (copy == NULL) return NULL;
        if (data.length != 0) memcpy(copy, data.bytes, data.length);
        *length = data.length;
        return copy;
    }
}

int32_t ICSKeychainDelete(const char *service, const char *account) {
    @autoreleasepool {
        if (service == NULL || account == NULL) return errSecParam;
        return SecItemDelete((__bridge CFDictionaryRef)ICSKeychainQuery(service, account));
    }
}

void ICSFree(void *pointer) {
    free(pointer);
}
