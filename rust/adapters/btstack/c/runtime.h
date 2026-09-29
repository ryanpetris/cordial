#ifndef CORDIAL_BTSTACK_RUNTIME_H
#define CORDIAL_BTSTACK_RUNTIME_H
#include <stdint.h>
#include "btstack_tlv.h"
#include "btstack_chipset.h"

typedef struct {
    void *context;
    uint32_t (*time_ms)(void *);
    void (*wake)(void *);
    int (*can_send)(void *);
    int (*send)(void *, uint8_t, const uint8_t *, uint16_t);
    void (*fatal)(void *);
} cordial_runtime_callbacks;

void cordial_runtime_init(const cordial_runtime_callbacks *, const btstack_tlv_t *, void *,
                     const btstack_chipset_t *);
void cordial_runtime_poll(void);
int32_t cordial_runtime_timeout(void);
void cordial_runtime_receive(uint8_t kind, uint8_t *data, uint16_t size);
void cordial_runtime_sent(void);

#endif
