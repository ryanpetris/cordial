#pragma once
#include <stdint.h>

typedef struct { uint8_t address[6], random; } cordial_ble_peer;
enum cordial_ble_kind {
    CORDIAL_BLE_READY=1, CORDIAL_BLE_FAILED, CORDIAL_BLE_FOUND, CORDIAL_BLE_CONNECTED, CORDIAL_BLE_SECURITY,
    CORDIAL_BLE_PROMPT, CORDIAL_BLE_DISCONNECTED, CORDIAL_BLE_SERVICE, CORDIAL_BLE_CHARACTERISTIC,
    CORDIAL_BLE_DESCRIPTOR, CORDIAL_BLE_DATA, CORDIAL_BLE_COMPLETE, CORDIAL_BLE_NOTIFICATION,
    CORDIAL_BLE_BONDS, CORDIAL_BLE_FORGOTTEN, CORDIAL_BLE_RESTARTING, CORDIAL_BLE_BOND_RESULT, CORDIAL_BLE_INCOMING
};
enum cordial_ble_error { CORDIAL_BLE_OK, CORDIAL_BLE_BUSY, CORDIAL_BLE_CAPACITY, CORDIAL_BLE_CONNECTION,
    CORDIAL_BLE_AUTH, CORDIAL_BLE_UNSUPPORTED, CORDIAL_BLE_STORAGE, CORDIAL_BLE_OVERFLOW, CORDIAL_BLE_TIMEOUT, CORDIAL_BLE_RADIO, CORDIAL_BLE_REPORT_SIZE };
enum cordial_ble_prompt { CORDIAL_BLE_CONFIRM, CORDIAL_BLE_ENTER, CORDIAL_BLE_DISPLAY };
typedef struct {
    uint64_t scan;
    uint32_t token, request, number;
    cordial_ble_peer peer, address;
    uint16_t start, end, handle, uuid, offset, length;
    int16_t rssi;
    uint8_t kind, code, properties, bonded, encrypted;
    // Optional properties: 0 not reported, 1 false, 2 true. Key size 0 is unknown.
    uint8_t authenticated, secure_connections, key_size;
    const uint8_t *data;
} cordial_ble_event;
typedef void (*cordial_ble_emit)(const cordial_ble_event *);
enum cordial_ble_command_kind { CORDIAL_BLE_SCAN=1, CORDIAL_BLE_CONNECT, CORDIAL_BLE_DISCONNECT,
    CORDIAL_BLE_REPLY, CORDIAL_BLE_SERVICES, CORDIAL_BLE_CHARACTERISTICS, CORDIAL_BLE_DESCRIPTORS,
    CORDIAL_BLE_READ, CORDIAL_BLE_WRITE, CORDIAL_BLE_FORGET, CORDIAL_BLE_ADOPT, CORDIAL_BLE_SUBSCRIBE, CORDIAL_BLE_IMPORT, CORDIAL_BLE_EXPORT, CORDIAL_BLE_RECONNECT, CORDIAL_BLE_ACCEPT,
    CORDIAL_BLE_READ_UUID };
typedef struct {
    uint64_t scan;
    uint32_t token, request, number;
    cordial_ble_peer peer;
    uint16_t start, end, handle, length;
    uint8_t kind, enabled, pairing, method, accept, response, descriptor;
    uint8_t data[512];
} cordial_ble_command;
int cordial_ble_start(cordial_ble_emit);
int cordial_ble_submit(const cordial_ble_command *);

void cordial_ble_identity(const uint8_t *irk);

unsigned cordial_ble_bond_capacity(void);
