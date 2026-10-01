// Exercise adapter GATT callbacks with two services and chunked report maps.
// Only the public query submissions are faked; event decoding is native.
#define gatt_client_discover_primary_services_by_uuid16 query_services
#define gatt_client_discover_characteristics_for_service query_characteristics
#define gatt_client_read_long_value_of_characteristic_using_value_handle query_read
#define gatt_client_discover_characteristic_descriptors query_descriptors
#define gatt_client_read_characteristic_descriptor_using_descriptor_handle query_reference
#define gatt_client_get_mtu query_mtu
#define gatt_client_write_value_of_characteristic write_request
#define gatt_client_write_long_value_of_characteristic write_long
#define gatt_client_request_to_write_without_response request_command
#define gatt_client_write_value_of_characteristic_without_response write_command
#include "../c/gatt.c"
#include <assert.h>
#include <stdio.h>
static cordial_connection link;
static uint16_t requested;
static uint8_t maps[2][CORDIAL_DESCRIPTOR_BYTES];
static unsigned lengths[2], map_count, write_mode, write_done;
static btstack_context_callback_registration_t *write_ready;
uint8_t query_mtu(hci_con_handle_t handle, uint16_t *mtu) { (void)handle; *mtu=23; return 0; }
uint8_t write_request(btstack_packet_handler_t cb,hci_con_handle_t h,uint16_t v,uint16_t n,uint8_t *p) {
    (void)cb;(void)h;(void)v;(void)n;(void)p;write_mode=1;return 0;
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
cordial_connection *cordial_by_handle(hci_con_handle_t handle) { return handle == link.handle ? &link : NULL; }
void cordial_fail(cordial_connection *l, uint8_t error) { l->closing = true; l->error = error; }
void cordial_ready(cordial_connection *l) { (void)l; }
void cordial_read_done(cordial_connection *l, uint8_t error) { (void)l; (void)error; }
void cordial_write_done(cordial_connection *l, uint8_t error) { (void)l; assert(!error); ++write_done; }
int cordial_emit_event(cordial_connection *l, cordial_event event) {
    (void)l; assert(event.kind == CORDIAL_DESCRIPTOR && event.service < 2);
    memcpy(maps[event.service], event.data, event.length);
    lengths[event.service] = event.length; ++map_count; return 1;
}
uint8_t query_services(btstack_packet_handler_t cb, hci_con_handle_t handle, uint16_t uuid) {
    (void)cb; assert(handle == link.handle && uuid == ORG_BLUETOOTH_SERVICE_HUMAN_INTERFACE_DEVICE); return 0;
}
uint8_t query_characteristics(btstack_packet_handler_t cb, hci_con_handle_t handle, gatt_client_service_t *service) {
    (void)cb; assert(handle == link.handle); requested = service->start_group_handle; return 0;
}
uint8_t query_read(btstack_packet_handler_t cb, hci_con_handle_t handle, uint16_t value) {
    (void)cb; assert(handle == link.handle); requested = value; return 0;
}
uint8_t query_descriptors(btstack_packet_handler_t cb, hci_con_handle_t handle, gatt_client_characteristic_t *characteristic) {
    (void)cb; assert(handle == link.handle); requested = characteristic->value_handle; return 0;
}
uint8_t query_reference(btstack_packet_handler_t cb, hci_con_handle_t handle, uint16_t value) {
    return query_read(cb, handle, value);
}
static void uuid(uint8_t *out, uint16_t value) {
    uint8_t full[16]; uuid_add_bluetooth_prefix(full, value); reverse_128(full, out);
}
static void complete(uint8_t status) {
    uint8_t event[] = {GATT_EVENT_QUERY_COMPLETE, 7, 0x40, 0, 0, 0, 0, 0, status};
    callback(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void service(uint16_t start, uint16_t end) {
    uint8_t event[28] = {GATT_EVENT_SERVICE_QUERY_RESULT, 26, 0x40, 0};
    little_endian_store_16(event, 8, start); little_endian_store_16(event, 10, end);
    uuid(event + 12, ORG_BLUETOOTH_SERVICE_HUMAN_INTERFACE_DEVICE);
    callback(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void characteristic(uint16_t value, uint16_t type) {
    uint8_t event[32] = {GATT_EVENT_CHARACTERISTIC_QUERY_RESULT, 30, 0x40, 0};
    little_endian_store_16(event, 8, value - 1); little_endian_store_16(event, 10, value);
    little_endian_store_16(event, 12, value + 1); little_endian_store_16(event, 14, ATT_PROPERTY_READ);
    uuid(event + 16, type); callback(HCI_EVENT_PACKET, 0, event, sizeof event);
}
static void chunk(uint16_t offset, const uint8_t *data, uint16_t length) {
    uint8_t event[128] = {GATT_EVENT_LONG_CHARACTERISTIC_VALUE_QUERY_RESULT, 0, 0x40, 0};
    assert(length <= sizeof event - 14); event[1] = 12 + length;
    little_endian_store_16(event, 10, offset); little_endian_store_16(event, 12, length);
    memcpy(event + 14, data, length); callback(HCI_EVENT_PACKET, 0, event, 14 + length);
}
static void start(void) {
    memset(&link, 0, sizeof link); link.handle = 0x40;
    map_count = 0; memset(lengths, 0, sizeof lengths);
    cordial_gatt_begin(&link); service(1, 20); service(21, 40); complete(0);
    assert(requested == 1);
    characteristic(3, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP);
    characteristic(5, ORG_BLUETOOTH_CHARACTERISTIC_REPORT); complete(0);
    assert(requested == 3);
}
int main(void) {
    const uint8_t first[] = {0x05, 0x01, 0x09, 0x06};
    const uint8_t second[] = {0x05, 0x0c, 0x09, 0x01, 0xa1, 0x01};
    start(); chunk(0, first, 2); chunk(2, first + 2, 2); complete(0);
    assert(requested == 21 && map_count == 1);
    characteristic(23, ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP);
    characteristic(25, ORG_BLUETOOTH_CHARACTERISTIC_REPORT); complete(0);
    assert(requested == 23); chunk(0, second, sizeof second); complete(0);
    assert(!link.closing && map_count == 2 && requested == 5);
    assert(lengths[0] == sizeof first && !memcmp(maps[0], first, sizeof first));
    assert(lengths[1] == sizeof second && !memcmp(maps[1], second, sizeof second));

    start(); chunk(0, first, sizeof first); complete(ATT_ERROR_UNLIKELY_ERROR);
    assert(link.closing && !map_count); // Partial reads must not be emitted.
    start(); chunk(2, first, sizeof first);
    assert(link.closing && !map_count); // Missing/reordered chunks are rejected.
    start(); uint8_t large[64] = {0};
    for (unsigned offset = 0; offset < CORDIAL_DESCRIPTOR_BYTES; offset += sizeof large) chunk(offset, large, sizeof large);
    chunk(CORDIAL_DESCRIPTOR_BYTES, first, 1);
    assert(link.closing && link.error == CORDIAL_UNSUPPORTED && !map_count);
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
        link.reports[0].characteristic.properties=cases[i].properties;
        write_mode=write_done=0;write_ready=NULL;
        assert(cordial_gatt_write(&link)==(cases[i].mode ? CORDIAL_OK:CORDIAL_REPORT_SIZE));
        assert(write_mode==cases[i].mode && !write_done);
        if(write_mode==3) { assert(write_ready);write_ready->callback(write_ready->context);assert(write_mode==4 && write_done==1); }
        else if(write_mode) { complete(0);assert(write_done==1); }
    }
    puts("Service-aware GATT map and write-mode regressions passed");
}
