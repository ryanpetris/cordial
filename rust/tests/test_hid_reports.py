"""Exercise native HID report admission and transport error classifications."""
import unittest

from test_native_ble import ROOT, run_c


class HidReportTests(unittest.TestCase):
    def test_ble_att_errors_keep_capacity_and_capability_distinct(self):
        source = (ROOT / "platforms/esp32s3/components/platform/nimble.c").read_text()
        helper = source[source.index("static uint8_t operation_error("):source.index("static void complete(")]
        run_c(r'''
#include <assert.h>
#include "ble.h"
#define BLE_HS_ENOTSUP 1
#define BLE_HS_EMSGSIZE 2
#define BLE_HS_ENOMEM 3
#define BLE_HS_ETIMEOUT 4
#define BLE_ATT_ERR_READ_NOT_PERMITTED 2
#define BLE_ATT_ERR_WRITE_NOT_PERMITTED 3
#define BLE_ATT_ERR_REQ_NOT_SUPPORTED 6
#define BLE_ATT_ERR_ATTR_NOT_LONG 11
#define BLE_ATT_ERR_INVALID_ATTR_VALUE_LEN 13
#define BLE_HS_ATT_ERR(x) (0x100+(x))
''' + helper + r'''
int main(void) {
    assert(operation_error(0)==CORDIAL_BLE_OK);
    assert(operation_error(BLE_HS_ENOTSUP)==CORDIAL_BLE_UNSUPPORTED);
    const int unsupported[]={2,3,6,11};
    for(unsigned i=0;i<sizeof unsupported/sizeof *unsupported;i++)
        assert(operation_error(BLE_HS_ATT_ERR(unsupported[i]))==CORDIAL_BLE_UNSUPPORTED);
    assert(operation_error(BLE_HS_EMSGSIZE)==CORDIAL_BLE_REPORT_SIZE);
    assert(operation_error(BLE_HS_ATT_ERR(13))==CORDIAL_BLE_REPORT_SIZE);
    assert(operation_error(BLE_HS_ENOMEM)==CORDIAL_BLE_CAPACITY);
    assert(operation_error(BLE_HS_ETIMEOUT)==CORDIAL_BLE_TIMEOUT);
    assert(operation_error(BLE_HS_ATT_ERR(14))==CORDIAL_BLE_CONNECTION);
}
''')

    def test_descriptor_reads_span_reports_and_ignore_late_callbacks(self):
        source = (ROOT / "platforms/esp32s3/components/platform/nimble.c").read_text()
        helper = source[source.index("static int read_callback("):
                        source.index("// Read Using Characteristic UUID")]
        run_c(r'''
#include <assert.h>
#include <string.h>
#include "ble.h"
#define BLE_HS_EDONE 1
#define BLE_HS_EMSGSIZE 2
#define BLE_ATT_ERR_ATTR_NOT_LONG 11
#define BLE_HS_ATT_ERR(x) (0x100+(x))
typedef struct {uint32_t read_request;uint16_t read_length;} link;
static link connection;
struct ble_gatt_error {int status;};
struct os_mbuf {uint16_t length;uint8_t data[513];};
struct ble_gatt_attr {uint16_t offset;struct os_mbuf *om;};
#define OS_MBUF_PKTLEN(m) ((m)->length)
static int os_mbuf_copydata(struct os_mbuf *m,int offset,int length,void *data) {memcpy(data,m->data+offset,length);return 0;}
static link *by_handle(uint16_t conn) {return conn==7?&connection:NULL;}
static unsigned chunks,completions;
static int completed_status;
static void complete(uint32_t request,int status) {assert(request==9);completions++;completed_status=status;}
static void emit(const cordial_ble_event *e) {assert(e->kind==CORDIAL_BLE_DATA && e->request==9 && e->length<=512);chunks++;}
''' + helper + r'''
int main(void) {
    struct ble_gatt_error error={0};
    struct os_mbuf buffer={.length=512};
    struct ble_gatt_attr attr={.om=&buffer};
    connection.read_request=9;
    for(unsigned i=0;i<4;i++) {
        attr.offset=i*512;
        assert(!read_callback(7,&error,&attr,(void *)(uintptr_t)9));
    }
    assert(connection.read_length==2048 && chunks==4 && !completions);
    error.status=BLE_HS_ATT_ERR(BLE_ATT_ERR_ATTR_NOT_LONG);
    assert(!read_callback(7,&error,NULL,(void *)(uintptr_t)9));
    assert(completions==1 && !completed_status && !connection.read_request);
    assert(!read_callback(7,&error,NULL,(void *)(uintptr_t)9) && completions==1);
    connection=(link){.read_request=9,.read_length=2048};error.status=0;buffer.length=1;attr.offset=2048;
    assert(read_callback(7,&error,&attr,(void *)(uintptr_t)9)==BLE_HS_EMSGSIZE);
    assert(!connection.read_request && completed_status==BLE_HS_EMSGSIZE);
    connection=(link){.read_request=9};attr.offset=1;
    assert(read_callback(7,&error,&attr,(void *)(uintptr_t)9)==BLE_HS_EMSGSIZE);
    connection=(link){.read_request=9};attr.offset=0;buffer.length=513;
    assert(read_callback(7,&error,&attr,(void *)(uintptr_t)9)==BLE_HS_EMSGSIZE);
    assert(!connection.read_request);
    connection=(link){.read_request=10};
    assert(!read_callback(7,&error,&attr,(void *)(uintptr_t)9) && connection.read_request==10);
}
''')

    def test_uuid_reads_deliver_the_first_value_and_separate_peer_errors(self):
        source = (ROOT / "platforms/esp32s3/components/platform/nimble.c").read_text()
        errors = source[source.index("static uint8_t operation_error("):source.index("static void complete(")]
        helper = source[source.index("static void complete_uuid("):source.index("static int write_callback(")]
        run_c(r'''
#include <assert.h>
#include <stdbool.h>
#include <string.h>
#include "ble.h"
#define BLE_HS_EDONE 1
#define BLE_HS_EMSGSIZE 2
#define BLE_HS_ENOMEM 3
#define BLE_HS_ETIMEOUT 4
#define BLE_HS_ENOTSUP 5
#define BLE_HS_ENOTCONN 6
#define BLE_ATT_ERR_ATTR_NOT_FOUND 10
#define BLE_ATT_ERR_READ_NOT_PERMITTED 2
#define BLE_ATT_ERR_WRITE_NOT_PERMITTED 3
#define BLE_ATT_ERR_INSUFFICIENT_AUTHOR 8
#define BLE_ATT_ERR_REQ_NOT_SUPPORTED 6
#define BLE_ATT_ERR_ATTR_NOT_LONG 11
#define BLE_ATT_ERR_INVALID_ATTR_VALUE_LEN 13
#define BLE_HS_ERR_ATT_BASE 0x100
#define BLE_HS_ERR_HCI_BASE 0x200
#define BLE_HS_ATT_ERR(x) (BLE_HS_ERR_ATT_BASE+(x))
typedef struct {uint32_t read_request;bool read_found;} link;
static link connection;
struct ble_gatt_error {int status;};
struct os_mbuf {uint16_t length;uint8_t data[513];};
struct ble_gatt_attr {uint16_t offset;struct os_mbuf *om;};
#define OS_MBUF_PKTLEN(m) ((m)->length)
static int os_mbuf_copydata(struct os_mbuf *m,int offset,int length,void *data) {memcpy(data,m->data+offset,length);return 0;}
static link *by_handle(uint16_t conn) {return conn==7?&connection:NULL;}
static unsigned values,completions;
static uint8_t code,first;
static void emit(const cordial_ble_event *e) {
    assert(e->request==9);
    if(e->kind==CORDIAL_BLE_DATA) {assert(e->offset==0 && e->length==16);first=e->data[0];values++;}
    else {assert(e->kind==CORDIAL_BLE_COMPLETE);code=e->code;completions++;}
}
''' + errors + helper + r'''
static void finish(int status) {
    struct ble_gatt_error error={status};
    connection.read_request=9;connection.read_found=false;
    assert(!uuid_callback(7,&error,NULL,(void *)(uintptr_t)9) && !connection.read_request);
}
int main(void) {
    struct ble_gatt_error error={0};
    struct os_mbuf buffer={.length=16,.data={1}};
    struct ble_gatt_attr attr={.om=&buffer};
    connection.read_request=9;
    assert(!uuid_callback(7,&error,&attr,(void *)(uintptr_t)9));
    buffer.data[0]=2;
    assert(!uuid_callback(7,&error,&attr,(void *)(uintptr_t)9));
    assert(values==1 && first==1 && !completions);
    error.status=BLE_HS_EDONE;
    assert(!uuid_callback(7,&error,NULL,(void *)(uintptr_t)9));
    assert(completions==1 && code==CORDIAL_BLE_OK && !connection.read_request);
    assert(!uuid_callback(7,&error,NULL,(void *)(uintptr_t)9) && completions==1);
    // Any ATT error response from the peer means it offers no readable value.
    const int peer[]={BLE_ATT_ERR_ATTR_NOT_FOUND,BLE_ATT_ERR_READ_NOT_PERMITTED,
        BLE_ATT_ERR_INSUFFICIENT_AUTHOR,BLE_ATT_ERR_REQ_NOT_SUPPORTED,BLE_ATT_ERR_INVALID_ATTR_VALUE_LEN,0xff};
    for(unsigned i=0;i<sizeof peer/sizeof *peer;i++) {
        finish(BLE_HS_ATT_ERR(peer[i]));assert(code==CORDIAL_BLE_UNSUPPORTED);
    }
    // Host and transport failures never look like a missing value.
    const int host[]={BLE_HS_ETIMEOUT,BLE_HS_ENOTCONN,BLE_HS_ENOMEM,BLE_HS_ENOTSUP,BLE_HS_EMSGSIZE,0x208};
    for(unsigned i=0;i<sizeof host/sizeof *host;i++) {
        finish(host[i]);assert(code!=CORDIAL_BLE_UNSUPPORTED && code!=CORDIAL_BLE_OK);
    }
    finish(BLE_HS_ETIMEOUT);assert(code==CORDIAL_BLE_TIMEOUT);
    finish(BLE_HS_ENOTSUP);assert(code==CORDIAL_BLE_CONNECTION);
    complete_uuid(9,BLE_HS_ENOMEM);assert(code==CORDIAL_BLE_CAPACITY);
    connection=(link){.read_request=9};buffer.length=513;error.status=0;
    assert(uuid_callback(7,&error,&attr,(void *)(uintptr_t)9)==BLE_HS_EMSGSIZE);
    assert(!connection.read_request && code==CORDIAL_BLE_REPORT_SIZE);
    connection=(link){.read_request=10};buffer.length=16;
    unsigned before=values+completions;
    assert(!uuid_callback(7,&error,&attr,(void *)(uintptr_t)9) && values+completions==before);
}
''')

    def test_classic_handshakes_distinguish_busy_unsupported_and_connection_errors(self):
        source = (ROOT / "adapters/btstack/c/profiles.c").read_text()
        helper = source[source.index("static uint8_t classic_report_error("):source.index("static void classic_event(")]
        profiles = ROOT / "adapters/btstack/c/profiles.h"
        hid = ROOT.parent / ".cache/dependencies/btstack/src/btstack_hid.h"
        if not hid.is_file():
            self.skipTest("pinned Bluetooth headers are unavailable")
        run_c(f'#include <assert.h>\n#include "{profiles}"\n#include "{hid}"\n' + helper + r'''
int main(void) {
    assert(classic_report_error(HID_HANDSHAKE_PARAM_TYPE_SUCCESSFUL)==CORDIAL_OK);
    assert(classic_report_error(HID_HANDSHAKE_PARAM_TYPE_NOT_READY)==CORDIAL_BUSY);
    assert(classic_report_error(HID_HANDSHAKE_PARAM_TYPE_ERR_INVALID_REPORT_ID)==CORDIAL_UNSUPPORTED);
    assert(classic_report_error(HID_HANDSHAKE_PARAM_TYPE_ERR_UNSUPPORTED_REQUEST)==CORDIAL_UNSUPPORTED);
    assert(classic_report_error(HID_HANDSHAKE_PARAM_TYPE_ERR_INVALID_PARAMETER)==CORDIAL_UNSUPPORTED);
    assert(classic_report_error(HID_HANDSHAKE_PARAM_TYPE_ERR_FATAL)==CORDIAL_CONNECTION);
}
''')

    def test_classic_long_reports_keep_the_payload_until_ack_and_require_room_below_mtu(self):
        source = (ROOT / "adapters/btstack/c/classic_output.c").read_text()
        helper = source[source.index("int cordial_classic_set_report("):]
        profiles = ROOT / "adapters/btstack/c/profiles.h"
        run_c(f'#include <assert.h>\n#include "{profiles}"\n' + r'''
#define HID_REPORT_ID_UNDEFINED 65535
#define HID_HOST_CONNECTION_ESTABLISHED 1
#define HID_HOST_W2_SEND_SET_REPORT 2
typedef int hid_report_type_t;
typedef struct {int state;uint16_t control_cid,report_id,report_len;hid_report_type_t report_type;const uint8_t *report;} hid_host_connection_t;
static hid_host_connection_t connection;
static unsigned remote_mtu=514,local_mtu=1024,requests;
static bool present=true;
static hid_host_connection_t *hid_host_get_connection_for_hid_cid(uint16_t cid) {assert(cid==9);return present?&connection:NULL;}
static unsigned l2cap_get_remote_mtu_for_local_cid(uint16_t cid) {assert(cid==7);return remote_mtu;}
static unsigned l2cap_max_mtu(void) {return local_mtu;}
static void l2cap_request_can_send_now_event(uint16_t cid) {assert(cid==7);requests++;}
''' + helper + r'''
int main(void) {
    uint8_t payload[512]={0};
    connection=(hid_host_connection_t){.state=HID_HOST_CONNECTION_ESTABLISHED,.control_cid=7};
    assert(cordial_classic_set_report(9,2,1,payload,512)==CORDIAL_REPORT_SIZE);
    assert(!requests && !connection.report);
    remote_mtu=515;
    assert(cordial_classic_set_report(9,2,1,payload,512)==CORDIAL_OK);
    assert(requests==1 && connection.report==payload && connection.report_len==512);
    assert(connection.report_id==1 && connection.report_type==2);
    assert(cordial_classic_set_report(9,2,1,payload,1)==CORDIAL_BUSY);
    assert(requests==1 && connection.report_len==512);
    connection.state=HID_HOST_CONNECTION_ESTABLISHED;
    local_mtu=513;
    assert(cordial_classic_set_report(9,2,1,payload,512)==CORDIAL_REPORT_SIZE);
    remote_mtu=514;
    assert(cordial_classic_set_report(9,2,HID_REPORT_ID_UNDEFINED,payload,512)==CORDIAL_OK);
    present=false;
    assert(cordial_classic_set_report(9,2,1,payload,1)==CORDIAL_CONNECTION);
}
''')


if __name__ == "__main__":
    unittest.main()
