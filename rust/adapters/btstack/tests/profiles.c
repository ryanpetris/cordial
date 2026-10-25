// Callback tests use the real native database and inject adapter events.
// They do not simulate a complete cryptographic pairing exchange.
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>
#include <string.h>
#include "gap.h"
#include "hci.h"
static btstack_linked_list_t test_hci_connections;
static bool use_test_hci_connections;
static void test_connection_iterator(btstack_linked_list_iterator_t *it) {
    if (use_test_hci_connections) btstack_linked_list_iterator_init(it, &test_hci_connections);
    else hci_connections_get_iterator(it);
}
static unsigned scan_parameter_updates;
static void test_scan_parameters(uint8_t type, uint16_t interval, uint16_t window) {
    scan_parameter_updates++;
    gap_set_scan_parameters(type, interval, window);
}
#define gap_set_scan_parameters test_scan_parameters
#define hci_connections_get_iterator test_connection_iterator
#include "../c/profiles.c"
#undef gap_set_scan_parameters
#undef hci_connections_get_iterator
#include "runtime.h"
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
static cordial_event last_event, previous_event;
static bool reject_descriptor, reject_security;
static int event_cb(void *ctx,const cordial_event *event){(void)ctx;previous_event=last_event;last_event=*event;return !((reject_descriptor && event->kind==CORDIAL_DESCRIPTOR) || (reject_security && event->kind==CORDIAL_SECURITY));}
static void scan_tokens_restart_duplicate_filtering(void) {
    ready=true;
    assert(cordial_profiles_scan(1,false,true)==CORDIAL_OK);
    unsigned previous=scan_parameter_updates;
    assert(cordial_profiles_scan(1,false,true)==CORDIAL_OK);
    assert(scan_parameter_updates==previous);
    assert(cordial_profiles_scan(2,false,true)==CORDIAL_OK);
    assert(scan_parameter_updates==previous+1);
    // Even coalesced stop/start must request the native parameter update,
    // which forces HCI to disable the active scan before re-enabling it.
    assert(cordial_profiles_scan(2,false,false)==CORDIAL_OK);
    assert(cordial_profiles_scan(3,false,true)==CORDIAL_OK);
    assert(scan_parameter_updates==previous+2);
    for(uint8_t type=0;type<5;type++) {
        uint8_t report[]={GAP_EVENT_ADVERTISING_REPORT,10,type,0,1,2,3,4,5,6,0xd0,0};
        packet_handler(HCI_EVENT_PACKET,0,report,sizeof report);
        assert(last_event.kind==CORDIAL_FOUND && last_event.operation==3);
        assert(last_event.code==(type<=1));
    }
    uint8_t keyboard[]={GAP_EVENT_ADVERTISING_REPORT,14,0,0,1,2,3,4,5,6,0xd0,4,3,0x19,0xc1,3};
    packet_handler(HCI_EVENT_PACKET,0,keyboard,sizeof keyboard);
    assert(last_event.number==0x03c1);
    keyboard[12]=2; // A one-byte Appearance is invalid, not a keyboard hint.
    packet_handler(HCI_EVENT_PACKET,0,keyboard,sizeof keyboard);
    assert(last_event.number==0);
    assert(cordial_profiles_scan(3,false,false)==CORDIAL_OK);
#ifdef ENABLE_CLASSIC
    assert(cordial_profiles_scan(6,true,false)==CORDIAL_OK);
    uint8_t inquiry_result[27]={GAP_EVENT_INQUIRY_RESULT,25,1,2,3,4,5,6};
    inquiry_result[9]=0xc0;inquiry_result[10]=5;
    packet_handler(HCI_EVENT_PACKET,0,inquiry_result,sizeof inquiry_result);
    assert(last_event.number==0x05c0);
    assert(last_event.kind==CORDIAL_FOUND && last_event.peer.transport==CORDIAL_CLASSIC);
    assert(last_event.address.transport==CORDIAL_CLASSIC && last_event.address.address[0]==6);
    assert(last_event.address.address[5]==1 && last_event.operation==6);
    assert(cordial_profiles_scan(6,false,false)==CORDIAL_OK);
#endif
    ready=false;
}
static void security_precedes_initial_connected_snapshot(void) {
    for(unsigned transport=CORDIAL_CLASSIC;transport<=CORDIAL_BLE;transport++) {
        cordial_connection l={.id={.generation=99,.slot=0},.peer={.transport=transport},
            .handle=HCI_CON_HANDLE_INVALID,.authenticated=true,.profile=true,.adopted=true};
        memset(&last_event,0,sizeof last_event);
        cordial_ready(&l);
        assert(last_event.kind==CORDIAL_CONNECTED && previous_event.kind==CORDIAL_SECURITY);
        assert(previous_event.link.generation==99);
        assert(previous_event.number==0); // Missing native handle means no known properties.
        memset(&last_event,0,sizeof last_event);
        memset(&previous_event,0,sizeof previous_event);
        cordial_ready(&l);
        assert(!last_event.kind && !previous_event.kind);
    }
}
static void resolved_reports_keep_identity_and_private_address(void) {
    ready=true;
    assert(cordial_profiles_scan(4,false,true)==CORDIAL_OK);
    assert(read_peer_rpa.opcode==HCI_OPCODE_HCI_LE_READ_PEER_RESOLVABLE_ADDRESS);
    assert(!strcmp(read_peer_rpa.format,"1B"));
    for(uint8_t type=2;type<=3;type++) {
        uint8_t report[]={GAP_EVENT_ADVERTISING_REPORT,19,0,type,1,2,3,4,5,6,0xd0,9,3,0x19,0xc2,3,4,9,'K','e','y'};
        memset(&last_event,0,sizeof last_event);
        packet_handler(HCI_EVENT_PACKET,0,report,sizeof report);
        assert(last_event.kind==CORDIAL_FOUND && last_event.address.transport==0xff); // Presence only.
        assert(rpa_reports[0].used && rpa_reports[0].peer.random==(type&1));
        uint8_t scan_response[]={GAP_EVENT_ADVERTISING_REPORT,10,4,type,1,2,3,4,5,6,0xd0,0};
        packet_handler(HCI_EVENT_PACKET,0,scan_response,sizeof scan_response);
        assert(rpa_reports[0].appearance==0x03c2);
        rpa_read=0; // The controller completes the queued public HCI command.
        uint8_t complete[]={HCI_EVENT_COMMAND_COMPLETE,10,1,0x2b,0x20,0,9,8,7,6,5,0x45};
        packet_handler(HCI_EVENT_PACKET,0,complete,sizeof complete);
        assert(last_event.kind==CORDIAL_FOUND && last_event.operation==4 && !last_event.code);
        assert(last_event.number==0x03c2);
        assert(last_event.length==3 && !memcmp(last_event.data,"Key",3));
        assert(last_event.peer.random==(type&1) && last_event.peer.address[0]==6);
        assert(last_event.address.random && last_event.address.address[0]==0x45);
        assert(last_event.address.address[5]==9 && rpa_read==NVM_NUM_DEVICE_DB_ENTRIES && !rpa_reports[0].used);
        for(unsigned failure=0;failure<3;failure++) {
            memset(&last_event,0,sizeof last_event);
            packet_handler(HCI_EVENT_PACKET,0,report,sizeof report);
            assert(last_event.kind==CORDIAL_FOUND && last_event.address.transport==0xff);
            memset(&last_event,0,sizeof last_event);
            rpa_read=0;
            uint8_t bad[]={HCI_EVENT_COMMAND_COMPLETE,10,1,0x2b,0x20,0,0,0,0,0,0,0};
            if(failure==0) bad[5]=ERROR_CODE_UNKNOWN_CONNECTION_IDENTIFIER;
            if(failure==2) { rpa_reports[0].scan=3;bad[11]=0x45; }
            packet_handler(HCI_EVENT_PACKET,0,bad,sizeof bad);
            assert(!last_event.kind && rpa_read==NVM_NUM_DEVICE_DB_ENTRIES && !rpa_reports[0].used);
        }
    }
    uint8_t report[]={GAP_EVENT_ADVERTISING_REPORT,10,0,2,1,2,3,4,5,6,0xd0,0};
    packet_handler(HCI_EVENT_PACKET,0,report,sizeof report);
    rpa_read=0;
    assert(cordial_profiles_scan(5,false,true)==CORDIAL_OK);
    packet_handler(HCI_EVENT_PACKET,0,report,sizeof report);
    assert(rpa_reports[0].scan==4 && rpa_reports[1].scan==5);
    memset(&last_event,0,sizeof last_event);
    uint8_t complete[]={HCI_EVENT_COMMAND_COMPLETE,10,1,0x2b,0x20,0,9,8,7,6,5,0x45};
    packet_handler(HCI_EVENT_PACKET,0,complete,sizeof complete);
    assert(!last_event.kind && rpa_reports[1].used);
    rpa_read=1;
    packet_handler(HCI_EVENT_PACKET,0,complete,sizeof complete);
    assert(last_event.kind==CORDIAL_FOUND && last_event.operation==5);
    assert(cordial_profiles_scan(5,false,false)==CORDIAL_OK);
    ready=false;
}
static void classic_security_waits_for_key_size_completion(void) {
    bd_addr_t addr={0x10,0x20,0x30,0x40,0x50,0x60};
    assert(!gap_connect(addr,BD_ADDR_TYPE_ACL));
    hci_connection_t *native=hci_connection_for_bd_addr_and_type(addr,BD_ADDR_TYPE_ACL);
    assert(native);native->con_handle=0x31;
    cordial_peer peer={.transport=CORDIAL_CLASSIC};memcpy(peer.address,addr,6);
    cordial_connection *l=allocate((cordial_link){.generation=101,.slot=0},peer,false);
    assert(l);l->handle=0x31;l->profile=true;l->adopted=true;
    uint8_t encrypted[]={HCI_EVENT_ENCRYPTION_CHANGE,4,0,0x31,0,1};
    packet_handler(HCI_EVENT_PACKET,0,encrypted,sizeof encrypted);
    assert(last_event.kind==CORDIAL_CONNECTED && previous_event.kind==CORDIAL_SECURITY);
    assert(previous_event.number==(1u<<19)); // Only bond state known so far.
    native->authentication_flags|=AUTH_FLAG_CONNECTION_ENCRYPTED;
    native->encryption_key_size=16;
    native->encryption_key_type=AUTHENTICATED_COMBINATION_KEY_GENERATED_FROM_P256;
    uint8_t ready[]={GAP_EVENT_SECURITY_LEVEL,4,0x31,0,LEVEL_4,0};
    packet_handler(HCI_EVENT_PACKET,0,ready,sizeof ready);
    assert(last_event.kind==CORDIAL_SECURITY);
    assert(last_event.number==((15u<<16)|(16u<<8)|7u));
    uint8_t disconnected[]={HCI_EVENT_DISCONNECTION_COMPLETE,4,0,0x31,0,0x16};
    cordial_runtime_receive(HCI_EVENT_PACKET,disconnected,sizeof disconnected);
    cordial_profiles_poll(0);
    assert(!hci_connection_for_bd_addr_and_type(addr,BD_ADDR_TYPE_ACL) && !l->used);
}
static void security_pressure_retries_without_disconnecting(void) {
    cordial_peer peer={.transport=CORDIAL_CLASSIC,.address={4,3,2,1,0,9}};
    cordial_connection *l=allocate((cordial_link){.generation=102,.slot=0},peer,false);
    assert(l);l->authenticated=true;l->profile=true;
    reject_security=true;
    cordial_ready(l);
    assert(last_event.kind==CORDIAL_CONNECTED && l->ready && l->security_pending);
    cordial_profiles_poll(0);
    assert(l->security_pending && !l->closing && !l->error);
    reject_security=false;
    cordial_profiles_poll(0);
    assert(last_event.kind==CORDIAL_SECURITY && !l->security_pending && !l->closing);
    reject_security=true;
    assert(!publish_security(l));
    assert(l->security_pending && !l->closing && !l->error);
    reject_security=false;
    cordial_profiles_poll(0);
    assert(!l->security_pending && !l->closing);
    memset(l,0,sizeof *l);
}
static void peer_service_queries(void) {
    att_connection_t connection = {.mtu = 23, .max_mtu = 23};
    uint8_t response[64];
    /* The keyboard's discovery request must receive a response. Silence
     * expires the peer's ATT transaction even while our own reads succeed. */
    uint8_t discover[] = {ATT_READ_BY_GROUP_TYPE_REQUEST, 1, 0, 0xff, 0xff, 0, 0x28};
    uint16_t n = att_handle_request(&connection, discover, sizeof discover, response);
    assert(n == 8 && response[0] == ATT_READ_BY_GROUP_TYPE_RESPONSE);
    assert(little_endian_read_16(response, 6) == ORG_BLUETOOTH_SERVICE_GENERIC_ACCESS);
    uint16_t end = little_endian_read_16(response, 4);
    little_endian_store_16(discover, 1, (uint16_t)(end + 1));
    n = att_handle_request(&connection, discover, sizeof discover, response);
    assert(n == 5 && response[0] == ATT_ERROR_RESPONSE && response[4] == ATT_ERROR_ATTRIBUTE_NOT_FOUND);
    uint8_t name[] = {ATT_READ_BY_TYPE_REQUEST, 1, 0, 0xff, 0xff, 0, 0x2a};
    n = att_handle_request(&connection, name, sizeof name, response);
    assert(n == 4 + sizeof("Cordial") - 1 && response[0] == ATT_READ_BY_TYPE_RESPONSE);
    assert(!memcmp(response + 4, "Cordial", sizeof("Cordial") - 1));
    uint16_t handle = little_endian_read_16(response, 2);
    uint8_t write[] = {ATT_WRITE_REQUEST, (uint8_t)handle, (uint8_t)(handle >> 8), 0};
    n = att_handle_request(&connection, write, sizeof write, response);
    assert(n == 5 && response[0] == ATT_ERROR_RESPONSE && response[4] == ATT_ERROR_WRITE_NOT_PERMITTED);
}
static void capacity_boundaries(void) {
    assert(sizeof classic_descriptors == (CORDIAL_CLASSIC_CONNECTIONS + 1) * CORDIAL_DESCRIPTOR_BYTES);
    memset(cordial_connections, 0, sizeof cordial_connections);
    memset(incoming, 0, sizeof incoming);
    cordial_bond_clear();
    ready = true;
    transport_enabled[0] = transport_enabled[1] = true;
    unsigned slot = 0;
    for (unsigned transport = 0; transport < 2; ++transport) {
        unsigned capacity = cordial_bond_capacity(transport + 1);
        assert(capacity == normal_limit(transport) + 1);
        for (unsigned i = 0; i < capacity; ++i) {
            uint8_t record[145] = {0};
            record[8] = transport + 1; record[10] = transport + 1; record[15] = i + 1;
            assert(cordial_bond_import(record, transport ? 145 : 34) >= 0);
            uint8_t copy[145] = {0};
            assert(cordial_bond_export(transport + 1, 0, record + 10, copy) == (transport ? 145 : 34));
            assert(!memcmp(copy + 10, record + 10, 6));
        }
        uint8_t extra[145] = {0};
        extra[8] = transport + 1; extra[15] = 240;
        assert(cordial_bond_import(extra, transport ? 145 : 34) < 0);
        for (unsigned i = 0; i < normal_limit(transport); ++i) {
            cordial_peer peer = {.transport = transport, .address = {transport + 1, 0, 0, 0, 0, i + 1}};
            cordial_connection *link = allocate((cordial_link){.slot=slot, .generation=1000+slot}, peer, false);
            assert(link); slot++;
        }
        cordial_peer extra_peer = {.transport=transport,.address={240}};
        assert(!allocate((cordial_link){.slot=slot,.generation=2000}, extra_peer, false));
    }
    cordial_peer snapshot[NVM_NUM_LINK_KEYS + NVM_NUM_DEVICE_DB_ENTRIES + 1];
    memset(snapshot, 0xa5, sizeof snapshot);
    unsigned capacity = NVM_NUM_LINK_KEYS + NVM_NUM_DEVICE_DB_ENTRIES;
    assert(cordial_profiles_bonds(snapshot, capacity) == (int)capacity);
    for (unsigned i = 0; i < sizeof(cordial_peer); ++i) assert(((uint8_t *)&snapshot[capacity])[i] == 0xa5);
    memset(snapshot, 0xa5, sizeof snapshot);
    assert(cordial_profiles_bonds(snapshot, 1) == -CORDIAL_CAPACITY);
    for (unsigned i = 0; i < sizeof(cordial_peer); ++i) assert(((uint8_t *)&snapshot[1])[i] == 0xa5);
    cordial_peer reconnect[CORDIAL_BLE_CONNECTIONS + 1];
    for (unsigned i=0;i<CORDIAL_BLE_CONNECTIONS+1;++i)
        reconnect[i]=(cordial_peer){.transport=CORDIAL_BLE,.address={2,0,0,0,0,i+1}};
    assert(cordial_profiles_reconnect(reconnect, CORDIAL_BLE_CONNECTIONS)==CORDIAL_OK);
    assert(cordial_profiles_reconnect(reconnect, CORDIAL_BLE_CONNECTIONS+1)==CORDIAL_CAPACITY);
    assert(!accept_classic((bd_addr_t){1,0,0,0,0,CORDIAL_CLASSIC_CONNECTIONS+1}, HCI_LINK_TYPE_ACL));
    cordial_peer candidate={.transport=CORDIAL_BLE,.address={241}};
    assert(slot==CORDIAL_LINKS-1);
    cordial_connection *pair=allocate((cordial_link){.slot=slot,.generation=3000},candidate,true);
    assert(pair && normal_links(CORDIAL_BLE)==CORDIAL_BLE_CONNECTIONS);
    assert(!allocate((cordial_link){.slot=slot,.generation=3001},(cordial_peer){.transport=CORDIAL_CLASSIC,.address={242}},true));
    memset(cordial_connections, 0, sizeof cordial_connections);
    cordial_bond_clear();
}

