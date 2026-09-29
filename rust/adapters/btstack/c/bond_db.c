#include "bond_db.h"
#include "ble/le_device_db.h"
#include "gap.h"
#include <stdint.h>
#include <string.h>
// The application admits seven regular entries per transport and reserves one
// for pairing. These tables are only the active view, never persistent storage.
static uint8_t le[8][145];
#ifdef ENABLE_CLASSIC
static uint8_t classic[8][145];
#endif
static int find(uint8_t records[8][145], unsigned random,
                const uint8_t *address) {
  for (int i = 0; i < 8; i++)
    if (records[i][8] && records[i][9] == random &&
        !memcmp(records[i] + 10, address, 6))
      return i;
  return -1;
}
static int empty(uint8_t records[8][145]) {
  for (int i = 0; i < 8; i++)
    if (!records[i][8])
      return i;
  return -1;
}
void cordial_bond_clear(void) {
  memset(le, 0, sizeof(le));
#ifdef ENABLE_CLASSIC
  memset(classic, 0, sizeof(classic));
#endif
}
int cordial_bond_import(const uint8_t *bytes, unsigned size) {
  if (!bytes || size < 17)
    return -1;
  uint8_t (*table)[145] = le;
  if (bytes[8] == 2) {
    if (size != 145)
      return -1;
  }
#ifdef ENABLE_CLASSIC
  else if (bytes[8] == 1) {
    if (size != 34)
      return -1;
    table = classic;
  }
#endif
  else
    return -1;
  int i = find(table, bytes[9], bytes + 10);
  if (i < 0)
    i = empty(table);
  if (i < 0)
    return -1;
  int privacy_changed =
      bytes[8] == 2 &&
      (table[i][8] != 2 || table[i][9] != bytes[9] ||
       memcmp(table[i] + 10, bytes + 10, 6) ||
       (table[i][81] & 2) != (bytes[81] & 2) ||
       ((bytes[81] & 2) && memcmp(table[i] + 109, bytes + 109, 16)));
  memset(table[i], 0, 145);
  memcpy(table[i], bytes, size);
  if (privacy_changed)
    (void)gap_load_resolving_list_from_le_device_db();
  return i;
}
int cordial_bond_export(unsigned transport, unsigned random, const uint8_t *address,
                   uint8_t *bytes) {
  uint8_t (*table)[145] = le;
  int size = 145;
#ifdef ENABLE_CLASSIC
  if (transport == 1) {
    table = classic;
    size = 34;
  } else
#endif
      if (transport != 2)
    return -1;
  int i = find(table, random, address);
  if (i < 0)
    return -1;
  memcpy(bytes, table[i], size);
  bytes[16] = 1;
  return size;
}
void le_device_db_init(void) {}
void le_device_db_set_local_bd_addr(bd_addr_t addr) { (void)addr; }
int le_device_db_count(void) {
  int n = 0;
  for (int i = 0; i < 8; i++)
    n += le[i][8] != 0;
  return n;
}
int le_device_db_max_count(void) { return 8; }
int le_device_db_add(int type, bd_addr_t addr, sm_key_t irk) {
  int i = find(le, type, addr);
  if (i < 0)
    i = empty(le);
  if (i < 0)
    return -1;
  memset(le[i], 0, sizeof(le[i]));
  le[i][8] = 2;
  le[i][9] = type;
  memcpy(le[i] + 10, addr, 6);
  // Peer security block starts at 81; IRK at +28.
  uint8_t nonzero = 0;
  if (irk) {
    for (unsigned j = 0; j < 16; j++)
      nonzero |= irk[j];
  }
  if (nonzero) {
    le[i][81] |= 2;
    memcpy(le[i] + 109, irk, 16);
  }
  return i;
}
void le_device_db_info(int i, int *type, bd_addr_t addr, sm_key_t irk) {
  if (i < 0 || i >= 8 || !le[i][8]) {
    if (type)
      *type = BD_ADDR_TYPE_UNKNOWN;
    if (addr)
      memset(addr, 0, 6);
    if (irk)
      memset(irk, 0, 16);
    return;
  }
  if (type)
    *type = le[i][9];
  if (addr)
    memcpy(addr, le[i] + 10, 6);
  if (irk)
    memcpy(irk, le[i] + 109, 16);
}
void le_device_db_encryption_set(int i, uint16_t ediv, uint8_t rand[8],
                                 sm_key_t ltk, int size, int auth,
                                 int authorized, int sc) {
  if (i < 0 || i >= 8)
    return;
  uint8_t *p = le[i] + 81;
  p[0] =
      (p[0] & 6) | 1 | (auth ? 8 : 0) | (authorized ? 16 : 0) | (sc ? 32 : 0);
  p[1] = size;
  p[2] = ediv;
  p[3] = ediv >> 8;
  memcpy(p + 4, rand, 8);
  memcpy(p + 12, ltk, 16);
}
void le_device_db_encryption_get(int i, uint16_t *ediv, uint8_t rand[8],
                                 sm_key_t ltk, int *size, int *auth,
                                 int *authorized, int *sc) {
  if (i < 0 || i >= 8)
    return;
  const uint8_t *p = le[i] + 81;
  if (ediv)
    *ediv = p[2] | p[3] << 8;
  if (rand)
    memcpy(rand, p + 4, 8);
  if (ltk)
    memcpy(ltk, p + 12, 16);
  if (size)
    *size = p[1];
  if (auth)
    *auth = !!(p[0] & 8);
  if (authorized)
    *authorized = !!(p[0] & 16);
  if (sc)
    *sc = !!(p[0] & 32);
}
void le_device_db_remove(int i) {
  if (i >= 0 && i < 8)
    memset(le[i], 0, 145);
}
void le_device_db_dump(void) {}
#ifdef ENABLE_CLASSIC
static void noop(void) {}
static void local(bd_addr_t addr) { (void)addr; }
static int get(bd_addr_t addr, link_key_t key, link_key_type_t *type) {
  int i = find(classic, 0, addr);
  if (i < 0)
    return 0;
  memcpy(key, classic[i] + 17, 16);
  *type = classic[i][33];
  return 1;
}
static void put(bd_addr_t addr, link_key_t key, link_key_type_t type) {
  int i = find(classic, 0, addr);
  if (i < 0)
    i = empty(classic);
  if (i < 0)
    return;
  classic[i][8] = 1;
  memcpy(classic[i] + 10, addr, 6);
  memcpy(classic[i] + 17, key, 16);
  classic[i][33] = type;
}
static void del(bd_addr_t addr) {
  int i = find(classic, 0, addr);
  if (i >= 0)
    memset(classic[i], 0, 145);
}
static int init(btstack_link_key_iterator_t *it) {
  it->context = 0;
  return 1;
}
static int next(btstack_link_key_iterator_t *it, bd_addr_t addr, link_key_t key,
                link_key_type_t *type) {
  unsigned i = (uintptr_t)it->context;
  while (i < 8) {
    unsigned at = i++;
    it->context = (void *)(uintptr_t)i;
    if (!classic[at][8])
      continue;
    memcpy(addr, classic[at] + 10, 6);
    memcpy(key, classic[at] + 17, 16);
    *type = classic[at][33];
    return 1;
  }
  return 0;
}
static void done(btstack_link_key_iterator_t *it) { (void)it; }
const btstack_link_key_db_t *cordial_link_key_db(void) {
  static const btstack_link_key_db_t db = {noop, local, noop, get, put,
                                           del,  init,  next, done};
  return &db;
}
#endif

unsigned cordial_bond_capacity(unsigned transport) {
#ifdef ENABLE_CLASSIC
  if (transport == 1)
    return sizeof(classic) / sizeof(classic[0]);
#endif
  return transport == 2 ? sizeof(le) / sizeof(le[0]) : 0;
}
