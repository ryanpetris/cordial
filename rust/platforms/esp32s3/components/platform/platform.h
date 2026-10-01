#pragma once
#include <stdint.h>
#include "esp_err.h"
#include "sdkconfig.h"
#include "ble.h"

void cordial_esp_critical_enter(void);
void cordial_esp_critical_exit(void);
void *cordial_esp_current_task(void);
void cordial_esp_wake(void *task);
void cordial_esp_wait(void);
esp_err_t cordial_esp_usb_phy(void);
void *cordial_esp_usb_registers(void);
esp_err_t cordial_esp_usb_interrupt(void (*handler)(void *), void *context);
#if CONFIG_CORDIAL_DEVELOPMENT
void cordial_esp_bootloader(void) __attribute__((noreturn));
#endif
#if CONFIG_BT_CONTROLLER_ONLY
esp_err_t cordial_esp_controller_start(void (*sent)(void), int (*received)(uint8_t *, uint16_t));
esp_err_t cordial_esp_controller_send(const uint8_t *data, uint16_t length);
#endif
