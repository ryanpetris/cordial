#include "ble.h"
#include "esp_random.h"
#include "nimble/nimble_port.h"
#include "nimble/nimble_port_freertos.h"
#include "host/ble_hs.h"
#include "host/ble_gap.h"
#include "host/ble_hs_pvcy.h"
#include "host/ble_esp_gap.h"
#include "host/ble_gatt.h"
#include "host/ble_sm.h"
#include "host/ble_store.h"
#include "services/gap/ble_svc_gap.h"
#include "bonds.h"
#include "host/util/util.h"
#include "store/config/ble_store_config.h"
#include "os/os_mbuf.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include <string.h>

void ble_store_config_init(void);
_Static_assert(MYNEWT_VAL(BLE_GATTS),"HID peers require ATT server replies");
_Static_assert(MYNEWT_VAL(BLE_STORE_MAX_BONDS)==8,"portable bond table capacity");
_Static_assert(!MYNEWT_VAL(BLE_STATIC_TO_DYNAMIC),"custom IRK requires static NimBLE privacy state");
typedef struct {
    uint32_t token;
    uint32_t read_request;
    cordial_ble_peer peer;
    bool started;
    uint16_t connection, mtu, read_length;
    uint8_t prompt, error;
    bool pairing, closing, secure, mtu_done, reported;
} link;
static link links[4];
static cordial_ble_emit emit;
static QueueHandle_t commands;
static struct ble_npl_event command_event;
static uint64_t scan_id;
static bool is_synced, scan_unresolved;
static ble_addr_t reconnect_peers[8];
static unsigned reconnect_count;
static bool auto_active, auto_cancel, auto_dirty, auto_consumed;
static struct { uint32_t attempt; uint16_t handle; } incoming={.handle=BLE_HS_CONN_HANDLE_NONE};
static uint32_t next_attempt, auto_attempt;
static struct ble_npl_callout incoming_timeout;
static void resume_radio(void);
static void schedule_radio(void) { ble_npl_eventq_put(nimble_port_get_dflt_eventq(), &command_event); }
static void reject_incoming(void) {
    if(incoming.handle==BLE_HS_CONN_HANDLE_NONE)return;
    int status=ble_gap_terminate(incoming.handle,BLE_ERR_REM_USER_CONN_TERM);
    incoming.attempt=0;
    if(status==BLE_HS_ENOTCONN) {
        incoming.handle=BLE_HS_CONN_HANDLE_NONE;auto_consumed=false;schedule_radio();
    } else {
        // Retry teardown if its completion was lost or the command failed.
        ble_npl_callout_reset(&incoming_timeout,ble_npl_time_ms_to_ticks32(5000));
    }
}
static void incoming_expired(struct ble_npl_event *event) {
    (void)event;reject_incoming();
}

// Start HID links at 7.5 ms (six 1.25 ms units). The default 30-50 ms
// range matches the observed 20 Hz mouse reports. Normal peer
// parameter-update requests remain handled by NimBLE.
static const struct ble_gap_conn_params hid_connection = {
    .scan_itvl=0x0010,.scan_window=0x0010,
    .itvl_min=6,.itvl_max=6,.latency=BLE_GAP_INITIAL_CONN_LATENCY,
    .supervision_timeout=BLE_GAP_INITIAL_SUPERVISION_TIMEOUT,
    .min_ce_len=BLE_GAP_INITIAL_CONN_MIN_CE_LEN,
    .max_ce_len=BLE_GAP_INITIAL_CONN_MAX_CE_LEN,
};
static int gap(struct ble_gap_event *,void *);
static int auto_gap(struct ble_gap_event *,void *);
// Keep on-air scan addresses for fresh pairing. Resolve identities through
// the application-owned identity keys without changing saved records.
static int stop_scan(void) {
    if (ble_gap_disc_active()) {
        int status=ble_gap_disc_cancel();
        if (status) return status;
    }
    if (scan_unresolved) {
        int status=ble_hs_pvcy_set_resolve_enabled(1);
        if (status) return status;
        scan_unresolved=false;
    }
    return 0;
}
// Call only after the target link is gone; the public API also terminates links.
static int unpair_view(const ble_addr_t *p) {
    int status=cordial_bonds_prepare_unpair(p);
    if(!status)status=ble_gap_unpair(p);
    if(status==BLE_HS_ENOENT)status=0;
    if(!status)cordial_bonds_clean(p);
    return status;
}
static int start_scan(void) {
    if (!scan_id || auto_active || ble_gap_disc_active()) return 0;
    for(unsigned i=0;i<4;i++)
        if(links[i].token && links[i].connection==BLE_HS_CONN_HANDLE_NONE) return 0;
    int status=ble_hs_pvcy_set_resolve_enabled(0);
    if(status) return status;
    scan_unresolved=true;
    struct ble_gap_disc_params params={.itvl=0x60,.window=0x30,.filter_duplicates=1};
    status=ble_gap_disc(BLE_OWN_ADDR_PUBLIC,BLE_HS_FOREVER,&params,gap,NULL);
    if(status) (void)stop_scan();
    return status;
}
static ble_addr_t scan_identity(ble_addr_t raw) {
    ble_addr_t identity=raw, resolved;
    if(raw.type==BLE_ADDR_RANDOM && (raw.val[5]&0xc0)==0x40 &&
       cordial_bonds_resolve_rpa(raw.val,resolved.val,&resolved.type) &&
       resolved.type<=BLE_ADDR_RANDOM && memcmp(resolved.val,BLE_ADDR_ANY->val,6)) identity=resolved;
    return identity;
}