static void pairing_preserves_normal_capacity(void) {
    reconnect_count = 0;
    for (unsigned transport = 0; transport < 2; ++transport) {
        memset(cordial_connections, 0, sizeof cordial_connections);
        memset(incoming, 0, sizeof incoming);
        cordial_bond_clear();
        ready = true;
        unsigned limit = normal_limit(transport);
        for (unsigned i = 0; i < limit - 1; ++i)
            assert(allocate((cordial_link){.slot=i, .generation=4000+i},
                (cordial_peer){.transport=transport, .address={1,0,0,0,0,i+1}}, false));
        cordial_peer peer = {.transport=transport, .address={2}};
        cordial_connection *pair = allocate((cordial_link){.slot=limit-1, .generation=5000}, peer, true);
        assert(pair);
        assert(!allocate((cordial_link){.slot=limit, .generation=5001},
            (cordial_peer){.transport=1-transport, .address={3}}, true));
        cordial_peer last = {.transport=transport, .address={4}};
        if (transport == CORDIAL_BLE) {
            reconnect_peers[0] = last; reconnect_count = 1;
            auto_dirty = true; auto_consumed = false;
            resume_radio();
            assert(ready && auto_active);
            assert(!gap_connect_cancel()); auto_active = false;
        }
        cordial_connection *normal = allocate((cordial_link){.slot=limit, .generation=5002}, last, false);
        assert(normal && normal_links(transport) == limit);
        if (transport == CORDIAL_BLE) {
            resume_radio();
            assert(!auto_active);
            reconnect_count = 0;
        }
        cordial_peer extra = {.transport=transport, .address={5}};
        cordial_link extra_id = {.slot=limit+1, .generation=5003};
        assert(!allocate(extra_id, extra, false));
        cordial_profiles_disconnect(normal->id);
        assert(normal->used && normal->closing && !allocate(extra_id, extra, false));
        cordial_profiles_poll(now_ms);
        assert(!normal->used);
        // A saved non-resident bond retains the reserve through physical teardown.
        uint8_t record[145] = {0};
        record[8] = transport + 1;
        memcpy(record + 10, peer.address, 6);
        if (transport == CORDIAL_BLE) { record[81] = 1; record[82] = 16; }
        assert(cordial_bond_import(record, transport ? 145 : 34) >= 0);
        pair->bonded = true;
        assert(cordial_profiles_adopt(pair->id) == CORDIAL_OK);
        assert(pair->adopted && pair->pairing && normal_links(transport) == limit - 1);
        cordial_profiles_disconnect(pair->id);
        if (transport == CORDIAL_CLASSIC) {
            memcpy(record + 10, extra.address, 6);
            assert(cordial_bond_import(record, 34) >= 0);
            assert(accept_classic(extra.address, HCI_LINK_TYPE_ACL));
        }
        normal = allocate(extra_id, extra, false);
        assert(normal && normal_links(transport) == limit);
        assert(!allocate((cordial_link){.slot=limit+2,.generation=5004}, last, false));
        if (transport == CORDIAL_CLASSIC) {
            memcpy(record + 10, last.address, 6);
            assert(cordial_bond_import(record, 34) >= 0);
            assert(!accept_classic(last.address, HCI_LINK_TYPE_ACL));
        }
        cordial_profiles_poll(now_ms);
        assert(!pair->used && has_key(peer) && normal_links(transport) == limit);
        cordial_profiles_disconnect(normal->id);
        cordial_profiles_poll(now_ms);
        // Only promotion transfers an adopted link to the normal allowance.
        pair = allocate((cordial_link){.slot=limit-1,.generation=5005}, peer, true);
        assert(pair); pair->bonded = true;
        assert(cordial_profiles_adopt(pair->id) == CORDIAL_OK);
        cordial_profiles_promote(pair->id);
        assert(!pair->pairing && pair->adopted && normal_links(transport) == limit);
        cordial_profiles_disconnect(pair->id);
        assert(!allocate(extra_id, extra, false));
        cordial_profiles_poll(now_ms);
        assert(has_key(peer) && allocate(extra_id, extra, false));
    }
    memset(cordial_connections, 0, sizeof cordial_connections);
    cordial_bond_clear();
}

