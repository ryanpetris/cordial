// Service-aware maps and report access through public GATT APIs. HIDS owns
// notification setup, but its report read/write API does not take a service.
#include "profiles_internal.h"
#include <string.h>

enum { QUERY_IDLE, QUERY_SERVICES, QUERY_REPORTS, QUERY_MAP, QUERY_DESCRIPTORS, QUERY_REFERENCE, QUERY_READ, QUERY_WRITE };
static uint8_t operation_error(uint8_t status) {
    switch (status) {
        case ATT_ERROR_SUCCESS: return CORDIAL_OK;
        case ATT_ERROR_READ_NOT_PERMITTED:
        case ATT_ERROR_WRITE_NOT_PERMITTED:
        case ATT_ERROR_REQUEST_NOT_SUPPORTED:
        case ATT_ERROR_ATTRIBUTE_NOT_LONG: return CORDIAL_UNSUPPORTED;
        case ATT_ERROR_INVALID_ATTRIBUTE_VALUE_LENGTH: return CORDIAL_REPORT_SIZE;
        default: return CORDIAL_CONNECTION;
    }
}

// Profile setup is serialized; Rust copies each completed map before the next
// service uses this buffer. Other connections never move or resize it.
static uint8_t report_map[CORDIAL_DESCRIPTOR_BYTES];
static uint16_t map_length;
static void callback(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size);
static void advance(cordial_connection *l);
static cordial_report *find(cordial_connection *l);

