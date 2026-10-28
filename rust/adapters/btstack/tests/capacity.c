// Exhaust the compiled native pools, then verify that released entries are reusable.
#include <assert.h>
#include <stdio.h>
#include "btstack_memory.h"
#include "profiles_internal.h"
#include "../c/classic_output.c"

#define CHECK_POOL(name, type, count) do { \
    type *entries[count]; \
    for (unsigned i = 0; i < count; ++i) { \
        entries[i] = btstack_memory_##name##_get(); \
        assert(entries[i]); \
    } \
    assert(!btstack_memory_##name##_get()); \
    for (unsigned i = 0; i < count; ++i) btstack_memory_##name##_free(entries[i]); \
    type *reused = btstack_memory_##name##_get(); \
    assert(reused); \
    btstack_memory_##name##_free(reused); \
} while (0)

static void descriptor_capacity(void) {
    uint8_t descriptors[(CORDIAL_CLASSIC_CONNECTIONS + 1) * CORDIAL_DESCRIPTOR_BYTES];
    hid_host_connection_t *links[CORDIAL_CLASSIC_CONNECTIONS + 1];
    hid_host_descriptor_storage = descriptors;
    hid_host_descriptor_storage_len = sizeof descriptors;
    for (unsigned i = 0; i < CORDIAL_CLASSIC_CONNECTIONS + 1; ++i) {
        links[i] = btstack_memory_hid_host_connection_get();
        assert(links[i]);
        btstack_linked_list_add_tail(&hid_host_connections, (btstack_linked_item_t *)links[i]);
        hid_descriptor_storage_init(links[i]);
        for (unsigned byte = 0; byte < CORDIAL_DESCRIPTOR_BYTES; ++byte)
            assert(hid_descriptor_storage_store(links[i], (uint8_t)i));
    }
    assert(!hid_descriptor_storage_get_available_space());
    assert(!hid_descriptor_storage_store(links[CORDIAL_CLASSIC_CONNECTIONS], 0));
    hid_descriptor_storage_delete(links[1]);
    assert(hid_descriptor_storage_get_available_space() == CORDIAL_DESCRIPTOR_BYTES);
    for (unsigned i = 2; i < CORDIAL_CLASSIC_CONNECTIONS + 1; ++i)
        for (unsigned byte = 0; byte < CORDIAL_DESCRIPTOR_BYTES; ++byte)
            assert(descriptors[links[i]->hid_descriptor_offset + byte] == i);
    hid_descriptor_storage_init(links[1]);
    for (unsigned byte = 0; byte < CORDIAL_DESCRIPTOR_BYTES; ++byte)
        assert(hid_descriptor_storage_store(links[1], 1));
    assert(!hid_descriptor_storage_get_available_space());
    for (unsigned i = 0; i < CORDIAL_CLASSIC_CONNECTIONS + 1; ++i)
        btstack_memory_hid_host_connection_free(links[i]);
    hid_host_deinit();
}

int main(void) {
    btstack_memory_init();
    CHECK_POOL(hci_connection, hci_connection_t, CORDIAL_ACTIVE_CONNECTIONS);
    CHECK_POOL(hid_host_connection, hid_host_connection_t, CORDIAL_CLASSIC_CONNECTIONS + 1);
    CHECK_POOL(gatt_client, gatt_client_t, CORDIAL_BLE_CONNECTIONS + 1);
    // Each Classic link has HID control, HID interrupt, and SDP. ATT and SMP
    // register fixed bearers, which do not allocate dynamic L2CAP channels.
    CHECK_POOL(l2cap_channel, l2cap_channel_t, 3 * (CORDIAL_CLASSIC_CONNECTIONS + 1));
    CHECK_POOL(sm_lookup_entry, sm_lookup_entry_t, CORDIAL_BLE_CONNECTIONS + 1);
    CHECK_POOL(whitelist_entry, whitelist_entry_t, CORDIAL_BLE_CONNECTIONS + 1);
    descriptor_capacity();
    puts("Native connection, channel, security, and accept-list pools passed");
}
