#pragma once
#include "host/ble_store.h"
int cordial_bonds_read(int, const union ble_store_key *, union ble_store_value *);
int cordial_bonds_write(int, const union ble_store_value *);
int cordial_bonds_delete(int, const union ble_store_key *);
int cordial_bonds_import(const unsigned char *, unsigned);
int cordial_bonds_export(const ble_addr_t *, unsigned char *);

int cordial_bonds_peers(ble_addr_t *, int *, int);

int cordial_bonds_resolve_rpa(const uint8_t *, uint8_t *, uint8_t *);

int cordial_bonds_same_privacy(const uint8_t *, unsigned);

void cordial_bonds_clear(void);
int cordial_bonds_dirty(const ble_addr_t *);
int cordial_bonds_mark_dirty(const ble_addr_t *);
void cordial_bonds_clean(const ble_addr_t *);
int cordial_bonds_prepare_unpair(const ble_addr_t *);
