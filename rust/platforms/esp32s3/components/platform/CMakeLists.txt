set(sources "platform.c")
if(CONFIG_BT_CONTROLLER_ONLY)
    list(APPEND sources "controller.c")
endif()
if(CONFIG_BT_NIMBLE_ENABLED)
    list(APPEND sources "nimble.c" "bonds.c")
endif()
idf_component_register(SRCS ${sources} INCLUDE_DIRS "."
    REQUIRES freertos esp_timer esp_system esp_hw_support bt bootloader_support)
