// HID over GATT through public GATT client APIs: one discovery of the HID
// services, their report maps and Report characteristics, CCCD writes,
// notification routing, and service-aware report reads and writes.
#include "profiles_internal.h"
#include <string.h>

enum { QUERY_IDLE, QUERY_DISCOVER, QUERY_SUBSCRIBE, QUERY_READ, QUERY_WRITE, QUERY_HASH };
enum { STAGE_HASH, STAGE_SERVICES, STAGE_CHARACTERISTICS, STAGE_MAP, STAGE_DESCRIPTORS, STAGE_REFERENCE, STAGE_LAYOUT };
// An error response, including the host's Invalid PDU for a truncated one,
// as opposed to the host's own statuses for a disconnect, missing bond,
// mismatch or timeout.
static bool device_error(uint8_t status) {
    return status && status != ATT_ERROR_HCI_DISCONNECT_RECEIVED && status != ATT_ERROR_BONDING_INFORMATION_MISSING
        && status != ATT_ERROR_DATA_MISMATCH && status != ATT_ERROR_TIMEOUT;
}
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

// One link discovers at a time. Rust copies each completed map before the
// next service uses this buffer; the report table is copied to the link when
// discovery ends. Other connections never move or resize this state.
static cordial_connection *owner;
// Verification issues its next query from poll once the link is idle. A
// verification query that failed mid-transfer drains until its completion
// so the GATT client is idle before the link accepts other requests.
static bool step_due, draining;
static uint8_t stage, service_count, service_cursor, table_count, table_cursor;
static struct { uint16_t start, end, map; } services[CORDIAL_SERVICES];
static cordial_report table[CORDIAL_LAYOUT_REPORTS];
static uint16_t table_end[CORDIAL_LAYOUT_REPORTS], reference;
// The Database Hash discovery read. A malformed or repeated value is the
// device's answer and counts as no hash.
static uint8_t table_hash[16];
static bool table_hashed, hash_malformed;
static uint8_t report_map[CORDIAL_DESCRIPTOR_BYTES];
static uint16_t map_length;
static gatt_client_notification_t notifications;
static uint8_t notify_value[2] = {1, 0}, indicate_value[2] = {2, 0};
static void callback(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size);
static void notified(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size);
static cordial_report *find(cordial_connection *l);

