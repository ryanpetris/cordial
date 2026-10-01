// The pinned host owns SET_REPORT payloads through the device's handshake.
// Compiling its unmodified implementation here exposes the connection state
// needed to admit 16-bit payload lengths and check the negotiated channel MTU.
#include "classic/hid_host.c"
#include "profiles.h"

int cordial_classic_set_report(uint16_t cid, hid_report_type_t type,
                              uint16_t report_id, const uint8_t *data, uint16_t size) {
    hid_host_connection_t *connection = hid_host_get_connection_for_hid_cid(cid);
    if (!connection || !connection->control_cid) return CORDIAL_CONNECTION;
    if (connection->state != HID_HOST_CONNECTION_ESTABLISHED) return CORDIAL_BUSY;
    uint32_t packet_size = (uint32_t)size + 1u + (report_id != HID_REPORT_ID_UNDEFINED);
    // HID transactions are smaller than the MTU; DATC segmentation is deprecated.
    if (packet_size >= l2cap_get_remote_mtu_for_local_cid(connection->control_cid)
            || packet_size > l2cap_max_mtu()) return CORDIAL_REPORT_SIZE;
    connection->report_type = type;
    connection->report_id = report_id;
    connection->report = data;
    connection->report_len = size;
    connection->state = HID_HOST_W2_SEND_SET_REPORT;
    l2cap_request_can_send_now_event(connection->control_cid);
    return CORDIAL_OK;
}
