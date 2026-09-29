// Exercise optional service callbacks without a radio or hardware.
#define gatt_client_discover_primary_services_by_uuid16 query_services
#define gatt_client_discover_characteristics_for_service query_characteristics
#define gatt_client_read_long_value_of_characteristic_using_value_handle query_read
#define gatt_client_discover_characteristic_descriptors query_descriptors
#define gatt_client_write_value_of_characteristic write_request
#define gatt_client_listen_for_characteristic_value_updates listen_updates
#define gatt_client_stop_listening_for_characteristic_value_updates stop_updates
#include "../c/information.c"
#include <assert.h>
#include <stdio.h>
static cordial_connection link;
static uint16_t requested;
static unsigned observations, subscriptions, listeners;
static uint8_t percent;
static bool read_success, reject_count;
cordial_connection *cordial_by_handle(hci_con_handle_t handle) { return handle==link.handle?&link:NULL; }
cordial_connection *cordial_by_id(cordial_link id) { return id.generation==link.id.generation?&link:NULL; }
int cordial_emit_event(cordial_connection *l, cordial_event e) {
    assert(l==&link && e.kind==CORDIAL_INFORMATION);
    if(e.service==0x180f) { assert(e.length==1 && e.data[0]==1); return !reject_count; }
    assert(e.service==0x2a19);
    observations++;percent=e.length?e.data[0]:255;read_success=e.code==0;return 1;
}
uint8_t query_services(btstack_packet_handler_t cb,hci_con_handle_t h,uint16_t uuid) {
    (void)cb;(void)h;requested=uuid;return 0;
}
uint8_t query_characteristics(btstack_packet_handler_t cb,hci_con_handle_t h,gatt_client_service_t *s) {
    (void)cb;(void)h;requested=s->start_group_handle;return 0;
}
uint8_t query_read(btstack_packet_handler_t cb,hci_con_handle_t h,uint16_t value) {
    (void)cb;(void)h;requested=value;return 0;
}
uint8_t query_descriptors(btstack_packet_handler_t cb,hci_con_handle_t h,gatt_client_characteristic_t *c) {
    (void)cb;(void)h;requested=c->value_handle;return 0;
}
uint8_t write_request(btstack_packet_handler_t cb,hci_con_handle_t h,uint16_t handle,uint16_t n,uint8_t *p) {
    (void)cb;(void)h;assert(handle==4 && n==2 && p[0]==1 && p[1]==0);subscriptions++;return 0;
}
void listen_updates(gatt_client_notification_t *n,btstack_packet_handler_t cb,hci_con_handle_t h,gatt_client_characteristic_t *c) {
    (void)n;(void)cb;(void)h;assert(!c);listeners++;
}
void stop_updates(gatt_client_notification_t *n) { (void)n;listeners--; }
static void uuid(uint8_t *out,uint16_t value) {
    uint8_t full[16];uuid_add_bluetooth_prefix(full,value);reverse_128(full,out);
}
static void done(uint8_t status) {
    uint8_t e[]={GATT_EVENT_QUERY_COMPLETE,7,0x40,0,0,0,0,0,status};
    info_callback(HCI_EVENT_PACKET,0,e,sizeof e);
}
static void poll_info(uint64_t now) {
    cordial_info_poll(&link,now);
    // Optional transactions leave a complete backend poll free for HID output.
    if(!link.info.pending)cordial_info_poll(&link,now);
}
static void discover(void) {
    memset(&link,0,sizeof link);link.handle=0x40;link.used=true;link.ready=true;
    link.id.generation=1;link.peer.transport=CORDIAL_BLE;
    poll_info(1);assert(requested==0x180f && cordial_profiles_info_busy(link.id));
    uint8_t s[28]={GATT_EVENT_SERVICE_QUERY_RESULT,26,0x40,0};
    little_endian_store_16(s,8,1);little_endian_store_16(s,10,9);uuid(s+12,0x180f);
    info_callback(HCI_EVENT_PACKET,0,s,sizeof s);done(0);
    reject_count=true;requested=0;poll_info(2);
    assert(link.info.count_pending && !link.info.pending && !requested);
    reject_count=false;
    poll_info(2);assert(requested==0x180a);done(0x0a);
    poll_info(3);assert(requested==0x1800);done(0);
    poll_info(4);assert(requested==1);
    uint8_t c[32]={GATT_EVENT_CHARACTERISTIC_QUERY_RESULT,30,0x40,0};
    little_endian_store_16(c,8,2);little_endian_store_16(c,10,3);little_endian_store_16(c,12,9);
    little_endian_store_16(c,14,ATT_PROPERTY_READ|ATT_PROPERTY_NOTIFY);uuid(c+16,0x2a19);
    info_callback(HCI_EVENT_PACKET,0,c,sizeof c);done(0);
}
int main(void) {
    discover();poll_info(5);assert(requested==3);
    uint8_t value[15]={GATT_EVENT_LONG_CHARACTERISTIC_VALUE_QUERY_RESULT,13,0x40,0};
    little_endian_store_16(value,12,1);value[14]=51;
    info_callback(HCI_EVENT_PACKET,0,value,sizeof value);done(0);
    cordial_info_poll(&link,6);
    assert(!link.info.pending && link.info.phase!=INFO_IDLE); // HID may claim the slot.
    assert(observations==1 && percent==51);
    poll_info(6);assert(requested==3);
    uint8_t d[26]={GATT_EVENT_ALL_CHARACTERISTIC_DESCRIPTORS_QUERY_RESULT,24,0x40,0};
    little_endian_store_16(d,8,4);uuid(d+10,0x2902);
    info_callback(HCI_EVENT_PACKET,0,d,sizeof d);done(0);
    poll_info(7);assert(subscriptions==1 && listeners==1);done(0);
    poll_info(8);assert(!cordial_profiles_info_busy(link.id));
    uint8_t notification[13]={GATT_EVENT_NOTIFICATION,11,0x40,0,0,0,0,0,3,0,1,0,50};
    info_callback(HCI_EVENT_PACKET,0,notification,sizeof notification);
    assert(percent==50 && observations==2);
    assert(cordial_profiles_info_refresh(link.id)==CORDIAL_OK);
    // HID requests keep the ATT slot until completed; optional refresh waits.
    link.writing=true;requested=0;poll_info(9);assert(!requested);
    link.writing=false;poll_info(10);assert(requested==3);done(1);
    assert(percent==255 && !read_success && !link.closing);poll_info(11);
    cordial_info_clear(&link);assert(!listeners);
    puts("Optional information discovery, notification, failure and ATT serialization checks passed");
}
