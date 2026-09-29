#include "platform.h"
#include "esp_bt.h"
#include "freertos/FreeRTOS.h"
#include "freertos/queue.h"
#include "freertos/task.h"
#include <string.h>

typedef struct { uint16_t length; uint8_t bytes[1024]; } packet;
static QueueHandle_t outgoing;
static TaskHandle_t sender;
static void (*sent_callback)(void);

static void available(void) { xTaskNotifyGive(sender); }
static void send_task(void *unused) {
    (void)unused;
    packet value;
    for (;;) {
        xQueueReceive(outgoing, &value, portMAX_DELAY);
        while (!esp_vhci_host_check_send_available()) {
            ulTaskNotifyTake(pdTRUE, portMAX_DELAY);
        }
        // This supported API can wait on a controller semaphore. Keep that
        // wait in a native task so Embassy's USB and owner keep running.
        esp_vhci_host_send_packet(value.bytes, value.length);
        sent_callback();
    }
}

esp_err_t cordial_esp_controller_send(const uint8_t *data, uint16_t length) {
    if (!outgoing || !data || length > sizeof(((packet *)0)->bytes)) return ESP_ERR_INVALID_ARG;
    packet value = { .length = length };
    memcpy(value.bytes, data, length);
    return xQueueSend(outgoing, &value, 0) == pdTRUE ? ESP_OK : ESP_ERR_NO_MEM;
}

esp_err_t cordial_esp_controller_start(void (*sent)(void), int (*received)(uint8_t *, uint16_t)) {
    static esp_vhci_host_callback_t callbacks;
    outgoing = xQueueCreate(1, sizeof(packet));
    if (!outgoing) return ESP_ERR_NO_MEM;
    sent_callback = sent;
    if (xTaskCreate(send_task, "cordial_vhci", 4096, NULL, 5, &sender) != pdPASS) {
        vQueueDelete(outgoing); outgoing = NULL;
        return ESP_ERR_NO_MEM;
    }
    callbacks.notify_host_send_available = available;
    callbacks.notify_host_recv = received;
    esp_bt_controller_config_t config = BT_CONTROLLER_INIT_CONFIG_DEFAULT();
    esp_err_t error = esp_bt_controller_init(&config);
    if (error == ESP_OK) error = esp_bt_controller_enable(ESP_BT_MODE_BLE);
    if (error == ESP_OK) error = esp_vhci_host_register_callback(&callbacks);
    if (error != ESP_OK) {
        vTaskDelete(sender); sender = NULL;
        vQueueDelete(outgoing); outgoing = NULL;
    }
    return error;
}
