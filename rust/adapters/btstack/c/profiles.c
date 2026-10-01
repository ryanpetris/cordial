#include "profiles_internal.h"
#ifdef ENABLE_CLASSIC
int cordial_classic_set_report(uint16_t cid, hid_report_type_t type,
                              uint16_t report_id, const uint8_t *data, uint16_t size);
#endif
#include "ble/le_device_db.h"
#include "host.h"
#include <string.h>
#ifdef ENABLE_CLASSIC
#include "classic/hid_host.h"
#endif

cordial_connection cordial_connections[CORDIAL_LINKS];
static cordial_emit emit;
static void *context;
static btstack_packet_callback_registration_t hci_events, sm_events;
#ifdef ENABLE_CLASSIC
static uint8_t classic_descriptors[8192];
static char pin[17];
#endif
static struct { uint32_t attempt; cordial_peer peer; uint16_t cid, handle; uint64_t deadline; bool closing, offered, auth_failed; } incoming[CORDIAL_LINKS];
static uint32_t next_attempt;
static cordial_peer reconnect_peers[8];
static size_t reconnect_count;
static bool auto_active, auto_started, auto_cancel, auto_dirty, auto_consumed, initiating_started;
static bool ready, scan_classic, scan_ble, inquiry, stopping, restarting, restart_notice;
static uint64_t scan_id, now_ms;
static bool states_pending, states_sent, scan_and_connect;
static cordial_connection *initiating, *gatt_setup;
// One explicit pairing at a time. Retain addresses only, never native keys.
static cordial_peer paired_before[16];
static unsigned paired_before_count;
// Controller-resolved advertisements contain an identity, not the on-air RPA.
// Read that RPA while its resolving-list entry still exists. Eight queued
// identities cover the native bond limit; ordinary advertisements need no query.
static struct {
    bool used, connectable;
    uint64_t scan;
    cordial_peer peer;
    int16_t rssi;
    uint16_t appearance;
    uint8_t name[31], length;
} rpa_reports[8];
static unsigned rpa_read = 8;
// LE Read Supported States has no parameters; use its wire definition here.
static const hci_cmd_t read_supported_states = { HCI_OPCODE_HCI_LE_READ_SUPPORTED_STATES, "" };
static const hci_cmd_t read_peer_rpa = { HCI_OPCODE_HCI_LE_READ_PEER_RESOLVABLE_ADDRESS, "1B" };
static void packet_handler(uint8_t, uint16_t, uint8_t *, uint16_t);
static void hids_handler(uint8_t, uint16_t, uint8_t *, uint16_t);
static void reject_incoming(unsigned i);
static void restart(void) {
    if (stopping || restarting) return;
    cordial_profiles_stop();
    restarting = restart_notice = true;
}