static void pending_classic_reserves_normal_capacity(void) {
    memset(cordial_connections, 0, sizeof cordial_connections);
    use_test_hci_connections = true;
    hci_connection_t acl[CORDIAL_CLASSIC_CONNECTIONS + 1] = {0};
    for (unsigned i = 0; i < CORDIAL_CLASSIC_CONNECTIONS + 1; ++i) {
        acl[i].address_type = BD_ADDR_TYPE_ACL;
        acl[i].address[0] = i + 1;
        btstack_linked_list_add_tail(&test_hci_connections, (btstack_linked_item_t *)&acl[i]);
    }
    cordial_peer paired = {.transport=CORDIAL_CLASSIC, .address={1}};
    cordial_connection *pair = allocate((cordial_link){.slot=0, .generation=6000}, paired, true);
    assert(pair);
    cordial_peer peer = {.transport=CORDIAL_CLASSIC, .address={2}};
    uint8_t record[34] = {0};
    record[8] = 1; memcpy(record + 10, peer.address, 6);
    assert(cordial_bond_import(record, sizeof record) >= 0);
    // The candidate's pending ACL transfers to normal ownership without double counting.
    assert(accept_classic(peer.address, HCI_LINK_TYPE_ACL));
    assert(allocate((cordial_link){.slot=1, .generation=6001}, peer, false));
    cordial_peer extra = {.transport=CORDIAL_CLASSIC, .address={42}};
    record[10] = 42;
    assert(cordial_bond_import(record, sizeof record) >= 0);
    assert(!accept_classic(extra.address, HCI_LINK_TYPE_ACL));
    assert(!allocate((cordial_link){.slot=2, .generation=6002}, extra, false));
    // Unowned ACLs awaiting teardown continue to reserve normal places.
    btstack_linked_list_remove(&test_hci_connections, (btstack_linked_item_t *)&acl[CORDIAL_CLASSIC_CONNECTIONS]);
    assert(accept_classic(extra.address, HCI_LINK_TYPE_ACL));
    assert(allocate((cordial_link){.slot=2, .generation=6002}, extra, false));
    test_hci_connections = NULL;
    use_test_hci_connections = false;
    memset(cordial_connections, 0, sizeof cordial_connections);
    cordial_bond_clear();
}

