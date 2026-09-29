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
#include "ble/gatt-service/hids_host.h"
#define CORDIAL_LINKS 4
#define CORDIAL_REPORT_BYTES 512
#define CORDIAL_DESCRIPTOR_BYTES 2048
#define CORDIAL_CLASSIC 0
#define CORDIAL_BLE 1

typedef struct {
    gatt_client_characteristic_t characteristic;
    uint16_t reference;
    uint8_t service, id, type;
} cordial_report;
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
    bool security_requested, bonded, numbered, writing, reading;
    uint8_t error, remote_io;
    uint64_t deadline;
    uint32_t sequence;
    uint16_t operation_service, operation_id, length, map_handle;
    uint8_t operation_type;
    uint8_t bytes[CORDIAL_REPORT_BYTES];
    gatt_client_service_t services[MAX_NUM_HID_SERVICES];
    cordial_report reports[HIDS_HOST_NUM_REPORTS];
    uint8_t service_count, report_count, service_cursor, report_cursor, query;
    btstack_context_callback_registration_t writable;
} cordial_connection;
extern cordial_connection cordial_connections[CORDIAL_LINKS];
cordial_connection *cordial_by_handle(hci_con_handle_t handle);
cordial_connection *cordial_by_id(cordial_link id);
void cordial_fail(cordial_connection *link, uint8_t error);
void cordial_ready(cordial_connection *link);
int cordial_emit_event(cordial_connection *link, cordial_event event);
void cordial_write_done(cordial_connection *link, uint8_t error);
void cordial_read_done(cordial_connection *link, uint8_t error);
void cordial_info_poll(cordial_connection *link, uint64_t now);
void cordial_info_clear(cordial_connection *link);
void cordial_gatt_begin(cordial_connection *link);
int cordial_gatt_write(cordial_connection *link);
int cordial_gatt_read(cordial_connection *link);
#endif