void cordial_gatt_init(void) {
    owner = NULL; step_due = false;
    gatt_client_listen_for_characteristic_value_updates(&notifications, notified, GATT_CLIENT_ANY_CONNECTION, NULL);
}
void cordial_gatt_end(cordial_connection *l) {
    if (owner == l) { owner = NULL; step_due = draining = false; }
}
static bool verifying(const cordial_connection *l) { return l->setup == SETUP_VERIFY; }
// The status a failed setup query ends the link with: an error response is
// the device's answer about its layout; a host failure (refused request,
// disconnect, timeout, missing bond) is recoverable.
static uint8_t query_error(uint8_t status) {
    return device_error(status) ? CORDIAL_UNSUPPORTED : CORDIAL_CONNECTION;
}
// A failed query ends discovery. Verification keeps the supplied layout,
// while setup cannot continue without a layout and ends with `error`.
static void abandon(cordial_connection *l, uint8_t error) {
    cordial_gatt_end(l);
    l->query = QUERY_IDLE;
    if (!verifying(l)) { cordial_fail(l, error); return; }
    // Without verification a failed CCCD write of the supplied layout cannot
    // be retried on confirmed handles; the device reconnects instead.
    if (l->resubscribe) { cordial_fail(l, CORDIAL_CONNECTION); return; }
    l->setup = SETUP_IDLE; l->cached = false;
    // Rust releases the supplied layout it kept for comparison.
    (void)cordial_emit_status(l, (cordial_event){ .kind = CORDIAL_LAYOUT, .code = 1 });
}
// The device's layout cannot be used. Verification ends the link as setup does.
static void unusable(cordial_connection *l) {
    cordial_gatt_end(l);
    l->query = QUERY_IDLE;
    cordial_fail(l, CORDIAL_UNSUPPORTED);
}
static bool subscribes(const cordial_report *r) {
    return r->type == HID_REPORT_TYPE_INPUT || (r->type == HID_REPORT_TYPE_FEATURE && (r->properties & 0x30));
}
static void verified(cordial_connection *l);
static void step(cordial_connection *l) {
    if (stage == STAGE_LAYOUT) { verified(l); return; }
    uint8_t status = 0;
    l->query = QUERY_DISCOVER;
    switch (stage) {
        case STAGE_HASH:
            table_hashed = hash_malformed = false;
            status = gatt_client_read_value_of_characteristics_by_uuid16(callback, l->handle, 1, 0xffff,
                ORG_BLUETOOTH_CHARACTERISTIC_DATABASE_HASH);
            break;
        case STAGE_SERVICES:
            status = gatt_client_discover_primary_services_by_uuid16(callback, l->handle, ORG_BLUETOOTH_SERVICE_HUMAN_INTERFACE_DEVICE);
            break;
        case STAGE_CHARACTERISTICS: {
            gatt_client_service_t service = { .start_group_handle = services[service_cursor].start,
                .end_group_handle = services[service_cursor].end };
            status = gatt_client_discover_characteristics_for_service(callback, l->handle, &service);
            break;
        }
        case STAGE_MAP:
            map_length = 0; l->read_simple = false;
            status = gatt_client_read_long_value_of_characteristic_using_value_handle(callback, l->handle, services[service_cursor].map);
            break;
        case STAGE_DESCRIPTORS: {
            gatt_client_characteristic_t characteristic = { .value_handle = table[table_cursor].value,
                .end_handle = table_end[table_cursor] };
            reference = 0;
            status = gatt_client_discover_characteristic_descriptors(callback, l->handle, &characteristic);
            break;
        }
        default:
            status = gatt_client_read_characteristic_descriptor_using_descriptor_handle(callback, l->handle, reference);
            break;
    }
    if (status) abandon(l, CORDIAL_CONNECTION);
}
// Reads the device's current Database Hash before a supplied layout is used.
void cordial_gatt_check_hash(cordial_connection *l) {
    l->setup = SETUP_HASH; l->hash_read = l->hash_equal = l->hash_malformed = false;
    l->query = QUERY_HASH;
    if (gatt_client_read_value_of_characteristics_by_uuid16(callback, l->handle, 1, 0xffff,
            ORG_BLUETOOTH_CHARACTERISTIC_DATABASE_HASH)) {
        l->query = QUERY_IDLE; cordial_fail(l, CORDIAL_CONNECTION);
    }
}
bool cordial_gatt_discover(cordial_connection *l, bool verify) {
    if (owner || l->closing) return false;
    owner = l; step_due = draining = false;
    stage = STAGE_HASH; service_count = service_cursor = table_count = table_cursor = 0;
    memset(services, 0, sizeof services); memset(table, 0, sizeof table);
    l->setup = verify ? SETUP_VERIFY : SETUP_DISCOVER;
    step(l);
    return true;
}
static void subscribed(cordial_connection *l) {
    l->setup = SETUP_IDLE;
    if (!l->profile) { l->profile = true; cordial_ready(l); }
}
// On confirmed handles (discovered or verified) a report that cannot notify
// leaves the link deaf, so it ends and the device reconnects. On a supplied
// layout's unconfirmed handles the remaining CCCDs are still written, then
// verification starts at once.
static void subscribe_failed(cordial_connection *l) {
    l->query = QUERY_IDLE;
    if (!l->cached) { cordial_fail(l, CORDIAL_CONNECTION); return; }
    l->resubscribe = true;
    ++l->cursor;
    cordial_gatt_subscribe(l);
}
void cordial_gatt_subscribe(cordial_connection *l) {
    for (; l->cursor < l->report_count; ++l->cursor) {
        cordial_report *r = &l->reports[l->cursor];
        if (!subscribes(r) || !r->cccd || !(r->properties & 0x30)) continue;
        l->query = QUERY_SUBSCRIBE;
        if (gatt_client_write_value_of_characteristic(callback, l->handle, r->cccd, 2,
                (r->properties & ATT_PROPERTY_NOTIFY) ? notify_value : indicate_value)) subscribe_failed(l);
        return;
    }
    subscribed(l);
}
static void discovered(cordial_connection *l) {
    bool input = false;
    for (unsigned i = 0; i < table_count; ++i) {
        const cordial_report *r = &table[i];
        input |= r->type == HID_REPORT_TYPE_INPUT;
        if (subscribes(r) && (!r->cccd || !(r->properties & 0x30))) { unusable(l); return; }
    }
    if (!input) { unusable(l); return; }
    if (verifying(l)) { stage = STAGE_LAYOUT; verified(l); return; }
    memcpy(l->reports, table, table_count * sizeof *table);
    l->report_count = table_count;
    l->hashed = table_hashed;
    memcpy(l->hash, table_hash, sizeof l->hash);
    cordial_gatt_end(l);
    l->setup = SETUP_SUBSCRIBE; l->cursor = 0;
    cordial_gatt_subscribe(l);
}
// Rust compares the result with the supplied layout. Accepting it switches
// routing in the same callback, so later input follows the Layout event. A
// refusal means the new layout cannot be used and ends the link.
static void verified(cordial_connection *l) {
    int status = cordial_emit_status(l, (cordial_event){ .kind = CORDIAL_LAYOUT, .service = service_count,
        .data = (const uint8_t *)table, .length = table_count * sizeof *table, .hash = table_hashed ? table_hash : NULL });
    if (!status) { step_due = true; return; }
    cordial_gatt_end(l);
    if (status < 0) { cordial_fail(l, (uint8_t)-status); return; }
    l->setup = SETUP_IDLE; l->cached = false;
    bool resubscribe = l->resubscribe;
    l->resubscribe = false;
    bool changed = table_count != l->report_count || memcmp(table, l->reports, table_count * sizeof *table);
    memcpy(l->reports, table, table_count * sizeof *table);
    l->report_count = table_count;
    l->hashed = table_hashed;
    memcpy(l->hash, table_hash, sizeof l->hash);
    if (changed || resubscribe) { l->setup = SETUP_SUBSCRIBE; l->cursor = 0; cordial_gatt_subscribe(l); }
}
static void completed(cordial_connection *l) {
    l->query = QUERY_IDLE;
    switch (stage) {
        case STAGE_HASH:
            stage = STAGE_SERVICES; break;
        case STAGE_SERVICES:
            if (!service_count) { unusable(l); return; }
            stage = STAGE_CHARACTERISTICS; break;
        case STAGE_CHARACTERISTICS:
            if (!services[service_cursor].map) { unusable(l); return; }
            stage = STAGE_MAP; break;
        case STAGE_MAP:
            if (!map_length) { unusable(l); return; }
            if (!cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_DESCRIPTOR, .service = service_cursor,
                    .length = map_length, .data = report_map, .code = verifying(l) })) {
                cordial_gatt_end(l); return;
            }
            stage = ++service_cursor < service_count ? STAGE_CHARACTERISTICS : STAGE_DESCRIPTORS;
            break;
        case STAGE_DESCRIPTORS:
            if (!reference) { unusable(l); return; }
            stage = STAGE_REFERENCE; break;
        default:
            if (!table[table_cursor].type) { unusable(l); return; }
            ++table_cursor; stage = STAGE_DESCRIPTORS; break;
    }
    if (stage == STAGE_DESCRIPTORS && table_cursor == table_count) { discovered(l); return; }
    if (verifying(l)) step_due = true; else step(l);
}
void cordial_gatt_poll(cordial_connection *l) {
    if (!l->ready || l->closing || l->peer.transport != CORDIAL_BLE) return;
    if (l->query || l->writing || l->reading || l->info.pending) return;
    if (owner == l && step_due) { step_due = false; step(l); return; }
    // Verify a supplied layout once its CCCDs, early input and the first
    // information pass are done, or at once when one of its CCCD writes failed.
    if (l->cached && l->setup == SETUP_IDLE && !owner && !cordial_early_pending(l->handle) &&
        (l->resubscribe || cordial_info_settled(l)))
        cordial_gatt_discover(l, true);
}
// The device's Database Hash, read by characteristic UUID over all handles.
static bool hash_value(uint8_t *packet, uint16_t size, uint8_t *out) {
    uint16_t length = gatt_event_characteristic_value_query_result_get_value_length(packet);
    const uint8_t *data = gatt_event_characteristic_value_query_result_get_value(packet);
    if (length != 16 || data + length > packet + size) return false;
    memcpy(out, data, 16);
    return true;
}
static void discovery_event(cordial_connection *l, uint8_t event, uint8_t *packet, uint16_t size) {
    switch (event) {
        case GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT:
            if (stage != STAGE_HASH) return;
            if (table_hashed || hash_malformed || !hash_value(packet, size, table_hash)) hash_malformed = true;
            else table_hashed = true;
            break;
        case GATT_EVENT_SERVICE_QUERY_RESULT: {
            if (stage != STAGE_SERVICES) return;
            if (service_count == CORDIAL_SERVICES) { unusable(l); return; }
            gatt_client_service_t service;
            gatt_event_service_query_result_get_service(packet, &service);
            services[service_count].start = service.start_group_handle;
            services[service_count++].end = service.end_group_handle;
            break;
        }
        case GATT_EVENT_CHARACTERISTIC_QUERY_RESULT: {
            if (stage != STAGE_CHARACTERISTICS) return;
            gatt_client_characteristic_t characteristic;
            gatt_event_characteristic_query_result_get_characteristic(packet, &characteristic);
            if (characteristic.uuid16 == ORG_BLUETOOTH_CHARACTERISTIC_REPORT_MAP) {
                if (services[service_cursor].map) { unusable(l); return; }
                services[service_cursor].map = characteristic.value_handle;
                return;
            }
            if (characteristic.uuid16 != ORG_BLUETOOTH_CHARACTERISTIC_REPORT) return;
            if (table_count == CORDIAL_LAYOUT_REPORTS) { unusable(l); return; }
            table_end[table_count] = characteristic.end_handle;
            table[table_count++] = (cordial_report){ .value = characteristic.value_handle,
                .properties = (uint8_t)characteristic.properties, .service = service_cursor };
            break;
        }
        case GATT_EVENT_ALL_CHARACTERISTIC_DESCRIPTORS_QUERY_RESULT: {
            if (stage != STAGE_DESCRIPTORS) return;
            gatt_client_characteristic_descriptor_t descriptor;
            gatt_event_all_characteristic_descriptors_query_result_get_characteristic_descriptor(packet, &descriptor);
            if (descriptor.uuid16 == ORG_BLUETOOTH_DESCRIPTOR_REPORT_REFERENCE) reference = descriptor.handle;
            if (descriptor.uuid16 == ORG_BLUETOOTH_DESCRIPTOR_GATT_CLIENT_CHARACTERISTIC_CONFIGURATION)
                table[table_cursor].cccd = descriptor.handle;
            break;
        }
        case GATT_EVENT_CHARACTERISTIC_DESCRIPTOR_QUERY_RESULT: {
            if (stage != STAGE_REFERENCE) return;
            uint16_t length = gatt_event_characteristic_descriptor_query_result_get_descriptor_length(packet);
            const uint8_t *data = gatt_event_characteristic_descriptor_query_result_get_descriptor(packet);
            if (length != 2 || data + length > packet + size || data[1] < HID_REPORT_TYPE_INPUT || data[1] > HID_REPORT_TYPE_FEATURE) {
                unusable(l); return;
            }
            cordial_report *report = &table[table_cursor];
            report->id = data[0]; report->type = data[1];
            for (unsigned i = 0; i < table_cursor; ++i) {
                const cordial_report *previous = &table[i];
                if (previous->service == report->service && previous->id == report->id && previous->type == report->type) {
                    unusable(l); return;
                }
            }
            break;
        }
        default: break;
    }
}
static void callback(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size) {
    (void)channel;
    if (type != HCI_EVENT_PACKET || size < 4) return;
    cordial_connection *l = cordial_by_handle(little_endian_read_16(packet, 2));
    if (!l || l->closing || !l->query) return;
    uint8_t event = hci_event_packet_get_type(packet);
    if (l->query == QUERY_DISCOVER && owner == l) discovery_event(l, event, packet, size);
    if (l->query == QUERY_HASH && event == GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT) {
        uint8_t hash[16];
        if (l->hash_read || !hash_value(packet, size, hash)) l->hash_malformed = true;
        else l->hash_equal = !memcmp(hash, l->hash, sizeof hash);
        l->hash_read = true;
        return;
    }
    if (l->closing || !l->query) return;
    switch (event) {
        case GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT:
        case GATT_EVENT_LONG_CHARACTERISTIC_VALUE_QUERY_RESULT: {
            bool map = l->query == QUERY_DISCOVER && stage == STAGE_MAP;
            if (!map && (l->query != QUERY_READ || !l->reading)) return;
            if (map && draining) return;
            bool simple = event == GATT_EVENT_CHARACTERISTIC_VALUE_QUERY_RESULT;
            if (simple != l->read_simple) return;
            uint16_t offset = simple ? 0 : gatt_event_long_characteristic_value_query_result_get_value_offset(packet);
            uint16_t length = simple ? gatt_event_characteristic_value_query_result_get_value_length(packet) : gatt_event_long_characteristic_value_query_result_get_value_length(packet);
            const uint8_t *data = simple ? gatt_event_characteristic_value_query_result_get_value(packet) : gatt_event_long_characteristic_value_query_result_get_value(packet);
            uint16_t *received = map ? &map_length : &l->length;
            uint16_t capacity = map ? sizeof report_map : sizeof l->bytes;
            if (offset != *received || data + length > packet + size) {
                if (!map) cordial_fail(l, CORDIAL_OVERFLOW);
                else if (verifying(l)) draining = true;
                else abandon(l, CORDIAL_CONNECTION);
                return;
            }
            if (offset > capacity || length > capacity - offset) {
                if (map) unusable(l); else cordial_fail(l, CORDIAL_OVERFLOW);
                return;
            }
            memcpy((map ? report_map : l->bytes) + offset, data, length); *received += length; break;
        }
        case GATT_EVENT_QUERY_COMPLETE: {
            uint8_t status = gatt_event_query_complete_get_att_status(packet);
            bool map = l->query == QUERY_DISCOVER && stage == STAGE_MAP;
            if (map && draining) { abandon(l, CORDIAL_CONNECTION); return; }
            if (status == ATT_ERROR_ATTRIBUTE_NOT_LONG && (map || l->query == QUERY_READ) && !l->read_simple) {
                uint16_t received = map ? map_length : l->length;
                if (received) {
                    // The owner validates the declared report length before use.
                    status = ATT_ERROR_SUCCESS;
                } else {
                    cordial_report *report = map ? NULL : find(l);
                    uint16_t handle = report ? report->value : map ? services[service_cursor].map : 0;
                    l->read_simple = true;
                    if (handle && !gatt_client_read_value_of_characteristic_using_value_handle(callback, l->handle, handle)) return;
                    if (map) { abandon(l, CORDIAL_CONNECTION); return; }
                    status = ATT_ERROR_UNLIKELY_ERROR;
                }
            }
            switch (l->query) {
                case QUERY_WRITE: l->query = QUERY_IDLE; cordial_write_done(l, operation_error(status)); return;
                case QUERY_READ: l->query = QUERY_IDLE; cordial_read_done(l, operation_error(status)); return;
                case QUERY_SUBSCRIBE:
                    if (status) { subscribe_failed(l); return; }
                    l->query = QUERY_IDLE; ++l->cursor; cordial_gatt_subscribe(l); return;
                case QUERY_HASH: {
                    l->query = QUERY_IDLE;
                    // An error response or a malformed value means the device
                    // has no usable hash. A host failure leaves the comparison
                    // unknown and ends setup.
                    if (status && !device_error(status)) { cordial_fail(l, CORDIAL_CONNECTION); return; }
                    l->setup = SETUP_NONE;
                    if (status || l->hash_malformed || !l->hash_equal) { l->cached = false; l->report_count = 0; }
                    cordial_begin_profile(l);
                    return;
                }
                default:
                    if (owner != l) return;
                    if (stage == STAGE_HASH && (!status || device_error(status))) {
                        if (status || hash_malformed) table_hashed = false;
                        completed(l); return;
                    }
                    if (status) { abandon(l, query_error(status)); return; }
                    completed(l); return;
            }
        }
        default: break;
    }
}
const cordial_report *cordial_gatt_report(const cordial_connection *l, uint16_t value) {
    for (unsigned i = 0; i < l->report_count; ++i)
        if (l->reports[i].value == value && l->reports[i].type != HID_REPORT_TYPE_OUTPUT) return &l->reports[i];
    return NULL;
}
// Input and Feature notifications route by value handle. Notifications that
// precede a link's Connected event wait in its early input buffer.
static void notified(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size) {
    (void)channel;
    if (type != HCI_EVENT_PACKET || size < 12) return;
    uint8_t event = hci_event_packet_get_type(packet);
    if (event != GATT_EVENT_NOTIFICATION && event != GATT_EVENT_INDICATION) return;
    hci_con_handle_t handle = gatt_event_notification_get_handle(packet);
    uint16_t value = gatt_event_notification_get_value_handle(packet);
    uint16_t length = gatt_event_notification_get_value_length(packet);
    const uint8_t *data = gatt_event_notification_get_value(packet);
    if (data + length > packet + size) return;
    cordial_connection *l = cordial_by_handle(handle);
    if (l && (l->closing || l->peer.transport != CORDIAL_BLE)) return;
    if (!l || !l->ready || cordial_early_pending(handle)) { cordial_early_push(handle, value, data, length); return; }
    const cordial_report *report = cordial_gatt_report(l, value);
    if (report) cordial_input(l, report->service, report->id, data, length);
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
        report->value, l->length, l->bytes) : ERROR_CODE_UNSUPPORTED_FEATURE_OR_PARAMETER_VALUE;
    l->query = QUERY_IDLE;
    cordial_write_done(l, result ? CORDIAL_CONNECTION : CORDIAL_OK);
}
int cordial_gatt_write(cordial_connection *l) {
    cordial_report *report = find(l);
    if (!report) return CORDIAL_UNSUPPORTED;
    // Until the automatic MTU exchange completes the default MTU applies.
    uint16_t mtu = 0;
    if (gatt_client_get_mtu(l->handle, &mtu) && !mtu) return CORDIAL_CONNECTION;
    uint8_t result;
    l->query = QUERY_WRITE;
    // Prefer HID Data Output for output reports that fit a Write Command.
    if (report->type == HID_REPORT_TYPE_OUTPUT &&
        (report->properties & ATT_PROPERTY_WRITE_WITHOUT_RESPONSE) && l->length <= mtu - 3) {
        l->writable.callback = without_response; l->writable.context = l;
        result = gatt_client_request_to_write_without_response(&l->writable, l->handle);
    } else if (report->properties & ATT_PROPERTY_WRITE) {
        if (l->length <= mtu - 3)
            result = gatt_client_write_value_of_characteristic(callback, l->handle, report->value, l->length, l->bytes);
        else
            result = gatt_client_write_long_value_of_characteristic(callback, l->handle, report->value, l->length, l->bytes);
    } else if ((report->properties & ATT_PROPERTY_WRITE_WITHOUT_RESPONSE) && l->length <= mtu - 3) {
        l->writable.callback = without_response; l->writable.context = l;
        result = gatt_client_request_to_write_without_response(&l->writable, l->handle);
    } else { l->query = QUERY_IDLE; return (report->properties & ATT_PROPERTY_WRITE_WITHOUT_RESPONSE) ? CORDIAL_REPORT_SIZE : CORDIAL_UNSUPPORTED; }
    if (result) l->query = QUERY_IDLE;
    return result ? CORDIAL_CONNECTION : CORDIAL_OK;
}
int cordial_gatt_read(cordial_connection *l) {
    cordial_report *report = find(l);
    if (!report || !(report->properties & ATT_PROPERTY_READ)) return CORDIAL_UNSUPPORTED;
    l->query = QUERY_READ; l->read_simple = false;
    uint8_t result = gatt_client_read_long_value_of_characteristic_using_value_handle(callback, l->handle, report->value);
    if (result) l->query = QUERY_IDLE;
    return result ? CORDIAL_CONNECTION : CORDIAL_OK;
}
