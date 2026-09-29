use esp_idf_sys as sys;

pub struct Indicator {
    pin: Option<(u8, bool)>,
    on: bool,
}

impl Indicator {
    pub fn new(pin: Option<(u8, bool)>) -> Self {
        if let Some((pin, active_low)) = pin {
            let config = sys::gpio_config_t {
                pin_bit_mask: 1u64 << pin,
                mode: sys::gpio_mode_t_GPIO_MODE_OUTPUT,
                ..Default::default()
            };
            unsafe {
                assert_eq!(
                    sys::gpio_set_level(pin.into(), active_low.into()),
                    sys::ESP_OK
                );
                assert_eq!(sys::gpio_config(&config), sys::ESP_OK);
            }
        }
        Self { pin, on: false }
    }

    pub fn set(&mut self, on: bool) {
        if on != self.on {
            if let Some((pin, active_low)) = self.pin {
                unsafe {
                    assert_eq!(
                        sys::gpio_set_level(pin.into(), (on ^ active_low).into()),
                        sys::ESP_OK
                    );
                }
            }
            self.on = on;
        }
    }
}
