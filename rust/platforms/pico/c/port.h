#pragma once
#ifndef __ASSEMBLER__
// Rust startup copies .data into RAM before flash commands disable XIP.
#define __not_in_flash(group) __attribute__((section(".data." group)))
#include <stddef.h>
void *cordial_radio_alloc(size_t size);
void cordial_radio_free(void *ptr);
#define cyw43_malloc cordial_radio_alloc
#define cyw43_free cordial_radio_free
#endif
