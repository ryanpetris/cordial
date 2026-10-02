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
const uint16_t cordial_layout_report_capacity = CORDIAL_LAYOUT_REPORTS;
static cordial_emit emit;
static void *context;
static btstack_packet_callback_registration_t hci_events, sm_events;
#ifdef ENABLE_CLASSIC
static uint8_t classic_descriptors[8192];
static char pin[17];
#endif
// Enabled transports, indexed by transport; Classic starts disabled and BLE
// enabled, and both keep their setting across restarts. A disabled transport
// does not scan, initiate or admit connections; disabled Classic is also not
// connectable over BR/EDR.
static bool transport_enabled[2];
// auth_error: the security failure seen before admission, as a cordial_error.
typedef struct { uint32_t attempt; cordial_peer peer; uint16_t cid, handle, supervision_ms; uint64_t deadline; bool closing, offered; uint8_t auth_error; } cordial_incoming;
static cordial_incoming incoming[CORDIAL_LINKS];
static uint32_t next_attempt;
static cordial_peer reconnect_peers[8];
static size_t reconnect_count;
static bool auto_active, auto_started, auto_cancel, auto_dirty, auto_consumed;
// A cancelled accept-list Create Connection ends by then. The controller
// answers a cancel itself, at once; no peer takes part. A host left waiting
// for that answer cannot initiate again until its power cycle.
#define AUTO_CANCEL_TIMEOUT_MS 10000
static uint64_t auto_cancel_deadline;
static bool ready, scan_classic, scan_ble, inquiry, stopping, restarting, restart_notice;
static uint64_t scan_id, now_ms;
static bool states_pending, states_sent, scan_and_connect;
// One explicit connection per transport is set up at a time. A Classic page
// and LE initiation run concurrently until the controller rejects that; the
// session then serializes initiation across transports.
static cordial_connection *initiating[2];
static bool initiating_started[2], serialize_initiation;
// The host resets its LE initiation state when any Create Connection command
// fails, while the controller may keep initiating. A command of one transport
// therefore waits until the other's is acknowledged, and an LE initiation that
// survives a refused page is cancelled directly (resync) and, for an explicit
// connection, started again later (requeue).
static bool page_acknowledged, le_acknowledged, le_resync, le_requeue, resync_sent;
// A closing link's page is cancelled with HCI Create Connection Cancel once
// the controller runs it (page_live, for page_address); L2CAP fails the
// page's channels on success.
static bool page_cancel, page_live;
#ifdef ENABLE_CLASSIC
static bd_addr_t page_address;
#endif
// How long a closing link may take to end before Bluetooth restarts. A link
// still connecting ends within the 5.12 s page timeout, or at once when an LE
// initiation is cancelled. An established link is disconnected at the HCI
// level, which ends at the latest when its supervision timeout expires; our
// paths never wait on an L2CAP disconnect for it.
#define CONNECTING_CLOSE_MS 10000
#define CLOSE_MARGIN_MS 2000
// Unreported supervision timeouts: LE's maximum, and the BR/EDR default of
// 0x7D00 slots, which also applies when a peer turns supervision off.
#define LE_SUPERVISION_MAX_MS 32000
#define CLASSIC_SUPERVISION_DEFAULT_MS 20000
// Input that arrives before a link's Connected event, keyed by connection
// handle: records of value handle (zero for Classic), length and bytes. The
// oldest record is dropped when a new one does not fit.
#define EARLY_BYTES 256
#define EARLY_REPORT 64
static struct { hci_con_handle_t handle; uint16_t used; uint8_t bytes[EARLY_BYTES]; } early[CORDIAL_LINKS];
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
static void reject_incoming(unsigned i);
static bool incoming_pending(uint8_t transport) {
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
        if ((incoming[i].attempt || incoming[i].closing) && incoming[i].peer.transport == transport) return true;
    return false;
}
static bool page_outstanding(void) {
    return initiating[CORDIAL_CLASSIC] && initiating_started[CORDIAL_CLASSIC];
}
static bool le_initiator_outstanding(void) {
    return auto_active || (initiating[CORDIAL_BLE] && initiating_started[CORDIAL_BLE]);
}
static bool unacknowledged(uint8_t transport) {
    if (transport == CORDIAL_CLASSIC) return page_outstanding() && !page_acknowledged;
    return (auto_active && !auto_started) || (initiating[CORDIAL_BLE] && initiating_started[CORDIAL_BLE] && !le_acknowledged);
}
// Statuses a controller uses to refuse a connection command it cannot run
// alongside its current procedures.
static bool concurrency_refused(uint8_t status) {
    return status == ERROR_CODE_COMMAND_DISALLOWED || status == ERROR_CODE_CONTROLLER_BUSY
        || status == ERROR_CODE_CONNECTION_REJECTED_DUE_TO_LIMITED_RESOURCES;
}
// Security procedure statuses. Only the peer rejecting our keys means the
// bond is gone; the link failing during the procedure is retried later.
static uint8_t security_error(uint8_t status) {
    switch (status) {
        case ERROR_CODE_AUTHENTICATION_FAILURE:              // Pairing Failed or failed authentication
        case ERROR_CODE_PIN_OR_KEY_MISSING:                  // the peer has no key for this bond
        case ERROR_CODE_PAIRING_NOT_ALLOWED:
        case ERROR_CODE_PAIRING_WITH_UNIT_KEY_NOT_SUPPORTED:
        case ERROR_CODE_INSUFFICIENT_SECURITY:               // the peer requires more than the bond gives
        case ERROR_CODE_CONNECTION_TERMINATED_DUE_TO_MIC_FAILURE: // the keys differ
            return CORDIAL_AUTHENTICATION;
        default:
            // Timeouts, terminations, parameter or scheduling failures, busy
            // controllers and the host's own statuses.
            return CORDIAL_CONNECTION;
    }
}
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
#ifdef ENABLE_CLASSIC
static cordial_connection *by_cid(uint16_t cid, uint8_t transport) {
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
        if (cordial_connections[i].used && cordial_connections[i].cid == cid && cordial_connections[i].peer.transport == transport)
            return &cordial_connections[i];
    return NULL;
}
#endif
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
int cordial_emit_status(cordial_connection *l, cordial_event event) {
    if (l) { event.link = l->id; event.peer = l->peer; }
    return emit(context, &event);
}
int cordial_emit_event(cordial_connection *l, cordial_event event) {
    int accepted = cordial_emit_status(l, event);
    if (accepted <= 0 && l && event.kind != CORDIAL_DISCONNECTED && event.kind != CORDIAL_SECURITY && event.kind != CORDIAL_INFORMATION)
        cordial_fail(l, accepted < 0 ? (uint8_t)-accepted : event.kind == CORDIAL_DESCRIPTOR ? CORDIAL_CAPACITY : CORDIAL_OVERFLOW);
    return accepted > 0;
}
static int early_find(hci_con_handle_t handle) {
    if (handle == HCI_CON_HANDLE_INVALID) return -1;
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i) if (early[i].used && early[i].handle == handle) return (int)i;
    return -1;
}
static void early_drop(hci_con_handle_t handle) {
    int i = early_find(handle);
    if (i >= 0) early[i].used = 0;
}
static void early_shift(unsigned i) {
    unsigned first = 3u + early[i].bytes[2];
    memmove(early[i].bytes, early[i].bytes + first, early[i].used - first);
    early[i].used -= first;
}
bool cordial_early_pending(hci_con_handle_t handle) { return early_find(handle) >= 0; }
static bool admitting(hci_con_handle_t handle) {
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
        if (incoming[i].attempt && incoming[i].handle == handle && incoming[i].peer.transport == CORDIAL_BLE) return true;
    return false;
}
// Keeps input for links that are being admitted or set up. Others drop it.
void cordial_early_push(hci_con_handle_t handle, uint16_t value, const uint8_t *data, uint16_t length) {
    if (length > EARLY_REPORT || handle == HCI_CON_HANDLE_INVALID) return;
    cordial_connection *l = cordial_by_handle(handle);
    if (l ? l->closing : !admitting(handle)) return;
    int slot = early_find(handle);
    if (slot < 0) {
        for (slot = 0; slot < CORDIAL_LINKS && early[slot].used; ++slot) {}
        if (slot == CORDIAL_LINKS) return;
        early[slot].handle = handle;
    }
    unsigned i = (unsigned)slot;
    while (early[i].used + 3u + length > EARLY_BYTES) early_shift(i);
    uint8_t *record = early[i].bytes + early[i].used;
    little_endian_store_16(record, 0, value); record[2] = (uint8_t)length;
    memcpy(record + 3, data, length);
    early[i].used += 3 + length;
}
// Delivers buffered input in order through the link's current routing. Input
// that meets queue pressure stays buffered for the next poll.
static void early_flush(cordial_connection *l) {
    if (!l->ready || l->closing) return;
    int slot = early_find(l->handle);
    if (slot < 0) return;
    unsigned i = (unsigned)slot;
    while (early[i].used) {
        const uint8_t *record = early[i].bytes, *data = record + 3;
        uint16_t length = record[2], service = 0;
        uint8_t id = 0;
        bool known = true;
        if (l->peer.transport == CORDIAL_BLE) {
            const cordial_report *report = cordial_gatt_report(l, little_endian_read_16(record, 0));
            known = report != NULL;
            if (report) { service = report->service; id = report->id; }
        } else if (l->numbered) {
            known = length > 0;
            if (known) { id = *data++; --length; }
        }
        if (known && !cordial_emit_status(l, (cordial_event){ .kind = CORDIAL_INPUT, .service = service,
                .report_id = id, .data = data, .length = length })) return;
        early_shift(i);
    }
}
static void finish(cordial_connection *l) {
    if (!l || l->ended) return;
    for (unsigned t = 0; t < 2; ++t) if (initiating[t] == l) initiating[t] = NULL;
    cordial_gatt_end(l);
    early_drop(l->handle);
    l->closing = true;
    cordial_info_clear(l);
    l->ended = true;
    if (l->pairing) gap_set_bondable_mode(0);
    // Selected/resolved saved identities are rejected before pairing. Track
    // this attempt's native identity even when authentication is unfinished.
    if (l->pairing && !l->adopted && new_pair_identity(l->peer) && cordial_profiles_forget(l->peer)) l->error = CORDIAL_STORAGE;
}
// The time a closing link may take to end, from what it is waiting for now.
static uint32_t close_bound(const cordial_connection *l) {
    if (l->handle == HCI_CON_HANDLE_INVALID) return CONNECTING_CLOSE_MS;
    uint32_t supervision = l->supervision_ms ? l->supervision_ms :
        l->peer.transport == CORDIAL_BLE ? LE_SUPERVISION_MAX_MS : CLASSIC_SUPERVISION_DEFAULT_MS;
    return supervision + CLOSE_MARGIN_MS;
}
static void track_supervision(hci_con_handle_t handle, uint16_t ms) {
    cordial_connection *l = cordial_by_handle(handle);
    if (l) l->supervision_ms = ms;
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
        if (incoming[i].attempt && incoming[i].handle == handle) incoming[i].supervision_ms = ms;
}
void cordial_profiles_disconnect(cordial_link id) {
    cordial_connection *l = cordial_by_id(id);
    if (!l || l->ended) return;
    // The bound starts when the link starts closing; repeated requests keep it.
    if (!l->closing) l->close_deadline = now_ms + close_bound(l);
    l->closing = true;
    if (l->pairing) gap_set_bondable_mode(0);
    if (l->handle != HCI_CON_HANDLE_INVALID) {
        if (gap_disconnect(l->handle) == ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER) finish(l);
        return;
    }
#ifdef ENABLE_CLASSIC
    if (l->peer.transport == CORDIAL_CLASSIC && l->cid) {
        // The HID host cannot stop its own page; the controller can.
        if (initiating[CORDIAL_CLASSIC] == l && page_outstanding()) page_cancel = true;
        hid_host_disconnect(l->cid); return;
    }
#endif
    if (initiating[CORDIAL_BLE] == l && initiating_started[CORDIAL_BLE]) { (void)gap_connect_cancel(); return; }
    finish(l);
}
// A closing link's connection completed: end it with the bound for what it
// now waits on.
static void reclose(cordial_connection *l) {
    l->close_deadline = now_ms + close_bound(l);
    cordial_profiles_disconnect(l->id);
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
    if (l->ready || l->closing || !l->authenticated || !l->profile || !l->adopted) return;
    l->ready = true; l->deadline = 0;
    (void)publish_security(l);
    cordial_event connected = { .kind = CORDIAL_CONNECTED, .number = CORDIAL_REPORT_BYTES, .code = l->cached };
    if (l->peer.transport == CORDIAL_BLE && !l->cached) {
        connected.data = (const uint8_t *)l->reports;
        connected.length = l->report_count * sizeof *l->reports;
        if (l->hashed) connected.hash = l->hash;
    }
    if (cordial_emit_event(l, connected)) early_flush(l);
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
void cordial_input(cordial_connection *l, uint16_t service, uint8_t id, const uint8_t *bytes, uint16_t size) {
    if (!l->ready || l->closing) return;
    if (size > CORDIAL_REPORT_BYTES) { cordial_fail(l, CORDIAL_OVERFLOW); return; }
    cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_INPUT, .service = service, .report_id = id, .data = bytes, .length = size });
}
// Starts HID setup once BLE security is satisfied. A supplied layout admits
// input at once and then rewrites its CCCDs; otherwise discovery runs first.
// A supplied layout with a Database Hash is used only when the device still
// reports that hash.
void cordial_begin_profile(cordial_connection *l) {
    if (!ready || l->closing || !l->authenticated || l->setup != SETUP_NONE) return;
    if (!l->cached) { (void)cordial_gatt_discover(l, false); return; }
    if (l->hashed && !l->hash_equal) { cordial_gatt_check_hash(l); return; }
    l->profile = true; l->setup = SETUP_SUBSCRIBE; l->cursor = 0;
    cordial_ready(l);
    if (!l->closing) cordial_gatt_subscribe(l);
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
        if (!ready || !transport_enabled[CORDIAL_CLASSIC] || !has_key(peer) || by_peer(peer)) { hid_host_decline_connection(cid); return; }
        for (unsigned i = 0; i < CORDIAL_LINKS; ++i) if (!incoming[i].attempt && !incoming[i].closing) {
            if (++next_attempt == 0) ++next_attempt;
            incoming[i] = (cordial_incoming){ .attempt = next_attempt, .peer = peer, .cid = cid, .offered = true,
                .handle = hid_subevent_incoming_connection_get_handle(packet), .deadline = now_ms + 5000 };
            if (!cordial_emit_event(NULL, (cordial_event){ .kind = CORDIAL_INCOMING, .peer = peer, .number = next_attempt })) {
                incoming[i].attempt = 0; hid_host_decline_connection(cid);
            }
            return;
        }
        hid_host_decline_connection(cid); return;
    }
    cordial_connection *l = by_cid(cid, CORDIAL_CLASSIC);
    if (!l) {
        // A link released while its page ran has no owner for the result.
        if (event == HID_SUBEVENT_CONNECTION_OPENED && size >= 15 && !hid_subevent_connection_opened_get_status(packet))
            hid_host_disconnect(cid);
        return;
    }
    if (l->closing && event != HID_SUBEVENT_CONNECTION_OPENED && event != HID_SUBEVENT_CONNECTION_CLOSED) return;
    switch (event) {
        case HID_SUBEVENT_CONNECTION_OPENED: {
            uint8_t status = size < 15 ? ERROR_CODE_UNSPECIFIED_ERROR : hid_subevent_connection_opened_get_status(packet);
            if (status && initiating[CORDIAL_CLASSIC] == l && !l->closing && !serialize_initiation &&
                !page_acknowledged && concurrency_refused(status) && le_initiator_outstanding()) {
                // The controller refused the page command during LE initiation.
                // Serialize from now on: the LE initiation is cancelled and the
                // page runs first.
                serialize_initiation = true;
                l->cid = 0; initiating_started[CORDIAL_CLASSIC] = false;
                return;
            }
            if (initiating[CORDIAL_CLASSIC] == l) initiating[CORDIAL_CLASSIC] = NULL;
            if (status) {
                if (!l->closing) l->error = CORDIAL_CONNECTION;
                l->cid = 0;
                cordial_profiles_disconnect(l->id); return;
            }
            l->handle = hid_subevent_connection_opened_get_con_handle(packet);
            if (l->closing) { reclose(l); return; }
            gap_request_security_level(l->handle, LEVEL_2);
            l->authenticated = gap_security_level(l->handle) >= LEVEL_2;
            if (l->authenticated) bonded(l);
            // A supplied descriptor admits input before the host's SDP query
            // ends. The host refuses control requests until then.
            if (l->cached && l->setup == SETUP_NONE) { l->profile = true; l->setup = SETUP_SDP; }
            cordial_ready(l); break;
        }
        case HID_SUBEVENT_DESCRIPTOR_AVAILABLE: {
            bool available = size >= 6 && !hid_subevent_descriptor_available_get_status(packet);
            const uint8_t *data = available ? hid_descriptor_storage_get_descriptor_data(cid) : NULL;
            uint16_t length = available ? hid_descriptor_storage_get_descriptor_len(cid) : 0;
            // The host's descriptor storage is shared by all links; running out
            // of it says nothing about this device. A missing or oversized
            // descriptor does.
            if (size >= 6 && hid_subevent_descriptor_available_get_status(packet) == ERROR_CODE_MEMORY_CAPACITY_EXCEEDED) {
                cordial_fail(l, CORDIAL_CAPACITY); return;
            }
            if (!data || !length || length > CORDIAL_DESCRIPTOR_BYTES) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
            // A link using a supplied descriptor verifies this one after Connected.
            if (l->cached) { l->setup = SETUP_IDLE; break; }
            l->numbered = btstack_hid_report_id_declared(data, length);
            if (!cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_DESCRIPTOR, .length = length, .data = data })) return;
            l->profile = true; cordial_ready(l); break;
        }
        case HID_SUBEVENT_REPORT: {
            if (size < 8) return;
            uint16_t length = hid_subevent_report_get_report_len(packet);
            const uint8_t *data = hid_subevent_report_get_report(packet);
            if (!length || length > size - 7 || *data++ != 0xa1) return;
            --length;
            if (!l->ready || cordial_early_pending(l->handle)) { cordial_early_push(l->handle, 0, data, length); return; }
            uint8_t id = 0;
            if (l->numbered) { if (!length) return; id = *data++; --length; }
            cordial_input(l, 0, id, data, length); break;
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
// Compares the SDP descriptor with the supplied one after Connected. Rust
// reports a difference; accepting the result switches report ID parsing. A
// descriptor Cordial cannot use ends the link as discovery would.
static void classic_verify(cordial_connection *l) {
    if (l->peer.transport != CORDIAL_CLASSIC || !l->ready || l->closing || !l->cached || l->setup != SETUP_IDLE) return;
    const uint8_t *data = hid_descriptor_storage_get_descriptor_data(l->cid);
    uint16_t length = hid_descriptor_storage_get_descriptor_len(l->cid);
    if (!data || !length || length > CORDIAL_DESCRIPTOR_BYTES) { cordial_fail(l, CORDIAL_UNSUPPORTED); return; }
    if (!cordial_emit_event(l, (cordial_event){ .kind = CORDIAL_DESCRIPTOR, .code = 1, .length = length, .data = data })) return;
    int status = cordial_emit_status(l, (cordial_event){ .kind = CORDIAL_LAYOUT, .service = 1 });
    if (!status) return;
    if (status < 0) { cordial_fail(l, (uint8_t)-status); return; }
    l->cached = false;
    l->numbered = btstack_hid_report_id_declared(data, length);
}
#endif

static void security_ready(cordial_connection *l) {
    if (!l || l->closing || l->security_requested) return;
    irk_lookup_state_t state = sm_identity_resolving_state(l->handle);
    if (state != IRK_LOOKUP_SUCCEEDED && state != IRK_LOOKUP_FAILED) return;
    if (!l->pairing) {
        // The connected address does not resolve with the saved device's IRK:
        // the device now uses keys from another bond.
        if (state == IRK_LOOKUP_FAILED) cordial_fail(l, CORDIAL_AUTHENTICATION);
        else if(gap_bonded(l->handle) && gap_encryption_key_size(l->handle)>=7) {
            // Encryption may finish while this ACL is awaiting core admission.
            l->authenticated=true;cordial_begin_profile(l);
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
            for(unsigned i=0;i<CORDIAL_LINKS;i++)if(incoming[i].attempt && incoming[i].handle==little_endian_read_16(packet,2))
                incoming[i].auth_error=security_error(sm_event_reencryption_complete_get_status(packet));
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
            // A saved device that asks to pair has lost its bond.
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
            if (sm_event_pairing_complete_get_status(packet)) {
                cordial_fail(l, security_error(sm_event_pairing_complete_get_status(packet))); break;
            }
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
            cordial_begin_profile(l); break;
        }
        case SM_EVENT_REENCRYPTION_COMPLETE:
            if (size < 12 || l->closing) return;
            if (sm_event_reencryption_complete_get_status(packet)) {
                cordial_fail(l, security_error(sm_event_reencryption_complete_get_status(packet))); break;
            }
            // Old keys on a pairing link, or a key too short to accept.
            if (l->pairing || gap_encryption_key_size(l->handle) < 7) { cordial_fail(l, CORDIAL_AUTHENTICATION); break; }
            l->authenticated = true;
            if (l->ready) (void)publish_security(l);
            cordial_begin_profile(l); break;
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
#ifdef ENABLE_CLASSIC
                gap_connectable_control(transport_enabled[CORDIAL_CLASSIC]);
#endif
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
            if (resync_sent && size >= 6 &&
                hci_event_command_complete_get_command_opcode(packet) == HCI_OPCODE_HCI_LE_CREATE_CONNECTION_CANCEL) {
                resync_sent = false;
                // Nothing was cancelled, and the host now waits for a completion
                // that never comes. Only its power cycle resets that state.
                if (packet[5]) {
                    auto_active = auto_started = auto_cancel = le_requeue = false;
                    if (initiating[CORDIAL_BLE] && initiating_started[CORDIAL_BLE] && !initiating[CORDIAL_BLE]->closing)
                        initiating[CORDIAL_BLE]->error = CORDIAL_CONNECTION;
                    initiating_started[CORDIAL_BLE] = false;
                    restart();
                }
            }
            break;
        case HCI_EVENT_DISCONNECTION_COMPLETE:
            if (size >= 6) {
                uint16_t handle = hci_event_disconnection_complete_get_connection_handle(packet);
                for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
                    if (incoming[i].handle == handle && (incoming[i].attempt || incoming[i].closing)) {
                        incoming[i].attempt=0;incoming[i].closing=false;
                        if(incoming[i].peer.transport==CORDIAL_BLE)auto_consumed=false;
                    }
                early_drop(handle);
                l = cordial_by_handle(handle); if (l) finish(l);
            }
            break;
        case HCI_EVENT_LE_META:
            if(size>=12 && packet[2]==HCI_SUBEVENT_LE_CONNECTION_UPDATE_COMPLETE && !packet[3])
                track_supervision(hci_subevent_le_connection_update_complete_get_connection_handle(packet),
                    hci_subevent_le_connection_update_complete_get_supervision_timeout(packet)*10u);
            // BTstack hides GAP completion for internal cancel/restart during
            // privacy-list updates. The raw event still retires that Create.
            if(auto_active && size>=4 && packet[3]==ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER &&
               (packet[2]==HCI_SUBEVENT_LE_CONNECTION_COMPLETE ||
                packet[2]==HCI_SUBEVENT_LE_ENHANCED_CONNECTION_COMPLETE_V1 ||
                packet[2]==HCI_SUBEVENT_LE_ENHANCED_CONNECTION_COMPLETE_V2))auto_started=false;
            // A completion before the direct cancel returns the host to idle,
            // so a later cancel failure leaves nothing stuck.
            if(resync_sent && size>=4 && (!packet[3] || packet[3]==ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER) &&
               (packet[2]==HCI_SUBEVENT_LE_CONNECTION_COMPLETE ||
                packet[2]==HCI_SUBEVENT_LE_ENHANCED_CONNECTION_COMPLETE_V1 ||
                packet[2]==HCI_SUBEVENT_LE_ENHANCED_CONNECTION_COMPLETE_V2))resync_sent=false;
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
                if(ready && !stopping && transport_enabled[CORDIAL_BLE] &&
                   gap_subevent_le_connection_complete_get_role(packet)==HCI_ROLE_MASTER && next_attempt!=UINT32_MAX) {
                    for(unsigned i=0;i<CORDIAL_LINKS;i++)if(!incoming[i].attempt && !incoming[i].closing) {
                        incoming[i]=(cordial_incoming){.attempt=++next_attempt,.peer=peer,.handle=handle,.deadline=now_ms+5000,
                            .supervision_ms=(uint16_t)(gap_subevent_le_connection_complete_get_supervision_timeout(packet)*10u)};
                        offer_incoming(i);return;
                    }
                }
                gap_disconnect(handle);return;
            }
            l = initiating_started[CORDIAL_BLE] ? initiating[CORDIAL_BLE] : NULL;
            if (l && le_requeue && !l->closing &&
                gap_subevent_le_connection_complete_get_status(packet) == ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER) {
                // Cancelled after a refused page: connect again after the page.
                le_requeue = false; initiating_started[CORDIAL_BLE] = false;
                return;
            }
            le_requeue = false;
            if (l) initiating[CORDIAL_BLE] = NULL;
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
            l->supervision_ms = gap_subevent_le_connection_complete_get_supervision_timeout(packet) * 10u;
            if (l->closing) reclose(l); else security_ready(l);
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
                uint8_t status=hci_event_command_status_get_status(packet);
                if(status) {
                    auto_active=auto_started=auto_cancel=false;
                    // Retry after the page when the controller refuses both at once.
                    if(!serialize_initiation && page_outstanding() && concurrency_refused(status))serialize_initiation=true;
                    else {
                        cordial_emit_event(NULL,(cordial_event){.kind=CORDIAL_FAILED,.code=CORDIAL_RADIO});
                        ready=false;
                    }
                } else auto_started=true;
                break;
            }
            if (size >= 6 && hci_event_command_status_get_command_opcode(packet) == HCI_OPCODE_HCI_LE_CREATE_CONNECTION &&
                initiating[CORDIAL_BLE] && initiating_started[CORDIAL_BLE]) {
                l = initiating[CORDIAL_BLE];
                uint8_t status = hci_event_command_status_get_status(packet);
                if (!status) le_acknowledged = true;
                else if (!serialize_initiation && !l->closing && page_outstanding() && concurrency_refused(status)) {
                    serialize_initiation = true; initiating_started[CORDIAL_BLE] = false;
                } else { l->error = CORDIAL_CONNECTION; finish(l); }
            }
#ifdef ENABLE_CLASSIC
            if (size >= 6 && hci_event_command_status_get_command_opcode(packet) == HCI_OPCODE_HCI_CREATE_CONNECTION) {
                if (!hci_event_command_status_get_status(packet)) {
                    page_acknowledged = page_outstanding();
                    if (page_acknowledged) {
                        page_live = true;
                        memcpy(page_address, initiating[CORDIAL_CLASSIC]->peer.address, sizeof page_address);
                    }
                }
                else if (le_initiator_outstanding()) le_resync = true;
            }
#endif
            break;
#ifdef ENABLE_CLASSIC
        case GAP_EVENT_INQUIRY_RESULT: {
            if (!scan_classic || !transport_enabled[CORDIAL_CLASSIC] || size < 27) return;
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
            if (page_live && !memcmp(page_address, peer.address, sizeof page_address)) page_live = false;
            l = by_peer(peer);
            if (l) {
                if (initiating[CORDIAL_CLASSIC] == l && initiating_started[CORDIAL_CLASSIC]) initiating[CORDIAL_CLASSIC] = NULL;
                if (!hci_event_connection_complete_get_status(packet)) {
                    l->handle = hci_event_connection_complete_get_connection_handle(packet);
                    if (l->closing) reclose(l);
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
        case HCI_EVENT_LINK_SUPERVISION_TIMEOUT_CHANGED:
            // 0.625 ms slots; zero turns supervision off and leaves the default.
            if (size >= 6) track_supervision(hci_event_link_supervision_timeout_changed_get_handle(packet),
                (uint16_t)(hci_event_link_supervision_timeout_changed_get_link_supervision_timeout(packet) * 5u / 8u));
            break;
        case HCI_EVENT_AUTHENTICATION_COMPLETE:
            if (size < 5) return;
            l = cordial_by_handle(hci_event_authentication_complete_get_connection_handle(packet));
            if (!l) return;
            if (hci_event_authentication_complete_get_status(packet))
                cordial_fail(l, security_error(hci_event_authentication_complete_get_status(packet)));
            else bonded(l);
            break;
        case HCI_EVENT_ENCRYPTION_CHANGE:
            if (size < 6) return;
            l = cordial_by_handle(hci_event_encryption_change_get_connection_handle(packet));
            if (!l || l->closing) return;
            // Encryption turned off without an error is not a key rejection.
            if (hci_event_encryption_change_get_status(packet)) {
                cordial_fail(l, security_error(hci_event_encryption_change_get_status(packet))); return;
            }
            if (!hci_event_encryption_change_get_encryption_enabled(packet)) { cordial_fail(l, CORDIAL_CONNECTION); return; }
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
    le_resync = resync_sent = le_requeue = page_cancel = page_live = false;
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
    if (classic) return CORDIAL_TRANSPORT;
#else
    if (classic && !transport_enabled[CORDIAL_CLASSIC]) return CORDIAL_TRANSPORT;
    if (!classic && inquiry) { gap_inquiry_stop(); inquiry = false; }
#endif
    if (ble && !transport_enabled[CORDIAL_BLE]) return CORDIAL_TRANSPORT;
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
    initiating_started[l->peer.transport]=true;
    if(l->peer.transport==CORDIAL_BLE)le_acknowledged=false; else page_acknowledged=false;
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
    for(size_t i=0;i<count;i++) {
        if(peers[i].transport!=CORDIAL_BLE)return CORDIAL_TRANSPORT;
        if(peers[i].random>1)return CORDIAL_ARGUMENT;
    }
    if(count==reconnect_count && (!count || !memcmp(peers,reconnect_peers,count*sizeof(*peers))))return CORDIAL_OK;
    memcpy(reconnect_peers,peers,count*sizeof(*peers));reconnect_count=count;
    auto_dirty=true;auto_consumed=false;
    return CORDIAL_OK;
}
// Saved layouts never apply to pairing. Out-of-range input leaves discovery on.
static void apply_layout(cordial_connection *l, const cordial_layout *layout) {
    if (!layout || l->pairing) return;
    if (l->peer.transport == CORDIAL_BLE) {
        if (!layout->reports || !layout->count || layout->count > CORDIAL_LAYOUT_REPORTS) return;
        memcpy(l->reports, layout->reports, layout->count * sizeof *l->reports);
        l->report_count = (uint8_t)layout->count;
        l->hashed = layout->hash != NULL;
        if (l->hashed) memcpy(l->hash, layout->hash, sizeof l->hash);
    } else {
        if (!layout->descriptor || !layout->length || layout->length > CORDIAL_DESCRIPTOR_BYTES) return;
        l->numbered = btstack_hid_report_id_declared(layout->descriptor, layout->length);
    }
    l->cached = true;
}
// Whether an explicit connection of this transport may start now.
static bool can_begin(uint8_t transport) {
    if (incoming_pending(transport)) return false;
    if (transport == CORDIAL_BLE)
        return transport_enabled[CORDIAL_BLE] && !auto_active && !unacknowledged(CORDIAL_CLASSIC) &&
            !(serialize_initiation && page_outstanding());
    return transport_enabled[CORDIAL_CLASSIC] && !unacknowledged(CORDIAL_BLE) && (!serialize_initiation || !le_initiator_outstanding());
}
static void begin_pending(uint8_t transport) {
    cordial_connection *l = initiating[transport];
    if (l && !initiating_started[transport] && can_begin(transport) && begin_connect(l)) cordial_fail(l, CORDIAL_CONNECTION);
}
int cordial_profiles_connect(cordial_link id, cordial_peer peer, bool pairing, const cordial_layout *layout) {
    if (!ready) return CORDIAL_RADIO;
    if (peer.transport > CORDIAL_BLE || peer.random > 1 || (peer.transport == CORDIAL_CLASSIC && peer.random)) return CORDIAL_ARGUMENT;
    if (!transport_enabled[peer.transport]) return CORDIAL_TRANSPORT;
    // Set up one explicit connection per transport while established HID
    // links keep running. Serialized sessions set up one in total.
    if (initiating[peer.transport] || by_peer(peer) || cordial_by_id(id)) return CORDIAL_BUSY;
    if (serialize_initiation && (initiating[CORDIAL_CLASSIC] || initiating[CORDIAL_BLE])) return CORDIAL_BUSY;
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
    apply_layout(l, layout);
    initiating[peer.transport] = l;
    if (pairing) gap_set_bondable_mode(1);
    initiating_started[peer.transport]=false;
    uint8_t status=can_begin(peer.transport) ? begin_connect(l):0;
    if (status) {
        initiating[peer.transport] = NULL; l->used = false;
        if (pairing) gap_set_bondable_mode(0);
        return CORDIAL_CONNECTION;
    }
    return CORDIAL_OK;
}
int cordial_profiles_incoming(uint32_t attempt, const cordial_link *accept, const cordial_layout *layout) {
    for(unsigned i=0;i<CORDIAL_LINKS;i++)if(incoming[i].attempt==attempt && attempt) {
        bool allowed=transport_enabled[incoming[i].peer.transport];
        cordial_connection *l=accept && allowed && has_key(incoming[i].peer) ? allocate(*accept,incoming[i].peer,false):NULL;
        if(!l){reject_incoming(i);return accept ? CORDIAL_CONNECTION:CORDIAL_OK;}
        apply_layout(l,layout);
        incoming[i].attempt=0;l->cid=incoming[i].cid;l->handle=incoming[i].handle;l->supervision_ms=incoming[i].supervision_ms;
#ifdef ENABLE_CLASSIC
        if(l->peer.transport==CORDIAL_CLASSIC) {
            if(hid_host_accept_connection(l->cid,HID_PROTOCOL_MODE_REPORT)){cordial_fail(l,CORDIAL_CONNECTION);return CORDIAL_CONNECTION;}
        } else
#endif
        {if(incoming[i].auth_error)cordial_fail(l,incoming[i].auth_error);else security_ready(l);}
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
        else return CORDIAL_ARGUMENT;
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
    } else return CORDIAL_ARGUMENT;
    return CORDIAL_OK;
#else
    return CORDIAL_TRANSPORT;
#endif
}
int cordial_profiles_set_transport(uint8_t transport, bool enabled) {
    if (transport > CORDIAL_BLE) return CORDIAL_ARGUMENT;
#ifndef ENABLE_CLASSIC
    if (transport == CORDIAL_CLASSIC) return enabled ? CORDIAL_TRANSPORT : CORDIAL_OK;
#else
    if (transport == CORDIAL_CLASSIC) {
        gap_connectable_control(enabled);
        if (!enabled && inquiry) { gap_inquiry_stop(); inquiry = false; }
        if (!enabled) scan_classic = false;
    }
#endif
    transport_enabled[transport] = enabled;
    if (enabled) return CORDIAL_OK;
    // Accept-list initiation pauses in resume_radio; its list stays for later.
    if (transport == CORDIAL_BLE && scan_ble) { gap_stop_scan(); scan_ble = false; }
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
        if (incoming[i].attempt && incoming[i].peer.transport == transport) reject_incoming(i);
    // A Classic page the host already started ends with its controller timeout.
    for (unsigned i = 0; i < CORDIAL_LINKS; ++i) {
        cordial_connection *l = &cordial_connections[i];
        if (l->used && !l->closing && l->peer.transport == transport) cordial_profiles_disconnect(l->id);
    }
    return CORDIAL_OK;
}
int cordial_profiles_can_write(cordial_link id) {
    cordial_connection *l = cordial_by_id(id);
    // One ATT request per connection: wait for setup, verification queries
    // and information reads. Classic hosts refuse requests during SDP.
    return l && l->ready && !l->closing && !l->writing && !l->reading && !l->info.pending && !l->query
        && l->setup != SETUP_SUBSCRIBE && l->setup != SETUP_SDP;
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
    // A serialized session gives a Classic page the controller exclusively.
    bool exclusive=serialize_initiation && initiating[CORDIAL_CLASSIC];
    bool pause=!ready || !transport_enabled[CORDIAL_BLE] || (scan_ble && !scan_and_connect) || initiating[CORDIAL_BLE] || exclusive || auto_dirty || !reconnect_count || auto_consumed;
    if(ready)begin_pending(CORDIAL_CLASSIC);
    if(auto_active) {
        if(ready && scan_ble && scan_and_connect)gap_start_scan();
        // Wait for Create Connection status before cancelling. A request still
        // queued in BTstack can otherwise be cancelled without a GAP event.
        if(pause && auto_started && !auto_cancel) {
            auto_cancel=true;auto_cancel_deadline=now_ms+AUTO_CANCEL_TIMEOUT_MS;(void)gap_connect_cancel();
        }
        return;
    }
    if(!ready || incoming_pending(CORDIAL_BLE))return;
    if(initiating[CORDIAL_BLE]) {
        if(scan_ble && scan_and_connect)gap_start_scan();
        begin_pending(CORDIAL_BLE);
        return;
    }
    if(scan_ble) {
        gap_start_scan();
        if(!scan_and_connect)return;
    }
    if(!transport_enabled[CORDIAL_BLE] || exclusive || unacknowledged(CORDIAL_CLASSIC) || !reconnect_count || auto_consumed)return;
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
#ifdef ENABLE_CLASSIC
    // Wait until the controller runs the page; drop the request once no
    // closing page remains.
    if (page_cancel && page_live && hci_can_send_command_packet_now()) {
        page_cancel = false;
        (void)hci_send_cmd(&hci_create_connection_cancel, page_address);
    } else if (page_cancel && !page_live &&
               !(page_outstanding() && initiating[CORDIAL_CLASSIC]->closing)) {
        page_cancel = false;
    }
#endif
    if (le_resync && hci_can_send_command_packet_now()) {
        le_resync = false;
        bool direct = initiating[CORDIAL_BLE] && initiating_started[CORDIAL_BLE];
        // An accept-list request the host had not sent yet is gone: start it again later.
        if (auto_active && !auto_started) auto_active = auto_cancel = false;
        else if (auto_active || direct) {
            le_requeue = direct; resync_sent = true;
            if (auto_active) { auto_cancel = true; auto_cancel_deadline = now_ms + AUTO_CANCEL_TIMEOUT_MS; }
            (void)hci_send_cmd(&hci_le_create_connection_cancel);
        }
    }
    if (auto_active && auto_cancel && now >= auto_cancel_deadline) {
        auto_active = auto_started = auto_cancel = false;
        restart();
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
    if (ready && scan_classic && transport_enabled[CORDIAL_CLASSIC] && !inquiry && !initiating[CORDIAL_CLASSIC] && !initiating[CORDIAL_BLE]) inquiry = gap_inquiry_start(4) == ERROR_CODE_SUCCESS;
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
        if (l->closing && !l->abandoned && l->close_deadline && now >= l->close_deadline) {
            // The host still owns this link's state: an LE initiator stuck
            // cancelling, an SDP client waiting on the page, or a connection
            // with GATT requests and listeners that point into this slot.
            // Only its power cycle retires that state; it discards remaining
            // connections with a disconnection event before the host is off.
            l->abandoned = true;
            restart();
            continue;
        }
        cordial_info_poll(l, now);
        if (l->security_pending && !l->closing) (void)publish_security(l);
        if (ready && l->peer.transport == CORDIAL_BLE && l->authenticated) cordial_begin_profile(l);
        cordial_gatt_poll(l);
#ifdef ENABLE_CLASSIC
        classic_verify(l);
#endif
        early_flush(l);
    }
    // Report the triggering link's own failure before invalidating other links.
    if (restart_notice && cordial_emit_event(NULL, (cordial_event){ .kind = CORDIAL_RESTARTING, .code = CORDIAL_RADIO }))
        restart_notice = false;
    if (stopping) {
        if(auto_active)return;
        for(unsigned i=0;i<CORDIAL_LINKS;i++)if(incoming[i].attempt || incoming[i].closing)return;
        for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
            if (cordial_connections[i].used && !cordial_connections[i].ended && !cordial_connections[i].abandoned) return;
        stopping = false;
        (void)hci_power_control(HCI_POWER_OFF);
    }
    // The power cycle retired abandoned links; any still open had no
    // connection to discard.
    if (hci_get_state() == HCI_STATE_OFF)
        for (unsigned i = 0; i < CORDIAL_LINKS; ++i)
            if (cordial_connections[i].used && cordial_connections[i].abandoned) finish(&cordial_connections[i]);
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
    scan_id = now_ms = 0;
    transport_enabled[CORDIAL_CLASSIC] = false; transport_enabled[CORDIAL_BLE] = true;
    memset(initiating,0,sizeof initiating);memset(initiating_started,0,sizeof initiating_started);
    serialize_initiation=page_acknowledged=le_acknowledged=le_resync=le_requeue=resync_sent=page_cancel=page_live=false;
    memset(early,0,sizeof early);
    memset(incoming,0,sizeof incoming);next_attempt=0;reconnect_count=0;
    auto_active=auto_started=auto_cancel=auto_dirty=auto_consumed=false;
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
    // Registering the HID services makes the adapter connectable.
    gap_connectable_control(0);
#endif
    cordial_gatt_init();
    gap_set_scan_parameters(1, 0x60, 0x30);
    // HID links: 7.5 ms interval, no peripheral latency, 2.56 s supervision.
    gap_set_connection_parameters(0x0010, 0x0010, 6, 6, 0, 0x0100, 0, 0);
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
