#pragma once
#include <stdint.h>
// Application codec bytes, independent of BTstack's private persistence layout.
int cordial_bond_import(const uint8_t *bytes, unsigned size);
int cordial_bond_export(unsigned transport, unsigned random, const uint8_t *address,
                   uint8_t *bytes);
void cordial_bond_clear(void);
#ifdef ENABLE_CLASSIC
#include "classic/btstack_link_key_db.h"
const btstack_link_key_db_t *cordial_link_key_db(void);
#endif

unsigned cordial_bond_capacity(unsigned transport);
