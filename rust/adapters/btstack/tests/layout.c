// Exercise saved-layout reconnection across profile, GATT and information
// callbacks: Connected at re-encryption, CCCD rewrites, request gating, early
// input, background verification and the Classic SDP path. GATT queries,
// security state and Classic host calls are faked; event decoding is native.
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#define gatt_client_discover_primary_services_by_uuid16 fake_services
#define gatt_client_discover_characteristics_for_service fake_characteristics
#define gatt_client_read_long_value_of_characteristic_using_value_handle fake_read
#define gatt_client_read_value_of_characteristic_using_value_handle fake_read
#define gatt_client_discover_characteristic_descriptors fake_descriptors
#define gatt_client_read_characteristic_descriptor_using_descriptor_handle fake_read
#define gatt_client_write_value_of_characteristic fake_write
#define gatt_client_read_value_of_characteristics_by_uuid16 fake_read_hash
#define gatt_client_listen_for_characteristic_value_updates fake_listen
#define gatt_client_stop_listening_for_characteristic_value_updates fake_stop
#define gap_set_connection_parameters fake_connection_parameters
#define sm_identity_resolving_state fake_irk_state
#define gap_bonded fake_bonded
#define gap_encryption_key_size fake_key_size
#define gap_disconnect fake_disconnect
#define gap_request_security_level fake_request_security
#define gap_security_level fake_security_level
#define hid_host_accept_connection fake_accept
#define hid_descriptor_storage_get_descriptor_data fake_descriptor_data
#define hid_descriptor_storage_get_descriptor_len fake_descriptor_len
#define hid_host_decline_connection fake_decline
#define hid_host_disconnect fake_hid_disconnect
#define gap_connectable_control fake_connectable
#define gatt_client_get_mtu fake_mtu
#define gatt_client_request_to_write_without_response fake_request_command
#define gatt_client_write_value_of_characteristic_without_response fake_write_command
#define hci_get_state fake_hci_state
#include "../c/profiles.c"
#include "../c/gatt.c"
#include "../c/information.c"
#include "runtime.h"

// Native storage for the real key databases.
static struct { uint32_t tag; unsigned len; uint8_t data[512]; } records[32];
static int get(void *ctx,uint32_t tag,uint8_t *out,uint32_t n){
    (void)ctx;
    for(unsigned i=0;i<32;i++) if(records[i].tag==tag){
        unsigned len=records[i].len<n?records[i].len:n;
        if(out) memcpy(out,records[i].data,len);
        return out?(int)len:(int)records[i].len;
    }
    return 0;
}
static int save(void *ctx,uint32_t tag,const uint8_t *in,uint32_t n){
    (void)ctx; assert(n<=512);
    for(unsigned pass=0;pass<2;pass++)
        for(unsigned i=0;i<32;i++) if(pass ? !records[i].tag : records[i].tag==tag){records[i].tag=tag;records[i].len=n;memcpy(records[i].data,in,n);return 0;}
    return -1;
}
static void del(void *ctx,uint32_t tag){(void)ctx;for(unsigned i=0;i<32;i++)if(records[i].tag==tag)memset(&records[i],0,sizeof(records[i]));}
static uint32_t time_cb(void *ctx){(void)ctx;return 0;}
static void wake_cb(void *ctx){(void)ctx;}
static void fatal_cb(void *ctx){(void)ctx;abort();}
static int can_send_cb(void *ctx){(void)ctx;return 0;}
static int send_cb(void *ctx,uint8_t type,const uint8_t *data,uint16_t len){(void)ctx;(void)type;(void)data;(void)len;return 0;}

// The application side: a log of events and a configurable answer.
static struct { uint8_t kind, code, report_id; uint16_t service, length; uint8_t data[16]; } events[64];
static unsigned event_count, accept_inputs = 1000;
static int layout_answer = 1;
static int event_cb(void *ctx, const cordial_event *event) {
    (void)ctx;
    if (event->kind == CORDIAL_INPUT) {
        if (!accept_inputs) return 0;
        --accept_inputs;
    }
    assert(event_count < 64);
    events[event_count].kind = event->kind; events[event_count].code = event->code;
    events[event_count].report_id = event->report_id; events[event_count].service = event->service;
    events[event_count].length = event->length;
    memcpy(events[event_count].data, event->data, event->length < 16 ? event->length : 16);
    ++event_count;
    return event->kind == CORDIAL_LAYOUT ? layout_answer : 1;
}
static unsigned kinds(uint8_t kind) {
    unsigned n = 0;
    for (unsigned i = 0; i < event_count; ++i) n += events[i].kind == kind;
    return n;
}