static bool peer_equal(cordial_peer a, cordial_peer b) {
    return a.transport == b.transport && a.random == b.random && !memcmp(a.address, b.address, 6);
}
static bool new_pair_identity(cordial_peer peer) {
    for (unsigned i = 0; i < paired_before_count; ++i)
        if (peer_equal(peer, paired_before[i])) return false;
    return true;
}
cordial_connection *cordial_by_id(cordial_link id) {
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i) {
        cordial_connection *l = &cordial_connections[i];
        if (l->used && l->id.slot == id.slot && l->id.generation == id.generation) return l;
    }
    return NULL;
}
cordial_connection *cordial_by_handle(hci_con_handle_t handle) {
    if (handle == HCI_CON_HANDLE_INVALID) return NULL;
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
        if (cordial_connections[i].used && cordial_connections[i].handle == handle) return &cordial_connections[i];
    return NULL;
}
static cordial_connection *by_peer(cordial_peer peer) {
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
        if (cordial_connections[i].used && peer_equal(cordial_connections[i].peer, peer)) return &cordial_connections[i];
    return NULL;
}
static cordial_connection *by_cid(uint16_t cid, uint8_t transport) {
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
        if (cordial_connections[i].used && cordial_connections[i].cid == cid && cordial_connections[i].peer.transport == transport)
            return &cordial_connections[i];
    return NULL;
}
static bool le_identity(int index, cordial_peer *peer) {
    if (index < 0 || index >= le_device_db_max_count()) return false;
    int type; sm_key_t irk; bd_addr_t address;
    le_device_db_info(index, &type, address, irk);
    if (type != BD_ADDR_TYPE_LE_PUBLIC && type != BD_ADDR_TYPE_LE_RANDOM) return false;
    *peer = (cordial_peer){ .transport = CORDIAL_BLE, .random = type == BD_ADDR_TYPE_LE_RANDOM };
    memcpy(peer->address, address, 6);
    return true;
}
static int le_index(cordial_peer peer) {
    for (int i = 0; i < le_device_db_max_count(); ++i) {
        cordial_peer candidate;
        if (le_identity(i, &candidate) && peer_equal(peer, candidate)) return i;
    }
    return -1;
}
static bool has_key(cordial_peer peer) {
#ifdef ENABLE_CLASSIC
    if (peer.transport == CORDIAL_CLASSIC) {
        link_key_t key; link_key_type_t type;
        return gap_get_link_key_for_bd_addr(peer.address, key, &type);
    }
#endif
    int index = le_index(peer), size = 0;
    if (index < 0) return false;
    le_device_db_encryption_get(index, NULL, NULL, NULL, &size, NULL, NULL, NULL);
    return size >= 7;
}
static void offer_incoming(unsigned i) {
    if(!incoming[i].attempt || incoming[i].closing || incoming[i].offered)return;
    cordial_peer p=incoming[i].peer;
    // Legacy LE completion can carry the on-air RPA for accept-list connects.
    // Wait for SM's identity lookup before exposing it to saved-device policy.
    if(p.random && (p.address[0]&0xc0)==0x40) {
        irk_lookup_state_t state=sm_identity_resolving_state(incoming[i].handle);
        if(state==IRK_LOOKUP_FAILED){reject_incoming(i);return;}
        if(state!=IRK_LOOKUP_SUCCEEDED)return;
        if(!le_identity(sm_le_device_index(incoming[i].handle),&p)){reject_incoming(i);return;}
    }
    incoming[i].peer=p;incoming[i].offered=true;
    if(!cordial_emit_event(NULL,(cordial_event){.kind=CORDIAL_INCOMING,.number=incoming[i].attempt,.peer=p}))reject_incoming(i);
}
int cordial_profiles_bonds(cordial_peer *peers, unsigned capacity) {
    unsigned count = 0;
#ifdef ENABLE_CLASSIC
    btstack_link_key_iterator_t iterator;
    if (!gap_link_key_iterator_init(&iterator)) return -CORDIAL_STORAGE;
    cordial_peer peer = { .transport = CORDIAL_CLASSIC };
    link_key_t key; link_key_type_t type;
    while (gap_link_key_iterator_get_next(&iterator, peer.address, key, &type)) {
        if (count < capacity) peers[count] = peer;
        ++count;
    }
    gap_link_key_iterator_done(&iterator);
#endif
    for (int i = 0; i < le_device_db_max_count(); ++i) {
        cordial_peer peer;
        // Include partial native identities so boot reconciliation can remove
        // records left by an interrupted pairing without an application policy.
        if (le_identity(i, &peer)) {
            if (count < capacity) peers[count] = peer;
            ++count;
        }
    }
    return count <= capacity ? (int)count : -CORDIAL_CAPACITY;
}
int cordial_profiles_forget(cordial_peer peer) {
#ifdef ENABLE_CLASSIC
    if (peer.transport == CORDIAL_CLASSIC) gap_drop_link_key_for_bd_addr(peer.address);
    else
#endif
    {
        int index = le_index(peer);
        if (index >= 0) le_device_db_remove(index);
        (void)gap_load_resolving_list_from_le_device_db();
    }
    if (has_key(peer)) return CORDIAL_STORAGE;
    for (unsigned i = 0; i < paired_before_count; ++i) if (peer_equal(peer, paired_before[i])) {
        paired_before[i] = paired_before[--paired_before_count];
        break;
    }
    return CORDIAL_OK;
}
int cordial_emit_event(cordial_connection *l, cordial_event event) {
    if (l) { event.link = l->id; event.peer = l->peer; }
    int accepted = emit(context, &event);
    if (accepted <= 0 && l && event.kind != CORDIAL_DISCONNECTED && event.kind != CORDIAL_SECURITY && event.kind != CORDIAL_INFORMATION)
        cordial_fail(l, accepted < 0 ? (uint8_t)-accepted : event.kind == CORDIAL_DESCRIPTOR ? CORDIAL_CAPACITY : CORDIAL_OVERFLOW);
    return accepted > 0;
}
static void finish(cordial_connection *l) {
    if (!l || l->ended) return;
    if (initiating == l) initiating = NULL;
    if (gatt_setup == l) gatt_setup = NULL;
    l->closing = true;
    cordial_info_clear(l);
    l->ended = true;
    if (l->pairing) gap_set_bondable_mode(0);
    // Selected/resolved saved identities are rejected before pairing. Track
    // this attempt's native identity even when authentication is unfinished.
    if (l->pairing && !l->adopted && new_pair_identity(l->peer) && cordial_profiles_forget(l->peer)) l->error = CORDIAL_STORAGE;
}
void cordial_profiles_disconnect(cordial_link id) {
    cordial_connection *l = cordial_by_id(id);
    if (!l || l->ended) return;
    l->closing = true;
    if (l->pairing) gap_set_bondable_mode(0);
    if (l->handle != HCI_CON_HANDLE_INVALID) {
        if (gap_disconnect(l->handle) == ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER) finish(l);
        return;
    }
#ifdef ENABLE_CLASSIC
    if (l->peer.transport == CORDIAL_CLASSIC && l->cid) { hid_host_disconnect(l->cid); return; }
#endif
    if (initiating == l && initiating_started) { (void)gap_connect_cancel(); return; }
    finish(l);
}
void cordial_fail(cordial_connection *l, uint8_t error) {
    if (!l || l->ended) return;
    l->error = error;
    cordial_profiles_disconnect(l->id);
}
static cordial_connection *allocate(cordial_link id, cordial_peer peer, bool pairing) {
    if (!id.generation || id.slot >= CORDIAL_LINKS || cordial_by_id(id) || by_peer(peer)) return NULL;
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i) {
        cordial_connection *l = &cordial_connections[i];
        if (l->used) continue;
        memset(l, 0, sizeof *l);
        l->used = true; l->id = id; l->peer = peer; l->pairing = pairing; l->adopted = !pairing;
        l->handle = HCI_CON_HANDLE_INVALID; l->remote_io = SSP_IO_CAPABILITY_UNKNOWN;
        l->deadline = now_ms + (pairing ? 180000 : 30000);
        return l;
    }
    return NULL;
}
static int publish_security(cordial_connection *l) {
    uint32_t value = 0;
    gap_connection_type_t type = gap_get_connection_type(l->handle);
    if (type != GAP_CONNECTION_INVALID) {
        uint8_t key_size = gap_encryption_key_size(l->handle);
        value = (gap_bonded(l->handle) << 3) | (1u << 19);
        if (key_size) {
            value |= 1u | (gap_authenticated(l->handle) << 1)
                | (gap_secure_connection(l->handle) << 2) | (7u << 16)
                | ((uint32_t)key_size << 8);
        } else if (type == GAP_CONNECTION_LE) {
            value |= 1u << 16; // Known unencrypted; no current encryption key.
        }
        // Classic's v1 Encryption Change precedes Read Encryption Key Size.
        // Until GAP_EVENT_SECURITY_LEVEL, zero means pending, not unencrypted.
    }
    l->security_pending = !cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_SECURITY, .number = value });
    return !l->security_pending;
}
void cordial_ready(cordial_connection *l) {
    if (l->profile && gatt_setup == l) gatt_setup = NULL;
    if (l->ready || l->closing || !l->authenticated || !l->profile || !l->adopted) return;
    l->ready = true; l->deadline = 0;
    (void)publish_security(l);
    cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_CONNECTED, .number = CORDIAL_REPORT_BYTES });
}
static void bonded(cordial_connection *l) {
    if (!l->pairing || l->bonded || l->closing) return;
    if (!has_key(l->peer)) { cordial_fail(l, CORDIAL_AUTHENTICATION); return; }
    l->bonded = true;
    cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_BONDED });
}
int cordial_profiles_adopt(cordial_link id) {
    cordial_connection *l = cordial_by_id(id);
    if (!l || !l->bonded || l->closing || l->ended || !has_key(l->peer)) return CORDIAL_CONNECTION;
    l->adopted = true; l->pairing = false; l->deadline = now_ms + 30000;
    gap_set_bondable_mode(0);
    cordial_ready(l);
    return CORDIAL_OK;
}
static void prompt(cordial_connection *l, uint8_t method, uint32_t number) {
    if (l && l->pairing && !l->closing)
        cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_PROMPT, .code = method, .number = number });
}
void cordial_write_done(cordial_connection *l, uint8_t error) {
    if (!l->writing) return;
    l->writing = false;
    cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_WRITTEN, .code = error, .number = l->sequence });
}
void cordial_read_done(cordial_connection *l, uint8_t error) {
    if (!l->reading) return;
    l->reading = false; l->read_deadline = 0;
        cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_READ, .code = error, .number = l->sequence,
            .service = l->operation_service, .report_id = l->operation_id == HID_REPORT_ID_UNDEFINED ? 0 : l->operation_id,
            .report_type = l->operation_type, .data = l->bytes, .length = error ? 0 : l->length });
}
static void input(cordial_connection *l, uint16_t service, uint8_t id, const uint8_t *bytes, uint16_t size) {
    if (!l->ready || l->closing) return;
    if (size > CORDIAL_REPORT_BYTES) { cordial_fail(l, CORDIAL_OVERFLOW); return; }
    cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_INPUT, .service = service, .report_id = id, .data = bytes, .length = size });
}
static void begin_hids(cordial_connection *l) {
    if (!ready || l->closing || !l->authenticated || l->cid || gatt_setup) return;
    gatt_setup = l;
    if (hids_host_connect(l->handle, hids_handler, HID_PROTOCOL_MODE_REPORT, &l->cid)) {
        gatt_setup = NULL; cordial_fail(l, CORDIAL_UNSUPPORTED);
    }
}
static void hids_handler(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size) {
    (void)channel;
    if ((type != HCI_EVENT_PACKET && type != HCI_EVENT_GATTSERVICE_META) || size < 5 || packet[0] != HCI_EVENT_GATTSERVICE_META) return;
    cordial_connection *l = by_cid(little_endian_read_16(packet, 3), CORDIAL_BLE);
    if (!l || l->closing) return;
    switch (hci_event_gattservice_meta_get_subevent_code(packet)) {
        case GATTSERVICE_SUBEVENT_HID_SERVICE_CONNECTED: {
            if (size < 8 || gattservice_subevent_hid_service_connected_get_status(packet)) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            unsigned count = gattservice_subevent_hid_service_connected_get_num_instances(packet);
            if (!count || count > MAX_NUM_HID_SERVICES) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            cordial_gatt_begin(l);
            break;
        }
        case GATTSERVICE_SUBEVENT_HID_REPORT: {
            if (size < 10) return;
            uint16_t length = gattservice_subevent_hid_report_get_report_len(packet);
            const uint8_t *data = gattservice_subevent_hid_report_get_report(packet);
            uint8_t id = gattservice_subevent_hid_report_get_report_id(packet);
            if (!length || length > size - 9 || data[0] != id) { cordial_fail(l, CORDIAL_CONNECTION); return; }
            input(l, gattservice_subevent_hid_report_get_service_index(packet), id, data + 1, length - 1);
            break;
        }
        default: break;
    }
}
#ifdef ENABLE_CLASSIC
static uint8_t classic_report_error(uint8_t handshake) {
    switch (handshake) {
        case HID_HANDSHAKE_PARAM_TYPE_SUCCESSFUL: return CORDIAL_OK;
        case HID_HANDSHAKE_PARAM_TYPE_NOT_READY: return CORDIAL_BUSY;
        case HID_HANDSHAKE_PARAM_TYPE_ERR_INVALID_REPORT_ID:
        case HID_HANDSHAKE_PARAM_TYPE_ERR_UNSUPPORTED_REQUEST:
        case HID_HANDSHAKE_PARAM_TYPE_ERR_INVALID_PARAMETER: return CORDIAL_UNSUPPORTED;
        default: return CORDIAL_CONNECTION;
    }
}
static void classic_event(uint8_t *packet, uint16_t size) {
    if (size < 5) return;
    uint8_t event = hci_event_hid_meta_get_subevent_code(packet);
    uint16_t cid = little_endian_read_16(packet, 3);
    if (event == HID_SUBEVENT_CONNECTION_CLOSED)
        for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
            if (incoming[i].cid == cid) incoming[i].attempt = 0;
    if (event == HID_SUBEVENT_INCOMING_CONNECTION) {
        if (size < 14 || hid_subevent_incoming_connection_get_status(packet)) return;
        cordial_peer peer = { .transport = CORDIAL_CLASSIC };
        hid_subevent_incoming_connection_get_address(packet, peer.address);
        if (!ready || !has_key(peer) || by_peer(peer)) { hid_host_decline_connection(cid); return; }
        for (unsigned i = 0; i < CORDIAL_LINKS; ++i) if (!incoming[i].attempt && !incoming[i].closing) {
            if (++next_attempt == 0) ++next_attempt;
            incoming[i].attempt = next_attempt; incoming[i].peer = peer; incoming[i].cid = cid;
            incoming[i].offered=true;incoming[i].auth_failed=false;
            incoming[i].handle = hid_subevent_incoming_connection_get_handle(packet);
            incoming[i].deadline = now_ms + 5000;
            if (!cordial_emit_event(NULL, (cordial_event){ .kind = CORDIAL_INCOMING, .peer = peer, .number = next_attempt })) {
                incoming[i].attempt = 0; hid_host_decline_connection(cid);
            }
            return;
        }
        hid_host_decline_connection(cid); return;
    }
    cordial_connection *l = by_cid(cid, CORDIAL_CLASSIC);
    if (!l) return;
    if (l->closing && event != HID_SUBEVENT_CONNECTION_OPENED && event != HID_SUBEVENT_CONNECTION_CLOSED) return;
    switch (event) {
        case HID_SUBEVENT_CONNECTION_OPENED:
            if (initiating == l) initiating = NULL;
            if (size < 15 || hid_subevent_connection_opened_get_status(packet)) {
                if (!l->closing) l->error = CORDIAL_CONNECTION;
                l->cid = 0;
                cordial_profiles_disconnect(l->id); return;
            }
            l->handle = hid_subevent_connection_opened_get_con_handle(packet);
            if (l->closing) { cordial_profiles_disconnect(l->id); return; }
            gap_request_security_level(l->handle, LEVEL_2);
            l->authenticated = gap_security_level(l->handle) >= LEVEL_2;
            if (l->authenticated) bonded(l);
            cordial_ready(l); break;
        case HID_SUBEVENT_DESCRIPTOR_AVAILABLE: {
            if (size < 6 || hid_subevent_descriptor_available_get_status(packet)) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            const uint8_t *data = hid_descriptor_storage_get_descriptor_data(cid);
            uint16_t length = hid_descriptor_storage_get_descriptor_len(cid);
            if (!length || length > CORDIAL_DESCRIPTOR_BYTES || !data) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            l->numbered = btstack_hid_report_id_declared(data, length);
            if (!cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_DESCRIPTOR, .length = length, .data = data })) return;
            l->profile = true; cordial_ready(l); break;
        }
        case HID_SUBEVENT_REPORT: {
            if (size < 8) return;
            uint16_t length = hid_subevent_report_get_report_len(packet);
            const uint8_t *data = hid_subevent_report_get_report(packet);
            if (!length || length > size - 7 || *data++ != 0xa1) return;
            --length; uint8_t id = 0;
            if (l->numbered) { if (!length) return; id = *data++; --length; }
            input(l, 0, id, data, length); break;
        }
        case HID_SUBEVENT_SET_REPORT_RESPONSE:
            if (size >= 6) cordial_write_done(l, classic_report_error(hid_subevent_set_report_response_get_handshake_status(packet)));
            break;
        case HID_SUBEVENT_GET_REPORT_RESPONSE: {
            if (size < 8 || !l->reading) return;
            uint16_t length = hid_subevent_get_report_response_get_report_len(packet);
            const uint8_t *data = hid_subevent_get_report_response_get_report(packet);
            uint8_t error = classic_report_error(hid_subevent_get_report_response_get_handshake_status(packet));
            if (error || length > size - 8) { cordial_read_done(l, error ? error : CORDIAL_CONNECTION); return; }
            if (l->numbered) {
                if (!length || *data++ != l->operation_id) { cordial_read_done(l, CORDIAL_CONNECTION); return; }
                --length;
            }
            if (length > sizeof l->bytes) { cordial_read_done(l, CORDIAL_REPORT_SIZE); return; }
            memcpy(l->bytes, data, length); l->length = length;
            cordial_read_done(l, CORDIAL_OK); break;
        }
        case HID_SUBEVENT_CONNECTION_CLOSED:
            // Do not reuse the slot while the underlying ACL still owns callbacks.
            if (l->handle == HCI_CON_HANDLE_INVALID) finish(l); else cordial_profiles_disconnect(l->id);
            break;
        case HID_SUBEVENT_VIRTUAL_CABLE_UNPLUG: cordial_fail(l, CORDIAL_AUTHENTICATION); break;
        default: break;
    }
}
#endif

