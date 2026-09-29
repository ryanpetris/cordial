/* Exercise the pinned SM's real address-resolution and security-request
 * decisions, including the compile-time options used by the firmware. */
#include "ble/sm.c"
/* Same security configuration, with the native memory key database for this test. */
#undef NVM_NUM_DEVICE_DB_ENTRIES
#undef BTSTACK_FILE__
#include "ble/le_device_db_memory.c"
#include <assert.h>
#include <stdio.h>
#include <stdlib.h>

static hci_connection_t connection;
void hci_dump_btstack_event(const uint8_t *packet, uint16_t size) { (void)packet; (void)size; }
hci_connection_t *hci_connection_for_handle(hci_con_handle_t handle) {
    return handle == connection.con_handle ? &connection : NULL;
}
void btstack_assert_failed(const char *file, uint16_t line) {
    fprintf(stderr, "%s:%u\n", file, line); abort();
}
static void resolved(bool success, bool requested, bool authenticated) {
    memset(&connection, 0, sizeof connection);
    connection.con_handle = 1;
    sm_connection_t *s = &connection.sm_connection;
    s->sm_handle = 1; s->sm_role = HCI_ROLE_MASTER;
    s->sm_engine_state = SM_INITIATOR_CONNECTED;
    s->sm_pairing_requested = requested;
    bd_addr_t address = {1, 2, 3, 4, 5, 6}; sm_key_t irk = {1}, ltk = {2}; uint8_t rand[8] = {0};
    le_device_db_init();
    int index = le_device_db_add(BD_ADDR_TYPE_LE_PUBLIC, address, irk);
    le_device_db_encryption_set(index, 0, rand, ltk, 16, authenticated, 0, 0);
    sm_auth_req = SM_AUTHREQ_BONDING | SM_AUTHREQ_SECURE_CONNECTION | SM_AUTHREQ_MITM_PROTECTION;
    /* Even a remote security request during IRK lookup must not silently
     * request fresh pairing or upgrade an authorized Just Works bond. */
    uint8_t request[] = {SM_CODE_SECURITY_REQUEST, SM_AUTHREQ_BONDING};
    sm_initiator_connected_handle_security_request(s, request);
    assert(!s->sm_security_request_received);
    sm_address_resolution_mode = ADDRESS_RESOLUTION_FOR_CONNECTION;
    sm_address_resolution_context = s;
    sm_address_resolution_test = index;
    sm_address_resolution_handle_event(success ? ADDRESS_RESOLUTION_SUCCEEDED : ADDRESS_RESOLUTION_FAILED);
    if (success && (!requested || authenticated)) assert(s->sm_engine_state == SM_INITIATOR_PH4_HAS_LTK);
    else if (requested) assert(s->sm_engine_state == SM_INITIATOR_PH1_W2_SEND_PAIRING_REQUEST);
    else assert(s->sm_engine_state == SM_INITIATOR_CONNECTED);
}
int main(void) {
    resolved(true, false, false); /* Real saved Just Works reconnect. */
    resolved(true, false, true);  /* Real saved passkey reconnect. */
    resolved(false, false, false); /* Missing key cannot start an implicit pair. */
    resolved(true, true, false); /* Regression proves requesting pairing is different. */
    puts("Pinned BTstack re-encrypts saved Just Works/passkey bonds without implicit pairing");
}
