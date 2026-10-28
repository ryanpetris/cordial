"""Exercise native privacy changes against asynchronous initiation and HCI failures."""
import unittest

from test_native_ble import ROOT, run_c


PREFIX = r'''
#include <assert.h>
#include <stdbool.h>
#include <string.h>
#include "ble.h"
enum { BLE_HS_ENOMEM=1, BLE_HS_ENOENT=2, BLE_HS_EBUSY=3, BLE_HS_ESTORE_FAIL=4,
    BLE_HS_EBADDATA=5, BLE_HS_ETIMEOUT=6, BLE_HS_ETIMEOUT_HCI=7, BLE_HS_EALREADY=8,
    BLE_HS_EPREEMPTED=9, BLE_HS_ENOTSYNCED=10, BLE_ERR_MEM_CAPACITY=7,
    BLE_ERR_CMD_DISALLOWED=12, BLE_ERR_CONN_SPVN_TMO=8, BLE_ERR_UNK_CONN_ID=2,
    BLE_STORE_OBJ_TYPE_PEER_SEC=1 };
#define BLE_HS_HCI_ERR(x) (0x200+(x))
typedef struct {uint8_t type,val[6];} ble_addr_t;
struct ble_gap_conn_desc {int unused;};
'''


class PrivacyTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.native = (ROOT / "platforms/esp32s3/components/platform/nimble.c").read_text()

    def test_controller_removal_precedes_host_deletion_and_failure_is_retryable(self):
        unpair = self.native[self.native.index("static int unpair_view("):
                             self.native.index("static int start_scan(")]
        run_c(PREFIX + r'''
union ble_store_key {struct {ble_addr_t peer_addr;} sec;};
union ble_store_value {struct {bool irk_present;} sec;};
static bool live,stored=true,has_irk=true,controller=true,dirty;
static int removal_error,delete_error,read_error,removes,deletes;
static int ble_gap_conn_find_by_addr(const ble_addr_t *p,struct ble_gap_conn_desc *d) {
    (void)p;(void)d;return live?0:BLE_HS_ENOENT;
}
static bool cordial_bonds_dirty(const ble_addr_t *p) {(void)p;return dirty;}
static int cordial_bonds_read(int type,const union ble_store_key *key,union ble_store_value *value) {
    assert(type==BLE_STORE_OBJ_TYPE_PEER_SEC && key->sec.peer_addr.type==1);
    if(read_error)return read_error;
    value->sec.irk_present=has_irk;return stored?0:BLE_HS_ENOENT;
}
static int ble_hs_pvcy_remove_entry(uint8_t type,const uint8_t *address) {
    assert(type==1 && address[0]==42);removes++;
    if(removal_error)return removal_error;
    if(!controller)return BLE_HS_HCI_ERR(BLE_ERR_UNK_CONN_ID);
    controller=false;return 0;
}
static int ble_store_util_delete_peer(const ble_addr_t *p) {
    (void)p;assert(!controller || !has_irk);deletes++;
    if(delete_error)return delete_error;
    stored=false;return 0;
}
static void cordial_bonds_clean(const ble_addr_t *p) {(void)p;dirty=false;}
''' + unpair + r'''
int main(void) {
    ble_addr_t p={.type=1,.val={42}};
    live=true;assert(unpair_view(&p)==BLE_HS_EBUSY && !removes && !deletes);
    live=false;removal_error=BLE_HS_HCI_ERR(BLE_ERR_CMD_DISALLOWED);
    assert(unpair_view(&p)==removal_error && stored && controller && !deletes);
    removal_error=0;assert(!unpair_view(&p) && !stored && !controller && deletes==1);
    assert(!unpair_view(&p) && removes==2 && deletes==2);
    // A failed replacement retains its dirty identity even if host keys are absent.
    dirty=true;controller=true;removal_error=0x212;
    assert(unpair_view(&p)==0x212 && !stored && dirty && controller && deletes==2);
    removal_error=0;delete_error=BLE_HS_ESTORE_FAIL;
    assert(unpair_view(&p)==delete_error && !stored && dirty && !controller);
    // Removing the already absent controller entry is an idempotent retry.
    delete_error=0;assert(!unpair_view(&p) && !stored && !dirty && !controller);
    stored=true;has_irk=false;int before=removes;
    assert(!unpair_view(&p) && removes==before);
    stored=dirty=has_irk=controller=true;before=removes;int deleted=deletes;
    read_error=BLE_HS_ESTORE_FAIL;assert(unpair_view(&p)==BLE_HS_ESTORE_FAIL);
    assert(stored && dirty && has_irk && controller && removes==before && deletes==deleted);
}
''')

    def test_privacy_commands_wait_for_auto_completion_without_reordering_commands(self):
        cancel = self.native[self.native.index("static int cancel_auto("):
                             self.native.index("static void resume_radio(void) {")]
        process = self.native[self.native.index("static bool changes_privacy("):
                              self.native.index("static void synced(")]
        run_c(PREFIX + r'''
#define pdTRUE 1
struct ble_npl_event {int unused;};
union ble_store_key {struct {ble_addr_t peer_addr;} sec;};
union ble_store_value {struct {bool irk_present;} sec;};
static bool is_synced=true,auto_active,auto_cancel,initiating,live,equal,dirty,has_irk=true,stored=true;
static int commands,cancel_error,cancels,resumes,executed,failed,last_status,read_error;
static cordial_ble_command queue[8];static unsigned count,order[8];
static int xQueuePeek(int q,cordial_ble_command *c,int wait) {
    (void)q;(void)wait;if(!count)return 0;*c=queue[0];return pdTRUE;
}
static int xQueueReceive(int q,cordial_ble_command *c,int wait) {
    if(!xQueuePeek(q,c,wait))return 0;
    memmove(queue,queue+1,--count*sizeof(*queue));return pdTRUE;
}
static int ble_gap_conn_cancel(void) {assert(auto_active);cancels++;return cancel_error;}
static int ble_gap_conn_active(void) {return initiating;}
static int ble_gap_conn_find_by_addr(const ble_addr_t *p,struct ble_gap_conn_desc *d) {
    (void)p;(void)d;return live?0:BLE_HS_ENOENT;
}
static bool cordial_bonds_same_privacy(const uint8_t *data,unsigned length) {(void)data;(void)length;return equal;}
static bool cordial_bonds_dirty(const ble_addr_t *p) {(void)p;return dirty;}
static ble_addr_t address(cordial_ble_peer p) {ble_addr_t a={.type=p.random};for(unsigned i=0;i<6;i++)a.val[i]=p.address[5-i];return a;}
static int cordial_bonds_read(int type,const union ble_store_key *key,union ble_store_value *value) {
    assert(type==BLE_STORE_OBJ_TYPE_PEER_SEC);(void)key;
    if(read_error)return read_error;
    value->sec.irk_present=has_irk;return stored?0:BLE_HS_ENOENT;
}
static void bond_complete(const cordial_ble_command *c,int status) {assert(c->request);failed++;last_status=status;}
static void command(const cordial_ble_command *c) {order[executed++]=c->kind;}
static void resume_radio(void) {assert(!count);resumes++;}
''' + cancel + process + r'''
int main(void) {
    queue[0]=(cordial_ble_command){.kind=CORDIAL_BLE_FORGET,.request=1};
    queue[1]=(cordial_ble_command){.kind=CORDIAL_BLE_CONNECT,.request=2};count=2;
    auto_active=initiating=true;
    process_commands(NULL);assert(count==2 && cancels==1 && auto_cancel && !executed && !resumes);
    // HCI completion alone does not retire the app's automatic initiation.
    initiating=false;process_commands(NULL);assert(count==2 && cancels==1 && !executed);
    auto_active=auto_cancel=false;process_commands(NULL);
    assert(!count && executed==2 && order[0]==CORDIAL_BLE_FORGET && order[1]==CORDIAL_BLE_CONNECT && resumes==1);
    // Controller completion can win the race with cancellation submission.
    queue[0]=(cordial_ble_command){.kind=CORDIAL_BLE_FORGET,.request=3};count=1;
    auto_active=true;cancel_error=BLE_HS_EALREADY;process_commands(NULL);
    assert(count==1 && auto_cancel && cancels==2 && executed==2);
    auto_active=auto_cancel=false;process_commands(NULL);assert(executed==3);
    count=1;auto_active=true;cancel_error=BLE_HS_HCI_ERR(BLE_ERR_CMD_DISALLOWED);
    process_commands(NULL);assert(count==1 && auto_cancel && cancels==3 && executed==3);
    auto_active=auto_cancel=false;process_commands(NULL);assert(executed==4);
    // A real cancellation failure returns that failure without deleting anything.
    count=1;auto_active=true;cancel_error=BLE_HS_ENOMEM;process_commands(NULL);
    assert(!count && failed==1 && last_status==BLE_HS_ENOMEM && executed==4);
    // An unrelated manual connection attempt is preserved and the caller can retry.
    count=1;auto_active=false;initiating=true;process_commands(NULL);
    assert(!count && failed==2 && last_status==BLE_HS_EBUSY && executed==4);
    cordial_ble_command c={.kind=CORDIAL_BLE_IMPORT,.length=145,.request=4};
    c.data[8]=2;c.data[9]=1;equal=true;
    assert(!changes_privacy(&c));dirty=true;assert(changes_privacy(&c));
    live=true;assert(!changes_privacy(&c));live=false;
    c.length=144;assert(!changes_privacy(&c));c.length=145;
    // A late successful connection uses the live-link import path, not removal.
    queue[0]=c;count=1;auto_active=true;initiating=false;cancel_error=0;
    process_commands(NULL);assert(count==1 && auto_cancel);
    auto_active=auto_cancel=false;live=true;process_commands(NULL);
    assert(!count && executed==5 && order[4]==CORDIAL_BLE_IMPORT);
    // A clean peer without an IRK does not interrupt either initiator.
    live=dirty=has_irk=false;initiating=true;cancel_error=0;
    c=(cordial_ble_command){.kind=CORDIAL_BLE_FORGET,.request=5};
    int cancelled=cancels,failures=failed;
    for(int automatic=0;automatic<2;automatic++) {
        auto_active=automatic;queue[0]=c;count=1;process_commands(NULL);
        assert(!count && !auto_cancel && cancels==cancelled && failed==failures);
    }
    assert(executed==7 && order[5]==CORDIAL_BLE_FORGET && order[6]==CORDIAL_BLE_FORGET);
    stored=false;assert(!changes_privacy(&c));
    dirty=true;assert(changes_privacy(&c));
    read_error=BLE_HS_ESTORE_FAIL;
    assert(!changes_privacy(&c));
    queue[0]=c;count=1;process_commands(NULL);
    assert(!count && cancels==cancelled && executed==8);
}
''')

    def test_bond_errors_keep_radio_busy_capacity_timeout_and_storage_distinct(self):
        errors = self.native[self.native.index("static uint8_t bond_error("):
                             self.native.index("static void complete(")]
        run_c(PREFIX + r'''
static cordial_ble_event last;
static void emit(const cordial_ble_event *e) {last=*e;}
''' + errors + r'''
int main(void) {
    assert(bond_error(0)==CORDIAL_BLE_OK);
    assert(bond_error(BLE_HS_ENOMEM)==CORDIAL_BLE_CAPACITY);
    assert(bond_error(0x207)==CORDIAL_BLE_CAPACITY);
    assert(bond_error(BLE_HS_EBUSY)==CORDIAL_BLE_BUSY);
    assert(bond_error(BLE_HS_EALREADY)==CORDIAL_BLE_BUSY);
    assert(bond_error(BLE_HS_EPREEMPTED)==CORDIAL_BLE_BUSY);
    assert(bond_error(0x20c)==CORDIAL_BLE_BUSY);
    assert(bond_error(BLE_HS_ETIMEOUT)==CORDIAL_BLE_TIMEOUT);
    assert(bond_error(BLE_HS_ETIMEOUT_HCI)==CORDIAL_BLE_TIMEOUT);
    assert(bond_error(0x208)==CORDIAL_BLE_TIMEOUT);
    assert(bond_error(BLE_HS_ESTORE_FAIL)==CORDIAL_BLE_STORAGE);
    assert(bond_error(BLE_HS_EBADDATA)==CORDIAL_BLE_STORAGE);
    assert(bond_error(BLE_HS_ENOENT)==CORDIAL_BLE_STORAGE);
    assert(bond_error(0x212)==CORDIAL_BLE_RADIO);
    assert(bond_error(BLE_HS_ENOTSYNCED)==CORDIAL_BLE_RADIO);
    cordial_ble_command c={.kind=CORDIAL_BLE_IMPORT,.request=42};
    bond_complete(&c,0x212);assert(last.kind==CORDIAL_BLE_BOND_RESULT && last.request==42 && last.code==CORDIAL_BLE_RADIO);
    c.kind=CORDIAL_BLE_FORGET;bond_complete(&c,BLE_HS_ETIMEOUT);
    assert(last.kind==CORDIAL_BLE_FORGOTTEN && last.code==CORDIAL_BLE_TIMEOUT);
}
''')


if __name__ == "__main__":
    unittest.main()
