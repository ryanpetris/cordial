#ifndef CORDIAL_BTSTACK_PROFILES_INTERNAL_H
#define CORDIAL_BTSTACK_PROFILES_INTERNAL_H
#include "profiles.h"
#include "ad_parser.h"
#include "bluetooth_data_types.h"
#include "bluetooth_gatt.h"
#include "btstack_event.h"
#include "btstack_hid_parser.h"
#include "hci.h"
#include "hci_cmd.h"
#include "l2cap.h"
#include "ble/gatt_client.h"
#include "ble/att_server.h"
#include "ble/sm.h"
#define CORDIAL_LINKS 4
#define CORDIAL_REPORT_BYTES 512
#define CORDIAL_DESCRIPTOR_BYTES 2048
#define CORDIAL_SERVICES 3
#define CORDIAL_CLASSIC 0
#define CORDIAL_BLE 1

typedef cordial_layout_report cordial_report;
// HID setup of one link. Discovery and verification own the shared GATT
// discovery state; subscription and Classic SDP block HID requests.
enum {
    SETUP_NONE, SETUP_HASH, SETUP_DISCOVER, SETUP_SUBSCRIBE, SETUP_IDLE, SETUP_VERIFY, SETUP_SDP
};
typedef struct { uint16_t start,end; uint8_t kind,instance; } cordial_info_service;
typedef struct { uint16_t value,end,uuid; uint8_t properties,instance; bool subscribed,retry; } cordial_info_endpoint;
typedef struct {
    cordial_info_service services[6];
    cordial_info_endpoint endpoints[32];
    gatt_client_notification_t listener;
    uint64_t due;
    uint16_t cccd;
    uint8_t phase,kind,cursor,count,service_count,batteries,subscription[2];
    bool pending,discovered,initial,battery_only,malformed,listening,yield_once,refresh_pending,count_pending,retry_discovery;
} cordial_information;
typedef struct cordial_connection {
    cordial_information info;
    uint64_t read_deadline;
    bool read_abandoned;
    cordial_link id;
    cordial_peer peer;
    hci_con_handle_t handle;
    uint16_t cid;
    bool used, pairing, adopted, authenticated, profile, ready, closing, ended;
    bool security_pending;
    bool security_requested, bonded, numbered, writing, reading, read_simple;
    // cached: the link routes reports by a supplied layout that has not been
    // verified yet. resubscribe: a CCCD write of that layout failed.
    bool cached, resubscribe;
    // The layout's GATT Database Hash. Before a supplied layout is used the
    // device's hash is read: hash_read, hash_equal and hash_malformed record
    // whether a value arrived, matched, or could not be used.
    bool hashed, hash_read, hash_equal, hash_malformed;
    uint8_t hash[16];
    uint8_t error, remote_io;
    // deadline: setup must finish by then. close_deadline: a closing link
    // whose end the host has not reported by then restarts Bluetooth.
    uint64_t deadline, close_deadline;
    // abandoned: the link outlived its close deadline. Its slot stays
    // reserved, since the host may still call into it, until the host's
    // power cycle retires the connection.
    bool abandoned;
    // The link's supervision timeout in milliseconds as the controller
    // reported it; zero when unreported.
    uint16_t supervision_ms;
    uint32_t sequence;
    uint16_t operation_service, operation_id, length;
    uint8_t operation_type;
    uint8_t bytes[CORDIAL_REPORT_BYTES];
    cordial_report reports[CORDIAL_LAYOUT_REPORTS];
    uint8_t report_count, query, setup, cursor;
    btstack_context_callback_registration_t writable;
} cordial_connection;
extern cordial_connection cordial_connections[CORDIAL_LINKS];
cordial_connection *cordial_by_handle(hci_con_handle_t handle);
cordial_connection *cordial_by_id(cordial_link id);
void cordial_fail(cordial_connection *link, uint8_t error);
void cordial_ready(cordial_connection *link);
void cordial_begin_profile(cordial_connection *link);
int cordial_emit_event(cordial_connection *link, cordial_event event);
// Returns the callback's result without failing the link on refusal.
int cordial_emit_status(cordial_connection *link, cordial_event event);
void cordial_input(cordial_connection *link, uint16_t service, uint8_t id, const uint8_t *bytes, uint16_t size);
bool cordial_early_pending(hci_con_handle_t handle);
void cordial_early_push(hci_con_handle_t handle, uint16_t value, const uint8_t *data, uint16_t length);
void cordial_write_done(cordial_connection *link, uint8_t error);
void cordial_read_done(cordial_connection *link, uint8_t error);
void cordial_info_poll(cordial_connection *link, uint64_t now);
void cordial_info_clear(cordial_connection *link);
bool cordial_info_settled(const cordial_connection *link);
void cordial_gatt_init(void);
bool cordial_gatt_discover(cordial_connection *link, bool verify);
void cordial_gatt_check_hash(cordial_connection *link);
void cordial_gatt_end(cordial_connection *link);
void cordial_gatt_subscribe(cordial_connection *link);
void cordial_gatt_poll(cordial_connection *link);
const cordial_report *cordial_gatt_report(const cordial_connection *link, uint16_t value);
int cordial_gatt_write(cordial_connection *link);
int cordial_gatt_read(cordial_connection *link);
#endif
