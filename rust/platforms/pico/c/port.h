#pragma once
#ifndef __ASSEMBLER__
#include <stddef.h>
void *cordial_radio_alloc(size_t size);
void cordial_radio_free(void *ptr);
#define cyw43_malloc cordial_radio_alloc
#define cyw43_free cordial_radio_free
#endif