static void accepted(cordial_connection *l, uint8_t status) {
    if (status) cordial_fail(l, CORDIAL_UNSUPPORTED);
}
void cordial_gatt_begin(cordial_connection *l) {
    l->query = QUERY_SERVICES;
    accepted(l, gatt_client_discover_primary_services_by_uuid16(callback, l->handle, ORG_BLUETOOTH_SERVICE_HUMAN_INTERFACE_DEVICE));
}
static void advance(cordial_connection *l) {
    if (l->closing) return;
    if (l->query == QUERY_REPORTS) {
        if (!l->map_handle) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
        map_length = 0; l->query = QUERY_MAP; l->read_simple = false;
        accepted(l, gatt_client_read_long_value_of_characteristic_using_value_handle(callback, l->handle, l->map_handle));
        return;
    }
    if (l->query == QUERY_SERVICES || l->query == QUERY_MAP) {
        if (l->query == QUERY_MAP) {
            if (!map_length) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            if (!cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_DESCRIPTOR, .service = l->service_cursor - 1,
                    .length = map_length, .data = report_map })) return;
        }
        if (l->service_cursor < l->service_count) {
            l->query = QUERY_REPORTS; l->map_handle = 0;
            accepted(l, gatt_client_discover_characteristics_for_service(callback, l->handle,
                &l->services[l->service_cursor++]));
            return;
        }
        l->query = QUERY_DESCRIPTORS;
    }
    if (l->query == QUERY_REFERENCE) { ++l->report_cursor; l->query = QUERY_DESCRIPTORS; }
    if (l->query == QUERY_DESCRIPTORS) {
        if (l->report_cursor == l->report_count) {
            if (!l->service_count || !l->report_count) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            l->query = QUERY_IDLE; l->profile = true; cordial_ready(l); return;
        }
        cordial_report *report = &l->reports[l->report_cursor];
        if (report->reference) {
            l->query = QUERY_REFERENCE;
            accepted(l, gatt_client_read_characteristic_descriptor_using_descriptor_handle(callback, l->handle, report->reference));
        } else {
            accepted(l, gatt_client_discover_characteristic_descriptors(callback, l->handle, &report->characteristic));
        }
    }
}
static void callback(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size) {
    (void)channel;
    if (type != HCI_EVENT_PACKET || size < 4) return;
    cordial_connection *l = cordial_by_handle(little_endian_read_16(packet, 2));
    if (!l || l->closing) return;
    uint8_t event = hci_event_packet_get_type(packet);
    switch (event) {
        case GATT_EVENT_SERVICE_QUERY_RESULT:
            if (l->query != QUERY_SERVICES) return;
            if (l->service_count == MAX_NUM_HID_SERVICES) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            gatt_event_service_query_result_get_service(packet, &l->services[l->service_count++]); break;
        case GATT_EVENT_CHARACTERISTIC_QUERY_RESULT: {
            if (l->query != QUERY_REPORTS) return;
            gatt_client_characteristic_t characteristic;
            gatt_event_characteristic_query_result_get_characteristic(packet, &characteristic);
            if (characteristic.uuid16 == ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP) {
                if (l->map_handle) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
                l->map_handle = characteristic.value_handle;
                return;
            }
            if (characteristic.uuid16 != ORG_BLUETOOTH_CHARACTERISTIC_REPORT) return;
            if (l->report_count == HIDS_HOST_NUM_REPORTS) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            cordial_report *report = &l->reports[l->report_count++];
            report->characteristic = characteristic;
            report->service = l->service_cursor - 1; break;
        }
        case GATT_EVENT_ALL_CHARACTERISTIC_DESCRIPTORS_QUERY_RESULT: {
            if (l->query != QUERY_DESCRIPTORS) return;
            gatt_client_characteristic_descriptor_t descriptor;
            gatt_event_all_characteristic_descriptors_query_result_get_characteristic_descriptor(packet, &descriptor);
            if (descriptor.uuid16 == ORG_BLUETOOTH_DESCRIPTOR_REPORT_REFERENCE)
                l->reports[l->report_cursor].reference = descriptor.handle;
            break;
        }
        case GATT_EVENT_CHARACTERISTIC_DESCRIPTOR_QUERY_RESULT: {
            if (l->query != QUERY_REFERENCE) return;
            uint16_t length = gatt_event_characteristic_descriptor_query_result_get_descriptor_length(packet);
            const uint8_t *data = gatt_event_characteristic_descriptor_query_result_get_descriptor(packet);
            if (length != 2 || data + length > packet + size || data[1] < HID_REPORT_TYPE_INPUT || data[1] > HID_REPORT_TYPE_FEATURE) {
                cordial_fail(l, CORDIAL_UNSUPPORTED); return;
            }
            cordial_report *report = &l->reports[l->report_cursor];
            report->id = data[0]; report->type = data[1];
            for (unsigned i = 0; i < l->report_cursor; ++i) {
                cordial_report *previous = &l->reports[i];
                if (previous->service == report->service && previous->id == report->id && previous->type == report->type) {
                    cordial_fail(l, CORDIAL_UNSUPPORTED); return;
                }
            }
            break;
        }
        case GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT:
        case GATT_EVENT_LONG_CHARACTERISTIC_VALUE_QUERY_RESULT: {
            bool map = l->query == QUERY_MAP;
            if (!map && (l->query != QUERY_READ || !l->reading)) return;
            bool simple = event == GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT;
            if (simple != l->read_simple) return;
            uint16_t offset = simple ? 0 : gatt_event_long_characteristic_value_query_result_get_value_offset(packet);
            uint16_t length = simple ? gatt_event_characteristic_value_query_result_get_value_length(packet) : gatt_event_long_characteristic_value_query_result_get_value_length(packet);
            const uint8_t *data = simple ? gatt_event_characteristic_value_query_result_get_value(packet) : gatt_event_long_characteristic_value_query_result_get_value(packet);
            uint16_t *received = map ? &map_length : &l->length;
            uint16_t capacity = map ? sizeof report_map : sizeof l->bytes;
            if (offset != *received || offset > capacity || length > capacity - offset || data + length > packet + size) {
                cordial_fail(l, map ? CORDIAL_UNSUPPORTED : CORDIAL_OVERFLOW); return;
            }
            memcpy((map ? report_map : l->bytes) + offset, data, length); *received += length; break;
        }
        case GATT_EVENT_QUERY_COMPLETE: {
            uint8_t status = gatt_event_query_complete_get_att_status(packet);
            if (status == ATT_ERROR_ATTRIBUTE_NOT_LONG && (l->query == QUERY_MAP || l->query == QUERY_READ) && !l->read_simple) {
                uint16_t received = l->query == QUERY_MAP ? map_length : l->length;
                if (received) {
                    // The owner validates the declared report length before use.
                    status = ATT_ERROR_SUCCESS;
                } else {
                    cordial_report *report = l->query == QUERY_READ ? find(l) : NULL;
                    uint16_t handle = report ? report->characteristic.value_handle : l->map_handle;
                    l->read_simple = true;
                    if (!gatt_client_read_value_of_characteristic_using_value_handle(callback, l->handle, handle)) return;
                    status = ATT_ERROR_UNLIKELY_ERROR;
                }
            }
            if (l->query == QUERY_WRITE) { l->query = QUERY_IDLE; cordial_write_done(l, operation_error(status)); return; }
            if (l->query == QUERY_READ) { l->query = QUERY_IDLE; cordial_read_done(l, operation_error(status)); return; }
            if (status) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            if (l->query == QUERY_DESCRIPTORS && !l->reports[l->report_cursor].reference) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            if (l->query == QUERY_REFERENCE && !l->reports[l->report_cursor].type) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            advance(l); break;
        }
        default: break;
    }
}
static cordial_report *find(cordial_connection *l) {
    uint16_t id = l->operation_id == HID_REPORT_ID_UNDEFINED ? 0 : l->operation_id;
    for (unsigned i = 0; i < l->report_count; ++i) {
        cordial_report *report = &l->reports[i];
        if (report->service == l->operation_service && report->type == l->operation_type && report->id == id) return report;
    }
    return NULL;
}
static void without_response(void *context) {
    cordial_connection *l = context;
    if (!l->used || l->closing || !l->writing) return;
    cordial_report *report = find(l);
    uint8_t result = report ? gatt_client_write_value_of_characteristic_without_response(l->handle,
        report->characteristic.value_handle, l->length, l->bytes) : ERROR_CODE_UNSUPPORTED_FEATURE_OR_PARAMETER_VALUE;
    l->query = QUERY_IDLE;
    cordial_write_done(l, result ? CORDIAL_CONNECTION : CORDIAL_OK);
}
int cordial_gatt_write(cordial_connection *l) {
    cordial_report *report = find(l);
    if (!report) return CORDIAL_UNSUPPORTED;
    uint16_t mtu = 23;
    if (gatt_client_get_mtu(l->handle, &mtu)) return CORDIAL_CONNECTION;
    uint8_t result;
    l->query = QUERY_WRITE;
    // Prefer HID Data Output for output reports that fit a Write Command.
    if (report->type == HID_REPORT_TYPE_OUTPUT &&
        (report->characteristic.properties & ATT_PROPERTY_WRITE_WITHOUT_RESPONSE) && l->length <= mtu - 3) {
        l->writable.callback = without_response; l->writable.context = l;
        result = gatt_client_request_to_write_without_response(&l->writable, l->handle);
    } else if (report->characteristic.properties & ATT_PROPERTY_WRITE) {
        if (l->length <= mtu - 3)
            result = gatt_client_write_value_of_characteristic(callback, l->handle, report->characteristic.value_handle, l->length, l->bytes);
        else
            result = gatt_client_write_long_value_of_characteristic(callback, l->handle, report->characteristic.value_handle, l->length, l->bytes);
    } else if ((report->characteristic.properties & ATT_PROPERTY_WRITE_WITHOUT_RESPONSE) && l->length <= mtu - 3) {
        l->writable.callback = without_response; l->writable.context = l;
        result = gatt_client_request_to_write_without_response(&l->writable, l->handle);
    } else { l->query = QUERY_IDLE; return (report->characteristic.properties & ATT_PROPERTY_WRITE_WITHOUT_RESPONSE) ? CORDIAL_REPORT_SIZE : CORDIAL_UNSUPPORTED; }
    if (result) l->query = QUERY_IDLE;
    return result ? CORDIAL_CONNECTION : CORDIAL_OK;
}
int cordial_gatt_read(cordial_connection *l) {
    cordial_report *report = find(l);
    if (!report || !(report->characteristic.properties & ATT_PROPERTY_READ)) return CORDIAL_UNSUPPORTED;
    l->query = QUERY_READ; l->read_simple = false;
    uint8_t result = gatt_client_read_long_value_of_characteristic_using_value_handle(callback, l->handle, report->characteristic.value_handle);
    if (result) l->query = QUERY_IDLE;
    return result ? CORDIAL_CONNECTION : CORDIAL_OK;
}
