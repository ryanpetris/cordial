#include "esp_attr.h"
#include "esp_flash.h"
#include "esp_flash_chips/esp_flash_types.h"
#include "esp_flash_chips/spi_flash_chip_driver.h"
#include "esp_flash_chips/spi_flash_chip_generic.h"
#include "esp_flash_chips/spi_flash_chip_gd.h"
#include "esp_flash_chips/spi_flash_chip_mxic.h"
#include "freertos/FreeRTOS.h"
#include "freertos/task.h"

// IDF 6.1 caches this ID after successful boot identification, before replacing
// chip->size with the image minimum. Its size decoders have no public pure API.
static uint32_t cached_capacity(const esp_flash_t *chip) {
    uint32_t id = chip->chip_id;
    uint32_t density = id & 0xff;
    if (id >> 24 || id >> 16 == 0 || id >> 16 == 0xff ||
        (id & 0xffff) == 0 || (id & 0xffff) == 0xffff) return 0;
    if (chip->chip_drv->detect_size == spi_flash_chip_generic_detect_size) {
        // The generic driver skips hexadecimal A-F after density 0x19.
        if (density > 0x19) {
            if (density < 0x20) return 0;
            density -= 6;
        }
    } else if (chip->chip_drv == &esp_flash_chip_mxic) {
        if (density > 0x30) density -= 0x20;
    } else if (chip->chip_drv != &esp_flash_chip_gd) {
        return 0;
    }
    if (density < 12 || density >= 32) return 0;
    return UINT32_C(1) << density;
}

// get_chip_caps can read SFDP. Match IDF's operation protection and host setup;
// this function and the selected SDK callbacks execute with flash cache disabled.
static esp_err_t IRAM_ATTR __attribute__((noinline, noclone)) check_addressing(esp_flash_t *chip) {
    if (chip->chip_drv->get_chip_caps == NULL) return ESP_ERR_NOT_SUPPORTED;
    if (chip->os_func == NULL || chip->os_func->start == NULL || chip->os_func->end == NULL)
        return ESP_ERR_FLASH_NOT_INITIALISED;
    esp_err_t result = chip->os_func->start(chip->os_func_data, ESP_FLASH_START_FLAG_NO_READ);
    if (result != ESP_OK) return result;
    chip->host->driver->dev_config(chip->host);
    unsigned caps = chip->chip_drv->get_chip_caps(chip);
    result = chip->os_func->end(chip->os_func_data);
    if (result != ESP_OK) return result;
    return caps & SPI_FLASH_CHIP_CAP_32MB_SUPPORT ? ESP_OK : ESP_ERR_NOT_SUPPORTED;
}

// Startup runs before application flash access. A missing measurement forbids
// growth, but the cached boot ID can retain access to an expanded filesystem.
esp_err_t cordial_esp_storage_capacity(uint32_t minimum, uint32_t *capacity,
                                     uint32_t *addressable) {
    esp_flash_t *chip = esp_flash_default_chip;
    if (chip == NULL || chip->chip_drv == NULL || chip->size == 0) {
        return ESP_ERR_FLASH_NOT_INITIALISED;
    }
    uint32_t physical = 0;
    esp_err_t result;
    for (unsigned attempt = 0; attempt < 3; ++attempt) {
        physical = 0;
        result = esp_flash_get_physical_size(NULL, &physical);
        if (result == ESP_OK) break;
        // IDF can fill the output before its end hook fails. Failed output is
        // not a measurement, and does not prevent another attempt.
        physical = 0;
        if (result != ESP_ERR_FLASH_UNSUPPORTED_CHIP &&
            result != ESP_ERR_FLASH_NO_RESPONSE && result != ESP_ERR_TIMEOUT) return result;
        if (attempt < 2) vTaskDelay(pdMS_TO_TICKS(10));
    }
    if (result != ESP_OK) {
        uint8_t byte;
        result = esp_flash_read(NULL, &byte, 0, sizeof(byte));
        if (result != ESP_OK) return result;
    } else if (physical == 0 || physical % 4096) {
        return ESP_ERR_INVALID_SIZE;
    }
    uint32_t limit = cached_capacity(chip);
    if (limit < chip->size) limit = chip->size;
    if (limit < physical) limit = physical;
    if (limit < minimum) limit = minimum;
    if (limit % 4096) return ESP_ERR_INVALID_SIZE;
    if (limit > 16 * 1024 * 1024) {
        result = check_addressing(chip);
        if (result != ESP_OK) return result;
    }
    chip->size = limit;
    *capacity = physical;
    *addressable = limit;
    return ESP_OK;
}