int main(void){
    cordial_runtime_callbacks cb={.time_ms=time_cb,.wake=wake_cb,.fatal=fatal_cb,.can_send=can_send_cb,.send=send_cb};
    btstack_tlv_t tlv={.get_tag=get,.store_tag=save,.delete_tag=del};
    cordial_runtime_init(&cb,&tlv,NULL,NULL); cordial_profiles_init(NULL,event_cb); transport_enabled[CORDIAL_CLASSIC] = true;
    scan_tokens_restart_duplicate_filtering();
    resolved_reports_keep_identity_and_private_address();
    security_precedes_initial_connected_snapshot();
    classic_security_waits_for_key_size_completion();
    security_pressure_retries_without_disconnecting();
    peer_service_queries();
    for(unsigned already_closing=0;already_closing<2;already_closing++){
        cordial_peer selected={.address={0x40,2,3,4,5,6},.transport=CORDIAL_BLE,.random=1};
        cordial_connection *l=allocate((cordial_link){.generation=already_closing+1,.slot=0},selected,true);
        assert(l); l->handle=0x40; l->security_requested=true; l->closing=already_closing;
        bd_addr_t identity={1,2,3,4,5,6}; sm_key_t irk={1},ltk={2}; uint8_t rand[8]={0};
        int index=le_device_db_add(BD_ADDR_TYPE_LE_PUBLIC,identity,irk); assert(index>=0);
        le_device_db_encryption_set(index,0,rand,ltk,16,1,0,1);
        uint8_t created[20]={SM_EVENT_IDENTITY_CREATED,18,0x40,0,1};
        reverse_bd_addr(selected.address,created+5); created[11]=BD_ADDR_TYPE_LE_PUBLIC;
        reverse_bd_addr(identity,created+12); little_endian_store_16(created,18,index);
        sm_handler(HCI_EVENT_PACKET,0,created,sizeof(created));
        assert(!l->peer.random && !memcmp(l->peer.address,identity,6));
        cordial_peer bonds[16]; assert(cordial_profiles_bonds(bonds,16)==1);
        // No successful Pairing Complete is delivered. Teardown follows key persistence.
        uint8_t failed[14]={SM_EVENT_PAIRING_COMPLETE,12,0x40,0}; failed[11]=ERROR_CODE_REMOTE_USER_TERMINATED_CONNECTION;
        sm_handler(HCI_EVENT_PACKET,0,failed,sizeof(failed));
        uint8_t disconnected[6]={HCI_EVENT_DISCONNECTION_COMPLETE,4,0,0x40,0,0x16};
        packet_handler(HCI_EVENT_PACKET,0,disconnected,sizeof(disconnected)); cordial_profiles_poll(1);
        assert(cordial_profiles_bonds(bonds,16)==0);
        assert(!cordial_by_id((cordial_link){.generation=already_closing+1,.slot=0}));
    }
    // An unresolved new RPA can disclose an already saved identity. The native
    // stack sends Identity Created for that same database index after replacement.
    bd_addr_t saved_identity={7,8,9,10,11,12}; sm_key_t saved_irk={5},saved_ltk={6}; uint8_t saved_rand[8]={0};
    int saved_index=le_device_db_add(BD_ADDR_TYPE_LE_PUBLIC,saved_identity,saved_irk);
    le_device_db_encryption_set(saved_index,0,saved_rand,saved_ltk,16,1,0,1);
    cordial_peer saved_peer={.transport=CORDIAL_BLE}; memcpy(saved_peer.address,saved_identity,6);
    assert(has_key(saved_peer));
    paired_before_count=(unsigned)cordial_profiles_bonds(paired_before,8); assert(paired_before_count==1);
    cordial_peer new_rpa={.address={0x41,2,3,4,5,6},.transport=CORDIAL_BLE,.random=1};
    assert(!has_key(new_rpa));
    paired_before_count = (unsigned)cordial_profiles_bonds(paired_before, 8);
    assert(paired_before_count == 1);
    cordial_connection *dup=allocate((cordial_link){.generation=3,.slot=0},new_rpa,true);
    dup->handle=0x40;dup->security_requested=true;dup->closing=true;
    uint8_t disclosed[20]={SM_EVENT_IDENTITY_CREATED,18,0x40,0,1};
    reverse_bd_addr(new_rpa.address,disclosed+5);disclosed[11]=BD_ADDR_TYPE_LE_PUBLIC;
    reverse_bd_addr(saved_identity,disclosed+12);little_endian_store_16(disclosed,18,saved_index);
    sm_handler(HCI_EVENT_PACKET,0,disclosed,sizeof(disclosed));
    finish(dup);
    assert(has_key(saved_peer) && "cleanup must not delete a previously saved identity disclosed by a new unresolved RPA");

    assert(cordial_profiles_adopt(dup->id) == CORDIAL_CONNECTION);
    cordial_profiles_poll(2);
    assert(!cordial_by_id((cordial_link){.generation=3,.slot=0}));

    // Reusing an existing Classic ACL emits HID opened without HCI created.
    cordial_peer classic={.address={9,8,7,6,5,4},.transport=CORDIAL_CLASSIC};
    cordial_connection *reuse=allocate((cordial_link){.generation=4,.slot=0},classic,false);
    assert(reuse); reuse->cid=42; initiating[CORDIAL_CLASSIC]=reuse;
    uint8_t opened[15]={HCI_EVENT_HID_META,13,HID_SUBEVENT_CONNECTION_OPENED,42,0,0};
    little_endian_store_16(opened,12,0x40);
    classic_event(opened,sizeof opened);
    assert(initiating[CORDIAL_CLASSIC] == NULL);
    cordial_profiles_disconnect(reuse->id); // Unknown native handle must terminate.
    assert(reuse->ended);
    cordial_profiles_poll(3);

    incoming[0].attempt=7; incoming[0].cid=43; incoming[0].handle=0x41;
    uint8_t closed[]={HCI_EVENT_HID_META,3,HID_SUBEVENT_CONNECTION_CLOSED,43,0};
    classic_event(closed,sizeof closed);
    assert(incoming[0].attempt == 0);
    incoming[0].attempt=8;
    uint8_t disconnected[]={HCI_EVENT_DISCONNECTION_COMPLETE,4,0,0x41,0,0x16};
    packet_handler(HCI_EVENT_PACKET,0,disconnected,sizeof disconnected);
    assert(incoming[0].attempt == 0);

    cordial_connection *read=allocate((cordial_link){.generation=5,.slot=0},classic,false);
    assert(read); read->reading=true; read->sequence=37; read->operation_type=HID_REPORT_TYPE_FEATURE;
    cordial_read_done(read,CORDIAL_CONNECTION);
    assert(last_event.kind==CORDIAL_READ && last_event.number==37 && last_event.code==CORDIAL_CONNECTION && !last_event.length);
    read->reading=true; read->read_deadline=4; read->sequence=38;
    cordial_profiles_poll(4);
    assert(last_event.kind==CORDIAL_READ && last_event.number==38 && last_event.code==CORDIAL_TIMEOUT);
    assert(read->read_abandoned && !read->reading && !read->ended && !read->closing);
    read->pairing=true;
    gap_set_bondable_mode(1);
    finish(read);
    assert(!gap_get_bondable_mode());
    cordial_profiles_poll(4);
    cordial_peer other_peer=classic; other_peer.address[5]^=1;
    cordial_connection *other=allocate((cordial_link){.generation=1,.slot=1},other_peer,false);
    assert(other);
    cordial_connection *refused=allocate((cordial_link){.generation=6,.slot=0},classic,false);
    assert(refused);
    reject_descriptor=true;
    assert(!cordial_emit_event(refused, (cordial_event){.kind=CORDIAL_DESCRIPTOR}));
    assert(refused->ended && refused->error==CORDIAL_CAPACITY);
    assert(other->used && !other->closing && !other->ended);
    cordial_profiles_poll(5);
    assert(last_event.kind==CORDIAL_DISCONNECTED && last_event.code==CORDIAL_CAPACITY);
    reject_descriptor=false;
    cordial_connection *retried=allocate((cordial_link){.generation=7,.slot=0},classic,false);
    assert(retried && cordial_emit_event(retried, (cordial_event){.kind=CORDIAL_DESCRIPTOR}));
    assert(!retried->closing);
    finish(retried);
    finish(other);
    capacity_boundaries();
    pairing_preserves_normal_capacity();
    pending_classic_reserves_normal_capacity();
    puts("Native profile callback regressions passed");
}
