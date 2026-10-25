"""Run platform flash detection against native SDK stubs."""
import json
from pathlib import Path
import re
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[1]
sys.path.insert(0, str(ROOT / "tools"))
import firmware_config


class FlashCapacity(unittest.TestCase):
    def compile_run(self, source, headers, fixture):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            for name, text in headers.items():
                path = root / name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_text(text)
            main = root / "fixture.c"
            main.write_text(fixture)
            binary = root / "fixture"
            subprocess.run(["cc", "-std=c11", "-Wall", "-Wextra", "-Werror", "-I", str(root),
                            str(source), str(main), "-o", str(binary)], check=True)
            subprocess.run([binary], check=True)

    def test_esp_uses_full_capacity_only_with_driver_addressing_support(self):
        header = """#pragma once
#include <stdint.h>
#include <stddef.h>
typedef int esp_err_t;
typedef struct esp_flash_t esp_flash_t;
typedef struct {
    unsigned (*get_chip_caps)(esp_flash_t *);
    esp_err_t (*detect_size)(esp_flash_t *, uint32_t *);
} driver_t;
typedef struct host_t host_t;
typedef struct { esp_err_t (*dev_config)(host_t *); } host_driver_t;
struct host_t { const host_driver_t *driver; };
typedef struct { esp_err_t (*start)(void *, uint32_t); esp_err_t (*end)(void *); } os_t;
struct esp_flash_t {
    uint32_t size, chip_id;
    const driver_t *chip_drv;
    const os_t *os_func;
    void *os_func_data;
    host_t *host;
};
#define IRAM_ATTR
#define ESP_FLASH_START_FLAG_NO_READ 2
extern const driver_t esp_flash_chip_gd, esp_flash_chip_mxic;
esp_err_t spi_flash_chip_generic_detect_size(esp_flash_t *, uint32_t *);
extern esp_flash_t *esp_flash_default_chip;
#define ESP_OK 0
#define ESP_ERR_INVALID_SIZE 1
#define ESP_ERR_NOT_SUPPORTED 2
#define ESP_ERR_FLASH_NOT_INITIALISED 3
#define ESP_ERR_FLASH_UNSUPPORTED_CHIP 4
#define ESP_ERR_FLASH_NO_RESPONSE 5
#define ESP_ERR_TIMEOUT 6
#define SPI_FLASH_CHIP_CAP_32MB_SUPPORT 2
esp_err_t esp_flash_get_physical_size(esp_flash_t *, uint32_t *);
esp_err_t esp_flash_read(esp_flash_t *, void *, uint32_t, uint32_t);
"""
        fixture = """#include "esp_flash.h"
#include <assert.h>
static uint32_t physical;
static unsigned capabilities, probes, reads, failures, delays;
static uint32_t addressable;
void vTaskDelay(unsigned ticks) { assert(ticks == 10); ++delays; }
static int error, read_error, end_error, start_error, caps_end_error;
static unsigned locked, configured, starts, ends;
static int start(void *arg, uint32_t flags) {
    assert(arg == NULL && !locked && flags == ESP_FLASH_START_FLAG_NO_READ);
    ++starts;
    if (start_error) return start_error;
    locked = 1;
    return ESP_OK;
}
static int end(void *arg) {
    assert(arg == NULL && locked);
    ++ends; locked = configured = 0;
    return caps_end_error;
}
static esp_err_t configure(host_t *host) {
    assert(host && locked); configured = 1; return ESP_OK;
}
static unsigned caps(esp_flash_t *chip) {
    (void)chip;
    assert(locked && configured);
    return capabilities;
}
esp_err_t spi_flash_chip_generic_detect_size(esp_flash_t *c, uint32_t *s) {
    (void)c; (void)s; assert(0); return 99;
}
static esp_err_t other_detect(esp_flash_t *c, uint32_t *s) {
    (void)c; (void)s; assert(0); return 99;
}
const driver_t esp_flash_chip_gd = { caps, other_detect };
const driver_t esp_flash_chip_mxic = { caps, other_detect };
static driver_t driver = { caps, spi_flash_chip_generic_detect_size };
static const os_t os = { start, end };
static const host_driver_t host_driver = { configure };
static host_t host = { &host_driver };
static esp_flash_t chip = {
    .size = BOARD_MINIMUM, .chip_drv = &driver, .os_func = &os, .host = &host,
};
esp_flash_t *esp_flash_default_chip = &chip;
esp_err_t esp_flash_get_physical_size(esp_flash_t *flash, uint32_t *size) {
    assert(flash == NULL);
    ++probes;
    if (probes <= failures) return error;
    *size = physical;
    return end_error;
}
esp_err_t esp_flash_read(esp_flash_t *flash, void *data, uint32_t off, uint32_t len) {
    assert(flash == NULL && data != NULL && off == 0 && len == 1);
    ++reads;
    return read_error;
}
esp_err_t cordial_esp_storage_capacity(uint32_t, uint32_t *, uint32_t *);
static esp_err_t capacity(uint32_t *result) {
    probes = reads = delays = starts = ends = 0;
    return cordial_esp_storage_capacity(BOARD_MINIMUM, result, &addressable);
}
int main(void) {
    uint32_t result = 0;
    for (unsigned mib = BOARD_MINIMUM / 1048576; mib <= 64; mib += 4) {
        physical = mib * 1048576;
        capabilities = SPI_FLASH_CHIP_CAP_32MB_SUPPORT;
        assert(capacity(&result) == ESP_OK);
        assert(result == physical && chip.size == physical && probes == 1 && reads == 0 && delays == 0);
    }
    failures = 1; error = ESP_ERR_FLASH_NO_RESPONSE;
    assert(capacity(&result) == ESP_OK && probes == 2 && reads == 0 && delays == 1);
    failures = 2; error = ESP_ERR_TIMEOUT;
    assert(capacity(&result) == ESP_OK && probes == 3 && delays == 2);
    failures = 3; error = ESP_ERR_FLASH_UNSUPPORTED_CHIP;
    assert(capacity(&result) == ESP_OK && probes == 3 && reads == 1 && delays == 2);
    assert(result == 0 && chip.size == physical && addressable == physical);
    chip.size = BOARD_MINIMUM;
    assert(capacity(&result) == ESP_OK && result == 0 && addressable == BOARD_MINIMUM);
    result = 123;
    read_error = 99;
    assert(capacity(&result) == 99 && result == 123 && probes == 3);
    read_error = 0;
    for (int e = ESP_ERR_FLASH_NO_RESPONSE; e <= ESP_ERR_TIMEOUT; ++e) {
        error = e;
        assert(capacity(&result) == ESP_OK && result == 0 && probes == 3 && reads == 1 && delays == 2);
        assert(addressable == BOARD_MINIMUM);
    }
    result = 123;
    error = 99;
    assert(capacity(&result) == 99 && probes == 1 && reads == 0 && delays == 0);
    failures = 0;
    end_error = ESP_ERR_TIMEOUT;
    assert(capacity(&result) == ESP_OK && result == 0 && probes == 3 && reads == 1 && delays == 2);
    assert(addressable == BOARD_MINIMUM);
    result = 123;
    end_error = 0;
    capabilities = 0;
    assert(capacity(&result) == ESP_ERR_NOT_SUPPORTED);
    driver.get_chip_caps = NULL;
    assert(capacity(&result) == ESP_ERR_NOT_SUPPORTED);
    assert(result == 123 && chip.size == BOARD_MINIMUM);
    physical = BOARD_MINIMUM;
    chip.size = 2 * BOARD_MINIMUM;
    assert(capacity(&result) == ESP_OK && result == BOARD_MINIMUM && addressable == 2 * BOARD_MINIMUM);
    assert(chip.size == addressable);
    physical = BOARD_MINIMUM - 4096;
    assert(capacity(&result) == ESP_OK && probes == 1 && reads == 0 && delays == 0);
    assert(result == physical && addressable == 2 * BOARD_MINIMUM);
    for (unsigned mib = 1; mib <= 2; ++mib) {
        physical = mib * 1048576;
        assert(capacity(&result) == ESP_OK && probes == 1 && reads == 0 && delays == 0);
        assert(result == physical && addressable == 2 * BOARD_MINIMUM);
    }
    physical = BOARD_MINIMUM + 1;
    assert(capacity(&result) == ESP_ERR_INVALID_SIZE && probes == 1);
    driver.get_chip_caps = caps;
    physical = 32 * 1048576; capabilities = SPI_FLASH_CHIP_CAP_32MB_SUPPORT;
    start_error = ESP_ERR_TIMEOUT;
    assert(capacity(&result) == ESP_ERR_TIMEOUT && starts == 1 && ends == 0);
    start_error = 0; caps_end_error = ESP_ERR_TIMEOUT;
    assert(capacity(&result) == ESP_ERR_TIMEOUT && starts == 1 && ends == 1 && !locked);
    caps_end_error = 0;
    assert(capacity(&result) == ESP_OK && starts == 1 && ends == 1 && !locked);
    // Cached boot evidence restores cold-boot bounds, but never supplies growth.
    failures = 3; error = ESP_ERR_TIMEOUT;
    const struct { uint32_t id, bytes; const driver_t *driver; } ids[] = {
        {0xef4017, 1u << 23, &driver}, {0xef4019, 1u << 25, &driver},
        {0xef4020, 1u << 26, &driver}, {0xc8401a, 1u << 26, &esp_flash_chip_gd},
        {0xc24018, 1u << 24, &esp_flash_chip_mxic},
        {0xc28038, 1u << 24, &esp_flash_chip_mxic},
        {0xc8401f, 1u << 31, &esp_flash_chip_gd},
        {0, 0, &driver}, {0xffffff, 0, &driver}, {0xef0000, 0, &driver},
        {0x004017, 0, &driver}, {0xff4017, 0, &driver}, {0x1ef4017, 0, &driver},
        {0xef401a, 0, &driver}, {0xef400b, 0, &driver}, {0xef4026, 0, &driver},
        {0xc84020, 0, &esp_flash_chip_gd}, {0xc24050, 0, &esp_flash_chip_mxic},
    };
    for (unsigned i = 0; i < sizeof(ids) / sizeof(ids[0]); ++i) {
        chip.size = BOARD_MINIMUM; chip.chip_id = ids[i].id; chip.chip_drv = ids[i].driver;
        assert(capacity(&result) == ESP_OK && result == 0 && probes == 3 && delays == 2);
        assert(addressable == (ids[i].bytes > BOARD_MINIMUM ? ids[i].bytes : BOARD_MINIMUM));
        if (addressable > 16 * 1048576) assert(starts == 1 && ends == 1);
    }
    driver.detect_size = other_detect;
    chip.chip_drv = &driver; chip.chip_id = 0xef4018; chip.size = BOARD_MINIMUM;
    assert(capacity(&result) == ESP_OK && addressable == BOARD_MINIMUM);
    driver.detect_size = spi_flash_chip_generic_detect_size;
    chip.chip_id = 0xef4018;
    failures = 0; physical = BOARD_MINIMUM;
    assert(capacity(&result) == ESP_OK && result == BOARD_MINIMUM && addressable == 16 * 1048576);
    chip.size = BOARD_MINIMUM; physical = 64 * 1048576; end_error = ESP_ERR_TIMEOUT;
    assert(capacity(&result) == ESP_OK && result == 0 && addressable == 16 * 1048576 && probes == 3);
    esp_flash_default_chip = NULL;
    assert(capacity(&result) == ESP_ERR_FLASH_NOT_INITIALISED && probes == 0);
    esp_flash_default_chip = &chip;
    chip.chip_drv = NULL;
    assert(capacity(&result) == ESP_ERR_FLASH_NOT_INITIALISED && probes == 0);
    chip.chip_drv = &driver; chip.size = 0;
    assert(capacity(&result) == ESP_ERR_FLASH_NOT_INITIALISED && probes == 0);
    return 0;
}
"""
        for minimum in (4194304, 8388608):
            with self.subTest(minimum=minimum), tempfile.TemporaryDirectory() as directory:
                root = Path(directory)
                raw = json.loads((ROOT / "boards/esp32s3.json").read_text())
                path = root / "config.json"
                path.write_text(json.dumps(raw | {"minimum_flash_bytes": minimum}))
                firmware_config.generate(firmware_config.load(path, "production"), root)
                generated = (root / "board.rs").read_text()
                value = int(re.search(r"MINIMUM_FLASH_BYTES: u32 = (\d+);", generated)[1])
                self.assertEqual(value, minimum)
                self.compile_run(ROOT / "platforms/esp32s3/components/platform/flash.c", {
                    "esp_flash.h": header,
                    "esp_attr.h": "#define IRAM_ATTR\n",
                    "esp_flash_chips/spi_flash_chip_generic.h": '#include "esp_flash.h"\n',
                    "esp_flash_chips/spi_flash_chip_gd.h": '#include "esp_flash.h"\n',
                    "esp_flash_chips/spi_flash_chip_mxic.h": '#include "esp_flash.h"\n',
                    "freertos/FreeRTOS.h": "#define pdMS_TO_TICKS(ms) (ms)\n",
                    "freertos/task.h": "void vTaskDelay(unsigned ticks);\n",
                    "esp_flash_chips/esp_flash_types.h": '#include "esp_flash.h"\n',
                    "esp_flash_chips/spi_flash_chip_driver.h": '#include "esp_flash.h"\n',
                }, f"#define BOARD_MINIMUM {value}u\n" + fixture)

    def test_pico_capacity_retry_timing(self):
        services = (ROOT / "platforms/pico/src/services.rs").read_text()
        detection = services[services.index("/// Detects the physical flash"):]
        fixture = r"""
use std::marker::PhantomData;
use std::sync::atomic::{AtomicUsize, Ordering};
static DELAYS: AtomicUsize = AtomicUsize::new(0);
mod embassy_time {
    pub struct Timer;
    impl Timer {
        pub async fn after_millis(ms: u64) {
            assert_eq!(ms, 10);
            super::DELAYS.fetch_add(1, super::Ordering::SeqCst);
        }
    }
}
mod embassy_rp {
    pub mod flash {
        #[derive(Debug, PartialEq)]
        pub enum Error { Other, OutOfBounds }
    }
}
trait Instance {}
trait Mode {}
impl Instance for () {}
impl Mode for () {}
struct Flash<'a, T, M, const N: usize> {
    replies: Vec<Result<u32, embassy_rp::flash::Error>>,
    calls: usize,
    marker: PhantomData<(&'a (), T, M)>,
}
impl<T, M, const N: usize> Flash<'_, T, M, N> {
    fn blocking_jedec_id(&mut self) -> Result<u32, embassy_rp::flash::Error> {
        self.calls += 1;
        self.replies.remove(0)
    }
}
fn main() {
    use embassy_rp::flash::Error::{Other, OutOfBounds};
    for (replies, expected, calls, delays) in [
        (vec![Ok(0xef4017)], Ok(Some(8 * 1048576)), 1, 0),
        (vec![Err(Other), Ok(0xef4017)], Ok(Some(8 * 1048576)), 2, 1),
        (vec![Ok(0), Err(Other), Ok(0xef4018)], Ok(Some(16 * 1048576)), 3, 2),
        (vec![Ok(0), Ok(0xffffff), Err(Other)], Ok(None), 3, 2),
        (vec![Ok(0xef4019)], Err(OutOfBounds), 1, 0),
        (vec![Ok(0xef4015)], Ok(Some(2 * 1048576)), 1, 0),
        (vec![Ok(0xef4014)], Ok(Some(1048576)), 1, 0),
        (vec![Ok(0xef4013)], Ok(Some(512 * 1024)), 1, 0),
        (vec![Ok(0xef4000), Ok(0xef4020), Ok(0xef40ff)], Ok(None), 3, 2),
    ] {
        DELAYS.store(0, Ordering::SeqCst);
        let mut flash = Flash::<(), (), {16 * 1048576}> {
            replies, calls: 0, marker: PhantomData,
        };
        {
            let mut future = std::pin::pin!(flash_capacity(&mut flash));
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert_eq!(std::future::Future::poll(future.as_mut(), &mut context), std::task::Poll::Ready(expected));
        }
        assert_eq!(DELAYS.load(Ordering::SeqCst), delays);
        assert_eq!(flash.calls, calls);
    }
}
"""
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            source = root / "pico.rs"
            source.write_text(fixture + detection)
            binary = root / "pico"
            subprocess.run(["rustc", "--edition=2024", "--cfg", 'feature="rp2040"',
                            "-A", "private_bounds", "-A", "private_interfaces",
                            str(source), "-o", str(binary)], check=True)
            subprocess.run([binary], check=True)

    def test_pico_jedec_command_preserves_interrupt_state(self):
        self.compile_run(ROOT / "platforms/pico/c/flash.c", {
            "hardware/flash.h": "#include <stdint.h>\n#include <stddef.h>\n"
                                "void flash_do_cmd(const uint8_t *, uint8_t *, size_t);\n",
            "hardware/sync.h": "#include <stdint.h>\nuint32_t save_and_disable_interrupts(void);\n"
                               "void restore_interrupts(uint32_t);\n",
        }, """#include <stdint.h>
#include <stddef.h>
#include <assert.h>
static int disabled;
uint32_t save_and_disable_interrupts(void) { assert(!disabled); disabled = 1; return 42; }
void restore_interrupts(uint32_t state) { assert(disabled && state == 42); disabled = 0; }
void flash_do_cmd(const uint8_t *command, uint8_t *response, size_t count) {
    assert(disabled && count == 4 && command[0] == 0x9f);
    assert(command[1] == 0 && command[2] == 0 && command[3] == 0);
    response[0] = 0; response[1] = 0xef; response[2] = 0x40; response[3] = 0x18;
}
uint32_t cordial_flash_jedec_id(void);
int main(void) {
    assert(cordial_flash_jedec_id() == 0xef4018);
    assert(!disabled);
    return 0;
}
""")


if __name__ == "__main__":
    unittest.main()