// Fake GATT client: one outstanding query and its callback.
enum { NONE, SERVICES, CHARACTERISTICS, READ, DESCRIPTORS, WRITE, HASH };
static unsigned discovery_queries;
static unsigned query;
static uint16_t target, written_value, writes;
static btstack_packet_handler_t query_callback, listener;
uint8_t fake_services(btstack_packet_handler_t cb, hci_con_handle_t h, uint16_t uuid) {
    (void)h; query = SERVICES; target = uuid; query_callback = cb; ++discovery_queries; return 0;
}
uint8_t fake_characteristics(btstack_packet_handler_t cb, hci_con_handle_t h, gatt_client_service_t *s) {
    (void)h; query = CHARACTERISTICS; target = s->start_group_handle; query_callback = cb; ++discovery_queries; return 0;
}
uint8_t fake_read(btstack_packet_handler_t cb, hci_con_handle_t h, uint16_t value) {
    (void)h; query = READ; target = value; query_callback = cb; ++discovery_queries; return 0;
}
uint8_t fake_descriptors(btstack_packet_handler_t cb, hci_con_handle_t h, gatt_client_characteristic_t *c) {
    (void)h; query = DESCRIPTORS; target = c->value_handle; query_callback = cb; ++discovery_queries; return 0;
}
uint8_t fake_read_hash(btstack_packet_handler_t cb, hci_con_handle_t h, uint16_t start, uint16_t end, uint16_t uuid) {
    (void)h; assert(start == 1 && end == 0xffff && uuid == ORG_BLUETOOTH_CHARACTERISTIC_DATABASE_HASH);
    query = HASH; query_callback = cb; return 0;
}
uint8_t fake_write(btstack_packet_handler_t cb, hci_con_handle_t h, uint16_t handle, uint16_t n, uint8_t *v) {
    (void)h; assert(n == 2); query = WRITE; target = handle; written_value = little_endian_read_16(v, 0);
    query_callback = cb; ++writes; return 0;
}
void fake_listen(gatt_client_notification_t *n, btstack_packet_handler_t cb, hci_con_handle_t h, gatt_client_characteristic_t *c) {
    (void)n; (void)c; if (h == GATT_CLIENT_ANY_CONNECTION) listener = cb;
}
void fake_stop(gatt_client_notification_t *n) { (void)n; }
static uint16_t parameters[8];
void fake_connection_parameters(uint16_t a, uint16_t b, uint16_t c, uint16_t d, uint16_t e, uint16_t f, uint16_t g, uint16_t h) {
    uint16_t values[8] = {a, b, c, d, e, f, g, h}; memcpy(parameters, values, sizeof values);
}
// Fake security and Classic host state.
static irk_lookup_state_t irk_state = IRK_LOOKUP_SUCCEEDED;
static uint8_t key_size;
irk_lookup_state_t fake_irk_state(hci_con_handle_t h) { (void)h; return irk_state; }
bool fake_bonded(hci_con_handle_t h) { (void)h; return true; }
uint8_t fake_key_size(hci_con_handle_t h) { (void)h; return key_size; }
static unsigned disconnects;
uint8_t fake_disconnect(hci_con_handle_t h) { (void)h; ++disconnects; return 0; }
void fake_request_security(hci_con_handle_t h, gap_security_level_t level) { (void)h; (void)level; }
gap_security_level_t fake_security_level(hci_con_handle_t h) { (void)h; return LEVEL_2; }
uint8_t fake_accept(uint16_t cid, hid_protocol_mode_t mode) { (void)cid; (void)mode; return 0; }
static unsigned declines, hid_disconnects, commands_written;
static btstack_context_callback_registration_t *queued_command;
static HCI_STATE host_state = HCI_STATE_WORKING;
HCI_STATE fake_hci_state(void) { return host_state; }
uint8_t fake_mtu(hci_con_handle_t h, uint16_t *mtu) { (void)h; *mtu = 23; return 0; }
uint8_t fake_request_command(btstack_context_callback_registration_t *r, hci_con_handle_t h) { (void)h; queued_command = r; return 0; }
uint8_t fake_write_command(hci_con_handle_t h, uint16_t v, uint16_t n, uint8_t *p) {
    (void)h; (void)v; (void)n; (void)p; ++commands_written; return 0;
}
static int connectable = -1;
uint8_t fake_decline(uint16_t cid) { (void)cid; ++declines; return 0; }
void fake_hid_disconnect(uint16_t cid) { (void)cid; ++hid_disconnects; }
void fake_connectable(uint8_t enable) { connectable = enable; }
static const uint8_t *sdp_descriptor;
static uint16_t sdp_length;
const uint8_t *fake_descriptor_data(uint16_t cid) { (void)cid; return sdp_descriptor; }
uint16_t fake_descriptor_len(uint16_t cid) { (void)cid; return sdp_length; }