static void security_ready(cordial_connection *l) {
    if (!l || l->closing || l->security_requested) return;
    irk_lookup_state_t state = sm_identity_resolving_state(l->handle);
    if (state != IRK_LOOKUP_SUCCEEDED && state != IRK_LOOKUP_FAILED) return;
    if (!l->pairing) {
        if (state == IRK_LOOKUP_FAILED) cordial_fail(l, CORDIAL_AUTHENTICATION);
        else if(gap_bonded(l->handle) && gap_encryption_key_size(l->handle)>=7) {
            // Encryption may finish while this ACL is awaiting core admission.
            l->authenticated=true;begin_hids(l);
        }
        return;
    }
    // Committed keys live in the application store. Clear a resolved active
    // entry so explicit pairing negotiates fresh keys; failure restores it.
    cordial_peer identity;
    if (state == IRK_LOOKUP_SUCCEEDED && le_identity(sm_le_device_index(l->handle), &identity) && has_key(identity)) {
        le_device_db_remove(sm_le_device_index(l->handle));
    }
    l->security_requested = true;
    sm_request_pairing(l->handle);
}
static void sm_handler(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size) {
    (void)channel;
    if (type != HCI_EVENT_PACKET || size < 4) return;
    cordial_connection *l = cordial_by_handle(little_endian_read_16(packet, 2));
    if(!l) {
        if(size>=12 && hci_event_packet_get_type(packet)==SM_EVENT_REENCRYPTION_COMPLETE &&
           sm_event_reencryption_complete_get_status(packet))
            for(unsigned i=0;i<CORDIAL_LINKS;i++)if(incoming[i].attempt && incoming[i].handle==little_endian_read_16(packet,2))incoming[i].auth_failed=true;
        return;
    }
    if (l->peer.transport != CORDIAL_BLE) return;
    bool pair = l->pairing && !l->closing && !l->adopted;
    switch (hci_event_packet_get_type(packet)) {
        case SM_EVENT_IDENTITY_CREATED:
            // Peer keys can be persisted before our key distribution finishes.
            // Record the identity now, including while cancellation is pending.
            if (size >= 20 && l->pairing && l->security_requested && !l->adopted) {
                l->peer.random = sm_event_identity_created_get_identity_addr_type(packet) != BD_ADDR_TYPE_LE_PUBLIC;
                sm_event_identity_created_get_identity_address(packet, l->peer.address);
            }
            break;
        case SM_EVENT_IDENTITY_RESOLVING_SUCCEEDED:
        case SM_EVENT_IDENTITY_RESOLVING_FAILED:
        case SM_EVENT_SECURITY_REQUEST: security_ready(l); break;
        case SM_EVENT_PAIRING_STARTED:
            if (!pair) { sm_bonding_decline(l->handle); cordial_fail(l, CORDIAL_AUTHENTICATION); }
            break;
        case SM_EVENT_JUST_WORKS_REQUEST:
            if (pair) sm_just_works_confirm(l->handle); else sm_bonding_decline(l->handle);
            break;
        case SM_EVENT_PASSKEY_INPUT_NUMBER:
            if (pair) prompt(l, CORDIAL_ENTER_PASSKEY, 0); else sm_bonding_decline(l->handle);
            break;
        case SM_EVENT_NUMERIC_COMPARISON_REQUEST:
            if (!pair) sm_bonding_decline(l->handle);
            else if (size >= 15) prompt(l, CORDIAL_CONFIRM, sm_event_numeric_comparison_request_get_passkey(packet));
            break;
        case SM_EVENT_PASSKEY_DISPLAY_NUMBER:
            if (pair && size >= 15) prompt(l, CORDIAL_DISPLAY_PASSKEY, sm_event_passkey_display_number_get_passkey(packet));
            break;
        case SM_EVENT_PAIRING_COMPLETE: {
            if (size < 14) return;
            if (sm_event_pairing_complete_get_status(packet)) { cordial_fail(l, CORDIAL_AUTHENTICATION); break; }
            // Native persistence precedes this callback. Even if cancellation
            // is already underway, retain the new identity for terminal cleanup.
            if (!l->pairing || !l->security_requested) { cordial_fail(l, CORDIAL_AUTHENTICATION); break; }
            cordial_peer identity;
            if (!le_identity(sm_le_device_index(l->handle), &identity)) { cordial_fail(l, CORDIAL_STORAGE); break; }
            l->peer = identity;
            if (!pair) { cordial_fail(l, CORDIAL_AUTHENTICATION); break; }
            bonded(l);
            (void)gap_load_resolving_list_from_le_device_db();
            l->authenticated = gap_encryption_key_size(l->handle) >= 7;
            if (!l->authenticated) { cordial_fail(l, CORDIAL_AUTHENTICATION); break; }
            begin_hids(l); break;
        }
        case SM_EVENT_REENCRYPTION_COMPLETE:
            if (size < 12 || l->closing) return;
            if (sm_event_reencryption_complete_get_status(packet) || l->pairing || gap_encryption_key_size(l->handle) < 7) {
                cordial_fail(l, CORDIAL_AUTHENTICATION); break;
            }
            l->authenticated = true;
            if (l->ready) (void)publish_security(l);
            begin_hids(l); break;
        default: break;
    }
}
static void found(cordial_peer peer, const uint8_t *name, uint16_t length, int16_t rssi, bool connectable, uint32_t device_class) {
    cordial_emit_event(NULL, (cordial_event){ .kind = CORDIAL_FOUND, .peer = peer, .address = peer, .operation = scan_id,
        .data = name, .length = length, .rssi = rssi, .code = connectable, .number = device_class });
}
static void resolved_found(cordial_peer peer, const uint8_t *name, uint16_t length, int16_t rssi, bool connectable, uint16_t appearance) {
    // Wake reconnect needs only identity and must not wait for the RPA query.
    cordial_emit_event(NULL, (cordial_event){ .kind = CORDIAL_FOUND, .peer = peer, .address = { .transport = 0xff }, .operation = scan_id,
        .rssi = rssi, .code = connectable });
    unsigned slot = 8;
    for (unsigned i = 0; i < 8; ++i) {
        if (rpa_reports[i].used && peer_equal(rpa_reports[i].peer, peer) &&
            (i != rpa_read || rpa_reports[i].scan == scan_id)) { slot = i; break; }
        if (!rpa_reports[i].used && slot == 8) slot = i;
    }
    if (slot == 8) return;
    if (!rpa_reports[slot].used || rpa_reports[slot].scan != scan_id ||
        !peer_equal(rpa_reports[slot].peer, peer)) {
        rpa_reports[slot].appearance = 0;
        rpa_reports[slot].length = 0;
    }
    if (appearance) rpa_reports[slot].appearance = appearance;
    rpa_reports[slot].used = true;
    rpa_reports[slot].scan = scan_id;
    rpa_reports[slot].peer = peer;
    rpa_reports[slot].rssi = rssi;
    rpa_reports[slot].connectable = connectable;
    if (length > sizeof(rpa_reports[slot].name)) length = sizeof(rpa_reports[slot].name);
    if (length) {
        memcpy(rpa_reports[slot].name, name, length);
        rpa_reports[slot].length = (uint8_t)length;
    }
}
static void rpa_complete(const uint8_t *packet, uint16_t size) {
    if (rpa_read == 8) return;
    unsigned slot = rpa_read;
    rpa_read = 8;
    if (size >= 12 && packet[5] == ERROR_CODE_SUCCESS && ready && scan_ble && rpa_reports[slot].scan == scan_id) {
        cordial_peer address = { .transport = CORDIAL_BLE, .random = 1 };
        reverse_bd_addr(&packet[6], address.address);
        // A zero result means no RPA is known. Do not advertise the identity
        // as a directly connectable address after its bond has been removed.
        if ((address.address[0] & 0xc0) == 0x40) {
            cordial_emit_event(NULL, (cordial_event){ .kind = CORDIAL_FOUND, .peer = rpa_reports[slot].peer,
                .address = address, .operation = scan_id, .data = rpa_reports[slot].name,
                .length = rpa_reports[slot].length, .rssi = rpa_reports[slot].rssi,
                .code = rpa_reports[slot].connectable, .number = rpa_reports[slot].appearance });
        }
    }
    rpa_reports[slot].used = false;
}
static void packet_handler(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size) {
    (void)channel;
    if (type != HCI_EVENT_PACKET || size < 2) return;
    cordial_peer peer = { .transport = CORDIAL_CLASSIC };
    cordial_connection *l;
    // Read the stack's negotiated properties; readiness is not MITM authentication.
    uint8_t event = hci_event_packet_get_type(packet);
    if ((event == HCI_EVENT_ENCRYPTION_CHANGE && size >= 6)
        || (event == HCI_EVENT_ENCRYPTION_CHANGE_V2 && size >= 7)
        || (event == HCI_EVENT_ENCRYPTION_KEY_REFRESH_COMPLETE && size >= 5)) {
        l = cordial_by_handle(little_endian_read_16(packet, 3));
        if (l && l->ready && !l->closing) (void)publish_security(l);
    } else if (event == GAP_EVENT_SECURITY_LEVEL && size >= 6) {
        l = cordial_by_handle(gap_event_security_level_get_handle(packet));
        if (l && l->ready && !l->closing) (void)publish_security(l);
    }
    switch (hci_event_packet_get_type(packet)) {
        case BTSTACK_EVENT_POWERON_FAILED:
            ready = false; cordial_emit_event(NULL, (cordial_event){ .kind = CORDIAL_FAILED, .code = CORDIAL_RADIO }); break;
        case BTSTACK_EVENT_STATE:
            if (!stopping && size >= 3 && btstack_event_state_get_state(packet) == HCI_STATE_WORKING) {
                states_pending=true;states_sent=false;scan_and_connect=false;
                memset(rpa_reports, 0, sizeof rpa_reports); rpa_read = 8;
                (void)gap_load_resolving_list_from_le_device_db();
            }
            break;
        case GAP_EVENT_ADVERTISING_REPORT: {
            if (!scan_ble || size < 12) return;
            peer.transport = CORDIAL_BLE;
            peer.random = gap_event_advertising_report_get_address_type(packet) & 1;
            gap_event_advertising_report_get_address(packet, peer.address);
            uint8_t length = gap_event_advertising_report_get_data_length(packet);
            const uint8_t *data = gap_event_advertising_report_get_data(packet), *name = NULL;
            uint8_t name_length = 0;
            uint16_t appearance = 0;
            if (length > size - 12) return;
            ad_context_t ad;
            for (ad_iterator_init(&ad, length, data); ad_iterator_has_more(&ad); ad_iterator_next(&ad)) {
                uint8_t tag = ad_iterator_get_data_type(&ad);
                if (tag == BLUETOOTH_DATA_TYPE_APPEARANCE && ad_iterator_get_data_len(&ad) == 2)
                    appearance = little_endian_read_16(ad_iterator_get_data(&ad), 0);
                if (tag == BLUETOOTH_DATA_TYPE_COMPLETE_LOCAL_NAME || (tag == BLUETOOTH_DATA_TYPE_SHORTENED_LOCAL_NAME && !name)) {
                    name = ad_iterator_get_data(&ad); name_length = ad_iterator_get_data_len(&ad);
                }
            }
            uint8_t event = gap_event_advertising_report_get_advertising_event_type(packet);
            uint8_t type = gap_event_advertising_report_get_address_type(packet);
            if (type == BD_ADDR_TYPE_LE_PUBLIC_IDENTITY || type == BD_ADDR_TYPE_LE_RANDOM_IDENTITY)
                resolved_found(peer, name, name_length, gap_event_advertising_report_get_rssi(packet), event <= 1, appearance);
            else if (type <= BD_ADDR_TYPE_LE_RANDOM)
                found(peer, name, name_length, gap_event_advertising_report_get_rssi(packet), event <= 1, appearance);
            break;
        }
        case HCI_EVENT_COMMAND_COMPLETE:
            if(states_pending && states_sent && size>=6 &&
               hci_event_command_complete_get_command_opcode(packet)==read_supported_states.opcode) {
                // Vol 4, Part E, LE Read Supported States: bit 23 is active
                // scanning plus initiating. An unavailable/failed query is exclusive.
                scan_and_connect=size>=14 && !packet[5] && (packet[8]&0x80);
                states_pending=false;
                if(!stopping) {ready=true;cordial_emit_event(NULL,(cordial_event){.kind=CORDIAL_READY});}
            }
            if (size >= 5 && hci_event_command_complete_get_command_opcode(packet) == read_peer_rpa.opcode)
                rpa_complete(packet, size);
            break;
        case HCI_EVENT_DISCONNECTION_COMPLETE:
            if (size >= 6) {
                uint16_t handle = hci_event_disconnection_complete_get_connection_handle(packet);
                for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
                    if (incoming[i].handle == handle && (incoming[i].attempt || incoming[i].closing)) {
                        incoming[i].attempt=0;incoming[i].closing=false;
                        if(incoming[i].peer.transport==CORDIAL_BLE)auto_consumed=false;
                    }
                l = cordial_by_handle(handle); if (l) finish(l);
            }
            break;
        case HCI_EVENT_LE_META:
            // BTstack hides GAP completion for internal cancel/restart during
            // privacy-list updates. The raw event still retires that Create.
            if(auto_active && size>=4 && packet[3]==ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER &&
               (packet[2]==HCI_SUBEVENT_LE_CONNECTION_COMPLETE ||
                packet[2]==HCI_SUBEVENT_LE_ENHANCED_CONNECTION_COMPLETE_V1 ||
                packet[2]==HCI_SUBEVENT_LE_ENHANCED_CONNECTION_COMPLETE_V2))auto_started=false;
            break;
        case HCI_EVENT_META_GAP:
            if (size < 36 || hci_event_gap_meta_get_subevent_code(packet) != GAP_SUBEVENT_LE_CONNECTION_COMPLETE) return;
            if(auto_active) {
                auto_active=auto_started=auto_cancel=false;
                uint8_t status=gap_subevent_le_connection_complete_get_status(packet);
                if(status) {
                    if(status!=ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER)restart();
                    return;
                }
                uint16_t handle=gap_subevent_le_connection_complete_get_connection_handle(packet);
                auto_consumed=true;
                peer.transport=CORDIAL_BLE;
                peer.random=gap_subevent_le_connection_complete_get_peer_address_type(packet)&1;
                gap_subevent_le_connection_complete_get_peer_address(packet,peer.address);
                if(ready && !stopping && gap_subevent_le_connection_complete_get_role(packet)==HCI_ROLE_MASTER && next_attempt!=UINT32_MAX) {
                    for(unsigned i=0;i<CORDIAL_LINKS;i++)if(!incoming[i].attempt && !incoming[i].closing) {
                        incoming[i].attempt=++next_attempt;incoming[i].peer=peer;incoming[i].handle=handle;
                        incoming[i].cid=0;incoming[i].deadline=now_ms+5000;
                        incoming[i].offered=incoming[i].auth_failed=false;
                        offer_incoming(i);return;
                    }
                }
                gap_disconnect(handle);return;
            }
            l = initiating && initiating->peer.transport == CORDIAL_BLE ? initiating : NULL;
            initiating = NULL;
            if (gap_subevent_le_connection_complete_get_status(packet)) {
                if (l) { if (!l->closing) l->error = CORDIAL_CONNECTION; finish(l); }
                // The unchanged host retains its initiator after non-cancel
                // LE failures. Drain links, then use its public OFF/ON cycle.
                if (l && gap_subevent_le_connection_complete_get_status(packet) != ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER)
                    restart();
                return;
            }
            if (!l || gap_subevent_le_connection_complete_get_role(packet) != HCI_ROLE_MASTER) {
                gap_disconnect(gap_subevent_le_connection_complete_get_connection_handle(packet)); return;
            }
            l->handle = gap_subevent_le_connection_complete_get_connection_handle(packet);
            if (l->closing) cordial_profiles_disconnect(l->id); else security_ready(l);
            break;
        case HCI_EVENT_COMMAND_STATUS:
            if(size>=6 && states_pending && states_sent &&
               hci_event_command_status_get_command_opcode(packet)==read_supported_states.opcode &&
               hci_event_command_status_get_status(packet)) {
                states_pending=false;scan_and_connect=false;
                if(!stopping) {ready=true;cordial_emit_event(NULL,(cordial_event){.kind=CORDIAL_READY});}
            }
            if(size>=6 && auto_active &&
               (hci_event_command_status_get_command_opcode(packet)==HCI_OPCODE_HCI_LE_CREATE_CONNECTION ||
                hci_event_command_status_get_command_opcode(packet)==HCI_OPCODE_HCI_LE_EXTENDED_CREATE_CONNECTION)) {
                if(hci_event_command_status_get_status(packet)) {
                    auto_active=auto_started=auto_cancel=false;
                    cordial_emit_event(NULL,(cordial_event){.kind=CORDIAL_FAILED,.code=CORDIAL_RADIO});
                    ready=false;
                } else auto_started=true;
                break;
            }
            if (size >= 6 && hci_event_command_status_get_status(packet) &&
                hci_event_command_status_get_command_opcode(packet) == HCI_OPCODE_HCI_LE_CREATE_CONNECTION &&
                initiating && initiating->peer.transport == CORDIAL_BLE) {
                l = initiating; l->error = CORDIAL_CONNECTION; finish(l);
            }
            break;
#ifdef ENABLE_CLASSIC
        case GAP_EVENT_INQUIRY_RESULT: {
            if (!scan_classic || size < 27) return;
            gap_event_inquiry_result_get_bd_addr(packet, peer.address);
            const uint8_t *name = NULL; uint16_t length = 0;
            if (gap_event_inquiry_result_get_name_available(packet)) {
                length = gap_event_inquiry_result_get_name_len(packet);
                name = gap_event_inquiry_result_get_name(packet);
                if (name + length > packet + size) return;
            }
            found(peer, name, length, gap_event_inquiry_result_get_rssi_available(packet) ? gap_event_inquiry_result_get_rssi(packet) : 127, true, gap_event_inquiry_result_get_class_of_device(packet));
            break;
        }
        case GAP_EVENT_INQUIRY_COMPLETE: inquiry = false; break;
        case HCI_EVENT_CONNECTION_COMPLETE:
            if (size < 13) return;
            hci_event_connection_complete_get_bd_addr(packet, peer.address);
            l = by_peer(peer);
            if (l) {
                if (initiating == l) initiating = NULL;
                if (!hci_event_connection_complete_get_status(packet)) {
                    l->handle = hci_event_connection_complete_get_connection_handle(packet);
                    if (l->closing) cordial_profiles_disconnect(l->id);
                }
            }
            break;
        case HCI_EVENT_IO_CAPABILITY_RESPONSE:
            if (size < 11) return;
            hci_event_io_capability_response_get_bd_addr(packet, peer.address); l = by_peer(peer);
            if (l) l->remote_io = hci_event_io_capability_response_get_io_capability(packet);
            break;
        case HCI_EVENT_PIN_CODE_REQUEST:
            if (size < 8) return;
            hci_event_pin_code_request_get_bd_addr(packet, peer.address); l = by_peer(peer);
            if (l && l->pairing && !l->closing) prompt(l, CORDIAL_ENTER_PIN, 0); else gap_pin_code_negative(peer.address);
            break;
        case HCI_EVENT_USER_PASSKEY_REQUEST:
            if (size < 8) return;
            hci_event_user_passkey_request_get_bd_addr(packet, peer.address); l = by_peer(peer);
            if (l && l->pairing && !l->closing) prompt(l, CORDIAL_ENTER_PASSKEY, 0); else gap_ssp_passkey_negative(peer.address);
            break;
        case HCI_EVENT_USER_CONFIRMATION_REQUEST:
            if (size < 12) return;
            hci_event_user_confirmation_request_get_bd_addr(packet, peer.address); l = by_peer(peer);
            if (!l || !l->pairing || l->closing) gap_ssp_confirmation_negative(peer.address);
            else if (l->remote_io != SSP_IO_CAPABILITY_DISPLAY_YES_NO) gap_ssp_confirmation_response(peer.address);
            else prompt(l, CORDIAL_CONFIRM, hci_event_user_confirmation_request_get_numeric_value(packet));
            break;
        case HCI_EVENT_USER_PASSKEY_NOTIFICATION:
            if (size < 12) return;
            hci_event_user_passkey_notification_get_bd_addr(packet, peer.address);
            prompt(by_peer(peer), CORDIAL_DISPLAY_PASSKEY, hci_event_user_passkey_notification_get_numeric_value(packet)); break;
        case HCI_EVENT_AUTHENTICATION_COMPLETE:
            if (size < 5) return;
            l = cordial_by_handle(hci_event_authentication_complete_get_connection_handle(packet));
            if (!l) return;
            if (hci_event_authentication_complete_get_status(packet)) cordial_fail(l, CORDIAL_AUTHENTICATION); else bonded(l);
            break;
        case HCI_EVENT_ENCRYPTION_CHANGE:
            if (size < 6) return;
            l = cordial_by_handle(hci_event_encryption_change_get_connection_handle(packet));
            if (!l || l->closing) return;
            if (hci_event_encryption_change_get_status(packet) || !hci_event_encryption_change_get_encryption_enabled(packet)) {
                cordial_fail(l, CORDIAL_AUTHENTICATION); return;
            }
            if (l->peer.transport == CORDIAL_CLASSIC) { l->authenticated = true; bonded(l); cordial_ready(l); }
            break;
        case HCI_EVENT_HID_META: classic_event(packet, size); break;
#endif
        default: break;
    }
}

