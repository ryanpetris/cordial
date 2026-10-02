#ifndef CORDIAL_BTSTACK_PROFILES_H
#define CORDIAL_BTSTACK_PROFILES_H
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>

typedef struct { uint64_t generation; uint8_t slot; } cordial_link;
typedef struct { uint8_t address[6], transport, random; } cordial_peer;
enum cordial_kind {
    CORDIAL_READY = 1, CORDIAL_FAILED, CORDIAL_FOUND, CORDIAL_INCOMING, CORDIAL_PROMPT, CORDIAL_BONDED,
    CORDIAL_DESCRIPTOR, CORDIAL_CONNECTED, CORDIAL_DISCONNECTED, CORDIAL_INPUT, CORDIAL_WRITTEN, CORDIAL_READ,
    CORDIAL_RESTARTING, CORDIAL_SECURITY, CORDIAL_INFORMATION, CORDIAL_LAYOUT
};
enum cordial_error {
    CORDIAL_OK, CORDIAL_BUSY, CORDIAL_CAPACITY, CORDIAL_CONNECTION, CORDIAL_AUTHENTICATION,
    CORDIAL_UNSUPPORTED, CORDIAL_STORAGE, CORDIAL_OVERFLOW, CORDIAL_TIMEOUT, CORDIAL_RADIO, CORDIAL_REPORT_SIZE,
    // The adapter's own refusals: a transport it lacks or has disabled, and
    // malformed arguments. CORDIAL_UNSUPPORTED is the device's HID layout.
    CORDIAL_TRANSPORT, CORDIAL_ARGUMENT
};
enum cordial_prompt { CORDIAL_CONFIRM, CORDIAL_ENTER_PASSKEY, CORDIAL_ENTER_PIN, CORDIAL_DISPLAY_PASSKEY };
// Most Report characteristics of one BLE layout; equals the application's limit.
#define CORDIAL_LAYOUT_REPORTS 32
// One Report characteristic: value and CCCD handles, GATT properties, owning
// service index, Report ID and report type (1 input, 2 output, 3 feature).
typedef struct { uint16_t value, cccd; uint8_t properties, service, id, type; } cordial_layout_report;
_Static_assert(sizeof(cordial_layout_report) == 8, "layout report ABI");
extern const uint16_t cordial_layout_report_capacity;
// A saved layout supplied to a connection. BLE links use the report table and
// Classic links the single report map. The callee copies what it keeps.
// `hash` is the saved GATT Database Hash (16 bytes) or NULL.
typedef struct {
    const cordial_layout_report *reports;
    const uint8_t *descriptor, *hash;
    uint16_t count, length;
} cordial_layout;
typedef struct {
    cordial_link link;
    cordial_peer peer, address; // CORDIAL_FOUND: address.transport == 0xff means identity-only presence.
    uint64_t operation;
    // CORDIAL_FOUND: BLE Appearance or Classic Class of Device; zero means unknown.
    // CORDIAL_SECURITY: bits 0..3 encrypted/authenticated/SC/bonded, 16..19 known flags; 8..15 key bytes.
    uint32_t number;
    // CORDIAL_DESCRIPTOR: code 1 marks a map found by verification of a supplied layout.
    // CORDIAL_CONNECTED: code 1 means the supplied layout is in use. Otherwise a
    // BLE link's data holds its discovered cordial_layout_report table.
    // CORDIAL_LAYOUT: verification finished with `service` maps and, for BLE, the
    // report table in data. Accepting it switches the link to that layout.
    // Code 1 means verification ended without a result.
    uint16_t service, length;
    int16_t rssi;
    uint8_t kind, code, report_id, report_type;
    const uint8_t *data;
    // CORDIAL_CONNECTED with a discovered table and CORDIAL_LAYOUT: the
    // device's GATT Database Hash (16 bytes), or NULL when it has none.
    const uint8_t *hash;
} cordial_event;
// Positive accepts the event; zero indicates pressure; negative is a cordial_error.
typedef int (*cordial_emit)(void *, const cordial_event *);

void cordial_profiles_init(void *context, cordial_emit emit);
void cordial_profiles_poll(uint64_t now);
int cordial_profiles_start(const uint8_t *address);
void cordial_profiles_stop(void);
bool cordial_profiles_scan_and_connect(void);
int cordial_profiles_reconnect(const cordial_peer *peers, size_t count);
int cordial_profiles_scan(uint64_t id, bool classic, bool ble);
int cordial_profiles_set_transport(uint8_t transport, bool enabled);
int cordial_profiles_connect(cordial_link, cordial_peer, bool pairing, const cordial_layout *layout);
int cordial_profiles_incoming(uint32_t attempt, const cordial_link *accept, const cordial_layout *layout);
void cordial_profiles_disconnect(cordial_link);
int cordial_profiles_adopt(cordial_link);
int cordial_profiles_reply(cordial_link, uint8_t method, bool accept, const char *value);
int cordial_profiles_write(cordial_link, uint32_t sequence, uint16_t service, uint8_t kind,
                      uint16_t report_id, const uint8_t *data, uint16_t size);
int cordial_profiles_read(cordial_link, uint32_t sequence, uint16_t service, uint8_t kind, uint16_t report_id);
int cordial_profiles_can_write(cordial_link);
int cordial_profiles_info_refresh(cordial_link);
int cordial_profiles_info_busy(cordial_link);
int cordial_profiles_bonds(cordial_peer *peers, unsigned capacity);
int cordial_profiles_forget(cordial_peer);

#endif
