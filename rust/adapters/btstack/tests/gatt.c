// Exercise adapter GATT callbacks: one discovery of two services with chunked
// report maps, CCCD writes, notification routing, background verification
// of a supplied layout and report write modes. Only the public query
// submissions are faked; event decoding is native.
#define gatt_client_discover_primary_services_by_uuid16 query_services
#define gatt_client_discover_characteristics_for_service query_characteristics
#define gatt_client_read_long_value_of_characteristic_using_value_handle query_read
#define gatt_client_read_value_of_characteristic_using_value_handle query_simple
#define gatt_client_discover_characteristic_descriptors query_descriptors
#define gatt_client_read_characteristic_descriptor_using_descriptor_handle query_reference
#define gatt_client_listen_for_characteristic_value_updates listen_updates
#define gatt_client_read_value_of_characteristics_by_uuid16 query_hash
#define gatt_client_get_mtu query_mtu
#define gatt_client_write_value_of_characteristic write_request
#define gatt_client_write_long_value_of_characteristic write_long
#define gatt_client_request_to_write_without_response request_command
#define gatt_client_write_value_of_characteristic_without_response write_command
#include "../c/gatt.c"
#include <assert.h>
#include <stdio.h>
enum { NONE, SERVICES, CHARACTERISTICS, READ, DESCRIPTORS, REFERENCE, WRITE, HASH };
static cordial_connection link;
static unsigned query, write_mode, write_done, map_count, readies, layouts, abandons, inputs, pushed;
static uint16_t requested, written[8], written_value[8], input_service;
static unsigned written_count;
static uint8_t maps[2][CORDIAL_DESCRIPTOR_BYTES], input_id, input_bytes[8];
static unsigned lengths[2];
static int layout_status = 1, map_status = 1;
static bool settled, verify_maps, reject_queries, reject_hash;
static unsigned begins;
static bool layout_hashed;
static uint8_t layout_hash[16];
// The device's Database Hash in device(): absent (Attribute Not Found) or this value.
static const uint8_t *device_hash;
static const uint8_t hash_a[16] = {0xa1, 2, 3}, hash_b[16] = {0xb1, 2, 3};
static cordial_report layout_table[CORDIAL_LAYOUT_REPORTS];
static unsigned layout_count;
static btstack_packet_handler_t listener;
static btstack_context_callback_registration_t *write_ready;
static uint8_t mtu_status;
static uint16_t mtu_value = 23;
uint8_t query_mtu(hci_con_handle_t handle, uint16_t *mtu) { (void)handle; *mtu=mtu_value; return mtu_status; }
uint8_t write_request(btstack_packet_handler_t cb,hci_con_handle_t h,uint16_t v,uint16_t n,uint8_t *p) {
    (void)cb;(void)h;
    if (link.query == QUERY_SUBSCRIBE) {
        assert(n == 2 && written_count < 8);
        written[written_count] = v; written_value[written_count++] = little_endian_read_16(p, 0);
    } else write_mode=1;
    query = WRITE; return 0;
}
uint8_t write_long(btstack_packet_handler_t cb,hci_con_handle_t h,uint16_t v,uint16_t n,uint8_t *p) {
    (void)cb;(void)h;(void)v;(void)n;(void)p;write_mode=2;return 0;
}
uint8_t request_command(btstack_context_callback_registration_t *cb,hci_con_handle_t h) {
    (void)h;write_mode=3;write_ready=cb;return 0;
}
uint8_t write_command(hci_con_handle_t h,uint16_t v,uint16_t n,uint8_t *p) {
    (void)h;(void)v;(void)n;(void)p;write_mode=4;return 0;
}
void listen_updates(gatt_client_notification_t *n,btstack_packet_handler_t cb,hci_con_handle_t h,gatt_client_characteristic_t *c) {
    (void)n; assert(h == GATT_CLIENT_ANY_CONNECTION && !c); listener = cb;
}
cordial_connection *cordial_by_handle(hci_con_handle_t handle) { return handle == link.handle ? &link : NULL; }
void cordial_fail(cordial_connection *l, uint8_t error) { l->closing = true; l->error = error; }
void cordial_ready(cordial_connection *l) { ++readies; l->ready = true; }
void cordial_read_done(cordial_connection *l, uint8_t error) { (void)l; (void)error; }
void cordial_write_done(cordial_connection *l, uint8_t error) { (void)l; assert(!error); ++write_done; }
bool cordial_info_settled(const cordial_connection *l) { (void)l; return settled; }
void cordial_begin_profile(cordial_connection *l) { (void)l; ++begins; }
uint8_t query_hash(btstack_packet_handler_t cb, hci_con_handle_t handle, uint16_t start, uint16_t end, uint16_t uuid) {
    (void)cb; assert(handle == link.handle && start == 1 && end == 0xffff && uuid == ORG_BLUETOOTH_CHARACTERISTIC_DATABASE_HASH);
    if (reject_hash) return GATT_CLIENT_IN_WRONG_STATE;
    query = HASH; return 0;
}
bool cordial_early_pending(hci_con_handle_t handle) { (void)handle; return false; }
void cordial_early_push(hci_con_handle_t handle, uint16_t value, const uint8_t *data, uint16_t length) {
    (void)handle; (void)value; (void)data; (void)length; ++pushed;
}
void cordial_input(cordial_connection *l, uint16_t service, uint8_t id, const uint8_t *bytes, uint16_t size) {
    (void)l; assert(size <= sizeof input_bytes);
    input_service = service; input_id = id; memcpy(input_bytes, bytes, size); ++inputs;
}
int cordial_emit_status(cordial_connection *l, cordial_event event) {
    (void)l;
    if (event.kind == CORDIAL_LAYOUT && event.code) { ++abandons; return 1; }
    if (event.kind == CORDIAL_LAYOUT) {
        ++layouts;
        if (layout_status > 0) {
            layout_count = event.length / sizeof(cordial_report);
            memcpy(layout_table, event.data, event.length);
            layout_hashed = event.hash != NULL;
            if (event.hash) memcpy(layout_hash, event.hash, 16);
        }
        return layout_status;
    }
    assert(event.kind == CORDIAL_DESCRIPTOR && event.service < 2 && event.code == verify_maps);
    memcpy(maps[event.service], event.data, event.length);
    lengths[event.service] = event.length; ++map_count; return map_status;
}
int cordial_emit_event(cordial_connection *l, cordial_event event) {
    int status = cordial_emit_status(l, event);
    if (status <= 0) cordial_fail(l, CORDIAL_CAPACITY);
    return status > 0;
}
uint8_t query_services(btstack_packet_handler_t cb, hci_con_handle_t handle, uint16_t uuid) {
    (void)cb; assert(handle == link.handle && uuid == ORG_BLUETOOTH_SERVICE_HUMAN_INTERFACE_DEVICE);
    query = SERVICES; return 0;
}
uint8_t query_characteristics(btstack_packet_handler_t cb, hci_con_handle_t handle, gatt_client_service_t *service) {
    (void)cb; assert(handle == link.handle); requested = service->start_group_handle;
    if (reject_queries) return GATT_CLIENT_IN_WRONG_STATE;
    query = CHARACTERISTICS; return 0;
}
uint8_t query_read(btstack_packet_handler_t cb, hci_con_handle_t handle, uint16_t value) {
    (void)cb; assert(handle == link.handle); requested = value; query = READ; return 0;
}
uint8_t query_simple(btstack_packet_handler_t cb, hci_con_handle_t handle, uint16_t value) {
    return query_read(cb, handle, value);
}
uint8_t query_descriptors(btstack_packet_handler_t cb, hci_con_handle_t handle, gatt_client_characteristic_t *characteristic) {
    (void)cb; assert(handle == link.handle && characteristic->end_handle > characteristic->value_handle);
    requested = characteristic->value_handle; query = DESCRIPTORS; return 0;
}
uint8_t query_reference(btstack_packet_handler_t cb, hci_con_handle_t handle, uint16_t value) {
    (void)cb; assert(handle == link.handle); requested = value; query = REFERENCE; return 0;
}
static void uuid(uint8_t *out, uint16_t value) {
    uint8_t full[16]; uuid_add_bluetooth_prefix(full, value); reverse_128(full, out);
}
static void complete(uint8_t status) {
    uint8_t event[] = {GATT_EVENT_QUERY_COMPLETE, 7, 0x40, 0, 0, 0, 0, 0, status};
    query = NONE;
    callback(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void service(uint16_t start, uint16_t end) {
    uint8_t event[28] = {GATT_EVENT_SERVICE_QUERY_RESULT, 26, 0x40, 0};
    little_endian_store_16(event, 8, start); little_endian_store_16(event, 10, end);
    uuid(event + 12, ORG_BLUETOOTH_SERVICE_HUMAN_INTERFACE_DEVICE);
    callback(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void characteristic(uint16_t value, uint16_t type, uint16_t properties) {
    uint8_t event[32] = {GATT_EVENT_CHARACTERISTIC_QUERY_RESULT, 30, 0x40, 0};
    little_endian_store_16(event, 8, value - 1); little_endian_store_16(event, 10, value);
    little_endian_store_16(event, 12, value + 2); little_endian_store_16(event, 14, properties);
    uuid(event + 16, type); callback(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void descriptor(uint16_t handle, uint16_t type) {
    uint8_t event[26] = {GATT_EVENT_ALL_CHARACTERISTIC_DESCRIPTORS_QUERY_RESULT, 24, 0x40, 0};
    little_endian_store_16(event, 8, handle); uuid(event + 10, type);
    callback(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void reference_value(uint8_t id, uint8_t type) {
    uint8_t event[14] = {GATT_EVENT_CHARACTERISTIC_DESCRIPTOR_QUERY_RESULT, 12, 0x40, 0};
    little_endian_store_16(event, 8, requested); little_endian_store_16(event, 10, 2);
    event[12] = id; event[13] = type; callback(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void chunk(uint16_t offset, const uint8_t *data, uint16_t length) {
    uint8_t event[128] = {GATT_EVENT_LONG_CHARACTERISTIC_VALUE_QUERY_RESULT, 0, 0x40, 0};
    assert(length <= sizeof event - 14); event[1] = 12 + length;
    little_endian_store_16(event, 10, offset); little_endian_store_16(event, 12, length);
    memcpy(event + 14, data, length); callback(HCI_EVENT_PACKET, 0, event, 14 + length);
}
static void notify(uint16_t value, uint8_t byte) {
    uint8_t event[13] = {GATT_EVENT_NOTIFICATION, 11, 0x40, 0};
    little_endian_store_16(event, 8, value); little_endian_store_16(event, 10, 1); event[12] = byte;
    listener(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void hash_result(const uint8_t *value, uint16_t length) {
    uint8_t event[12 + 17] = {GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT, (uint8_t)(10 + length), 0x40, 0};
    assert(length <= 17);
    little_endian_store_16(event, 8, 0x10); little_endian_store_16(event, 10, length);
    memcpy(event + 12, value, length); callback(HCI_EVENT_PACKET, 0, event, 12 + length);
}
// Verification issues one query per idle poll; setup chains queries.
static void advance(void) {
    if (link.setup == SETUP_VERIFY && query == NONE) cordial_gatt_poll(&link);
}
// The device has no Database Hash.
static void no_hash(void) {
    advance(); assert(query == HASH);
    complete(ATT_ERROR_ATTRIBUTE_NOT_FOUND); advance();
}
static const uint8_t first[] = {0x05, 0x01, 0x09, 0x06}, second[] = {0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01};
// The device: service 1..20 with map 3, input 5 (notify) and output 9;
// service 21..40 with map 23 and a notifying feature 25.
static void report(uint16_t value, uint8_t id, uint8_t type, bool cccd) {
    advance(); assert(query == DESCRIPTORS && requested == value);
    descriptor(value + 1, ORG_BLUETOOTH_DESCRIPTOR_REPORT_REFERENCE);
    if (cccd) descriptor(value + 2, ORG_BLUETOOTH_DESCRIPTOR_GATT_CLIENT_CHARACTERISTIC_CONFIGURATION);
    complete(0); advance(); assert(query == REFERENCE && requested == value + 1);
    reference_value(id, type); complete(0);
}
static void device(uint16_t input_value) {
    advance(); assert(query == HASH);
    if (device_hash) { hash_result(device_hash, 16); complete(0); }
    else complete(ATT_ERROR_ATTRIBUTE_NOT_FOUND);
    advance(); assert(query == SERVICES);
    service(1, 20); service(21, 40); complete(0);
    advance(); assert(query == CHARACTERISTICS && requested == 1);
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ);
    characteristic(input_value, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_READ | ATT_PROPERTY_NOTIFY);
    characteristic(9, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_READ | ATT_PROPERTY_WRITE);
    complete(0);
    advance(); assert(query == READ && requested == 3);
    chunk(0, first, 2); chunk(2, first + 2, 2); complete(0);
    advance(); assert(query == CHARACTERISTICS && requested == 21 && map_count == 1);
    characteristic(23, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ);
    characteristic(25, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_READ | ATT_PROPERTY_INDICATE);
    complete(0);
    advance(); assert(query == READ && requested == 23);
    chunk(0, second, sizeof second); complete(0);
    assert(map_count == 2);
    report(input_value, 1, HID_REPORT_TYPE_INPUT, true);
    report(9, 2, HID_REPORT_TYPE_OUTPUT, false);
    report(25, 1, HID_REPORT_TYPE_FEATURE, true);
}
static void reset(void) {
    memset(&link, 0, sizeof link); link.handle = 0x40; link.used = true;
    link.peer.transport = CORDIAL_BLE; link.authenticated = link.adopted = true;
    query = NONE; map_count = readies = layouts = abandons = inputs = pushed = written_count = begins = 0;
    device_hash = NULL; reject_hash = false;
    memset(lengths, 0, sizeof lengths); layout_status = map_status = 1; settled = verify_maps = false;
    owner = NULL; step_due = false;
}
static void discovered_once_with_subscriptions_and_routing(void) {
    reset();
    assert(cordial_gatt_discover(&link, false) && link.setup == SETUP_DISCOVER);
    cordial_connection other = link; other.handle = 0x41;
    assert(!cordial_gatt_discover(&other, false)); // One link discovers at a time.
    device(5);
    // CCCDs are written for the input and the notifying feature, in order.
    assert(query == WRITE && written_count == 1 && written[0] == 7 && written_value[0] == 1);
    assert(!readies && link.setup == SETUP_SUBSCRIBE && !owner);
    complete(0); assert(written_count == 2 && written[1] == 27 && written_value[1] == 2);
    complete(0);
    assert(readies == 1 && link.profile && link.setup == SETUP_IDLE && !link.closing);
    assert(lengths[0] == sizeof first && !memcmp(maps[0], first, sizeof first));
    assert(lengths[1] == sizeof second && !memcmp(maps[1], second, sizeof second));
    assert(link.report_count == 3);
    assert(link.reports[0].value == 5 && link.reports[0].cccd == 7 && link.reports[0].service == 0 &&
           link.reports[0].id == 1 && link.reports[0].type == HID_REPORT_TYPE_INPUT &&
           link.reports[0].properties == (ATT_PROPERTY_READ | ATT_PROPERTY_NOTIFY));
    assert(link.reports[1].value == 9 && !link.reports[1].cccd && link.reports[1].type == HID_REPORT_TYPE_OUTPUT);
    assert(link.reports[2].value == 25 && link.reports[2].service == 1 && link.reports[2].type == HID_REPORT_TYPE_FEATURE);
    notify(5, 0x42); assert(inputs == 1 && input_service == 0 && input_id == 1 && input_bytes[0] == 0x42);
    notify(25, 0x43); assert(inputs == 2 && input_service == 1 && input_id == 1);
    notify(9, 0x44); notify(77, 0x45); assert(inputs == 2); // Output and unknown handles.
    link.ready = false; notify(5, 0x46); assert(inputs == 2 && pushed == 1); // Early input waits.
}
static void discovery_failures_end_the_link(void) {
    reset(); cordial_gatt_discover(&link, false); no_hash();
    service(1, 20); complete(0); characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ); complete(0);
    chunk(0, first, sizeof first); complete(ATT_ERROR_UNLIKELY_ERROR);
    // Partial reads are not emitted; the device's error response is its answer.
    assert(link.closing && link.error == CORDIAL_UNSUPPORTED && !map_count && !owner);
    reset(); cordial_gatt_discover(&link, false); no_hash();
    service(1, 20); complete(0); characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ); complete(0);
    chunk(2, first, sizeof first);
    // Missing or reordered chunks are rejected without blaming the layout.
    assert(link.closing && link.error == CORDIAL_CONNECTION && !map_count);
    // Host failures during discovery are recoverable: a disconnect while a
    // query is pending, and a refused query.
    reset(); cordial_gatt_discover(&link, false); no_hash();
    service(1, 20); complete(ATT_ERROR_HCI_DISCONNECT_RECEIVED);
    assert(link.closing && link.error == CORDIAL_CONNECTION && !owner);
    reset(); cordial_gatt_discover(&link, false); no_hash();
    service(1, 20); reject_queries = true; complete(0); reject_queries = false;
    assert(link.closing && link.error == CORDIAL_CONNECTION && !owner);
    reset(); cordial_gatt_discover(&link, false); no_hash();
    service(1, 20); complete(0); characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ); complete(0);
    uint8_t large[64] = {0};
    for (unsigned offset = 0; offset < CORDIAL_DESCRIPTOR_BYTES; offset += sizeof large) chunk(offset, large, sizeof large);
    chunk(CORDIAL_DESCRIPTOR_BYTES, first, 1);
    assert(link.closing && link.error == CORDIAL_UNSUPPORTED && !map_count);
    // An input report without a CCCD cannot deliver input.
    reset(); cordial_gatt_discover(&link, false); no_hash();
    service(1, 20); complete(0);
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ);
    characteristic(5, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_NOTIFY); complete(0);
    chunk(0, first, sizeof first); complete(0);
    report(5, 1, HID_REPORT_TYPE_INPUT, false);
    assert(link.closing && link.error == CORDIAL_UNSUPPORTED && !written_count);
    // Duplicate report references are rejected.
    reset(); cordial_gatt_discover(&link, false); no_hash();
    service(1, 20); complete(0);
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ);
    characteristic(5, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_NOTIFY);
    characteristic(9, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_NOTIFY); complete(0);
    chunk(0, first, sizeof first); complete(0);
    report(5, 1, HID_REPORT_TYPE_INPUT, true); report(9, 1, HID_REPORT_TYPE_INPUT, true);
    assert(link.closing && !written_count);
    // A failed CCCD write ends a discovered link before Connected; the device
    // reconnects and setup runs again.
    reset(); cordial_gatt_discover(&link, false); no_hash();
    service(1, 20); complete(0);
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ);
    characteristic(5, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_NOTIFY); complete(0);
    chunk(0, first, sizeof first); complete(0);
    report(5, 1, HID_REPORT_TYPE_INPUT, true);
    assert(written_count == 1); complete(ATT_ERROR_WRITE_NOT_PERMITTED);
    assert(link.closing && link.error == CORDIAL_CONNECTION && !readies);
}
// The link already routes by the supplied table and has written its CCCDs.
static void cached(uint16_t input_value) {
    reset();
    link.ready = link.profile = link.cached = true; link.setup = SETUP_IDLE;
    link.report_count = 3;
    link.reports[0] = (cordial_report){ .value = input_value, .cccd = input_value + 2, .service = 0, .id = 1,
        .type = HID_REPORT_TYPE_INPUT, .properties = ATT_PROPERTY_READ | ATT_PROPERTY_NOTIFY };
    link.reports[1] = (cordial_report){ .value = 9, .service = 0, .id = 2, .type = HID_REPORT_TYPE_OUTPUT,
        .properties = ATT_PROPERTY_READ | ATT_PROPERTY_WRITE };
    link.reports[2] = (cordial_report){ .value = 25, .cccd = 27, .service = 1, .id = 1,
        .type = HID_REPORT_TYPE_FEATURE, .properties = ATT_PROPERTY_READ | ATT_PROPERTY_INDICATE };
    verify_maps = true;
}
static void verification_runs_in_the_background(void) {
    // Verification waits for the information pass and an idle link.
    cached(5);
    cordial_gatt_poll(&link); assert(query == NONE && !owner);
    settled = true; link.writing = true;
    cordial_gatt_poll(&link); assert(query == NONE && !owner);
    link.writing = false; cordial_gatt_poll(&link);
    assert(query == HASH && owner == &link && link.setup == SETUP_VERIFY);
    no_hash(); assert(query == SERVICES);
    service(1, 20); service(21, 40); complete(0);
    // A completed query yields: writes and reads go first.
    assert(query == NONE && !link.query);
    link.reading = true; cordial_gatt_poll(&link); assert(query == NONE);
    link.reading = false;
    notify(5, 0x50); assert(inputs == 1 && input_id == 1); // Routing continues meanwhile.
    cordial_gatt_poll(&link); assert(query == CHARACTERISTICS);
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ);
    characteristic(5, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_READ | ATT_PROPERTY_NOTIFY);
    characteristic(9, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_READ | ATT_PROPERTY_WRITE);
    complete(0); advance(); chunk(0, first, sizeof first); complete(0);
    advance();
    characteristic(23, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ);
    characteristic(25, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_READ | ATT_PROPERTY_INDICATE);
    complete(0); advance(); chunk(0, second, sizeof second); complete(0);
    assert(map_count == 2);
    report(5, 1, HID_REPORT_TYPE_INPUT, true);
    report(9, 2, HID_REPORT_TYPE_OUTPUT, false);
    report(25, 1, HID_REPORT_TYPE_FEATURE, true);
    // An equal layout writes nothing and leaves verification finished.
    assert(layouts == 1 && layout_count == 3 && !owner && !link.cached && link.setup == SETUP_IDLE);
    assert(!written_count && query == NONE && !link.closing);
    cordial_gatt_poll(&link); assert(query == NONE);

    // A changed layout switches routing when accepted and writes its CCCDs.
    cached(5); settled = true;
    cordial_gatt_poll(&link); device(11);
    assert(layouts == 1 && link.reports[0].value == 11 && link.reports[0].cccd == 13);
    assert(link.setup == SETUP_SUBSCRIBE && query == WRITE && written[0] == 13);
    notify(11, 0x51); assert(inputs == 1 && input_service == 0 && input_id == 1);
    notify(5, 0x52); assert(inputs == 1);
    complete(0); assert(written[1] == 27); complete(0);
    assert(link.setup == SETUP_IDLE && !link.closing && readies == 0);
    // A changed layout whose CCCDs cannot be written ends the link as discovery does.
    cached(5); settled = true;
    cordial_gatt_poll(&link); device(11);
    complete(ATT_ERROR_WRITE_NOT_PERMITTED);
    assert(link.closing && link.error == CORDIAL_CONNECTION);

    // Queue pressure delays the result; it is offered again from poll.
    cached(5); settled = true; layout_status = 0;
    cordial_gatt_poll(&link); device(11);
    assert(layouts == 1 && link.reports[0].value == 5 && owner == &link);
    layout_status = 1; cordial_gatt_poll(&link);
    assert(layouts == 2 && link.reports[0].value == 11 && !owner);

    // A layout Rust cannot use ends the link with the discovery error.
    cached(5); settled = true; layout_status = -CORDIAL_CAPACITY;
    cordial_gatt_poll(&link); device(11);
    assert(link.closing && link.error == CORDIAL_CAPACITY && !owner && !written_count);
    // So do refused maps.
    cached(5); settled = true; map_status = 0;
    cordial_gatt_poll(&link); no_hash(); service(1, 20); complete(0); advance();
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ); complete(0);
    advance(); chunk(0, first, sizeof first); complete(0);
    assert(link.closing && link.error == CORDIAL_CAPACITY && !owner);
    // And a device whose current layout discovery would refuse.
    cached(5); settled = true;
    cordial_gatt_poll(&link); no_hash(); service(1, 20); complete(0); advance();
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ);
    characteristic(5, ORG_BLUETOOTH_CHARACTERISTIC_REPORT, ATT_PROPERTY_NOTIFY); complete(0);
    advance(); chunk(0, first, sizeof first); complete(0);
    report(5, 1, HID_REPORT_TYPE_INPUT, false);
    assert(link.closing && link.error == CORDIAL_UNSUPPORTED && !layouts && !owner);
    cached(5); settled = true;
    cordial_gatt_poll(&link); no_hash(); service(1, 20); complete(0); advance();
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ); complete(0);
    advance(); uint8_t large[64] = {0};
    for (unsigned offset = 0; offset < CORDIAL_DESCRIPTOR_BYTES; offset += sizeof large) chunk(offset, large, sizeof large);
    chunk(CORDIAL_DESCRIPTOR_BYTES, first, 1);
    assert(link.closing && link.error == CORDIAL_UNSUPPORTED && !owner);

    // Transport failures abandon verification and keep the supplied layout.
    cached(5); settled = true;
    cordial_gatt_poll(&link); no_hash(); service(1, 20); complete(0);
    advance(); complete(ATT_ERROR_UNLIKELY_ERROR);
    assert(!link.closing && !owner && !link.cached && link.setup == SETUP_IDLE && link.reports[0].value == 5);
    assert(abandons == 1);
    // A malformed transfer drains: the link waits until the GATT client is idle.
    cached(5); settled = true;
    cordial_gatt_poll(&link); no_hash(); service(1, 20); complete(0); advance();
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP, ATT_PROPERTY_READ); complete(0);
    advance(); chunk(2, first, sizeof first); chunk(0, first, sizeof first);
    assert(link.query && owner == &link && !abandons);
    cordial_gatt_poll(&link); assert(query == READ); // Nothing new is issued.
    complete(0);
    assert(!link.closing && !owner && !link.cached && !map_count && !link.query && abandons == 1);
    cached(5); settled = true;
    cordial_gatt_poll(&link); no_hash(); assert(query == SERVICES);
    service(1, 20); complete(0);
    // A rejected query submission is a transport failure too.
    reject_queries = true; advance(); reject_queries = false;
    assert(!link.closing && !owner && !link.cached && link.setup == SETUP_IDLE && abandons == 1);
    notify(5, 0x53); assert(inputs == 1 && input_id == 1);
}
// A failed CCCD write on unconfirmed saved handles.
static void failed_cached_write(void) {
    cached(5); link.setup = SETUP_SUBSCRIBE; link.cursor = 0;
    cordial_gatt_subscribe(&link); assert(query == WRITE && written[0] == 7);
    complete(ATT_ERROR_WRITE_NOT_PERMITTED);
    // The rest of the pass is still written.
    assert(!link.closing && query == WRITE && written_count == 2 && written[1] == 27);
    complete(0);
    assert(!link.closing && link.setup == SETUP_IDLE && link.resubscribe);
}
static void failed_cached_subscription_verifies_at_once(void) {
    failed_cached_write();
    cordial_gatt_poll(&link); assert(query == HASH && owner == &link); // No information wait.
    device(5);
    // The confirmed layout is subscribed again; success keeps the link.
    assert(layouts == 1 && link.setup == SETUP_SUBSCRIBE && written_count == 3 && written[2] == 7);
    complete(0); complete(0);
    assert(!link.closing && link.setup == SETUP_IDLE && !link.resubscribe && !link.cached);
    cordial_gatt_poll(&link); assert(query == NONE);
    // A failure on the confirmed handles ends the link instead of leaving it deaf.
    failed_cached_write();
    cordial_gatt_poll(&link); device(5);
    complete(ATT_ERROR_WRITE_NOT_PERMITTED);
    assert(link.closing && link.error == CORDIAL_CONNECTION && written_count == 3);
    // So does a verification that cannot confirm the handles.
    failed_cached_write();
    cordial_gatt_poll(&link); assert(query == HASH);
    complete(ATT_ERROR_HCI_DISCONNECT_RECEIVED);
    assert(link.closing && link.error == CORDIAL_CONNECTION && !owner);
}
static void database_hash_in_discovery(void) {
    // Read once over all handles and reported with the table.
    reset(); device_hash = hash_a; cordial_gatt_discover(&link, false); device(5);
    complete(0); complete(0);
    assert(readies == 1 && link.hashed && !memcmp(link.hash, hash_a, 16));
    // An error response from the device means it has no hash.
    reset(); cordial_gatt_discover(&link, false); device(5); complete(0); complete(0);
    assert(readies == 1 && !link.hashed && !link.closing);
    // So do malformed and repeated values, and the host's Invalid PDU.
    reset(); cordial_gatt_discover(&link, false); hash_result(hash_a, 15); complete(0);
    assert(!link.closing && query == SERVICES && !table_hashed);
    reset(); cordial_gatt_discover(&link, false); hash_result(hash_a, 16); hash_result(hash_b, 16); complete(0);
    assert(!link.closing && query == SERVICES && !table_hashed);
    reset(); cordial_gatt_discover(&link, false); complete(ATT_ERROR_INVALID_PDU);
    assert(!link.closing && query == SERVICES && !table_hashed);
    // Host failures teach nothing and end setup.
    for (unsigned c = 0; c < 4; ++c) {
        reset(); reject_hash = c == 3; assert(cordial_gatt_discover(&link, false));
        if (c < 3) complete(c == 0 ? ATT_ERROR_TIMEOUT : c == 1 ? ATT_ERROR_HCI_DISCONNECT_RECEIVED : ATT_ERROR_BONDING_INFORMATION_MISSING);
        assert(link.closing && link.error == CORDIAL_CONNECTION && !owner);
    }
}
static void verification_compares_the_hash(void) {
    cached(5); link.hashed = true; memcpy(link.hash, hash_a, 16); settled = true; device_hash = hash_a;
    cordial_gatt_poll(&link); device(5);
    assert(layouts == 1 && layout_hashed && !memcmp(layout_hash, hash_a, 16) && !written_count);
    // A hash-only change is reported; the table is unchanged, so no CCCD writes.
    cached(5); link.hashed = true; memcpy(link.hash, hash_a, 16); settled = true; device_hash = hash_b;
    cordial_gatt_poll(&link); device(5);
    assert(layouts == 1 && layout_hashed && !memcmp(layout_hash, hash_b, 16) && !written_count);
    assert(!memcmp(link.hash, hash_b, 16) && link.setup == SETUP_IDLE);
    cached(5); link.hashed = true; settled = true;
    cordial_gatt_poll(&link); device(5);
    assert(layouts == 1 && !layout_hashed && !link.hashed);
    // A malformed hash is no hash.
    cached(5); link.hashed = true; settled = true;
    cordial_gatt_poll(&link); assert(query == HASH);
    hash_result(hash_a, 15); complete(0); advance();
    assert(!link.closing && !abandons && query == SERVICES && !table_hashed);
    // A host failure abandons verification quietly.
    cached(5); settled = true;
    cordial_gatt_poll(&link); complete(ATT_ERROR_HCI_DISCONNECT_RECEIVED);
    assert(!link.closing && abandons == 1 && !owner && !link.cached);
}
// A saved layout with a hash waits for the device's current hash.
static void pending_admission(bool rejected) {
    cached(5); link.ready = link.profile = false; link.setup = SETUP_NONE;
    link.hashed = true; memcpy(link.hash, hash_a, 16); reject_hash = rejected;
    cordial_gatt_check_hash(&link);
}
static void supplied_hash_is_checked_before_use(void) {
    pending_admission(false);
    assert(query == HASH && link.setup == SETUP_HASH && link.query);
    hash_result(hash_a, 16); complete(0);
    assert(begins == 1 && link.hash_equal && link.cached && link.setup == SETUP_NONE && link.report_count == 3);
    // A different, absent, malformed or repeated hash drops the saved layout,
    // so setup discovers.
    for (unsigned c = 0; c < 6; ++c) {
        pending_admission(false);
        if (c == 0) { hash_result(hash_b, 16); complete(0); }
        else if (c == 1) complete(ATT_ERROR_ATTRIBUTE_NOT_FOUND);
        else if (c == 2) complete(ATT_ERROR_INSUFFICIENT_AUTHORIZATION);
        else if (c == 3) { hash_result(hash_a, 15); complete(0); }
        else if (c == 4) { hash_result(hash_a, 16); hash_result(hash_a, 16); complete(0); }
        else complete(ATT_ERROR_INVALID_PDU);
        assert(begins == 1 && !link.cached && !link.report_count && !link.closing && link.setup == SETUP_NONE);
    }
    // Host failures leave the comparison unknown and end the attempt with a
    // recoverable error; the saved layout stays usable for the next one.
    for (unsigned c = 0; c < 4; ++c) {
        pending_admission(c == 3);
        if (c == 0) complete(ATT_ERROR_TIMEOUT);
        else if (c == 1) complete(ATT_ERROR_HCI_DISCONNECT_RECEIVED);
        else if (c == 2) complete(ATT_ERROR_BONDING_INFORMATION_MISSING);
        assert(!begins && link.closing && link.error == CORDIAL_CONNECTION);
    }
}
static void write_modes(void) {
    // Output Data prefers advertised commands, while feature writes and long
    // values retain request/long-write semantics. Request-only reports still work.
    const struct { uint8_t type, properties; uint16_t length; unsigned mode; } cases[] = {
        {HID_REPORT_TYPE_OUTPUT, 0x0c, 19, 3},
        {HID_REPORT_TYPE_OUTPUT, 0x04, 19, 3},
        {HID_REPORT_TYPE_OUTPUT, 0x08, 19, 1},
        {HID_REPORT_TYPE_FEATURE, 0x0c, 19, 1},
        {HID_REPORT_TYPE_OUTPUT, 0x0c, 21, 2},
        {HID_REPORT_TYPE_OUTPUT, 0x04, 21, 0},
    };
    for(unsigned i=0;i<sizeof(cases)/sizeof(cases[0]);i++) {
        memset(&link,0,sizeof link);link.handle=0x40;link.used=true;link.writing=true;
        link.report_count=1;link.operation_type=cases[i].type;link.operation_id=9;link.length=cases[i].length;
        link.reports[0].type=cases[i].type;link.reports[0].id=9;
        link.reports[0].properties=cases[i].properties;
        write_mode=write_done=0;write_ready=NULL;
        assert(cordial_gatt_write(&link)==(cases[i].mode ? CORDIAL_OK:CORDIAL_REPORT_SIZE));
        assert(write_mode==cases[i].mode && !write_done);
        if(write_mode==3) { assert(write_ready);write_ready->callback(write_ready->context);assert(write_mode==4 && write_done==1); }
        else if(write_mode) { complete(0);assert(write_done==1); }
    }
    // Writes use the default MTU while its exchange is pending.
    mtu_status = GATT_CLIENT_IN_WRONG_STATE; write_mode = 0;
    link.operation_type = link.reports[0].type = HID_REPORT_TYPE_FEATURE; link.reports[0].properties = 0x08; link.length = 19;
    assert(cordial_gatt_write(&link) == CORDIAL_OK && write_mode == 1); complete(0);
    mtu_value = 0; link.writing = true;
    assert(cordial_gatt_write(&link) == CORDIAL_CONNECTION);
    mtu_status = 0; mtu_value = 23;
}
int main(void) {
    cordial_gatt_init(); assert(listener);
    discovered_once_with_subscriptions_and_routing();
    discovery_failures_end_the_link();
    verification_runs_in_the_background();
    failed_cached_subscription_verifies_at_once();
    database_hash_in_discovery();
    verification_compares_the_hash();
    supplied_hash_is_checked_before_use();
    write_modes();
    puts("Single GATT discovery, subscription, routing, verification and write-mode regressions passed");
}
