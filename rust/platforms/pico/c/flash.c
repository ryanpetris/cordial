#include "hardware/flash.h"
#include "hardware/sync.h"
#include <stdint.h>

// Startup runs before core 1, radio DMA, or USB can access flash.
uint32_t cordial_flash_jedec_id(void) {
    uint8_t command[4] = {0x9f, 0, 0, 0};
    uint8_t response[4];
    uint32_t interrupts = save_and_disable_interrupts();
    flash_do_cmd(command, response, sizeof(command));
    restore_interrupts(interrupts);
    return (uint32_t)response[1] << 16 | (uint32_t)response[2] << 8 | response[3];
}
