cmake_minimum_required(VERSION 3.20)
include(${PICO_SDK_PATH}/external/pico_sdk_import.cmake)
project(cordial_radio C CXX ASM)
pico_sdk_init()

# Rust owns startup, clocks, interrupts, USB, and scheduling. The SDK supplies
# flash commands and radio bus helpers without its BTstack or IP stack.
add_library(cordial_flash STATIC flash.c)
target_link_libraries(cordial_flash PRIVATE hardware_flash hardware_sync)
target_compile_definitions(cordial_flash PRIVATE PICO_NO_BINARY_INFO=1 NDEBUG
    PICO_PANIC_FUNCTION= PICO_SW_SPIN_LOCKS_NO_EXTEXCLALL=1
    PICO_RUNTIME_SKIP_INIT_PER_CORE_IRQ_PRIORITIES=1)
target_compile_options(cordial_flash PRIVATE -include ${CMAKE_CURRENT_LIST_DIR}/port.h
    -Os -ffunction-sections -fdata-sections)

if(CORDIAL_SDK_RADIO)
add_library(cordial_radio STATIC radio.c)
target_link_libraries(cordial_radio PRIVATE cyw43_driver_picow
    pico_cyw43_driver_headers hardware_clocks hardware_timer pico_time_headers)
target_include_directories(cordial_radio PRIVATE ${CORDIAL_CONFIG_DIR})
target_compile_options(cordial_radio PRIVATE -include ${CMAKE_CURRENT_LIST_DIR}/port.h
    -Os -ffunction-sections -fdata-sections)
target_compile_definitions(cordial_radio PRIVATE NDEBUG CYW43_ENABLE_BLUETOOTH=1
    CYW43_LWIP=0 PICO_CYW43_LOGGING_ENABLED=0 PICO_NO_BINARY_INFO=1
    # Rust startup does not enable the RP2350 GPIO coprocessor.
    PICO_USE_GPIO_COPROCESSOR=0
    PICO_RUNTIME_SKIP_INIT_PER_CORE_IRQ_PRIORITIES=1 PICO_PANIC_FUNCTION=
    # SDK locks are used only on core 0; Rust owns processor initialization.
    PICO_SW_SPIN_LOCKS_NO_EXTEXCLALL=1)

endif()
