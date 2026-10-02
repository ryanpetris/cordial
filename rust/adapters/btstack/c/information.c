// Optional BAS, DIS and GAP reads never determine HID connection readiness.
#include "profiles_internal.h"
#include <string.h>
enum { INFO_IDLE, INFO_SERVICES, INFO_CHARACTERISTICS, INFO_READ, INFO_DESCRIPTORS, INFO_SUBSCRIBE };
static const uint16_t service_uuids[] = {0x180f, 0x180a, 0x1800};
static void info_callback(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size);
static bool battery(uint16_t uuid) { return uuid == 0x2a19 || uuid == 0x2bed || uuid == 0x2bf0 || uuid == 0x2be9; }
static bool wanted(uint16_t service, uint16_t uuid) {
    return service == 0x180f ? battery(uuid) : service == 0x1800 ? (uuid == 0x2a00 || uuid == 0x2a01) :
        ((uuid >= 0x2a24 && uuid <= 0x2a29) || uuid == 0x2a50);
}
static void observation(cordial_connection *l, cordial_info_endpoint *e, const uint8_t *bytes, uint16_t length, bool success) {
    e->retry = !cordial_emit_event(l, (cordial_event){.kind=CORDIAL_INFORMATION, .service=e->uuid,
        .report_id=e->instance, .data=bytes, .length=length, .code=success?0:1});
}
void cordial_info_clear(cordial_connection *l) {
    if(l->info.listening)gatt_client_stop_listening_for_characteristic_value_updates(&l->info.listener);
    memset(&l->info,0,sizeof l->info);
}
static void complete(cordial_connection *l, bool success) {
    cordial_information *i=&l->info;
    i->pending=false; i->yield_once=true;
    switch(i->phase) {
        case INFO_SERVICES:
            if(i->kind==0 && success)i->count_pending=true;
            if(!success)i->retry_discovery=true;
            if(++i->kind==3) { i->cursor=0; i->phase=INFO_CHARACTERISTICS; }
            break;
        case INFO_CHARACTERISTICS:
            if(!success)i->retry_discovery=true;
            if(++i->cursor==i->service_count) { i->cursor=0; i->phase=INFO_READ; }
            break;
        case INFO_READ: {
            cordial_info_endpoint *e=&i->endpoints[i->cursor];
            observation(l,e,l->bytes,success && !i->malformed ? l->length:0,success && !i->malformed);
            if(i->initial && battery(e->uuid) && e->value<e->end)
                i->phase=INFO_DESCRIPTORS;
            else ++i->cursor;
            break;
        }
        case INFO_DESCRIPTORS:
            if(success && i->cccd && (i->endpoints[i->cursor].properties & 0x30)) i->phase=INFO_SUBSCRIBE;
            else { ++i->cursor; i->phase=INFO_READ; }
            break;
        case INFO_SUBSCRIBE:
            i->endpoints[i->cursor].subscribed=success;
            ++i->cursor; i->phase=INFO_READ; break;
        default: break;
    }
}
int cordial_profiles_info_refresh(cordial_link id) {
    cordial_connection *l=cordial_by_id(id);
    if(!l || !l->ready || l->closing)return CORDIAL_CONNECTION;
    if(l->peer.transport!=CORDIAL_BLE)return CORDIAL_OK;
    cordial_information *i=&l->info;
    if(i->phase!=INFO_IDLE || i->pending) { i->refresh_pending=true; return CORDIAL_OK; }
    if(!i->discovered) { i->discovered=true; i->initial=true; i->phase=INFO_SERVICES; i->kind=0; }
    else { i->initial=false; i->phase=INFO_READ; }
    i->cursor=0; i->battery_only=false;
    return CORDIAL_OK;
}
int cordial_profiles_info_busy(cordial_link id) {
    cordial_connection *l=cordial_by_id(id);
    return l && l->peer.transport==CORDIAL_BLE && (l->info.pending || l->info.phase!=INFO_IDLE || l->info.refresh_pending);
}
bool cordial_info_settled(const cordial_connection *l) {
    return l->info.discovered && l->info.phase==INFO_IDLE && !l->info.pending;
}
void cordial_info_poll(cordial_connection *l, uint64_t now) {
    if(!l->ready || l->closing || l->peer.transport!=CORDIAL_BLE || l->query || l->setup==SETUP_SUBSCRIBE || l->writing || l->reading)return;
    cordial_information *i=&l->info;
    if(i->pending)return;
    if(i->yield_once) { i->yield_once=false; return; }
    if(i->count_pending) {
        if(!cordial_emit_event(l,(cordial_event){.kind=CORDIAL_INFORMATION,.service=0x180f,.data=&i->batteries,.length=1}))return;
        i->count_pending=false;
    }
    if(i->phase==INFO_IDLE) {
        if(now<i->due && !i->refresh_pending)return;
        if(i->retry_discovery)cordial_info_clear(l);
        bool discovered=i->discovered;
        (void)cordial_profiles_info_refresh(l->id);
        i->battery_only=discovered && !i->refresh_pending;
        i->refresh_pending=false;
        return;
    }
    if(i->phase==INFO_CHARACTERISTICS && !i->service_count) { i->phase=INFO_READ; i->cursor=0; }
    if(i->phase==INFO_READ) {
        while(i->cursor<i->count && i->battery_only && (!battery(i->endpoints[i->cursor].uuid) || (i->endpoints[i->cursor].subscribed && !i->endpoints[i->cursor].retry)))++i->cursor;
        if(i->cursor==i->count) { i->phase=INFO_IDLE; i->initial=false; i->due=now+60000; return; }
    }
    i->pending=true; i->malformed=false;
    uint8_t status=0;
    switch(i->phase) {
        case INFO_SERVICES:
            status=gatt_client_discover_primary_services_by_uuid16(info_callback,l->handle,service_uuids[i->kind]);break;
        case INFO_CHARACTERISTICS: {
            cordial_info_service *s=&i->services[i->cursor];
            gatt_client_service_t service={.start_group_handle=s->start,.end_group_handle=s->end};
            status=gatt_client_discover_characteristics_for_service(info_callback,l->handle,&service);break;
        }
        case INFO_READ:
            l->length=0;
            status=gatt_client_read_long_value_of_characteristic_using_value_handle(info_callback,l->handle,i->endpoints[i->cursor].value);break;
        case INFO_DESCRIPTORS: {
            cordial_info_endpoint *e=&i->endpoints[i->cursor];
            gatt_client_characteristic_t c={.value_handle=e->value,.end_handle=e->end};
            i->cccd=0;
            status=gatt_client_discover_characteristic_descriptors(info_callback,l->handle,&c);break;
        }
        case INFO_SUBSCRIBE: {
            if(!i->listening) {
                gatt_client_listen_for_characteristic_value_updates(&i->listener,info_callback,l->handle,NULL);
                i->listening=true;
            }
            i->subscription[0]=(i->endpoints[i->cursor].properties & 0x10)?1:2;
            i->subscription[1]=0;
            status=gatt_client_write_value_of_characteristic(info_callback,l->handle,i->cccd,2,i->subscription);break;
        }
        default: i->pending=false; return;
    }
    if(status)complete(l,false);
}
static void info_callback(uint8_t type, uint16_t channel, uint8_t *packet, uint16_t size) {
    (void)channel;
    if(type!=HCI_EVENT_PACKET || size<4)return;
    cordial_connection *l=cordial_by_handle(little_endian_read_16(packet,2));
    if(!l || l->closing || !l->ready)return;
    cordial_information *i=&l->info;
    uint8_t event=hci_event_packet_get_type(packet);
    if(event==GATT_EVENT_NOTIFICATION || event==GATT_EVENT_INDICATION) {
        if(size<12)return;
        uint16_t handle=gatt_event_notification_get_value_handle(packet);
        uint16_t length=gatt_event_notification_get_value_length(packet);
        const uint8_t *data=gatt_event_notification_get_value(packet);
        if(data+length>packet+size || length>CORDIAL_REPORT_BYTES)return;
        for(unsigned n=0;n<i->count;n++)if(i->endpoints[n].value==handle && battery(i->endpoints[n].uuid)) {
            observation(l,&i->endpoints[n],data,length,true);break;
        }
        return;
    }
    if(!i->pending)return;
    switch(event) {
        case GATT_EVENT_SERVICE_QUERY_RESULT: {
            if(i->phase!=INFO_SERVICES || i->service_count==6)return;
            gatt_client_service_t s;gatt_event_service_query_result_get_service(packet,&s);
            if(!s.start_group_handle || s.end_group_handle<s.start_group_handle)return;
            if(i->kind==0 && i->batteries==4)return;
            i->services[i->service_count++]=(cordial_info_service){.start=s.start_group_handle,.end=s.end_group_handle,
                .kind=i->kind,.instance=i->kind==0?i->batteries++:0};break;
        }
        case GATT_EVENT_CHARACTERISTIC_QUERY_RESULT: {
            if(i->phase!=INFO_CHARACTERISTICS || i->count==32)return;
            cordial_info_service *s=&i->services[i->cursor];
            gatt_client_characteristic_t c;gatt_event_characteristic_query_result_get_characteristic(packet,&c);
            if(c.value_handle<s->start || c.end_handle>s->end || c.end_handle<c.value_handle)return;
            if(wanted(service_uuids[s->kind],c.uuid16) && (c.properties & ATT_PROPERTY_READ))
                i->endpoints[i->count++]=(cordial_info_endpoint){.value=c.value_handle,.end=c.end_handle,.uuid=c.uuid16,
                    .properties=(uint8_t)c.properties,.instance=s->instance};
            break;
        }
        case GATT_EVENT_ALL_CHARACTERISTIC_DESCRIPTORS_QUERY_RESULT: {
            if(i->phase!=INFO_DESCRIPTORS)return;
            gatt_client_characteristic_descriptor_t d;gatt_event_all_characteristic_descriptors_query_result_get_characteristic_descriptor(packet,&d);
            cordial_info_endpoint *e=&i->endpoints[i->cursor];
            if(d.uuid16==0x2902 && d.handle>e->value && d.handle<=e->end)i->cccd=d.handle;
            if(d.uuid16==0x2904 && e->uuid==0x2a19 && d.handle>e->value && d.handle<=e->end && i->count<32)
                i->endpoints[i->count++]=(cordial_info_endpoint){.value=d.handle,.end=d.handle,.uuid=0x2904,.properties=ATT_PROPERTY_READ,.instance=e->instance};
            break;
        }
        case GATT_EVENT_LONG_CHARACTERISTIC_VALUE_QUERY_RESULT: {
            if(i->phase!=INFO_READ)return;
            uint16_t offset=gatt_event_long_characteristic_value_query_result_get_value_offset(packet);
            uint16_t length=gatt_event_long_characteristic_value_query_result_get_value_length(packet);
            const uint8_t *data=gatt_event_long_characteristic_value_query_result_get_value(packet);
            if(offset!=l->length || offset>sizeof l->bytes || length>sizeof l->bytes-offset || data+length>packet+size) {
                i->malformed=true;return;
            }
            memcpy(l->bytes+offset,data,length);l->length+=length;break;
        }
        case GATT_EVENT_QUERY_COMPLETE:
            complete(l,gatt_event_query_complete_get_att_status(packet)==0);break;
        default: break;
    }
}