static cordial_ble_peer peer(ble_addr_t address) {
    cordial_ble_peer value = { .random = address.type & 1 };
    for (unsigned i=0;i<6;i++) value.address[i]=address.val[5-i];
    return value;
}
static ble_addr_t address(cordial_ble_peer peer) {
    ble_addr_t value = { .type=peer.random ? BLE_ADDR_RANDOM : BLE_ADDR_PUBLIC };
    for (unsigned i=0;i<6;i++) value.val[i]=peer.address[5-i];
    return value;
}
static link *by_token(uint32_t token) {
    if(!token)return NULL;
    for (unsigned i=0;i<4;i++) if (links[i].token==token) return &links[i];
    return NULL;
}
static link *by_handle(uint16_t handle) {
    for (unsigned i=0;i<4;i++) if (links[i].token && links[i].connection==handle) return &links[i];
    return NULL;
}
// ATT failures distinguish an unavailable operation from a transport failure.
static uint8_t operation_error(int status) {
    if (!status) return CORDIAL_BLE_OK;
    if (status == BLE_HS_ENOTSUP || status == BLE_HS_ATT_ERR(BLE_ATT_ERR_READ_NOT_PERMITTED)
            || status == BLE_HS_ATT_ERR(BLE_ATT_ERR_WRITE_NOT_PERMITTED)
            || status == BLE_HS_ATT_ERR(BLE_ATT_ERR_REQ_NOT_SUPPORTED)
            || status == BLE_HS_ATT_ERR(BLE_ATT_ERR_ATTR_NOT_LONG)) return CORDIAL_BLE_UNSUPPORTED;
    if (status == BLE_HS_EMSGSIZE || status == BLE_HS_ATT_ERR(BLE_ATT_ERR_INVALID_ATTR_VALUE_LEN)) return CORDIAL_BLE_REPORT_SIZE;
    if (status == BLE_HS_ENOMEM) return CORDIAL_BLE_CAPACITY;
    if (status == BLE_HS_ETIMEOUT) return CORDIAL_BLE_TIMEOUT;
    return CORDIAL_BLE_CONNECTION;
}
static void complete(uint32_t request,int status) {
    cordial_ble_event e={.kind=CORDIAL_BLE_COMPLETE,.request=request,.code=operation_error(status)}; emit(&e);
}
static bool bonds(const ble_addr_t *absent) {
    ble_addr_t addresses[8]; int count=0;
    if (cordial_bonds_peers(addresses,&count,8) || count<0 || count>8) return false;
    cordial_ble_peer peers[8];
    for (int i=0;i<count;i++) {
        // ble_gap_unpair may log a native deletion failure yet return success.
        if(absent && addresses[i].type==absent->type && !memcmp(addresses[i].val,absent->val,6)) return false;
        peers[i]=peer(addresses[i]);
    }
    cordial_ble_event e={.kind=CORDIAL_BLE_BONDS,.data=(const uint8_t *)peers,.length=count*sizeof(peers[0])};emit(&e);return true;
}
static void fail(link *l,uint8_t code) {
    if (!l) return;
    cordial_ble_event e={.kind=CORDIAL_BLE_DISCONNECTED,.token=l->token,.code=code};
    if (l->connection!=BLE_HS_CONN_HANDLE_NONE) ble_gap_terminate(l->connection,BLE_ERR_REM_USER_CONN_TERM);
    else if(l->started) ble_gap_conn_cancel();
    // The physical disconnect callback retires an established link. Remember
    // cancellation until then so late security events cannot admit it.
    if (l->connection!=BLE_HS_CONN_HANDLE_NONE) { l->error=code;l->closing=true; return; }
    emit(&e); memset(l,0,sizeof(*l));
}
// GAP cancellation is asynchronous. Explicit requests wait here until the
// auto initiator has delivered its completion callback.
static void resume_radio(void) {
    if(!is_synced)return;
    link *pending=NULL;
    for(unsigned i=0;i<4;i++) if(links[i].token && links[i].connection==BLE_HS_CONN_HANDLE_NONE) {
        pending=&links[i];break;
    }
    if(auto_active) {
        if(!auto_cancel && (pending || scan_id || auto_dirty || !reconnect_count || auto_consumed)) {
            int status=ble_gap_conn_cancel();
            // ACL completion retires the initiator before NimBLE publishes
            // CONNECT after remote-version exchange. EALREADY still waits for
            // that callback, just like controller-side Command Disallowed.
            if(status && status!=BLE_HS_EALREADY && status!=BLE_HS_HCI_ERR(BLE_ERR_CMD_DISALLOWED))goto failed;
            auto_cancel=true;
        }
        return;
    }
    if(incoming.handle!=BLE_HS_CONN_HANDLE_NONE)return;
    if(pending) {
        if(pending->started)return;
        int status=stop_scan();
        ble_addr_t p=address(pending->peer);
        if(!status)status=ble_gap_connect(BLE_OWN_ADDR_PUBLIC,&p,30000,&hid_connection,gap,(void *)(uintptr_t)pending->token);
        if(status) { fail(pending,CORDIAL_BLE_CONNECTION);schedule_radio(); }
        else pending->started=true;
        return;
    }
    if(scan_id) { if(start_scan())goto failed;return; }
    if(!reconnect_count || auto_consumed)return;
    if(stop_scan())goto failed;
    if(ble_gap_wl_set(reconnect_peers,reconnect_count))goto failed;
    auto_dirty=false;
    if(auto_attempt==UINT32_MAX)goto failed;
    ++auto_attempt;
    if(ble_gap_connect(BLE_OWN_ADDR_PUBLIC,NULL,BLE_HS_FOREVER,&hid_connection,auto_gap,(void *)(uintptr_t)auto_attempt))goto failed;
    auto_active=true;
    return;
failed:;
    cordial_ble_event e={.kind=CORDIAL_BLE_FAILED,.code=CORDIAL_BLE_RADIO};emit(&e);
    scan_id=0;reconnect_count=0;
}
static void security_ready(link *l) {
    if (!l || l->closing || !l->secure || !l->mtu_done) return;
    struct ble_gap_conn_desc desc;
    if (ble_gap_conn_find(l->connection,&desc) || !bonds(NULL)) { fail(l,CORDIAL_BLE_STORAGE);return; }
    if (!l->reported) {
        cordial_ble_event connected={.kind=CORDIAL_BLE_CONNECTED,.token=l->token,.number=l->mtu-3};emit(&connected);
    }
    cordial_ble_event e={.kind=CORDIAL_BLE_SECURITY,.token=l->token,.peer=peer(desc.peer_id_addr),
        .bonded=desc.sec_state.bonded,.encrypted=desc.sec_state.encrypted,
        .authenticated=1+desc.sec_state.authenticated,.key_size=desc.sec_state.key_size};
    // NimBLE exposes SC on the bond, not ble_gap_sec_state. This central uses
    // the peer LTK; only report its metadata after matching the encrypted link.
    struct ble_store_key_sec key={.peer_addr=desc.peer_id_addr};
    struct ble_store_value_sec value;
    if (desc.sec_state.encrypted && desc.sec_state.bonded &&
        !ble_store_read_peer_sec(&key,&value) && value.ltk_present &&
        value.key_size==desc.sec_state.key_size && value.authenticated==desc.sec_state.authenticated)
        e.secure_connections=1+value.sc;
    l->reported=true;emit(&e);
}
static int mtu_callback(uint16_t conn,const struct ble_gatt_error *error,uint16_t mtu,void *arg) {
    (void)conn;link *l=by_token((uint32_t)(uintptr_t)arg);
    if (!l) return 0;
    l->mtu=error->status ? 23:mtu;l->mtu_done=true;security_ready(l);return 0;
}
static void connected(link *l) {
    if (l->closing) { ble_gap_terminate(l->connection,BLE_ERR_REM_USER_CONN_TERM);return; }
    if (ble_gattc_exchange_mtu(l->connection,mtu_callback,(void *)(uintptr_t)l->token)) { l->mtu=23;l->mtu_done=true; }
    struct ble_gap_conn_desc desc;
    if(!ble_gap_conn_find(l->connection,&desc) && desc.sec_state.encrypted && desc.sec_state.bonded) {
        l->secure=true;security_ready(l);return;
    }
    int status=ble_gap_security_initiate(l->connection);
    if(status && status!=BLE_HS_EALREADY)fail(l,CORDIAL_BLE_AUTH);
}
static int auto_gap(struct ble_gap_event *event,void *arg) {
    uint32_t attempt=(uint32_t)(uintptr_t)arg;
    if(event->type==BLE_GAP_EVENT_CONNECT && (attempt!=auto_attempt || !auto_active)) {
        if(!event->connect.status)ble_gap_terminate(event->connect.conn_handle,BLE_ERR_REM_USER_CONN_TERM);
        return 0;
    }
    // The ACL can disconnect during remote-version exchange, before NimBLE
    // reports CONNECT. Older accepted auto links retain their own callback ID.
    if(event->type==BLE_GAP_EVENT_DISCONNECT && attempt==auto_attempt && auto_active) {
        auto_active=auto_cancel=false;schedule_radio();
    }
    return gap(event,NULL);
}
static int gap(struct ble_gap_event *event,void *arg) {
    link *l=by_token((uint32_t)(uintptr_t)arg);
    switch (event->type) {
    case BLE_GAP_EVENT_DISC: {
        struct ble_hs_adv_fields fields;
        if (!scan_id || !scan_unresolved || event->disc.addr.type>BLE_ADDR_RANDOM ||
            ble_hs_adv_parse_fields(&fields,event->disc.data,event->disc.length_data)) return 0;
        cordial_ble_event e={.kind=CORDIAL_BLE_FOUND,.scan=scan_id,.peer=peer(scan_identity(event->disc.addr)),.address=peer(event->disc.addr),.rssi=event->disc.rssi,
            .code=event->disc.event_type==BLE_HCI_ADV_RPT_EVTYPE_ADV_IND || event->disc.event_type==BLE_HCI_ADV_RPT_EVTYPE_DIR_IND,
            .number=fields.appearance_is_present ? fields.appearance:0,
            .data=fields.name,.length=fields.name_len};emit(&e);return 0;
    }
    case BLE_GAP_EVENT_DISC_COMPLETE:
        // Resolving-list updates stop discovery; NimBLE has released preemption
        // before this callback. Resume only if scanning is still requested.
        if(event->disc_complete.reason==BLE_HS_EPREEMPTED && start_scan()) {
            cordial_ble_event e={.kind=CORDIAL_BLE_FAILED,.code=CORDIAL_BLE_RADIO};emit(&e);
        }
        schedule_radio();return 0;
    case BLE_GAP_EVENT_CONNECT:
        if(!arg) {
            bool expected=auto_active;
            auto_active=auto_cancel=false;
            if(!event->connect.status) {
                struct ble_gap_conn_desc desc;
                if(!expected || incoming.attempt || next_attempt==UINT32_MAX ||
                   ble_gap_conn_find(event->connect.conn_handle,&desc)) {
                    ble_gap_terminate(event->connect.conn_handle,BLE_ERR_REM_USER_CONN_TERM);
                } else {
                    incoming.attempt=++next_attempt;incoming.handle=event->connect.conn_handle;
                    auto_consumed=true;
                    ble_npl_callout_reset(&incoming_timeout,ble_npl_time_ms_to_ticks32(5000));
                    cordial_ble_event e={.kind=CORDIAL_BLE_INCOMING,.number=incoming.attempt,.peer=peer(desc.peer_id_addr)};emit(&e);
                }
            }
            // A completed initiation failure releases NimBLE's master state.
            // Keep waiting for eligible peers; host resets have reset_cb.
            schedule_radio();return 0;
        }
        if (!l) { if(!event->connect.status) ble_gap_terminate(event->connect.conn_handle,BLE_ERR_REM_USER_CONN_TERM);schedule_radio();return 0; }
        if (event->connect.status) { l->started=false;fail(l,l->closing ? l->error:CORDIAL_BLE_CONNECTION);schedule_radio();return 0; }
        l->connection=event->connect.conn_handle;
        connected(l);schedule_radio();return 0;
    case BLE_GAP_EVENT_DISCONNECT: {
        if(incoming.handle==event->disconnect.conn.conn_handle) {
            incoming.attempt=0;incoming.handle=BLE_HS_CONN_HANDLE_NONE;auto_consumed=false;ble_npl_callout_stop(&incoming_timeout);
        }
        schedule_radio();
        l=by_handle(event->disconnect.conn.conn_handle);if (!l) return 0;
        cordial_ble_event e={.kind=CORDIAL_BLE_DISCONNECTED,.token=l->token,
            .code=l->error ? l->error:(l->closing ? CORDIAL_BLE_OK:CORDIAL_BLE_CONNECTION)};
        if(!bonds(NULL)) e.code=CORDIAL_BLE_STORAGE;
        emit(&e);memset(l,0,sizeof(*l));return 0;
    }
    case BLE_GAP_EVENT_ENC_CHANGE: {
        l=by_handle(event->enc_change.conn_handle);if (!l || l->closing) return 0;
        struct ble_gap_conn_desc desc;
        if (event->enc_change.status) { fail(l,CORDIAL_BLE_AUTH);return 0; }
        int status=ble_gap_conn_find(l->connection,&desc);
        if (status || !desc.sec_state.encrypted || !desc.sec_state.bonded) { fail(l,CORDIAL_BLE_AUTH);return 0; }
        l->secure=true;security_ready(l);return 0;
    }
    case BLE_GAP_EVENT_PASSKEY_ACTION: {
        l=by_handle(event->passkey.conn_handle);if (!l || !l->pairing || l->closing) { if(l) fail(l,CORDIAL_BLE_AUTH);return 0; }
        cordial_ble_event e={.kind=CORDIAL_BLE_PROMPT,.token=l->token};
        l->prompt=event->passkey.params.action;
        if (l->prompt==BLE_SM_IOACT_DISP) {
            struct ble_sm_io io={.action=BLE_SM_IOACT_DISP,.passkey=esp_random()%1000000};
            int status=ble_sm_inject_io(l->connection,&io);
            if (status) { fail(l,CORDIAL_BLE_AUTH);return 0; }
            e.code=CORDIAL_BLE_DISPLAY;e.number=io.passkey;
        } else if (l->prompt==BLE_SM_IOACT_INPUT) e.code=CORDIAL_BLE_ENTER;
        else if (l->prompt==BLE_SM_IOACT_NUMCMP) { e.code=CORDIAL_BLE_CONFIRM;e.number=event->passkey.params.numcmp; }
        else { fail(l,CORDIAL_BLE_AUTH);return 0; }
        emit(&e);return 0;
    }
    case BLE_GAP_EVENT_REPEAT_PAIRING: {
        l=by_handle(event->repeat_pairing.conn_handle);
        if(!l || !l->pairing || l->closing)return BLE_GAP_REPEAT_PAIRING_IGNORE;
        struct ble_gap_conn_desc desc;
        if(ble_gap_conn_find(l->connection,&desc))return BLE_GAP_REPEAT_PAIRING_IGNORE;
        struct ble_store_key_sec key={.peer_addr=desc.peer_id_addr};
        struct ble_store_value_sec value;
        if(!ble_store_read_peer_sec(&key,&value) && value.irk_present && cordial_bonds_mark_dirty(&desc.peer_id_addr))return BLE_GAP_REPEAT_PAIRING_IGNORE;
        if(ble_store_util_delete_peer(&desc.peer_id_addr))return BLE_GAP_REPEAT_PAIRING_IGNORE;
        return BLE_GAP_REPEAT_PAIRING_RETRY;
    }
    case BLE_GAP_EVENT_NOTIFY_RX: {
        l=by_handle(event->notify_rx.conn_handle);if (!l || !l->secure || l->closing) return 0;
        uint8_t data[512];uint16_t length=OS_MBUF_PKTLEN(event->notify_rx.om);
        if (length>sizeof(data) || os_mbuf_copydata(event->notify_rx.om,0,length,data)) { fail(l,CORDIAL_BLE_OVERFLOW);return 0; }
        cordial_ble_event e={.kind=CORDIAL_BLE_NOTIFICATION,.token=l->token,.handle=event->notify_rx.attr_handle,.data=data,.length=length};emit(&e);return 0;
    }
    default:return 0;
    }
}
static int service_callback(uint16_t conn,const struct ble_gatt_error *error,const struct ble_gatt_svc *service,void *arg) {
    (void)conn;uint32_t request=(uint32_t)(uintptr_t)arg;
    if (error->status) { complete(request,error->status==BLE_HS_EDONE ? 0:error->status);return 0; }
    cordial_ble_event e={.kind=CORDIAL_BLE_SERVICE,.request=request,.start=service->start_handle,.end=service->end_handle};emit(&e);return 0;
}
static int characteristic_callback(uint16_t conn,const struct ble_gatt_error *error,const struct ble_gatt_chr *c,void *arg) {
    (void)conn;uint32_t request=(uint32_t)(uintptr_t)arg;
    if (error->status) { complete(request,error->status==BLE_HS_EDONE ? 0:error->status);return 0; }
    cordial_ble_event e={.kind=CORDIAL_BLE_CHARACTERISTIC,.request=request,.start=c->def_handle,.handle=c->val_handle,.properties=c->properties,.uuid=ble_uuid_u16(&c->uuid.u)};emit(&e);return 0;
}
static int descriptor_callback(uint16_t conn,const struct ble_gatt_error *error,uint16_t characteristic,const struct ble_gatt_dsc *d,void *arg) {
    (void)conn;(void)characteristic;uint32_t request=(uint32_t)(uintptr_t)arg;
    if (error->status) { complete(request,error->status==BLE_HS_EDONE ? 0:error->status);return 0; }
    cordial_ble_event e={.kind=CORDIAL_BLE_DESCRIPTOR,.request=request,.handle=d->handle,.uuid=ble_uuid_u16(&d->uuid.u)};emit(&e);return 0;
}
static int read_callback(uint16_t conn,const struct ble_gatt_error *error,struct ble_gatt_attr *attr,void *arg) {
    uint32_t request=(uint32_t)(uintptr_t)arg;
    link *l=by_handle(conn);
    if (!l || l->read_request!=request) return 0;
    if (error->status) {
        int status=error->status;
        // A fixed-length value can reject the trailing Read Blob after a full
        // Read response. The owner checks the report's declared length.
        if (status==BLE_HS_EDONE || (status==BLE_HS_ATT_ERR(BLE_ATT_ERR_ATTR_NOT_LONG) && l->read_length)) status=0;
        l->read_request=0;complete(request,status);return 0;
    }
    uint8_t data[512];uint16_t length=OS_MBUF_PKTLEN(attr->om);
    if (length>sizeof(data) || os_mbuf_copydata(attr->om,0,length,data)) { l->read_request=0;complete(request,BLE_HS_EMSGSIZE);return BLE_HS_EMSGSIZE; }
    // Report Map reads can be longer than individual HID reports. Owners
    // validate the assembled descriptor, report or information value.
    if (attr->offset!=l->read_length || length>2048-l->read_length) {
        l->read_request=0;complete(request,BLE_HS_EMSGSIZE);return BLE_HS_EMSGSIZE;
    }
    l->read_length+=length;
    cordial_ble_event e={.kind=CORDIAL_BLE_DATA,.request=request,.offset=attr->offset,.length=length,.data=data};emit(&e);return 0;
}
static int write_callback(uint16_t conn,const struct ble_gatt_error *error,struct ble_gatt_attr *attr,void *arg) {
    (void)conn;(void)attr;complete((uint32_t)(uintptr_t)arg,error->status);return 0;
}
static void command(const cordial_ble_command *c) {
    link *l=by_token(c->token);int status=0;
    if(!is_synced && c->kind!=CORDIAL_BLE_FORGET) {
        if(c->kind==CORDIAL_BLE_CONNECT) {
            cordial_ble_event e={.kind=CORDIAL_BLE_DISCONNECTED,.token=c->token,.code=CORDIAL_BLE_RADIO};emit(&e);
        } else if(c->request) complete(c->request,BLE_HS_ENOTSYNCED);
        return;
    }
    if(c->kind==CORDIAL_BLE_RECONNECT) {
        if(c->length>sizeof(cordial_ble_peer)*8 || c->length%sizeof(cordial_ble_peer)) {
            cordial_ble_event e={.kind=CORDIAL_BLE_FAILED,.code=CORDIAL_BLE_CAPACITY};emit(&e);return;
        }
        reconnect_count=c->length/sizeof(cordial_ble_peer);
        for(unsigned i=0;i<reconnect_count;i++) {
            cordial_ble_peer p;memcpy(&p,c->data+i*sizeof(p),sizeof(p));reconnect_peers[i]=address(p);
        }
        auto_dirty=true;auto_consumed=false;return;
    }
    if(c->kind==CORDIAL_BLE_ACCEPT) {
        bool valid=incoming.attempt && incoming.attempt==c->number;
        if(!valid) {
            if(c->token) { cordial_ble_event e={.kind=CORDIAL_BLE_DISCONNECTED,.token=c->token,.code=CORDIAL_BLE_CONNECTION};emit(&e); }
            return;
        }
        uint16_t handle=incoming.handle;
        incoming.attempt=0;ble_npl_callout_stop(&incoming_timeout);
        if(!c->token) { reject_incoming();return; }
        for(unsigned i=0;i<4;i++)if(!links[i].token){l=&links[i];break;}
        if(!l) {
            reject_incoming();
            cordial_ble_event e={.kind=CORDIAL_BLE_DISCONNECTED,.token=c->token,.code=CORDIAL_BLE_CAPACITY};emit(&e);return;
        }
        incoming.handle=BLE_HS_CONN_HANDLE_NONE;
        *l=(link){.token=c->token,.connection=handle,.mtu=23};connected(l);return;
    }
    if (c->kind==CORDIAL_BLE_SCAN) {
        scan_id=c->enabled ? c->scan:0;
        status=stop_scan();
        if(status) { cordial_ble_event e={.kind=CORDIAL_BLE_FAILED,.code=CORDIAL_BLE_RADIO};emit(&e); }
        return;
    }
    if(c->kind==CORDIAL_BLE_IMPORT || c->kind==CORDIAL_BLE_EXPORT) {
        uint8_t data[145];ble_addr_t p=address(c->peer);
        bool change=c->kind==CORDIAL_BLE_IMPORT && !cordial_bonds_same_privacy(c->data,c->length);
        if(c->kind==CORDIAL_BLE_IMPORT) {
            if(c->length!=145 || c->data[8]!=2 || c->data[9]>1)status=BLE_HS_EINVAL;
            else {
                p.type=c->data[9];for(unsigned i=0;i<6;i++)p.val[i]=c->data[15-i];
                change=change || cordial_bonds_dirty(&p);
                struct ble_gap_conn_desc live;
                if(change && !ble_gap_conn_find_by_addr(&p,&live)) {
                    // The established link keeps its negotiated session keys.
                    // Update the authoritative host view now; defer controller
                    // privacy replacement until Disconnected -> sync.
                    status=cordial_bonds_mark_dirty(&p);
                    change=false;
                }
                if(change) {
                    status=stop_scan();
                    if(!status) {status=unpair_view(&p);}
                }
                if(!status)status=cordial_bonds_import(c->data,c->length);
                if(!status && change) {
                    union ble_store_key key={0};union ble_store_value value;key.sec.peer_addr=p;
                    int read=cordial_bonds_read(BLE_STORE_OBJ_TYPE_PEER_SEC,&key,&value);
                    if(!read)status=ble_store_write_peer_sec(&value.sec);
                    else if(read!=BLE_HS_ENOENT)status=read;
                }
            }
        } else status=cordial_bonds_export(&p,data);
        // A failed resolving-list update must not look installed on the next import.
        if(status && change) (void)unpair_view(&p);
        if(!status && !bonds(NULL))status=BLE_HS_ESTORE_FAIL;
        if(change && start_scan())status=BLE_HS_ESTORE_FAIL;
        cordial_ble_event e={.kind=CORDIAL_BLE_BOND_RESULT,.request=c->request,.code=status?CORDIAL_BLE_STORAGE:CORDIAL_BLE_OK,
            .data=data,.length=(!status && c->kind==CORDIAL_BLE_EXPORT)?145:0};emit(&e);return;
    }
    if (c->kind==CORDIAL_BLE_FORGET) {
        ble_addr_t p=address(c->peer);
        status=stop_scan();
        if(!status) { status=unpair_view(&p); }
        bool stored=!status && bonds(&p);
        int scanning=start_scan();
        cordial_ble_event e={.kind=CORDIAL_BLE_FORGOTTEN,.request=c->request,.code=stored ? CORDIAL_BLE_OK:CORDIAL_BLE_STORAGE};emit(&e);
        if(scanning) { cordial_ble_event failed={.kind=CORDIAL_BLE_FAILED,.code=CORDIAL_BLE_RADIO};emit(&failed); }
        return;
    }
    if (c->kind==CORDIAL_BLE_CONNECT) {
        for (unsigned i=0;i<4;i++) if (!links[i].token) { l=&links[i];break; }
        if (!l) { cordial_ble_event e={.kind=CORDIAL_BLE_DISCONNECTED,.token=c->token,.code=CORDIAL_BLE_CAPACITY};emit(&e);return; }
        *l=(link){.token=c->token,.connection=BLE_HS_CONN_HANDLE_NONE,.pairing=c->pairing,.mtu=23};
        l->peer=c->peer;
        return;
    }
    if (!l || l->closing) { if(c->request) complete(c->request,BLE_HS_ENOTCONN);return; }
    if (c->kind==CORDIAL_BLE_ADOPT) { l->pairing=false;return; }
    if (c->kind==CORDIAL_BLE_DISCONNECT) {
        l->closing=true;
        if (l->connection==BLE_HS_CONN_HANDLE_NONE) { if (!l->started || ble_gap_conn_cancel()) fail(l,CORDIAL_BLE_OK); }
        else ble_gap_terminate(l->connection,BLE_ERR_REM_USER_CONN_TERM);
        return;
    }
    if (c->kind==CORDIAL_BLE_REPLY) {
        if (!c->accept || !l->pairing) { fail(l,CORDIAL_BLE_AUTH);return; }
        struct ble_sm_io io={.action=l->prompt};
        if (l->prompt==BLE_SM_IOACT_INPUT && c->method==CORDIAL_BLE_ENTER) io.passkey=c->number;
        else if (l->prompt==BLE_SM_IOACT_NUMCMP && c->method==CORDIAL_BLE_CONFIRM) io.numcmp_accept=1;
        else if (l->prompt==BLE_SM_IOACT_DISP && c->method==CORDIAL_BLE_DISPLAY) return;
        else { fail(l,CORDIAL_BLE_AUTH);return; }
        status=ble_sm_inject_io(l->connection,&io);
        if (status) fail(l,CORDIAL_BLE_AUTH);
        return;
    }
    void *arg=(void *)(uintptr_t)c->request;
    switch(c->kind) {
    case CORDIAL_BLE_SERVICES: { ble_uuid16_t uuid=BLE_UUID16_INIT(c->number);status=ble_gattc_disc_svc_by_uuid(l->connection,&uuid.u,service_callback,arg);break; }
    case CORDIAL_BLE_CHARACTERISTICS:status=ble_gattc_disc_all_chrs(l->connection,c->start,c->end,characteristic_callback,arg);break;
    case CORDIAL_BLE_DESCRIPTORS:status=ble_gattc_disc_all_dscs(l->connection,c->start-1,c->end,descriptor_callback,arg);break;
    case CORDIAL_BLE_READ:
        l->read_request=c->request;l->read_length=0;
        status=ble_gattc_read_long(l->connection,c->handle,0,read_callback,arg);
        if(status) l->read_request=0;
        break;
    case CORDIAL_BLE_SUBSCRIBE: {
        uint8_t value[2]={c->enabled ? 2:1,0};
        status=ble_gattc_write_flat(l->connection,c->start,value,2,write_callback,arg);break;
    }
    case CORDIAL_BLE_WRITE:
        if(c->response && c->length > l->mtu - 3) {
            struct os_mbuf *value=ble_hs_mbuf_from_flat(c->data,c->length);
            if(!value) status=BLE_HS_ENOMEM;
            else status=ble_gattc_write_long(l->connection,c->handle,0,value,write_callback,arg);
        }
        else if(c->response) status=ble_gattc_write_flat(l->connection,c->handle,c->data,c->length,write_callback,arg);
        else { status=ble_gattc_write_no_rsp_flat(l->connection,c->handle,c->data,c->length);complete(c->request,status);return; }
        break;
    default:status=BLE_HS_ENOTSUP;
    }
    if(status) complete(c->request,status);
}
static void process_commands(struct ble_npl_event *event) {
    (void)event;cordial_ble_command c;
    while (xQueueReceive(commands,&c,0)==pdTRUE) command(&c);
    resume_radio();
}
static void synced(void) {
    int status=ble_hs_util_ensure_addr(0);
    cordial_ble_event e={.kind=CORDIAL_BLE_READY};
    if(status || !bonds(NULL)) { e.kind=CORDIAL_BLE_FAILED;e.code=CORDIAL_BLE_STORAGE; }
    is_synced=e.kind==CORDIAL_BLE_READY;
    emit(&e);
}
static void reset(int reason) {
    (void)reason;is_synced=false;scan_id=0;scan_unresolved=false;
    reconnect_count=0;auto_active=auto_cancel=auto_dirty=auto_consumed=false;incoming.attempt=0;incoming.handle=BLE_HS_CONN_HANDLE_NONE;
    ble_npl_callout_stop(&incoming_timeout);
    cordial_bonds_clear();
    bool stored=bonds(NULL);
    // NimBLE reports established disconnects before reset_cb. Retire any
    // remaining initiation as well, before the shared Restarting event.
    for(unsigned i=0;i<4;i++) if(links[i].token) {
        cordial_ble_event e={.kind=CORDIAL_BLE_DISCONNECTED,.token=links[i].token,
            .code=stored ? CORDIAL_BLE_RADIO:CORDIAL_BLE_STORAGE};emit(&e);memset(&links[i],0,sizeof(links[i]));
    }
    cordial_ble_event e={.kind=stored ? CORDIAL_BLE_RESTARTING:CORDIAL_BLE_FAILED,.code=stored ? CORDIAL_BLE_RADIO:CORDIAL_BLE_STORAGE};emit(&e);
}
static uint8_t local_irk[16];
void cordial_ble_identity(const uint8_t *irk) { for(unsigned i=0;i<16;i++)local_irk[i]=irk[15-i]; }
static int generate_key(uint8_t kind,struct ble_store_gen_key *key,uint16_t handle) {
    (void)handle;
    if(kind!=BLE_STORE_GEN_KEY_IRK)return BLE_HS_ENOTSUP;
    memcpy(key->irk,local_irk,16);return 0;
}
static int store_status(struct ble_store_status_event *event,void *arg) { (void)event;(void)arg;return BLE_HS_ENOMEM; }
static void run(void *arg) { (void)arg;nimble_port_run();nimble_port_freertos_deinit(); }
int cordial_ble_start(cordial_ble_emit callback) {
    emit=callback;
    commands=xQueueCreate(8,sizeof(cordial_ble_command));if(!commands) return CORDIAL_BLE_CAPACITY;
    if(nimble_port_init()!=0) return CORDIAL_BLE_RADIO;
    ble_svc_gap_init();
    if(ble_svc_gap_device_name_set("Cordial")) return CORDIAL_BLE_RADIO;
    ble_npl_event_init(&command_event,process_commands,NULL);
    ble_npl_callout_init(&incoming_timeout,nimble_port_get_dflt_eventq(),incoming_expired,NULL);
    ble_hs_cfg.sync_cb=synced;ble_hs_cfg.reset_cb=reset;ble_hs_cfg.store_status_cb=store_status;
    ble_hs_cfg.sm_io_cap=BLE_HS_IO_KEYBOARD_DISPLAY;ble_hs_cfg.sm_bonding=1;ble_hs_cfg.sm_sc=1;
    ble_hs_cfg.sm_our_key_dist=BLE_SM_PAIR_KEY_DIST_ENC | BLE_SM_PAIR_KEY_DIST_ID;
    ble_hs_cfg.sm_their_key_dist=BLE_SM_PAIR_KEY_DIST_ENC | BLE_SM_PAIR_KEY_DIST_ID;
    ble_store_config_init();
    ble_hs_cfg.store_gen_key_cb=generate_key;
    ble_hs_cfg.store_read_cb=cordial_bonds_read;
    ble_hs_cfg.store_write_cb=cordial_bonds_write;
    ble_hs_cfg.store_delete_cb=cordial_bonds_delete;
    ble_att_set_preferred_mtu(517);
    nimble_port_freertos_init(run);return CORDIAL_BLE_OK;
}
int cordial_ble_submit(const cordial_ble_command *c) {
    if (!commands || !c || xQueueSend(commands,c,0)!=pdTRUE) return CORDIAL_BLE_CAPACITY;
    ble_npl_eventq_put(nimble_port_get_dflt_eventq(),&command_event);return CORDIAL_BLE_OK;
}

unsigned cordial_ble_bond_capacity(void) { return MYNEWT_VAL(BLE_STORE_MAX_BONDS); }
