#include "runtime.h"
#include "btstack_memory.h"
#include "btstack_run_loop.h"
#include "hci.h"
#include "bond_db.h"
#include <string.h>
// Packet in src/transport.rs reserves this writable prefix for in-place events.
_Static_assert(HCI_INCOMING_PRE_BUFFER_SIZE <= 8, "Rust receive packet needs more headroom");
#ifdef ENABLE_CLASSIC
#include "classic/btstack_link_key_db.h"
#endif

static cordial_runtime_callbacks callbacks;
static void (*packet_handler)(uint8_t, uint8_t *, uint16_t);

static uint32_t now(void) { return callbacks.time_ms(callbacks.context); }
static void wake(void) { callbacks.wake(callbacks.context); }
static void set_timer(btstack_timer_source_t *timer, uint32_t timeout) {
    timer->timeout = now() + timeout + 1;
}
static void enqueue(btstack_context_callback_registration_t *registration) {
    btstack_run_loop_base_add_callback(registration);
    wake();
}
static const btstack_run_loop_t run_loop = {
    .init = btstack_run_loop_base_init,
    .add_data_source = btstack_run_loop_base_add_data_source,
    .remove_data_source = btstack_run_loop_base_remove_data_source,
    .enable_data_source_callbacks = btstack_run_loop_base_enable_data_source_callbacks,
    .disable_data_source_callbacks = btstack_run_loop_base_disable_data_source_callbacks,
    .set_timer = set_timer,
    .add_timer = btstack_run_loop_base_add_timer,
    .remove_timer = btstack_run_loop_base_remove_timer,
    .dump_timer = btstack_run_loop_base_dump_timer,
    .get_time_ms = now,
    .poll_data_sources_from_irq = wake,
    .execute_on_main_thread = enqueue,
    .trigger_exit = wake,
};

static void transport_init(const void *config) { (void)config; }
static int transport_open(void) { return 0; }
static int transport_close(void) { return 0; }
static void transport_handler(void (*handler)(uint8_t, uint8_t *, uint16_t)) { packet_handler = handler; }
static int can_send(uint8_t kind) {
    return (kind == HCI_COMMAND_DATA_PACKET || kind == HCI_ACL_DATA_PACKET) && callbacks.can_send(callbacks.context);
}
static int send(uint8_t kind, uint8_t *data, int size) {
    if (size < 0 || size > 1023) return -1;
    return callbacks.send(callbacks.context, kind, data, (uint16_t)size);
}
static const hci_transport_t transport = {
    .name = "Cordial", .init = transport_init, .open = transport_open, .close = transport_close,
    .register_packet_handler = transport_handler, .can_send_packet_now = can_send, .send_packet = send,
};

void cordial_runtime_init(const cordial_runtime_callbacks *cb, const btstack_tlv_t *tlv, void *storage,
                     const btstack_chipset_t *chipset) {
    callbacks = *cb;
    btstack_memory_init();
    btstack_run_loop_init(&run_loop);
    hci_init(&transport, NULL);
    if (chipset) hci_set_chipset(chipset);
    btstack_tlv_set_instance(tlv, storage);
#ifdef ENABLE_CLASSIC
    hci_set_link_key_db(cordial_link_key_db());
#endif
    cordial_bond_clear();
}
void cordial_runtime_poll(void) {
    btstack_run_loop_base_poll_data_sources();
    btstack_run_loop_base_execute_callbacks();
    btstack_run_loop_base_process_timers(now());
}
int32_t cordial_runtime_timeout(void) { return btstack_run_loop_base_get_time_until_timeout(now()); }
void cordial_runtime_receive(uint8_t kind, uint8_t *data, uint16_t size) { packet_handler(kind, data, size); }
void cordial_runtime_sent(void) {
    uint8_t event[] = { HCI_EVENT_TRANSPORT_PACKET_SENT, 0 };
    packet_handler(HCI_EVENT_PACKET, event, sizeof event);
}
void btstack_assert_failed(const char *file, uint16_t line) {
    (void)file; (void)line;
    callbacks.fatal(callbacks.context);
    for (;;) {}
}