static void reject_incoming(unsigned i) {
#ifdef ENABLE_CLASSIC
    if(incoming[i].peer.transport==CORDIAL_CLASSIC)hid_host_decline_connection(incoming[i].cid);
#endif
    uint8_t status=gap_disconnect(incoming[i].handle);
    incoming[i].closing=incoming[i].peer.transport==CORDIAL_BLE && status!=ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER;
    if(incoming[i].peer.transport==CORDIAL_BLE && !incoming[i].closing)auto_consumed=false;
    incoming[i].deadline=now_ms+5000;
    incoming[i].attempt=0;
}
bool cordial_profiles_scan_and_connect(void) { return ready && scan_and_connect; }
void cordial_profiles_stop(void) {
    states_pending=states_sent=scan_and_connect=false;
    restarting = restart_notice = false;
    stopping = true;
    if (scan_ble) gap_stop_scan();
#ifdef ENABLE_CLASSIC
    if (inquiry) { gap_inquiry_stop(); inquiry = false; }
#endif
    reconnect_count=0;
    for(unsigned i=0;i<CORDIAL_LINKS;i++)if(incoming[i].attempt)reject_incoming(i);
    ready = false; scan_classic = false; scan_ble = false;
    gap_set_bondable_mode(0);
    // Drain while HCI is WORKING: HALTING handles only its list head, which
    // can be a pending create with no connection handle.
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
        if (cordial_connections[i].used) cordial_profiles_disconnect(cordial_connections[i].id);
}
int cordial_profiles_scan(uint64_t id, bool classic, bool ble) {
    if (!ready) return CORDIAL_RADIO;
#ifndef ENABLE_CLASSIC
    if (classic) return CORDIAL_UNSUPPORTED;
#else
    if (!classic && inquiry) { gap_inquiry_stop(); inquiry = false; }
#endif
    if (scan_ble && !ble) gap_stop_scan();
    // Force an actual disable/re-enable to reset controller duplicate filtering
    // when foreground discovery takes over from background listening.
    if (ble && scan_id != id) gap_set_scan_parameters(1, 0x60, 0x30);

    if (!ble || scan_id != id) {
        for (unsigned i = 0; i < 8; ++i) if (i != rpa_read) rpa_reports[i].used = false;
    }
    scan_id = id; scan_classic = classic; scan_ble = ble;
    return CORDIAL_OK;
}
static uint8_t begin_connect(cordial_connection *l) {
    initiating_started=true;
#ifdef ENABLE_CLASSIC
    if(l->peer.transport==CORDIAL_CLASSIC) {
        if(inquiry){gap_inquiry_stop();inquiry=false;}
        return hid_host_connect(l->peer.address,HID_PROTOCOL_MODE_REPORT,&l->cid);
    }
#endif
    if(scan_ble && !scan_and_connect)gap_stop_scan();
    return gap_connect(l->peer.address,l->peer.random ? BD_ADDR_TYPE_LE_RANDOM:BD_ADDR_TYPE_LE_PUBLIC);
}
int cordial_profiles_reconnect(const cordial_peer *peers,size_t count) {
    if(!ready)return restarting || stopping ? CORDIAL_BUSY:CORDIAL_RADIO;
    if(count>8)return CORDIAL_CAPACITY;
    for(size_t i=0;i<count;i++)if(peers[i].transport!=CORDIAL_BLE || peers[i].random>1)return CORDIAL_UNSUPPORTED;
    if(count==reconnect_count && (!count || !memcmp(peers,reconnect_peers,count*sizeof(*peers))))return CORDIAL_OK;
    memcpy(reconnect_peers,peers,count*sizeof(*peers));reconnect_count=count;
    auto_dirty=true;auto_consumed=false;
    return CORDIAL_OK;
}
int cordial_profiles_connect(cordial_link id, cordial_peer peer, bool pairing) {
    if (!ready) return CORDIAL_RADIO;
    if (peer.transport > CORDIAL_BLE || peer.random > 1 || (peer.transport == CORDIAL_CLASSIC && peer.random)) return CORDIAL_UNSUPPORTED;
#ifndef ENABLE_CLASSIC
    if (peer.transport == CORDIAL_CLASSIC) return CORDIAL_UNSUPPORTED;
#endif
    // The controller has one initiator. Serialize physical setup across
    // transports while allowing all established HID links to keep running.
    if (initiating || by_peer(peer) || cordial_by_id(id)) return CORDIAL_BUSY;
    if (has_key(peer) == pairing) return CORDIAL_AUTHENTICATION;
    if (pairing) {
        cordial_peer peers[16];
        int count = cordial_profiles_bonds(peers, 16);
        if (count < 0) return -count;
        unsigned selected_count = 0;
        for (int i = 0; i < count; ++i) selected_count += peers[i].transport == peer.transport;
        if (selected_count >= 8) return CORDIAL_CAPACITY;
        for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
            if (cordial_connections[i].used && cordial_connections[i].pairing && !cordial_connections[i].adopted) return CORDIAL_BUSY;
        paired_before_count = (unsigned)count;
        memcpy(paired_before, peers, sizeof(cordial_peer) * paired_before_count);
    }
    cordial_connection *l = allocate(id, peer, pairing);
    if (!l) return CORDIAL_CAPACITY;
    initiating = l;
    if (pairing) gap_set_bondable_mode(1);
    initiating_started=false;
    bool pending=auto_active;
    for(unsigned i=0;i<CORDIAL_LINKS;i++)pending|=incoming[i].attempt || incoming[i].closing;
    uint8_t status=pending ? 0:begin_connect(l);
    if (status) {
        initiating = NULL; l->used = false;
        if (pairing) gap_set_bondable_mode(0);
        return CORDIAL_CONNECTION;
    }
    return CORDIAL_OK;
}
int cordial_profiles_incoming(uint32_t attempt, const cordial_link *accept) {
    for(unsigned i=0;i<CORDIAL_LINKS;i++)if(incoming[i].attempt==attempt && attempt) {
        cordial_connection *l=accept && has_key(incoming[i].peer) ? allocate(*accept,incoming[i].peer,false):NULL;
        if(!l){reject_incoming(i);return accept ? CORDIAL_CONNECTION:CORDIAL_OK;}
        incoming[i].attempt=0;l->cid=incoming[i].cid;l->handle=incoming[i].handle;
#ifdef ENABLE_CLASSIC
        if(l->peer.transport==CORDIAL_CLASSIC) {
            if(hid_host_accept_connection(l->cid,HID_PROTOCOL_MODE_REPORT)){cordial_fail(l,CORDIAL_CONNECTION);return CORDIAL_CONNECTION;}
        } else
#endif
        {if(incoming[i].auth_failed)cordial_fail(l,CORDIAL_AUTHENTICATION);else security_ready(l);}
        return CORDIAL_OK;
    }
    return CORDIAL_CONNECTION;
}
int cordial_profiles_reply(cordial_link id, uint8_t method, bool accept, const char *value) {
    cordial_connection *l = cordial_by_id(id);
    if (!l || !l->pairing || l->closing || l->adopted) return CORDIAL_AUTHENTICATION;
    uint32_t passkey = 0;
    if (method == CORDIAL_ENTER_PASSKEY && accept) {
        if (!value || strlen(value) != 6) return CORDIAL_AUTHENTICATION;
        for (unsigned i = 0; i < 6; ++i) {
            if (value[i] < '0' || value[i] > '9') return CORDIAL_AUTHENTICATION;
            passkey = passkey * 10 + (value[i] - '0');
        }
    }
    if (l->peer.transport == CORDIAL_BLE) {
        if (!accept) sm_bonding_decline(l->handle);
        else if (method == CORDIAL_CONFIRM) sm_numeric_comparison_confirm(l->handle);
        else if (method == CORDIAL_ENTER_PASSKEY) sm_passkey_input(l->handle, passkey);
        else return CORDIAL_UNSUPPORTED;
        return CORDIAL_OK;
    }
#ifdef ENABLE_CLASSIC
    if (method == CORDIAL_ENTER_PIN) {
        if (!accept) gap_pin_code_negative(l->peer.address);
        else {
            if (!value || !*value || strlen(value) > 16) return CORDIAL_AUTHENTICATION;
            memcpy(pin, value, strlen(value) + 1); gap_pin_code_response(l->peer.address, pin);
        }
    } else if (method == CORDIAL_ENTER_PASSKEY) {
        if (accept) gap_ssp_passkey_response(l->peer.address, passkey); else gap_ssp_passkey_negative(l->peer.address);
    } else if (method == CORDIAL_CONFIRM) {
        if (accept) gap_ssp_confirmation_response(l->peer.address); else gap_ssp_confirmation_negative(l->peer.address);
    } else return CORDIAL_UNSUPPORTED;
    return CORDIAL_OK;
#else
    return CORDIAL_UNSUPPORTED;
#endif
}
int cordial_profiles_can_write(cordial_link id) {
    cordial_connection *l = cordial_by_id(id);
    return l && l->ready && !l->closing && !l->writing && !l->reading && !l->info.pending;
}
int cordial_profiles_write(cordial_link id, uint32_t sequence, uint16_t service, uint8_t kind,
                      uint16_t report_id, const uint8_t *data, uint16_t size) {
    if (!cordial_profiles_can_write(id)) return CORDIAL_BUSY;
    cordial_connection *l = cordial_by_id(id);
    if (!data || size > sizeof l->bytes || (kind != HID_REPORT_TYPE_OUTPUT && kind != HID_REPORT_TYPE_FEATURE)) return CORDIAL_UNSUPPORTED;
    l->sequence = sequence; l->operation_service = service; l->operation_id = report_id;
    l->operation_type = kind; l->length = size; memcpy(l->bytes, data, size); l->writing = true;
    int status;
#ifdef ENABLE_CLASSIC
    if (l->peer.transport == CORDIAL_CLASSIC)
        status = service ? CORDIAL_UNSUPPORTED :
            cordial_classic_set_report(l->cid, kind, report_id, l->bytes, size);
    else
#endif
        status = cordial_gatt_write(l);
    if (status) l->writing = false;
    return status;
}
int cordial_profiles_read(cordial_link id, uint32_t sequence, uint16_t service, uint8_t kind, uint16_t report_id) {
    if (!cordial_profiles_can_write(id)) return CORDIAL_BUSY;
    cordial_connection *l = cordial_by_id(id);
    if (l->read_abandoned) return CORDIAL_UNSUPPORTED;
    if (kind < HID_REPORT_TYPE_INPUT || kind > HID_REPORT_TYPE_FEATURE) return CORDIAL_UNSUPPORTED;
    l->sequence = sequence; l->operation_service = service; l->operation_id = report_id;
    l->operation_type = kind; l->length = 0; l->reading = true;
    l->read_deadline = now_ms + 10000;
    int status;
#ifdef ENABLE_CLASSIC
    if (l->peer.transport == CORDIAL_CLASSIC)
        status = service ? CORDIAL_UNSUPPORTED : (hid_host_send_get_report(l->cid, kind, report_id) ? CORDIAL_CONNECTION : CORDIAL_OK);
    else
#endif
        status = cordial_gatt_read(l);
    if (status) l->reading = false;
    return status;
}
static void resume_radio(void) {
    bool pause=!ready || (scan_ble && !scan_and_connect) || initiating || auto_dirty || !reconnect_count || auto_consumed;
    if(auto_active) {
        if(ready && scan_ble && scan_and_connect)gap_start_scan();
        // Wait for Create Connection status before cancelling. A request still
        // queued in BTstack can otherwise be cancelled without a GAP event.
        if(pause && auto_started && !auto_cancel) {
            auto_cancel=true;(void)gap_connect_cancel();
        }
        return;
    }
    if(!ready)return;
    for(unsigned i=0;i<CORDIAL_LINKS;i++)if(incoming[i].attempt || incoming[i].closing)return;
    if(initiating) {
        if(scan_ble && scan_and_connect)gap_start_scan();
        if(!initiating_started && begin_connect(initiating))cordial_fail(initiating,CORDIAL_CONNECTION);
        return;
    }
    if(scan_ble) {
        gap_start_scan();
        if(!scan_and_connect)return;
    }
    if(!reconnect_count || auto_consumed)return;
    if(!scan_ble)gap_stop_scan();
    if(auto_dirty) {
        if(gap_whitelist_clear())goto failed;
        auto_dirty=false;
    }
    for(size_t i=0;i<reconnect_count;i++) {
        uint8_t status=gap_whitelist_add(reconnect_peers[i].random ? BD_ADDR_TYPE_LE_RANDOM:BD_ADDR_TYPE_LE_PUBLIC,reconnect_peers[i].address);
        // Removed controller entries remain allocated until their HCI ack.
        if(status==BTSTACK_MEMORY_ALLOC_FAILED)return;
        // BTstack reports an already-present address as Command Disallowed.
        if(status && status!=ERROR_CODE_COMMAND_DISALLOWED)goto failed;
    }
    if(gap_connect_with_whitelist())goto failed;
    auto_active=true;auto_started=auto_cancel=false;return;
failed:
    cordial_emit_event(NULL,(cordial_event){.kind=CORDIAL_FAILED,.code=CORDIAL_RADIO});ready=false;
}
void cordial_profiles_poll(uint64_t now) {
    now_ms = now;
    if(states_pending && !states_sent && hci_can_send_command_packet_now()) {
        states_sent=true;
        if(hci_send_cmd(&read_supported_states))states_sent=false;
    }
    resume_radio();
    if (ready && rpa_read == 8 && hci_can_send_command_packet_now()) {
        for (unsigned i = 0; i < 8; ++i) if (rpa_reports[i].used) {
            rpa_read = i;
            if (hci_send_cmd(&read_peer_rpa, rpa_reports[i].peer.random, rpa_reports[i].peer.address)) {
                rpa_read = 8;
                rpa_reports[i].used = false;
            }
            break;
        }
    }
#ifdef ENABLE_CLASSIC
    if (ready && scan_classic && !inquiry && !initiating) inquiry = gap_inquiry_start(4) == ERROR_CODE_SUCCESS;
#endif
    for(unsigned i=0;i<CORDIAL_LINKS;i++) {
        if((incoming[i].attempt || incoming[i].closing) && now>=incoming[i].deadline)reject_incoming(i);
        else if(incoming[i].attempt && incoming[i].peer.transport==CORDIAL_BLE)offer_incoming(i);
    }
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i) {
        cordial_connection *l = &cordial_connections[i];
        if (!l->used) continue;
        if (l->ended) {
            if (cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_DISCONNECTED, .code = l->error })) memset(l, 0, sizeof *l);
            continue;
        }
        if (l->reading && l->peer.transport==CORDIAL_CLASSIC && l->read_deadline && now>=l->read_deadline) {
            // No request token exists in HIDP replies. Never issue another read
            // after timeout on this link: a late reply could match its report ID.
            l->read_abandoned=true; cordial_read_done(l,CORDIAL_TIMEOUT);
        }
        if (l->deadline && now >= l->deadline && !l->closing) cordial_fail(l, CORDIAL_TIMEOUT);
        cordial_info_poll(l, now);
        if (l->security_pending && !l->closing) (void)publish_security(l);
        if (ready && l->peer.transport == CORDIAL_BLE && l->authenticated) begin_hids(l);
    }
    // Report the triggering link's own failure before invalidating other links.
    if (restart_notice && cordial_emit_event(NULL, (cordial_event){ .kind = CORDIAL_RESTARTING, .code = CORDIAL_RADIO }))
        restart_notice = false;
    if (stopping) {
        if(auto_active)return;
        for(unsigned i=0;i<CORDIAL_LINKS;i++)if(incoming[i].attempt || incoming[i].closing)return;
        for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
            if (cordial_connections[i].used && !cordial_connections[i].ended) return;
        stopping = false;
        (void)hci_power_control(HCI_POWER_OFF);
    }
    if (restarting && hci_get_state() == HCI_STATE_OFF) {
        restarting = false;
        if (cordial_profiles_start(NULL)) cordial_emit_event(NULL, (cordial_event){ .kind = CORDIAL_FAILED, .code = CORDIAL_RADIO });
    }
}
void cordial_profiles_init(void *ctx, cordial_emit callback) {
    memset(cordial_connections, 0, sizeof cordial_connections);
    context = ctx; emit = callback;
    ready = scan_classic = scan_ble = inquiry = stopping = restarting = restart_notice = false;
    states_pending=states_sent=scan_and_connect=false;
    scan_id = now_ms = 0; initiating = gatt_setup = NULL;
    memset(incoming,0,sizeof incoming);next_attempt=0;reconnect_count=0;
    auto_active=auto_started=auto_cancel=auto_dirty=auto_consumed=initiating_started=false;
    memset(rpa_reports, 0, sizeof rpa_reports); rpa_read = 8;
    l2cap_init(); sm_init(); gatt_client_init();
    att_server_init(profile_data, NULL, NULL);
    sm_set_secure_connections_only_mode(false);
    sm_set_io_capabilities(IO_CAPABILITY_KEYBOARD_DISPLAY);
    sm_set_authentication_requirements(SM_AUTHREQ_BONDING | SM_AUTHREQ_SECURE_CONNECTION | SM_AUTHREQ_MITM_PROTECTION);
    sm_set_encryption_key_size_range(7, 16);
    gap_set_bondable_mode(0);
#ifdef ENABLE_CLASSIC
    gap_ssp_set_io_capability(SSP_IO_CAPABILITY_DISPLAY_YES_NO);
    gap_ssp_set_authentication_requirement(SSP_IO_AUTHREQ_MITM_PROTECTION_REQUIRED_GENERAL_BONDING);
    gap_ssp_set_auto_accept(0);
    gap_set_security_level(LEVEL_2);
    gap_set_page_timeout(0x2000);
    gap_set_local_name("Cordial");
    gap_set_class_of_device(0x0100);
    gap_set_default_link_policy_settings(LM_LINK_POLICY_ENABLE_SNIFF_MODE | LM_LINK_POLICY_ENABLE_ROLE_SWITCH);
    gap_discoverable_control(0);
    hid_host_init(classic_descriptors, sizeof classic_descriptors);
    hid_host_register_packet_handler(packet_handler);
#endif
    // Notifications still use HIDS. Read maps through our service-aware GATT
    // path; no native descriptor storage or cross-client compaction is needed.
    hids_host_init(NULL, 0);
    gap_set_scan_parameters(1, 0x60, 0x30);
    gap_set_scan_duplicate_filter(true);
    hci_events.callback = packet_handler; hci_add_event_handler(&hci_events);
    sm_events.callback = sm_handler; sm_add_event_handler(&sm_events);
}
int cordial_profiles_start(const uint8_t *address) {
    if (address) {
        bd_addr_t copy;
        memcpy(copy, address, sizeof copy);
        hci_set_bd_addr(copy);
    }
    return hci_power_control(HCI_POWER_ON) ? CORDIAL_RADIO : CORDIAL_OK;
}
