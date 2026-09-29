#include <string.h>
#include "cyw43.h"
#include "hardware/clocks.h"
#include "hardware/dma.h"
#include "hardware/pio.h"
#include "hardware/timer.h"

static uint8_t fallback_mac[6];

// Driver calls are synchronous on one cooperative Rust executor. No driver
// operation runs from an interrupt or the second core.
void cyw43_thread_enter(void) {}
void cyw43_thread_exit(void) {}
void cyw43_thread_lock_check(void) {}
void cyw43_post_poll_hook(void) {}
void cyw43_schedule_internal_poll_dispatch(void (*func)(void)) { (void)func; }
void cyw43_bluetooth_hci_process(void) {}
void cyw43_delay_us(uint32_t us) { busy_wait_us_32(us); }
void cyw43_delay_ms(uint32_t ms) { busy_wait_us((uint64_t)ms * 1000); }
void cyw43_await_background_or_timeout_us(uint32_t us) { busy_wait_us_32(us); }
void cyw43_hal_get_mac(int idx, uint8_t mac[6]) {
    (void)idx;
    memcpy(mac, cyw43_state.mac, 6);
}
void cyw43_hal_generate_laa_mac(int idx, uint8_t mac[6]) {
    (void)idx;
    memcpy(mac, fallback_mac, 6);
}

// The radio firmware requires the WLAN transport, but the adapter has no IP
// interface. These are the driver's documented callbacks for a port without IP.
void cyw43_cb_tcpip_init(cyw43_t *self, int itf) { (void)self; (void)itf; }
void cyw43_cb_tcpip_deinit(cyw43_t *self, int itf) { (void)self; (void)itf; }
void cyw43_cb_tcpip_set_link_up(cyw43_t *self, int itf) { (void)self; (void)itf; }
void cyw43_cb_tcpip_set_link_down(cyw43_t *self, int itf) { (void)self; (void)itf; }
void cyw43_cb_process_ethernet(void *self, int itf, size_t len, const uint8_t *data) {
    (void)self; (void)itf; (void)len; (void)data;
}

int cordial_radio_init(uint8_t mac[6], const uint8_t fallback[6], uint32_t sys_hz) {
    memcpy(fallback_mac, fallback, 6);
    // Embassy has already configured the clocks. Populate SDK bookkeeping
    // without reinitializing clocks or resetting peripherals.
    clock_set_reported_hz(clk_sys, sys_hz);
    // Rust lends PIO0 and DMA0/2 to this adapter. Hide all other resources from
    // the SDK allocator, especially DMA1, which belongs to flash storage.
    for (uint i = 0; i < NUM_DMA_CHANNELS; ++i) {
        if (i != 0 && i != 2) dma_channel_claim(i);
    }
    for (uint i = 1; i < NUM_PIOS; ++i) {
        pio_claim_sm_mask(pio_get_instance(i), 0xf);
    }
    cyw43_init(&cyw43_state);
    int result = cyw43_bluetooth_hci_init();
    if (!result) memcpy(mac, cyw43_state.mac, 6);
    return result;
}

void cordial_radio_poll(void) {
    // Match the upstream port's 50 ms sleep countdown, while servicing bus
    // work on every iteration of the Rust radio task.
    static uint32_t last_sleep;
    uint32_t now = time_us_32();
    if (cyw43_poll) {
        if ((uint32_t)(now - last_sleep) >= 50000) {
            if (cyw43_sleep) --cyw43_sleep;
            last_sleep = now;
        }
        cyw43_poll();
    }
}

int cordial_radio_led(uint32_t pin, bool value) {
    return cyw43_gpio_set(&cyw43_state, pin, value);
}
