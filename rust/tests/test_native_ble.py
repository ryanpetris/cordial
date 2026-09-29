"""Run the binding's cache walks against the pinned SDK's public API semantics."""
from pathlib import Path
import subprocess
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]


def run_c(source):
    with tempfile.TemporaryDirectory() as directory:
        cfile, binary = Path(directory, "test.c"), Path(directory, "test")
        cfile.write_text(source)
        subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror",
                        "-I", str(ROOT / "platforms/esp32s3/components/platform"),
                        str(cfile), "-o", str(binary)], check=True)
        subprocess.run([str(binary)], check=True)


class CacheWalkTests(unittest.TestCase):
    def test_preempted_discovery_resumes_without_restarting_cancelled_scans(self):
        native = (ROOT / "platforms/esp32s3/components/platform/nimble.c").read_text()
        stop = native[native.index("static int stop_scan(void)"):native.index("// Call only after")]
        start = native[native.index("static int start_scan(void)"):native.index("static ble_addr_t scan_identity")]
        completed = native[native.index("    case BLE_GAP_EVENT_DISC_COMPLETE:"):native.index("    case BLE_GAP_EVENT_CONNECT:")]
        run_c(r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
#include "ble.h"
#define BLE_OWN_ADDR_PUBLIC 0
#define BLE_HS_FOREVER (-1)
#define BLE_HS_CONN_HANDLE_NONE 65535
#define BLE_GAP_EVENT_DISC_COMPLETE 1
#define BLE_HS_EPREEMPTED 2
static uint64_t scan_id;
static bool scan_unresolved, active, preempted, auto_active;
static void schedule_radio(void) {}
static int starts, failures, resolution=1, start_error;
static struct {uint32_t token;uint16_t connection;} links[4];
struct ble_gap_disc_params {int itvl,window,filter_duplicates;};
struct ble_gap_event {int type;struct {int reason;} disc_complete;};
static int gap(struct ble_gap_event *,void *);
static int ble_gap_disc_active(void) {return active;}
static int ble_gap_disc_cancel(void) {active=false;return 0;}
static int ble_hs_pvcy_set_resolve_enabled(int enabled) {resolution=enabled;return 0;}
static int ble_gap_disc(int own,int duration,const struct ble_gap_disc_params *p,
                       int (*cb)(struct ble_gap_event *,void *),void *arg) {
    assert(!preempted && !resolution && own==BLE_OWN_ADDR_PUBLIC);
    assert(duration==BLE_HS_FOREVER && p->itvl && cb==gap && !arg);
    starts++;active=!start_error;return start_error;
}
static void emit(const cordial_ble_event *e) {assert(e->kind==CORDIAL_BLE_FAILED && e->code==CORDIAL_BLE_RADIO);failures++;}
''' + stop + start + r'''
static int gap(struct ble_gap_event *event,void *arg) {(void)arg;switch(event->type) {
''' + completed + r'''
}return 0;}
int main(void) {
    struct ble_gap_event event={.type=BLE_GAP_EVENT_DISC_COMPLETE,.disc_complete={BLE_HS_EPREEMPTED}};
    scan_id=9;assert(!start_scan() && active && starts==1);
    /* NimBLE clears preemption before DISC_COMPLETE. A key-list update stopped scanning. */
    preempted=true;active=false;preempted=false;
    assert(!gap(&event,0) && active && starts==2 && scan_unresolved);
    assert(!gap(&event,0) && starts==2); /* already active */
    active=false;scan_id=0;assert(!gap(&event,0) && starts==2); /* disabled */
    scan_id=10;event.disc_complete.reason=0;
    assert(!gap(&event,0) && starts==2); /* intentional cancel */
    event.disc_complete.reason=BLE_HS_EPREEMPTED;
    links[0].token=1;links[0].connection=BLE_HS_CONN_HANDLE_NONE;
    assert(!gap(&event,0) && starts==2); /* connection initiation owns the radio */
    links[0].connection=5;assert(!gap(&event,0) && starts==3 && active);
    active=false;start_error=7;
    assert(!gap(&event,0) && starts==4 && failures==1 && !scan_unresolved && resolution);
    return 0;
}
''')

    def test_accept_list_waits_for_any_peer_and_serializes_scan_and_manual_connect(self):
        source = (ROOT / "platforms/esp32s3/components/platform/nimble.c").read_text()
        resume = source[source.index("static void resume_radio(void) {"):source.index("static void auth_diagnostic(")]
        connect = source[source.index("        if(!arg) {", source.index("    case BLE_GAP_EVENT_CONNECT:")):source.index("        if (!l) { if(!event->connect.status)")]
        wrapper = source[source.index("static int auto_gap(struct ble_gap_event *event"):source.index("static int gap(struct ble_gap_event *event")]
        expired = source[source.index("static void reject_incoming(void)"):source.index("// Start HID links")]
        disconnected = source[source.index("        if(incoming.handle==event->disconnect"):source.index("        l=by_handle(event->disconnect")]
        run_c(r'''
#include <assert.h>
#include <stdbool.h>
#include <string.h>
#include "ble.h"
#define BLE_HS_CONN_HANDLE_NONE 65535
#define BLE_OWN_ADDR_PUBLIC 0
#define BLE_HS_FOREVER (-1)
#define BLE_HS_ENOTCONN 4
#define BLE_HS_EALREADY 3
#define BLE_HS_HCI_ERR(x) (0x200+(x))
#define BLE_ERR_CMD_DISALLOWED 12
#define BLE_HS_EAPP 1
#define BLE_HS_EPREEMPTED 2
#define BLE_ERR_REM_USER_CONN_TERM 0x13
typedef struct {uint8_t type,val[6];} ble_addr_t;
typedef struct {uint32_t token;uint16_t connection;cordial_ble_peer peer;bool started;} link;
static link links[4];
static bool is_synced=true,auto_active,auto_cancel,auto_dirty,auto_consumed;
static unsigned reconnect_count;
static ble_addr_t reconnect_peers[8];
static uint64_t scan_id;
static struct {uint32_t attempt;uint16_t handle;} incoming={.handle=BLE_HS_CONN_HANDLE_NONE};
static uint32_t next_attempt,auto_attempt;
static int incoming_timeout, hid_connection, scans, cancels, connects, programmed, failures, offers, terminated, cancel_error;
static bool manual, scanning, cancelling;
enum {BLE_GAP_EVENT_CONNECT=1,BLE_GAP_EVENT_DISCONNECT=2};
struct ble_gap_event {int type;struct {int status;uint16_t conn_handle;} connect;struct {struct {uint16_t conn_handle;} conn;} disconnect;};
struct ble_npl_event {int unused;};
struct ble_gap_conn_desc {ble_addr_t peer_id_addr;};
static int gap(struct ble_gap_event *,void *);
static int auto_gap(struct ble_gap_event *,void *);
static ble_addr_t address(cordial_ble_peer p) {ble_addr_t a={.type=p.random};memcpy(a.val,p.address,6);return a;}
static cordial_ble_peer peer(ble_addr_t p) {cordial_ble_peer a={.random=p.type};memcpy(a.address,p.val,6);return a;}
static int stop_scan(void){scanning=false;return 0;}
static int start_scan(void){assert(!auto_active);scans++;scanning=true;return 0;}
static int ble_gap_conn_cancel(void){assert(auto_active && !cancelling);cancelling=!cancel_error;cancels++;return cancel_error;}
static int ble_gap_wl_set(const ble_addr_t *peers,unsigned count){assert(count==2 && peers[0].val[0]==1 && peers[1].val[0]==2);programmed++;return 0;}
static int ble_gap_connect(int own,const ble_addr_t *p,int timeout,const int *params,int (*cb)(struct ble_gap_event *,void *),void *arg){
    assert(!scanning && !cancelling && own==0 && params==&hid_connection);
    if(p){assert(timeout==30000 && arg==(void *)(uintptr_t)7 && cb==gap);manual=true;}
    else {assert(timeout==BLE_HS_FOREVER && arg==(void *)(uintptr_t)auto_attempt && cb==auto_gap);manual=false;}
    connects++;return 0;
}
static void fail(link *l,uint8_t code){(void)l;(void)code;assert(false);}
static void schedule_radio(void){}
static int ble_gap_terminate(uint16_t handle,int why){assert(handle==42 && why==BLE_ERR_REM_USER_CONN_TERM);terminated++;return 0;}
static int ble_gap_conn_find(uint16_t handle,struct ble_gap_conn_desc *desc){assert(handle==42);desc->peer_id_addr=reconnect_peers[1];return 0;}
static unsigned ble_npl_time_ms_to_ticks32(unsigned ms){return ms;}
static void ble_npl_callout_stop(int *callout){assert(callout==&incoming_timeout);}
static void ble_npl_callout_reset(int *callout,unsigned ticks){assert(callout==&incoming_timeout && ticks==5000);}
static void emit(const cordial_ble_event *e){
    if(e->kind==CORDIAL_BLE_FAILED){failures++;return;}
    assert(e->kind==CORDIAL_BLE_INCOMING && e->number==(unsigned)offers+1 && e->peer.address[0]==2);offers++;
}
''' + resume + expired + wrapper + r'''
static void disconnected(struct ble_gap_event *event) {
''' + disconnected + r'''
}

static int gap(struct ble_gap_event *event,void *arg) {
    if(event->type==BLE_GAP_EVENT_DISCONNECT){disconnected(event);return 0;}
''' + connect + r'''
    assert(false);return 0;
}
static void cancelled(void){
    struct ble_gap_event e={.connect={.status=BLE_HS_EAPP}};
    cancelling=false;gap(&e,NULL);resume_radio();
}
int main(void){
    reconnect_count=2;reconnect_peers[0].val[0]=1;reconnect_peers[1].val[0]=2;
    resume_radio();assert(auto_active && connects==1 && programmed==1 && !manual);
    resume_radio();assert(connects==1); // No blind per-peer timer or reprogramming.
    scan_id=9;resume_radio();assert(cancels==1 && scans==0);
    resume_radio();assert(cancels==1);
    cancelled();assert(scans==1 && !auto_active);
    scan_id=0;resume_radio();assert(auto_active && connects==2);
    links[0]=(link){.token=7,.connection=BLE_HS_CONN_HANDLE_NONE};
    resume_radio();assert(cancels==2 && connects==2);
    cancelled();assert(manual && connects==3 && links[0].started);
    links[0].token=0;resume_radio();assert(auto_active && connects==4);
    scan_id=9;cancel_error=BLE_HS_HCI_ERR(BLE_ERR_CMD_DISALLOWED);
    resume_radio();assert(auto_active && auto_cancel && !failures && is_synced);
    scan_id=0;cancel_error=0;
    struct ble_gap_event e={.connect={.conn_handle=42}};
    gap(&e,NULL);assert(offers==1 && incoming.attempt==1 && !auto_active);
    resume_radio();assert(connects==4); // Wait for application admission.
    links[0]=(link){.token=7,.connection=BLE_HS_CONN_HANDLE_NONE};
    resume_radio();assert(connects==4 && !links[0].started); // Late auto success fences manual Connect.
    links[0].token=0;
    incoming_expired(NULL);assert(terminated==1 && !incoming.attempt);
    resume_radio();assert(connects==4); // Wait for the rejected ACL to retire.
    e.disconnect.conn.conn_handle=42;disconnected(&e);resume_radio();
    assert(auto_active && connects==5); // Same desired list rearms after expiry.
    e.connect.status=123;gap(&e,NULL);resume_radio();
    assert(auto_active && connects==6 && is_synced); // Terminal initiation failure retries.
    scan_id=9;cancel_error=BLE_HS_EALREADY;resume_radio();resume_radio();
    assert(auto_active && auto_cancel && connects==6 && !scanning && is_synced);
    e.connect.status=0;gap(&e,NULL);resume_radio();
    assert(!auto_active && incoming.attempt==2 && offers==2 && connects==6);
    // A new attempt disconnects before delayed CONNECT. A previous auto link's
    // disconnect must not retire the new initiator.
    scan_id=0;cancel_error=0;incoming.attempt=0;incoming.handle=BLE_HS_CONN_HANDLE_NONE;auto_consumed=false;
    resume_radio();assert(auto_active && connects==7);
    e.type=BLE_GAP_EVENT_DISCONNECT;e.disconnect.conn.conn_handle=99;
    auto_gap(&e,(void *)(uintptr_t)(auto_attempt-1));assert(auto_active);
    auto_gap(&e,(void *)(uintptr_t)auto_attempt);assert(!auto_active);
    resume_radio();assert(auto_active && connects==8);
    assert(!failures);
}
''')

    def test_btstack_resolves_private_advertisers_before_offering_admission(self):
        source = (ROOT / "adapters/btstack/c/profiles.c").read_text()
        helper = source[source.index("static void offer_incoming("):source.index("int cordial_profiles_bonds(")]
        run_c(r'''
#include <assert.h>
#include <stdbool.h>
#include <stdint.h>
#include <stddef.h>
typedef int irk_lookup_state_t;
enum {IRK_LOOKUP_SUCCEEDED=1,IRK_LOOKUP_FAILED=2,CORDIAL_INCOMING=4};
typedef struct {uint8_t address[6],random,transport;} cordial_peer;
typedef struct {int kind;uint32_t number;cordial_peer peer;} cordial_event;
static struct {uint32_t attempt;uint16_t handle;cordial_peer peer;bool closing,offered;} incoming[4];
static int state,offers,rejects;
static bool accept=true;
static cordial_event last;
static int sm_identity_resolving_state(uint16_t h){assert(h==42);return state;}
static int sm_le_device_index(uint16_t h){assert(h==42);return 3;}
static bool le_identity(int index,cordial_peer *p){assert(index==3);*p=(cordial_peer){.address={1,2,3,4,5,6},.transport=1};return true;}
static void reject_incoming(unsigned i){assert(i==0);incoming[i].closing=true;rejects++;}
static int cordial_emit_event(void *l,cordial_event event){assert(!l);last=event;offers++;return accept;}
''' + helper + r'''
int main(void){
    incoming[0].attempt=1;incoming[0].handle=42;
    incoming[0].peer=(cordial_peer){.random=1,.transport=1,.address={0x45,1,2,3,4,5}};
    offer_incoming(0);assert(!offers && !rejects); // SM still resolving RPA.
    state=IRK_LOOKUP_SUCCEEDED;offer_incoming(0);
    assert(offers==1 && last.peer.address[0]==1 && !last.peer.random && last.number==1);
    offer_incoming(0);assert(offers==1);
    incoming[0].offered=false;accept=false;offer_incoming(0);assert(rejects==1 && incoming[0].closing);
    incoming[0].closing=incoming[0].offered=false;incoming[0].peer.random=1;incoming[0].peer.address[0]=0x45;
    state=IRK_LOOKUP_FAILED;offer_incoming(0);assert(rejects==2 && offers==2);
}
''')

    def test_btstack_admission_recovers_encryption_completed_before_admission(self):
        source = (ROOT / "adapters/btstack/c/profiles.c").read_text()
        helper = source[source.index("static void security_ready("):source.index("static void sm_handler(")]
        run_c(r'''
#include <assert.h>
#include <stdbool.h>
#include <stddef.h>
typedef int irk_lookup_state_t;
enum {IRK_LOOKUP_SUCCEEDED=1,IRK_LOOKUP_FAILED=2,CORDIAL_AUTHENTICATION=3};
typedef struct {bool closing,security_requested,pairing,authenticated;int handle;} cordial_connection;
typedef struct {int unused;} cordial_peer;
static int state=IRK_LOOKUP_SUCCEEDED,key_size=16,started,failed;
static bool bonded=true;
static irk_lookup_state_t sm_identity_resolving_state(int h){(void)h;return state;}
static void cordial_fail(cordial_connection *l,int error){(void)l;assert(error==CORDIAL_AUTHENTICATION);failed++;}
static int gap_encryption_key_size(int h){(void)h;return key_size;}
static bool gap_bonded(int h){(void)h;return bonded;}
static void begin_hids(cordial_connection *l){assert(l->authenticated);started++;}
static int sm_le_device_index(int h){return h;}
static bool le_identity(int i,cordial_peer *p){(void)i;(void)p;return false;}
static bool has_key(cordial_peer p){(void)p;return false;}
static void le_device_db_remove(int i){(void)i;assert(false);}
static void sm_request_pairing(int h){(void)h;assert(false);}
''' + helper + r'''
int main(void){
    cordial_connection l={.handle=42};
    security_ready(&l);assert(l.authenticated && started==1 && !failed);
    l.authenticated=false;key_size=0;security_ready(&l);assert(!l.authenticated && started==1);
    key_size=16;bonded=false;security_ready(&l);assert(!l.authenticated && started==1);
    bonded=true;state=IRK_LOOKUP_FAILED;security_ready(&l);assert(failed==1 && !l.authenticated);
}
''')

    def test_portable_peer_only_bonds_inventory_conversion_and_deletion(self):
        source = (ROOT / "platforms/esp32s3/components/platform/bonds.c").read_text()
        source = "\n".join(line for line in source.splitlines() if not line.startswith("#include"))
        native=(ROOT / "platforms/esp32s3/components/platform/nimble.c").read_text()
        imports=native[native.index("    if(c->kind==CORDIAL_BLE_IMPORT"):native.index("    if (c->kind==CORDIAL_BLE_FORGET)")]
        run_c(r'''
#include <assert.h>
#include <stdbool.h>
#include "ble.h"
#include <stdint.h>
#include <string.h>
enum { BLE_HS_EINVAL=1, BLE_HS_ENOMEM=2, BLE_HS_ENOENT=3, BLE_HS_EBUSY=4, BLE_HS_ESTORE_FAIL=5,
    BLE_STORE_OBJ_TYPE_OUR_SEC=1, BLE_STORE_OBJ_TYPE_PEER_SEC=2 };
typedef struct { uint8_t type, val[6]; } ble_addr_t;
static const ble_addr_t any={.type=255};
#define BLE_ADDR_ANY (&any)
static int ble_addr_cmp(const ble_addr_t *a,const ble_addr_t *b) {return memcmp(a,b,sizeof(*a));}
struct ble_store_key_sec {ble_addr_t peer_addr;uint8_t idx;};
struct ble_store_value_sec {ble_addr_t peer_addr;uint8_t key_size;uint16_t ediv;uint64_t rand_num;
    uint8_t ltk[16],ltk_present,irk[16],irk_present,csrk[16],csrk_present;uint32_t sign_counter;unsigned authenticated,sc;};
union ble_store_key {struct ble_store_key_sec sec;};
union ble_store_value {struct ble_store_value_sec sec;};
static int ble_store_config_read(int t,const union ble_store_key *k,union ble_store_value *v) {(void)t;(void)k;(void)v;return BLE_HS_ENOENT;}
static int ble_store_config_write(int t,const union ble_store_value *v) {(void)t;(void)v;return 0;}
static int ble_store_config_delete(int t,const union ble_store_key *k) {(void)t;(void)k;return BLE_HS_ENOENT;}
''' + source + r'''
int cordial_ble_resolve_key(const uint8_t *irk,const uint8_t *rpa) { return irk[0]==28 && rpa[5]==0x40; }
static int live, removes, adds, scans;
static cordial_ble_event last;
struct ble_gap_conn_desc { int unused; };
static int ble_gap_conn_find_by_addr(const ble_addr_t *p, struct ble_gap_conn_desc *d) {(void)p;(void)d;return live?0:BLE_HS_ENOENT;}
static int stop_scan(void) {scans++;return 0;}
static int start_scan(void) {scans++;return 0;}
static int unpair_view(const ble_addr_t *p) {
    assert(!live);removes++;
    assert(!cordial_bonds_prepare_unpair(p));
    union ble_store_key k={.sec={.peer_addr=*p}};
    while(!cordial_bonds_delete(BLE_STORE_OBJ_TYPE_OUR_SEC,&k)) {}
    while(!cordial_bonds_delete(BLE_STORE_OBJ_TYPE_PEER_SEC,&k)) {}
    cordial_bonds_clean(p);return 0;
}
static int ble_store_write_peer_sec(const struct ble_store_value_sec *v) {
    union ble_store_value value={.sec=*v};adds++;return cordial_bonds_write(BLE_STORE_OBJ_TYPE_PEER_SEC,&value);
}
static ble_addr_t address(cordial_ble_peer p) {ble_addr_t a={.type=p.random};for(unsigned i=0;i<6;i++)a.val[i]=p.address[5-i];return a;}
static bool bonds(const ble_addr_t *p) {(void)p;return true;}
static void emit(const cordial_ble_event *e) {last=*e;}
static void command(const cordial_ble_command *c) {int status=0;
''' + imports + r'''
}
int main(void) {
    uint8_t r[145]={0},out[145];r[0]=77;r[8]=2;r[9]=1;r[16]=1;
    for(int j=0;j<6;j++)r[10+j]=0xc0+j;
    uint8_t *p=r+81;p[0]=47;p[1]=16;p[2]=0x34;p[3]=0x12;
    for(int j=4;j<64;j++)p[j]=j;
    assert(!cordial_bonds_same_privacy(r,145));
    assert(!cordial_bonds_import(r,145));
    assert(cordial_bonds_same_privacy(r,145));
    r[0]++; assert(cordial_bonds_same_privacy(r,145)); r[0]--;
    r[109]++; assert(!cordial_bonds_same_privacy(r,145)); r[109]--;
    uint8_t raw[6]={0,0,0,0,0,0x40},resolved[6],type;
    assert(cordial_bonds_resolve_rpa(raw,resolved,&type) && type==1 && resolved[0]==0xc5);
    ble_addr_t peers[8];int count;
    assert(!cordial_bonds_peers(peers,&count,8) && count==1);
    assert(peers[0].type==1 && peers[0].val[0]==0xc5);
    union ble_store_key k={.sec={.peer_addr=peers[0]}};union ble_store_value v;
    assert(cordial_bonds_read(BLE_STORE_OBJ_TYPE_OUR_SEC,&k,&v)==BLE_HS_ENOENT);
    assert(!cordial_bonds_read(BLE_STORE_OBJ_TYPE_PEER_SEC,&k,&v));
    assert(v.sec.ediv==0x1234 && v.sec.rand_num==UINT64_C(0x0405060708090a0b));
    assert(v.sec.ltk[0]==27 && v.sec.irk[0]==43 && v.sec.csrk[0]==59);
    assert(!cordial_bonds_write(BLE_STORE_OBJ_TYPE_PEER_SEC,&v));
    assert(!cordial_bonds_export(&peers[0],out) && !memcmp(r,out,145));
    assert(cordial_bonds_delete(BLE_STORE_OBJ_TYPE_OUR_SEC,&k)==BLE_HS_ENOENT);
    assert(!cordial_bonds_mark_dirty(&peers[0]));
    assert(!cordial_bonds_delete(BLE_STORE_OBJ_TYPE_PEER_SEC,&k));
    assert(cordial_bonds_dirty(&peers[0]));
    assert(cordial_bonds_delete(BLE_STORE_OBJ_TYPE_PEER_SEC,&k)==BLE_HS_ENOENT);
    assert(!cordial_bonds_peers(peers,&count,8) && count==0);
    assert(!cordial_bonds_prepare_unpair(&k.sec.peer_addr));
    assert(!cordial_bonds_read(BLE_STORE_OBJ_TYPE_PEER_SEC,&k,&v) && v.sec.irk_present);
    assert(!cordial_bonds_delete(BLE_STORE_OBJ_TYPE_PEER_SEC,&k));
    cordial_bonds_clean(&k.sec.peer_addr);assert(!cordial_bonds_dirty(&k.sec.peer_addr));
    // A real two-direction record must terminate each delete-all loop.
    memcpy(r+17,r+81,64);assert(!cordial_bonds_import(r,145));
    assert(!cordial_bonds_delete(BLE_STORE_OBJ_TYPE_OUR_SEC,&k));
    assert(cordial_bonds_delete(BLE_STORE_OBJ_TYPE_OUR_SEC,&k)==BLE_HS_ENOENT);
    assert(!cordial_bonds_delete(BLE_STORE_OBJ_TYPE_PEER_SEC,&k));
    for(int i=0;i<8;i++) {r[15]=i;assert(!cordial_bonds_import(r,145));}
    r[15]=8;assert(cordial_bonds_import(r,145)==BLE_HS_ENOMEM);
    assert(!cordial_bonds_peers(peers,&count,8) && count==8);
    cordial_bonds_clear();assert(!cordial_bonds_peers(peers,&count,8) && count==0);
    assert(!cordial_bonds_same_privacy(r,145));
    cordial_ble_command c={.kind=CORDIAL_BLE_IMPORT,.length=145};memcpy(c.data,r,145);
    command(&c);assert(!last.code && removes==1 && adds==1 && scans==2);
    command(&c);assert(!last.code && removes==1 && adds==1 && scans==2);
    // Changed privacy cannot tear down an established link.
    live=1;c.data[109]++;command(&c);assert(!last.code && removes==1 && adds==1);
    c.data[109]--;
    ble_addr_t addr={.type=r[9]};reverse(addr.val,r+10,6);
    assert(!cordial_bonds_mark_dirty(&addr));
    command(&c);assert(!last.code && cordial_bonds_dirty(&addr) && removes==1);
    live=0;command(&c);assert(!last.code && !cordial_bonds_dirty(&addr) && removes==2 && adds==2);
    // A native late-identity write can change IRK without a repeat callback.
    k.sec.peer_addr=addr;assert(!cordial_bonds_read(BLE_STORE_OBJ_TYPE_PEER_SEC,&k,&v));
    v.sec.irk[0]++;assert(!cordial_bonds_write(BLE_STORE_OBJ_TYPE_PEER_SEC,&v));
    assert(cordial_bonds_dirty(&addr));
}
''')

    def test_nimble_scan_preserves_private_address_and_restores_resolution(self):
        source = (ROOT / "platforms/esp32s3/components/platform/nimble.c").read_text()
        functions = source[source.index("static int stop_scan("):
                           source.index("static ble_addr_t address(")]
        begin=functions.index("// Call only after the target link")
        end=functions.index("static int start_scan(",begin)
        functions=functions[:begin]+functions[end:]
        bonds = source[source.index("static bool bonds("):source.index("static void fail(")]
        report = source[source.index("        struct ble_hs_adv_fields fields;"):
                        source.index("    case BLE_GAP_EVENT_DISC_COMPLETE:")].rsplit("    }", 1)[0]
        run_c(r'''
#include <stdbool.h>
#include <assert.h>
#include <string.h>
#include "ble.h"
enum { BLE_ADDR_PUBLIC, BLE_ADDR_RANDOM, BLE_OWN_ADDR_PUBLIC=0,
    BLE_HS_FOREVER=0, BLE_HS_CONN_HANDLE_NONE=65535,
    BLE_HCI_ADV_RPT_EVTYPE_ADV_IND=0, BLE_HCI_ADV_RPT_EVTYPE_DIR_IND=1 };
typedef struct { uint8_t type, val[6]; } ble_addr_t;
static const ble_addr_t any;
#define BLE_ADDR_ANY (&any)
struct ble_gap_disc_params { int itvl, window, filter_duplicates; };
struct ble_hs_adv_fields { uint8_t *name; uint8_t name_len; uint16_t appearance; bool appearance_is_present; };
static uint16_t appearance;
struct ble_gap_event { struct { ble_addr_t addr; uint8_t *data;
    int length_data, rssi, event_type; } disc; };
static struct { unsigned token, connection; } links[4];
static uint64_t scan_id;
static bool scan_unresolved, active, auto_active, resolution=true;
static unsigned cancels, starts, queries, emitted;
static int scan_error, resolve_error;
static bool resolved;
static ble_addr_t identity;
static cordial_ble_event last;
static ble_addr_t stored[8];
static int count, store_error;
static int cordial_bonds_peers(ble_addr_t *out,int *n,int capacity) {
    assert(capacity==8);memcpy(out,stored,sizeof stored);*n=count;return store_error;
}
static void emit(const cordial_ble_event *e) { last=*e;emitted++; }
static int ble_gap_disc_active(void) { return active; }
static int ble_gap_disc_cancel(void) { cancels++;active=false;return 0; }
static int ble_hs_pvcy_set_resolve_enabled(int enabled) {
    assert(!active);if(resolve_error) return resolve_error;
    resolution=enabled;return 0;
}
static int gap(struct ble_gap_event *e,void *arg);
static int ble_gap_disc(int own,int duration,struct ble_gap_disc_params *params,
    int (*cb)(struct ble_gap_event *,void *),void *arg) {
    (void)own;(void)duration;(void)params;(void)cb;(void)arg;
    assert(!resolution);starts++;active=!scan_error;return scan_error;
}
static bool cordial_bonds_resolve_rpa(const uint8_t *raw,uint8_t *out,uint8_t *type) {
    assert(raw[5]==0x45);queries++;memcpy(out,identity.val,6);*type=identity.type;return resolved;
}
static int ble_hs_adv_parse_fields(struct ble_hs_adv_fields *f,const void *data,int len) {
    (void)data;(void)len;memset(f,0,sizeof *f);f->appearance=appearance;f->appearance_is_present=appearance!=0;return 0;
}
''' + functions + bonds + r'''
static int gap(struct ble_gap_event *event,void *arg) {
    (void)arg;
''' + report + r'''
}
int main(void) {
    assert(!start_scan() && !starts && resolution);
    scan_id=1;assert(!start_scan() && active && scan_unresolved && !resolution);
    struct ble_gap_event e={.disc={.addr={.type=1,.val={1,2,3,4,5,0x45}}}};
    identity=(ble_addr_t){.type=0,.val={9,8,7,6,5,4}};resolved=true;
    appearance=0x03c1;gap(&e,NULL);assert(emitted==1 && queries==1 && last.number==0x03c1);
    appearance=0;gap(&e,NULL);assert(last.number==0);
    assert(last.peer.address[0]==4 && !last.peer.random);
    assert(last.address.address[0]==0x45 && last.address.random);
    assert(last.scan==1 && last.code);
    resolved=false;gap(&e,NULL);assert(last.peer.address[0]==0x45);
    resolved=true;memset(&identity,0,sizeof identity);gap(&e,NULL);
    assert(last.peer.address[0]==0x45); // A local-IRK match is not a peer identity.
    e.disc.addr.type=2;gap(&e,NULL);assert(emitted==4); // Never copy an identity as raw.
    assert(!stop_scan() && !active && resolution && !scan_unresolved && cancels==1);
    e.disc.addr.type=1;gap(&e,NULL);assert(emitted==4);
    links[0].token=1;links[0].connection=BLE_HS_CONN_HANDLE_NONE;
    assert(!start_scan() && starts==1 && resolution); // No toggle during initiation.
    links[0].connection=1;assert(!start_scan() && starts==2 && active);
    assert(!stop_scan() && resolution);
    scan_error=5;assert(start_scan()==5 && !scan_unresolved && resolution);
    scan_error=0;resolve_error=6;assert(start_scan()==6 && !scan_unresolved && resolution);
    // A successful native unpair return cannot hide keys that remain stored.
    stored[0]=e.disc.addr;count=1;
    assert(bonds(NULL) && !bonds(&e.disc.addr));
    count=0;assert(bonds(&e.disc.addr));
    store_error=1;assert(!bonds(&e.disc.addr));
    return 0;
}
''')





    def test_native_authentication_preserves_failure_status_without_changing_admission(self):
        source = (ROOT / "platforms/esp32s3/components/platform/nimble.c").read_text()
        helper = source[source.index("static void auth_diagnostic("):source.index("static void security_ready(")]
        encryption = source[source.index("    case BLE_GAP_EVENT_ENC_CHANGE: {"):source.index("    case BLE_GAP_EVENT_PASSKEY_ACTION: {")]
        encryption = encryption[encryption.index("{")+1:encryption.rindex("}")]
        for development in (0, 1):
            run_c(f"#define CONFIG_CORDIAL_DEVELOPMENT {development}\n" + r'''
#include <assert.h>
#include <stdbool.h>
#include "ble.h"
#define BLE_HS_CONN_HANDLE_NONE 65535
struct ble_gap_conn_desc { struct {bool encrypted,bonded;} sec_state; };
struct ble_gap_event {struct {uint16_t conn_handle;int status;} enc_change;};
typedef struct {uint32_t token;uint16_t connection;bool closing,secure;} link;
static link current={.token=42,.connection=1};
static struct ble_gap_conn_desc state;
static int lookup_error, failures, successes, reports;
static cordial_ble_event last;
static link *by_handle(uint16_t h) {return h==current.connection ? &current:0;}
static int ble_gap_conn_find(uint16_t h,struct ble_gap_conn_desc *out) {
    assert(h==1);*out=state;return lookup_error;
}
#if CONFIG_CORDIAL_DEVELOPMENT
static void emit(const cordial_ble_event *e) {last=*e;reports++;}
#endif
static void fail(link *l,uint8_t code) {assert(l==&current && code==CORDIAL_BLE_AUTH);failures++;}
static void security_ready(link *l) {assert(l==&current);successes++;}
''' + helper + r'''
static int encryption_changed(struct ble_gap_event *event) {link *l;
''' + encryption + r'''
}
int main(void) {
    struct ble_gap_event e={.enc_change={.conn_handle=1,.status=0x40b}};
    assert(!encryption_changed(&e) && failures==1 && successes==0);
#if CONFIG_CORDIAL_DEVELOPMENT
    assert(reports==1 && last.kind==CORDIAL_BLE_AUTH_FAILURE && last.token==42);
    assert(last.code==CORDIAL_AUTH_ENCRYPTION && last.number==0x40b && !last.encrypted && !last.bonded);
    auth_fail(&current,CORDIAL_AUTH_INITIATE,6);
    assert(last.code==CORDIAL_AUTH_INITIATE && last.number==6);
    auth_diagnostic(&current,CORDIAL_AUTH_CLEAR,0);
    assert(last.code==CORDIAL_AUTH_CLEAR && last.number==0);
#else
    assert(!reports && !last.kind);
#endif
    int before=failures;
    e.enc_change.status=0;state.sec_state.encrypted=true;
    assert(!encryption_changed(&e) && failures==before+1 && successes==0);
#if CONFIG_CORDIAL_DEVELOPMENT
    assert(last.code==CORDIAL_AUTH_STATE && last.number==0 && last.encrypted && !last.bonded);
#endif
    state.sec_state.bonded=true;
    assert(!encryption_changed(&e) && failures==before+1 && successes==1 && current.secure);
    lookup_error=7;
    assert(!encryption_changed(&e) && failures==before+2 && successes==1);
#if CONFIG_CORDIAL_DEVELOPMENT
    assert(last.code==CORDIAL_AUTH_STATE && last.number==7);
#endif
}
''')

if __name__ == "__main__":
    unittest.main()