static void finish_query(uint16_t handle, uint8_t status) {
    uint8_t event[] = {GATT_EVENT_QUERY_COMPLETE, 7, (uint8_t)handle, (uint8_t)(handle >> 8), 0, 0, 0, 0, status};
    query = NONE;
    query_callback(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void notify(uint16_t handle, uint16_t value, uint8_t length, uint8_t byte) {
    uint8_t event[12 + 64] = {GATT_EVENT_NOTIFICATION, (uint8_t)(10 + length), (uint8_t)handle, (uint8_t)(handle >> 8)};
    little_endian_store_16(event, 8, value); little_endian_store_16(event, 10, length);
    memset(event + 12, byte, length);
    listener(HCI_EVENT_PACKET, 0, event, 12 + length);
}
static void reencryption_ended(uint16_t handle, uint8_t status) {
    uint8_t event[12] = {SM_EVENT_REENCRYPTION_COMPLETE, 10, (uint8_t)handle, (uint8_t)(handle >> 8)};
    event[11] = status;
    sm_handler(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void reencrypted(uint16_t handle) { reencryption_ended(handle, 0); }
static void hid_event(uint8_t subevent, uint16_t cid, uint8_t status, uint16_t handle) {
    uint8_t event[15] = {HCI_EVENT_HID_META, 13, subevent, (uint8_t)cid, (uint8_t)(cid >> 8), status};
    little_endian_store_16(event, 12, handle);
    classic_event(event, subevent == HID_SUBEVENT_DESCRIPTOR_AVAILABLE ? 6 : sizeof event);
}
static void classic_report(uint16_t cid, const uint8_t *report, uint8_t length) {
    uint8_t event[7 + 1 + 16] = {HCI_EVENT_HID_META, (uint8_t)(5 + 1 + length), HID_SUBEVENT_REPORT, (uint8_t)cid, (uint8_t)(cid >> 8)};
    little_endian_store_16(event, 5, 1 + length); event[7] = 0xa1; memcpy(event + 8, report, length);
    classic_event(event, 8 + length);
}
static unsigned next_attempt_index;
static cordial_link admit(cordial_peer peer, uint16_t handle, uint16_t cid, const cordial_layout *layout) {
    unsigned i = next_attempt_index++ % CORDIAL_LINKS;
    incoming[i].attempt = 100 + i; incoming[i].peer = peer; incoming[i].handle = handle; incoming[i].cid = cid;
    incoming[i].deadline = 100000; incoming[i].offered = true; incoming[i].closing = false; incoming[i].auth_error = CORDIAL_OK;
    cordial_link id = { .generation = 200 + i, .slot = (uint8_t)i };
    if (handle == 0x40) notify(handle, 5, 2, 0x11); // Input that precedes admission waits.
    assert(cordial_profiles_incoming(incoming[i].attempt, &id, layout) == CORDIAL_OK);
    return id;
}
static cordial_peer saved_ble(void) {
    bd_addr_t address = {0xc0, 1, 2, 3, 4, 5}; sm_key_t irk = {1}, ltk = {2}; uint8_t rand[8] = {0};
    int index = le_device_db_add(BD_ADDR_TYPE_LE_RANDOM, address, irk);
    assert(index >= 0); le_device_db_encryption_set(index, 0, rand, ltk, 16, 1, 0, 1);
    cordial_peer peer = { .transport = CORDIAL_BLE, .random = 1 }; memcpy(peer.address, address, 6);
    return peer;
}
static const cordial_layout_report keyboard[] = {
    { .value = 5, .cccd = 7, .properties = ATT_PROPERTY_READ | ATT_PROPERTY_NOTIFY, .service = 0, .id = 1, .type = HID_REPORT_TYPE_INPUT },
    { .value = 9, .properties = ATT_PROPERTY_WRITE, .service = 0, .id = 2, .type = HID_REPORT_TYPE_OUTPUT },
    { .value = 25, .cccd = 27, .properties = ATT_PROPERTY_INDICATE, .service = 1, .id = 3, .type = HID_REPORT_TYPE_FEATURE },
};
// Finishes the information pass: no optional services exist.
static void information_pass(cordial_link id) {
    for (unsigned n = 0; n < 16 && query != HASH; ++n) {
        cordial_profiles_poll(1);
        if (query == SERVICES && target != ORG_BLUETOOTH_SERVICE_HUMAN_INTERFACE_DEVICE) {
            assert(!cordial_profiles_can_write(id)); finish_query(0x40, ATT_ERROR_ATTRIBUTE_NOT_FOUND);
        }
    }
}

static void saved_ble_layout_admits_input_at_reencryption(void) {
    cordial_peer peer = saved_ble();
    cordial_layout_report table[3]; memcpy(table, keyboard, sizeof table);
    cordial_layout layout = { .reports = table, .count = 3 };
    key_size = 0; event_count = 0; writes = 0;
    cordial_link id = admit(peer, 0x40, 0, &layout);
    table[0].value = 0x99; // C keeps its own copy.
    cordial_connection *l = cordial_by_id(id);
    assert(l && l->cached && !l->ready && !event_count && query == NONE);
    notify(0x40, 5, 2, 0x12); notify(0x40, 77, 2, 0x13); notify(0x40, 25, 1, 0x14);
    key_size = 16; reencrypted(0x40);
    // Connected at once, from the supplied layout, with buffered input after it.
    assert(event_count == 5 && events[0].kind == CORDIAL_SECURITY);
    assert(events[1].kind == CORDIAL_CONNECTED && events[1].code == 1 && !events[1].length);
    assert(events[2].kind == CORDIAL_INPUT && events[2].data[0] == 0x11 && events[2].report_id == 1 && events[2].length == 2);
    assert(events[3].kind == CORDIAL_INPUT && events[3].data[0] == 0x12);
    assert(events[4].kind == CORDIAL_INPUT && events[4].data[0] == 0x14 && events[4].service == 1 && events[4].report_id == 3);
    assert(!cordial_early_pending(0x40));
    // No discovery: the CCCDs are rewritten, with requests and information waiting.
    assert(query == WRITE && target == 7 && written_value == 1 && writes == 1);
    assert(!cordial_profiles_can_write(id));
    cordial_profiles_poll(1); assert(query == WRITE && writes == 1);
    finish_query(0x40, 0); assert(query == WRITE && target == 27 && written_value == 2);
    assert(!cordial_profiles_can_write(id));
    finish_query(0x40, 0); assert(query == NONE && l->setup == SETUP_IDLE);
    assert(cordial_profiles_can_write(id));
    notify(0x40, 5, 2, 0x15); assert(events[event_count - 1].data[0] == 0x15);
    // Verification follows the information pass and finds the same layout.
    unsigned before = event_count;
    information_pass(id);
    assert(query == HASH && l->setup == SETUP_VERIFY);
    assert(!cordial_profiles_can_write(id));
    assert(kinds(CORDIAL_CONNECTED) == 1 && event_count >= before);
    cordial_profiles_disconnect(id);
    uint8_t ended[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, 0x40, 0, 0x16};
    packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended);
    cordial_profiles_poll(2);
    assert(!cordial_by_id(id) && !owner);
}
// GATT results for the fake device below.
static void uuid(uint8_t *out, uint16_t value) {
    uint8_t full[16]; uuid_add_bluetooth_prefix(full, value); reverse_128(full, out);
}
static void deliver(uint8_t *event, uint16_t size) { query_callback(HCI_EVENT_PACKET, 0, event, size); }
static void send_hash(const uint8_t *hash) {
    uint8_t event[28] = {GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT, 26, 0x40, 0};
    little_endian_store_16(event, 8, 0x10); little_endian_store_16(event, 10, 16); memcpy(event + 12, hash, 16);
    deliver(event, sizeof event);
}
static const uint8_t hash_a[16] = {0xa1}, hash_b[16] = {0xb1};
static const uint8_t unnumbered_map[] = {0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xc0};
// A device with one HID service: map 3, input 5 with CCCD 7.
static void discover_device(const uint8_t *hash) {
    assert(query == HASH); send_hash(hash); finish_query(0x40, 0);
    assert(query == SERVICES);
    uint8_t service[28] = {GATT_EVENT_SERVICE_QUERY_RESULT, 26, 0x40, 0};
    little_endian_store_16(service, 8, 1); little_endian_store_16(service, 10, 20);
    uuid(service + 12, ORG_BLUETOOTH_SERVICE_HUMAN_INTERFACE_DEVICE);
    deliver(service, sizeof service); finish_query(0x40, 0);
    assert(query == CHARACTERISTICS);
    for (unsigned i = 0; i < 2; ++i) {
        uint16_t value = i ? 5 : 3;
        uint8_t c[32] = {GATT_EVENT_CHARACTERISTIC_QUERY_RESULT, 30, 0x40, 0};
        little_endian_store_16(c, 8, value - 1); little_endian_store_16(c, 10, value);
        little_endian_store_16(c, 12, value + 2);
        little_endian_store_16(c, 14, i ? ATT_PROPERTY_READ | ATT_PROPERTY_NOTIFY : ATT_PROPERTY_READ);
        uuid(c + 16, i ? ORG_BLUETOOTH_CHARACTERISTIC_REPORT : ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP);
        deliver(c, sizeof c);
    }
    finish_query(0x40, 0);
    assert(query == READ && target == 3);
    uint8_t map[14 + sizeof unnumbered_map] = {GATT_EVENT_LONG_CHARACTERISTIC_VALUE_QUERY_RESULT, 12 + sizeof unnumbered_map, 0x40, 0};
    little_endian_store_16(map, 12, sizeof unnumbered_map); memcpy(map + 14, unnumbered_map, sizeof unnumbered_map);
    deliver(map, sizeof map); finish_query(0x40, 0);
    assert(query == DESCRIPTORS && target == 5);
    for (unsigned i = 0; i < 2; ++i) {
        uint8_t d[26] = {GATT_EVENT_ALL_CHARACTERISTIC_DESCRIPTORS_QUERY_RESULT, 24, 0x40, 0};
        little_endian_store_16(d, 8, 6 + i);
        uuid(d + 10, i ? ORG_BLUETOOTH_DESCRIPTOR_GATT_CLIENT_CHARACTERISTIC_CONFIGURATION : ORG_BLUETOOTH_DESCRIPTOR_REPORT_REFERENCE);
        deliver(d, sizeof d);
    }
    finish_query(0x40, 0);
    assert(query == READ && target == 6);
    uint8_t reference[14] = {GATT_EVENT_CHARACTERISTIC_DESCRIPTOR_QUERY_RESULT, 12, 0x40, 0};
    little_endian_store_16(reference, 8, 6); little_endian_store_16(reference, 10, 2);
    reference[12] = 1; reference[13] = HID_REPORT_TYPE_INPUT;
    deliver(reference, sizeof reference); finish_query(0x40, 0);
}
static void saved_hash_is_checked_before_admission(void) {
    cordial_peer peer = saved_ble();
    cordial_layout layout = { .reports = keyboard, .count = 3, .hash = hash_a };
    // The same hash: Connected without discovery, early input after it.
    key_size = 0; event_count = 0; writes = 0; discovery_queries = 0;
    cordial_link id = admit(peer, 0x40, 0, &layout);
    key_size = 16; reencrypted(0x40);
    assert(query == HASH && !kinds(CORDIAL_CONNECTED) && !cordial_profiles_can_write(id));
    notify(0x40, 5, 2, 0x12);
    send_hash(hash_a); finish_query(0x40, 0);
    assert(!discovery_queries && kinds(CORDIAL_CONNECTED) == 1);
    assert(events[1].kind == CORDIAL_CONNECTED && events[1].code == 1);
    assert(events[2].kind == CORDIAL_INPUT && events[2].data[0] == 0x11 && events[3].data[0] == 0x12);
    assert(query == WRITE && target == 7);
    cordial_profiles_disconnect(id);
    uint8_t ended[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, 0x40, 0, 0x16};
    packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended);
    cordial_profiles_poll(30);
    // A different hash: discovery runs before Connected, which reports the new layout.
    key_size = 0; event_count = 0;
    id = admit(peer, 0x40, 0, &layout);
    key_size = 16; reencrypted(0x40);
    send_hash(hash_b); finish_query(0x40, 0);
    assert(!kinds(CORDIAL_CONNECTED) && query == HASH);
    notify(0x40, 5, 2, 0x13);
    discover_device(hash_b);
    assert(!kinds(CORDIAL_CONNECTED) && query == WRITE && target == 7);
    finish_query(0x40, 0);
    unsigned connected = event_count;
    for (unsigned i = 0; i < event_count; ++i) if (events[i].kind == CORDIAL_CONNECTED) connected = i;
    assert(connected < event_count && !events[connected].code && events[connected].length == 8);
    assert(events[connected + 1].kind == CORDIAL_INPUT && events[connected + 1].data[0] == 0x11);
    assert(events[connected + 2].kind == CORDIAL_INPUT && events[connected + 2].data[0] == 0x13);
    assert(kinds(CORDIAL_DESCRIPTOR) == 1 && !cordial_by_id(id)->cached);
    assert(cordial_by_id(id)->hashed && !memcmp(cordial_by_id(id)->hash, hash_b, 16));
    cordial_profiles_disconnect(id);
    packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended);
    cordial_profiles_poll(31);
    // A disconnect while the hash read is pending ends with a recoverable error.
    key_size = 0; event_count = 0;
    id = admit(peer, 0x40, 0, &layout);
    key_size = 16; reencrypted(0x40);
    assert(query == HASH);
    finish_query(0x40, ATT_ERROR_HCI_DISCONNECT_RECEIVED);
    packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended);
    cordial_profiles_poll(32);
    assert(!cordial_by_id(id) && !kinds(CORDIAL_CONNECTED));
    assert(events[event_count - 1].kind == CORDIAL_DISCONNECTED && events[event_count - 1].code == CORDIAL_CONNECTION);
}
static void discovered_ble_layout_reports_its_table(void) {
    cordial_connection *l = allocate((cordial_link){ .generation = 300, .slot = 0 }, (cordial_peer){ .transport = CORDIAL_BLE }, false);
    assert(l);
    l->authenticated = l->profile = true; l->handle = 0x42;
    memcpy(l->reports, keyboard, sizeof keyboard); l->report_count = 3;
    event_count = 0; cordial_ready(l);
    assert(events[1].kind == CORDIAL_CONNECTED && !events[1].code && events[1].length == sizeof keyboard);
    assert(!memcmp(events[1].data, keyboard, 16));
    finish(l); cordial_profiles_poll(3);
}
static void early_input_is_bounded_and_survives_queue_pressure(void) {
    cordial_peer peer = saved_ble();
    cordial_layout layout = { .reports = keyboard, .count = 3 };
    key_size = 0; event_count = 0;
    cordial_link id = admit(peer, 0x40, 0, &layout);
    // 11-byte records: the buffer keeps the newest 23 of 40 (the first, 0x11, is dropped too).
    for (uint8_t n = 0; n < 40; ++n) notify(0x40, 5, 8, n);
    accept_inputs = 5;
    key_size = 16; reencrypted(0x40);
    assert(kinds(CORDIAL_CONNECTED) == 1 && kinds(CORDIAL_INPUT) == 5);
    assert(events[2].data[0] == 40 - 23 && events[6].data[0] == 40 - 19);
    // Later notifications queue behind the rest instead of overtaking it.
    notify(0x40, 5, 8, 0x77);
    assert(kinds(CORDIAL_INPUT) == 5 && cordial_early_pending(0x40));
    accept_inputs = 1000; cordial_profiles_poll(4);
    assert(kinds(CORDIAL_INPUT) == 24 && !cordial_early_pending(0x40));
    for (unsigned i = 2, expected = 40 - 23; i < 2 + 23; ++i, ++expected) assert(events[i].data[0] == expected);
    assert(events[25].data[0] == 0x77);
    cordial_profiles_disconnect(id);
    uint8_t ended[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, 0x40, 0, 0x16};
    packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended);
    cordial_profiles_poll(5);
    // A rejected admission discards what it buffered.
    incoming[0].attempt = 9; incoming[0].peer = peer; incoming[0].handle = 0x43; incoming[0].closing = false;
    notify(0x43, 5, 2, 0x21); assert(cordial_early_pending(0x43));
    assert(cordial_profiles_incoming(9, NULL, NULL) == CORDIAL_OK);
    uint8_t rejected[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, 0x43, 0, 0x16};
    packet_handler(HCI_EVENT_PACKET, 0, rejected, sizeof rejected);
    assert(!cordial_early_pending(0x43));
    notify(0x44, 5, 2, 0x22); assert(!cordial_early_pending(0x44)); // Unknown connections keep nothing.
}
static const uint8_t unnumbered[] = {0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xc0};
static const uint8_t numbered[] = {0x05, 0x01, 0x09, 0x06, 0xa1, 0x01, 0x85, 0x04, 0x75, 0x08, 0x95, 0x01, 0x81, 0x02, 0xc0};
static cordial_link classic_admission(uint8_t last) {
    cordial_peer peer = { .transport = CORDIAL_CLASSIC, .address = {0x10, 0x20, 0x30, 0x40, 0x50, last} };
    link_key_t key = {7};
    gap_store_link_key_for_bd_addr(peer.address, key, AUTHENTICATED_COMBINATION_KEY_GENERATED_FROM_P256);
    cordial_layout layout = { .descriptor = unnumbered, .length = sizeof unnumbered };
    event_count = 0;
    return admit(peer, 0x41, 0x50, &layout);
}
static void classic_layout_skips_sdp_and_follows_a_changed_descriptor(void) {
    cordial_link id = classic_admission(0x60);
    uint8_t report[] = {0x31};
    classic_report(0x50, report, 1); // Interrupt reports arrive during setup.
    hid_event(HID_SUBEVENT_CONNECTION_OPENED, 0x50, 0, 0x41);
    assert(events[0].kind == CORDIAL_SECURITY && events[1].kind == CORDIAL_CONNECTED && events[1].code == 1);
    assert(events[2].kind == CORDIAL_INPUT && events[2].length == 1 && events[2].data[0] == 0x31 && !events[2].report_id);
    // The host refuses control requests until its SDP query ends.
    assert(!cordial_profiles_can_write(id));
    sdp_descriptor = numbered; sdp_length = sizeof numbered;
    hid_event(HID_SUBEVENT_DESCRIPTOR_AVAILABLE, 0x50, 0, 0);
    assert(cordial_profiles_can_write(id) && event_count == 3);
    cordial_profiles_poll(6);
    assert(events[3].kind == CORDIAL_DESCRIPTOR && events[3].code == 1 && events[3].length == sizeof numbered);
    assert(events[4].kind == CORDIAL_LAYOUT && events[4].service == 1);
    uint8_t numbered_report[] = {0x04, 0x32};
    classic_report(0x50, numbered_report, 2);
    assert(events[5].kind == CORDIAL_INPUT && events[5].report_id == 4 && events[5].data[0] == 0x32);
    cordial_profiles_poll(7); assert(event_count == 6); // Verified once.
    finish(cordial_by_id(id)); cordial_profiles_poll(8);

    // A descriptor Cordial cannot use ends the link as discovery would.
    id = classic_admission(0x61);
    hid_event(HID_SUBEVENT_CONNECTION_OPENED, 0x50, 0, 0x41);
    layout_answer = -CORDIAL_UNSUPPORTED;
    hid_event(HID_SUBEVENT_DESCRIPTOR_AVAILABLE, 0x50, 0, 0);
    cordial_profiles_poll(9);
    assert(cordial_by_id(id)->closing && cordial_by_id(id)->error == CORDIAL_UNSUPPORTED);
    layout_answer = 1;
    finish(cordial_by_id(id)); cordial_profiles_poll(10);
    // A missing or oversized SDP descriptor is the device's; a full host
    // descriptor store is not.
    static uint8_t oversized[CORDIAL_DESCRIPTOR_BYTES + 1];
    static const struct { uint8_t status; const uint8_t *data; uint16_t length; uint8_t error; } results[] = {
        {ERROR_CODE_UNSUPPORTED_FEATURE_OR_PARAMETER_VALUE, NULL, 0, CORDIAL_UNSUPPORTED},
        {0, oversized, sizeof oversized, CORDIAL_UNSUPPORTED},
        {ERROR_CODE_MEMORY_CAPACITY_EXCEEDED, NULL, 0, CORDIAL_CAPACITY},
    };
    for (unsigned c = 0; c < 3; ++c) {
        id = classic_admission(0x62 + c);
        hid_event(HID_SUBEVENT_CONNECTION_OPENED, 0x50, 0, 0x41);
        sdp_descriptor = results[c].data; sdp_length = results[c].length;
        hid_event(HID_SUBEVENT_DESCRIPTOR_AVAILABLE, 0x50, results[c].status, 0);
        assert(cordial_by_id(id)->closing && cordial_by_id(id)->error == results[c].error);
        finish(cordial_by_id(id)); cordial_profiles_poll(11);
    }
    // The host reports a failed SDP query as a failed connection.
    id = classic_admission(0x66);
    hid_event(HID_SUBEVENT_CONNECTION_OPENED, 0x50, 0, 0x41);
    hid_event(HID_SUBEVENT_CONNECTION_OPENED, 0x50, ERROR_CODE_CONNECTION_TIMEOUT, 0);
    assert(cordial_by_id(id)->closing && cordial_by_id(id)->error == CORDIAL_CONNECTION);
    finish(cordial_by_id(id)); cordial_profiles_poll(12);
}
static void classic_follows_its_setting(void) {
    // Off: not connectable, now and after every restart.
    assert(cordial_profiles_set_transport(CORDIAL_CLASSIC, false) == CORDIAL_OK && connectable == 0);
    uint8_t working[] = {BTSTACK_EVENT_STATE, 1, HCI_STATE_WORKING};
    connectable = -1; packet_handler(HCI_EVENT_PACKET, 0, working, sizeof working);
    assert(connectable == 0);
    // Incoming Classic connections are declined at the HID level.
    cordial_peer peer = { .transport = CORDIAL_CLASSIC, .address = {0x10, 0x20, 0x30, 0x40, 0x50, 0x70} };
    link_key_t key = {7};
    gap_store_link_key_for_bd_addr(peer.address, key, AUTHENTICATED_COMBINATION_KEY_GENERATED_FROM_P256);
    uint8_t request[14] = {HCI_EVENT_HID_META, 12, HID_SUBEVENT_INCOMING_CONNECTION, 0x51, 0};
    reverse_bd_addr(peer.address, request + 5); little_endian_store_16(request, 11, 0x45);
    event_count = 0; declines = 0;
    classic_event(request, sizeof request);
    assert(declines == 1 && !event_count);
    // No paging, inquiry or discovery results.
    cordial_link id = { .generation = 400, .slot = 0 };
    assert(cordial_profiles_connect(id, peer, false, NULL) == CORDIAL_TRANSPORT && !cordial_by_id(id));
    assert(cordial_profiles_scan(9, true, false) == CORDIAL_TRANSPORT);
    // Malformed requests are the caller's, never the device's.
    cordial_peer bad = peer; bad.random = 1;
    assert(cordial_profiles_connect(id, bad, false, NULL) == CORDIAL_ARGUMENT);
    bad.transport = 7;
    assert(cordial_profiles_connect(id, bad, false, NULL) == CORDIAL_ARGUMENT);
    assert(cordial_profiles_set_transport(7, true) == CORDIAL_ARGUMENT);
    assert(cordial_profiles_reconnect(&peer, 1) == CORDIAL_TRANSPORT);
    scan_classic = true;
    uint8_t result[27] = {GAP_EVENT_INQUIRY_RESULT, 25, 1, 2, 3, 4, 5, 6};
    packet_handler(HCI_EVENT_PACKET, 0, result, sizeof result);
    cordial_profiles_poll(20);
    assert(!event_count && !inquiry);
    scan_classic = false;

    // On: connectable, and the setting survives restarts.
    assert(cordial_profiles_set_transport(CORDIAL_CLASSIC, true) == CORDIAL_OK && connectable == 1);
    connectable = -1; packet_handler(HCI_EVENT_PACKET, 0, working, sizeof working);
    assert(connectable == 1);
    classic_event(request, sizeof request);
    assert(declines == 1 && events[0].kind == CORDIAL_INCOMING);
    // Turning off rejects a pending attempt and closes Classic links.
    cordial_connection *l = allocate((cordial_link){ .generation = 401, .slot = 1 },
        (cordial_peer){ .transport = CORDIAL_CLASSIC, .address = {1} }, false);
    assert(l); l->cid = 0x52;
    uint32_t attempt = 0;
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i) if (incoming[i].cid == 0x51) attempt = incoming[i].attempt;
    assert(attempt);
    assert(cordial_profiles_set_transport(CORDIAL_CLASSIC, false) == CORDIAL_OK && connectable == 0);
    assert(declines == 2 && hid_disconnects == 1 && l->closing);
    cordial_link late = { .generation = 402, .slot = 2 };
    assert(cordial_profiles_incoming(attempt, &late, NULL) == CORDIAL_CONNECTION && !cordial_by_id(late));
    finish(l); cordial_profiles_poll(21);
}
// Only a peer rejecting the keys is an authentication failure; the link
// failing during a security procedure is a recoverable connection failure.
static void security_failures_are_classified(void) {
    static const struct { uint8_t status, error; } cases[] = {
        {ERROR_CODE_PIN_OR_KEY_MISSING, CORDIAL_AUTHENTICATION},
        {ERROR_CODE_AUTHENTICATION_FAILURE, CORDIAL_AUTHENTICATION},
        {ERROR_CODE_CONNECTION_TERMINATED_DUE_TO_MIC_FAILURE, CORDIAL_AUTHENTICATION},
        {ERROR_CODE_INSUFFICIENT_SECURITY, CORDIAL_AUTHENTICATION},
        {ERROR_CODE_CONNECTION_TIMEOUT, CORDIAL_CONNECTION},
        {ERROR_CODE_REMOTE_USER_TERMINATED_CONNECTION, CORDIAL_CONNECTION},
        {ERROR_CODE_CONNECTION_TERMINATED_BY_LOCAL_HOST, CORDIAL_CONNECTION},
        {ERROR_CODE_LMP_RESPONSE_TIMEOUT_LL_RESPONSE_TIMEOUT, CORDIAL_CONNECTION},
        {ERROR_CODE_INSTANT_PASSED, CORDIAL_CONNECTION},
        {ERROR_CODE_UNACCEPTABLE_CONNECTION_PARAMETERS, CORDIAL_CONNECTION},
        {ERROR_CODE_CONNECTION_FAILED_TO_BE_ESTABLISHED, CORDIAL_CONNECTION},
        {ERROR_CODE_CONTROLLER_BUSY, CORDIAL_CONNECTION},
    };
    cordial_peer peer = saved_ble();
    uint8_t ended[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, 0x40, 0, 0x16};
    for (unsigned c = 0; c < sizeof cases / sizeof cases[0]; ++c) {
        for (unsigned admitted = 0; admitted < 2; ++admitted) {
            key_size = 0; event_count = 0;
            cordial_link id;
            if (admitted) {
                // Re-encryption ends after core admitted the link.
                id = admit(peer, 0x40, 0, NULL);
                reencryption_ended(0x40, cases[c].status);
            } else {
                // Re-encryption ends while the connection awaits admission.
                incoming[0].attempt = 77; incoming[0].peer = peer; incoming[0].handle = 0x40;
                incoming[0].closing = false; incoming[0].auth_error = CORDIAL_OK; incoming[0].deadline = 100000;
                reencryption_ended(0x40, cases[c].status);
                id = (cordial_link){ .generation = 500 + c, .slot = 0 };
                assert(cordial_profiles_incoming(77, &id, NULL) == CORDIAL_OK);
            }
            assert(cordial_by_id(id)->closing && cordial_by_id(id)->error == cases[c].error);
            packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended);
            cordial_profiles_poll(40);
            assert(events[event_count - 1].kind == CORDIAL_DISCONNECTED && events[event_count - 1].code == cases[c].error);
        }
    }
    // A saved device that asks to pair, or whose address no longer resolves,
    // has lost its bond.
    cordial_link id = admit(peer, 0x40, 0, NULL);
    uint8_t started[] = {SM_EVENT_PAIRING_STARTED, 9, 0x40, 0};
    sm_handler(HCI_EVENT_PACKET, 0, started, sizeof started);
    assert(cordial_by_id(id)->error == CORDIAL_AUTHENTICATION);
    packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended); cordial_profiles_poll(41);
    irk_state = IRK_LOOKUP_FAILED;
    id = admit(peer, 0x40, 0, NULL);
    assert(cordial_by_id(id)->error == CORDIAL_AUTHENTICATION);
    irk_state = IRK_LOOKUP_SUCCEEDED;
    packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended); cordial_profiles_poll(42);
    // Classic encryption and authentication failures follow the same rule.
    for (unsigned c = 0; c < 4; ++c) {
        cordial_connection *l = allocate((cordial_link){ .generation = 600 + c, .slot = 1 },
            (cordial_peer){ .transport = CORDIAL_CLASSIC, .address = {2, (uint8_t)c} }, false);
        assert(l); l->handle = 0x48;
        uint8_t status = c & 1 ? ERROR_CODE_PIN_OR_KEY_MISSING : ERROR_CODE_LMP_RESPONSE_TIMEOUT_LL_RESPONSE_TIMEOUT;
        uint8_t error = c & 1 ? CORDIAL_AUTHENTICATION : CORDIAL_CONNECTION;
        if (c < 2) {
            uint8_t change[] = {HCI_EVENT_ENCRYPTION_CHANGE, 4, status, 0x48, 0, 0};
            packet_handler(HCI_EVENT_PACKET, 0, change, sizeof change);
        } else {
            uint8_t authentication[] = {HCI_EVENT_AUTHENTICATION_COMPLETE, 3, status, 0x48, 0};
            packet_handler(HCI_EVENT_PACKET, 0, authentication, sizeof authentication);
        }
        assert(l->error == error);
        finish(l); cordial_profiles_poll(43);
    }
    // Encryption turned off without an error is not a key rejection.
    cordial_connection *l = allocate((cordial_link){ .generation = 610, .slot = 1 },
        (cordial_peer){ .transport = CORDIAL_CLASSIC, .address = {3} }, false);
    l->handle = 0x48;
    uint8_t paused[] = {HCI_EVENT_ENCRYPTION_CHANGE, 4, 0, 0x48, 0, 0};
    packet_handler(HCI_EVENT_PACKET, 0, paused, sizeof paused);
    assert(l->error == CORDIAL_CONNECTION);
    finish(l); cordial_profiles_poll(44);
}
// A closing link may take until its supervision timeout plus a margin to end
// once established, or the connecting bound otherwise.
static cordial_connection *closing(uint8_t transport, uint16_t handle) {
    static uint8_t next = 0;
    cordial_connection *l = allocate((cordial_link){ .generation = 700 + next, .slot = 2 },
        (cordial_peer){ .transport = transport, .address = {4, next} }, false);
    ++next;
    assert(l); l->handle = handle; l->cid = 0x53;
    return l;
}
static void bounded(cordial_connection *l, uint64_t start, uint32_t bound) {
    cordial_profiles_poll(start);
    cordial_profiles_disconnect(l->id);
    assert(l->closing && !l->ended && l->close_deadline == start + bound);
    if (l->handle != HCI_CON_HANDLE_INVALID) {
        uint8_t ended[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, (uint8_t)l->handle, (uint8_t)(l->handle >> 8), 0x16};
        packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended);
    } else finish(l);
    cordial_profiles_poll(start + 1);
    assert(!l->used);
}
static void close_bounds_follow_the_link(void) {
    // Classic without a reported timeout: the BR/EDR default of 20 s.
    bounded(closing(CORDIAL_CLASSIC, 0x49), 1000, 20000 + 2000);
    // A reported Classic timeout of 0x1F40 slots (5 s).
    cordial_connection *l = closing(CORDIAL_CLASSIC, 0x4a);
    uint8_t changed[] = {HCI_EVENT_LINK_SUPERVISION_TIMEOUT_CHANGED, 4, 0x4a, 0, 0x40, 0x1f};
    packet_handler(HCI_EVENT_PACKET, 0, changed, sizeof changed);
    bounded(l, 2000, 5000 + 2000);
    // LE without a reported timeout: LE's maximum of 32 s.
    bounded(closing(CORDIAL_BLE, 0x4b), 3000, 32000 + 2000);
    // An LE connection update to 2.56 s.
    l = closing(CORDIAL_BLE, 0x4c);
    uint8_t update[] = {HCI_EVENT_LE_META, 10, HCI_SUBEVENT_LE_CONNECTION_UPDATE_COMPLETE, 0, 0x4c, 0, 6, 0, 0, 0, 0x00, 0x01};
    packet_handler(HCI_EVENT_PACKET, 0, update, sizeof update);
    bounded(l, 4000, 2560 + 2000);
    // Without a connection there is no disconnect to wait for: the connecting bound.
    bounded(closing(CORDIAL_CLASSIC, HCI_CON_HANDLE_INVALID), 5000, 10000);
    hid_disconnects = 0;
}
// The close bound starts when a link starts closing; repeated requests keep
// it, and a completed connection sets the bound for its disconnect.
static void close_bounds_start_once(void) {
    cordial_connection *l = closing(CORDIAL_CLASSIC, 0x51);
    cordial_profiles_poll(1000);
    cordial_profiles_disconnect(l->id);
    assert(l->close_deadline == 1000 + 22000);
    cordial_profiles_poll(5000);
    cordial_profiles_disconnect(l->id);
    assert(l->close_deadline == 1000 + 22000);
    uint8_t ended[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, 0x51, 0, 0x16};
    packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended);
    cordial_profiles_poll(5001);
    // A page that completes while closing.
    l = closing(CORDIAL_CLASSIC, HCI_CON_HANDLE_INVALID);
    initiating[CORDIAL_CLASSIC] = l; initiating_started[CORDIAL_CLASSIC] = true;
    cordial_profiles_poll(6000);
    cordial_profiles_disconnect(l->id);
    assert(l->close_deadline == 6000 + 10000);
    cordial_profiles_poll(8000);
    uint8_t connected[13] = {HCI_EVENT_CONNECTION_COMPLETE, 11, 0, 0x52, 0};
    reverse_bd_addr(l->peer.address, connected + 5); connected[11] = 1;
    packet_handler(HCI_EVENT_PACKET, 0, connected, sizeof connected);
    assert(l->handle == 0x52 && l->close_deadline == 8000 + 22000);
    uint8_t retired[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, 0x52, 0, 0x16};
    packet_handler(HCI_EVENT_PACKET, 0, retired, sizeof retired);
    cordial_profiles_poll(8001);
    assert(!l->used);
    hid_disconnects = 0;
}
// A Classic offer in an entry a BLE offer used does not inherit its
// supervision timeout.
static void incoming_entries_start_clean(void) {
    incoming[0] = (cordial_incoming){ .attempt = 0, .peer = { .transport = CORDIAL_BLE }, .handle = 0x40,
        .supervision_ms = 2560 };
    cordial_peer peer = { .transport = CORDIAL_CLASSIC, .address = {0x10, 0x20, 0x30, 0x40, 0x50, 0x71} };
    link_key_t key = {7};
    gap_store_link_key_for_bd_addr(peer.address, key, AUTHENTICATED_COMBINATION_KEY_GENERATED_FROM_P256);
    uint8_t request[14] = {HCI_EVENT_HID_META, 12, HID_SUBEVENT_INCOMING_CONNECTION, 0x54, 0};
    reverse_bd_addr(peer.address, request + 5); little_endian_store_16(request, 11, 0x46);
    classic_event(request, sizeof request);
    assert(incoming[0].attempt && incoming[0].cid == 0x54 && !incoming[0].supervision_ms);
    cordial_link id = { .generation = 900, .slot = 0 };
    assert(cordial_profiles_incoming(incoming[0].attempt, &id, NULL) == CORDIAL_OK);
    cordial_connection *l = cordial_by_id(id);
    cordial_profiles_poll(9000);
    cordial_profiles_disconnect(id);
    assert(l->close_deadline == 9000 + 22000);
    uint8_t ended[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, 0x46, 0, 0x16};
    packet_handler(HCI_EVENT_PACKET, 0, ended, sizeof ended);
    cordial_profiles_poll(9001);
    hid_disconnects = 0;
}
static void end_restart(void) {
    restarting = restart_notice = stopping = false; ready = true; host_state = HCI_STATE_WORKING;
}
// A link past its close deadline keeps its slot while the host can still
// call into it, and restarts Bluetooth; the slot frees once the host has
// discarded the connection.
static void abandoned_links_keep_their_slot(void) {
    cordial_connection *l = allocate((cordial_link){ .generation = 800, .slot = 2 },
        (cordial_peer){ .transport = CORDIAL_BLE, .address = {8} }, false);
    assert(l);
    l->handle = 0x4e; l->ready = l->profile = l->authenticated = true; l->setup = SETUP_IDLE;
    l->reports[0] = (cordial_report){ .value = 9, .properties = ATT_PROPERTY_WRITE_WITHOUT_RESPONSE, .id = 2,
        .type = HID_REPORT_TYPE_OUTPUT };
    l->report_count = 1;
    uint8_t payload[1] = {1};
    queued_command = NULL;
    assert(cordial_profiles_write(l->id, 1, 0, HID_REPORT_TYPE_OUTPUT, 2, payload, 1) == CORDIAL_OK);
    assert(queued_command && queued_command->context == l);
    cordial_profiles_poll(1000);
    cordial_profiles_disconnect(l->id);
    event_count = 0;
    cordial_profiles_poll(1000 + 34000 - 1);
    assert(!l->abandoned && !restarting);
    cordial_profiles_poll(1000 + 34000);
    assert(l->used && l->abandoned && !l->ended && restarting);
    cordial_profiles_poll(1000 + 34001);
    assert(l->used && !kinds(CORDIAL_DISCONNECTED));
    // The host regains send capacity before it discards the connection.
    queued_command->callback(queued_command->context);
    assert(!commands_written && !kinds(CORDIAL_WRITTEN));
    // A new link cannot take the reserved slot.
    cordial_connection *other = allocate((cordial_link){ .generation = 801, .slot = 3 },
        (cordial_peer){ .transport = CORDIAL_BLE, .address = {9} }, false);
    assert(other && other != l);
    finish(other); cordial_profiles_poll(1000 + 34002);
    // The power cycle discards the connection; the slot frees and is reused.
    uint8_t discarded[] = {HCI_EVENT_DISCONNECTION_COMPLETE, 4, 0, 0x4e, 0, 0x16};
    packet_handler(HCI_EVENT_PACKET, 0, discarded, sizeof discarded);
    event_count = 0;
    cordial_profiles_poll(1000 + 34003);
    assert(kinds(CORDIAL_DISCONNECTED) == 1 && !l->used);
    cordial_connection *taken[CORDIAL_LINKS] = {0};
    bool reused = false;
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i) {
        taken[i] = allocate((cordial_link){ .generation = 802 + i, .slot = (uint8_t)i },
            (cordial_peer){ .transport = CORDIAL_BLE, .address = {10, (uint8_t)i} }, false);
        reused |= taken[i] == l;
    }
    assert(reused);
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i) if (taken[i]) finish(taken[i]);
    cordial_profiles_poll(1000 + 34004);
    end_restart();
    // A connecting link has nothing for the host to discard: it ends once the
    // host is off. A late HID result for its channel is closed.
    l = closing(CORDIAL_CLASSIC, HCI_CON_HANDLE_INVALID);
    initiating[CORDIAL_CLASSIC] = l; initiating_started[CORDIAL_CLASSIC] = true;
    cordial_profiles_poll(2000);
    cordial_profiles_disconnect(l->id);
    cordial_profiles_poll(2000 + 10000);
    assert(l->abandoned && restarting && l->used);
    event_count = 0;
    host_state = HCI_STATE_OFF;
    cordial_profiles_poll(2000 + 10001);
    cordial_profiles_poll(2000 + 10002);
    assert(kinds(CORDIAL_DISCONNECTED) == 1 && !l->used);
    hid_disconnects = 0;
    hid_event(HID_SUBEVENT_CONNECTION_OPENED, 0x53, 0, 0x49);
    assert(hid_disconnects == 1);
    hid_disconnects = 0;
    end_restart();
}
int main(void) {
    cordial_runtime_callbacks cb = {.time_ms=time_cb,.wake=wake_cb,.fatal=fatal_cb,.can_send=can_send_cb,.send=send_cb};
    btstack_tlv_t tlv = {.get_tag=get,.store_tag=save,.delete_tag=del};
    cordial_runtime_init(&cb, &tlv, NULL, NULL); cordial_profiles_init(NULL, event_cb);
    assert(connectable == 0); // Classic starts off.
    transport_enabled[CORDIAL_CLASSIC] = true;
    // HID links use a 7.5 ms interval without peripheral latency.
    assert(parameters[0] == 0x10 && parameters[1] == 0x10 && parameters[2] == 6 && parameters[3] == 6);
    assert(!parameters[4] && parameters[5] == 0x100 && !parameters[6] && !parameters[7]);
    assert(listener);
    ready = true;
    saved_ble_layout_admits_input_at_reencryption();
    discovered_ble_layout_reports_its_table();
    saved_hash_is_checked_before_admission();
    security_failures_are_classified();
    close_bounds_follow_the_link();
    close_bounds_start_once();
    abandoned_links_keep_their_slot();
    early_input_is_bounded_and_survives_queue_pressure();
    classic_layout_skips_sdp_and_follows_a_changed_descriptor();
    classic_follows_its_setting();
    transport_enabled[CORDIAL_CLASSIC] = true;
    incoming_entries_start_clean();
    puts("Saved layout reconnection, early input and Classic SDP regressions passed");
}
