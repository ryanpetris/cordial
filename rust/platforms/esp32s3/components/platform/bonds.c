// Application-owned portable records; accessed only from the NimBLE host task.
#include "bonds.h"
#include "host/ble_hs.h"
#include "store/config/ble_store_config.h"
#include <string.h>
static unsigned char records[8][145];
// A repeat-pair callback can remove host keys while the controller still has
// the old IRK. Retain only its identity until the live link is gone.
static ble_addr_t dirty[8];
static unsigned dirty_count;
void cordial_bonds_clear(void) {
  memset(records, 0, sizeof(records));
  dirty_count = 0;
}
int cordial_bonds_dirty(const ble_addr_t *addr) {
  for (unsigned i = 0; i < dirty_count; i++)
    if (!ble_addr_cmp(addr, &dirty[i]))
      return 1;
  return 0;
}
int cordial_bonds_mark_dirty(const ble_addr_t *addr) {
  if (cordial_bonds_dirty(addr))
    return 0;
  if (dirty_count == 8)
    return BLE_HS_ENOMEM;
  dirty[dirty_count++] = *addr;
  return 0;
}
void cordial_bonds_clean(const ble_addr_t *addr) {
  for (unsigned i = 0; i < dirty_count; i++)
    if (!ble_addr_cmp(addr, &dirty[i])) {
      dirty[i] = dirty[--dirty_count];
      return;
    }
}
static void reverse(unsigned char *dst, const unsigned char *src, unsigned n) {
  for (unsigned i = 0; i < n; i++)
    dst[i] = src[n - 1 - i];
}
static int match(const unsigned char *r, const ble_addr_t *addr) {
  unsigned char a[6];
  reverse(a, addr->val, 6);
  return r[8] && r[9] == addr->type && !memcmp(r + 10, a, 6);
}
static int find(const ble_addr_t *addr) {
  for (int i = 0; i < 8; i++)
    if (match(records[i], addr))
      return i;
  return -1;
}
static int free_record(void) {
  for (int i = 0; i < 8; i++)
    if (!records[i][8])
      return i;
  return -1;
}
static unsigned offset(int type) {
  return type == BLE_STORE_OBJ_TYPE_OUR_SEC ? 17 : 81;
}
static int security(int type) {
  return type == BLE_STORE_OBJ_TYPE_OUR_SEC ||
         type == BLE_STORE_OBJ_TYPE_PEER_SEC;
}
int cordial_bonds_import(const unsigned char *r, unsigned size) {
  if (size != 145 || r[8] != 2 || r[9] > 1)
    return BLE_HS_EINVAL;
  ble_addr_t addr = {.type = r[9]};
  reverse(addr.val, r + 10, 6);
  int i = find(&addr);
  if (i < 0)
    i = free_record();
  if (i < 0)
    return BLE_HS_ENOMEM;
  memcpy(records[i], r, 145);
  return 0;
}
int cordial_bonds_export(const ble_addr_t *addr, unsigned char *r) {
  int i = find(addr);
  if (i < 0)
    return BLE_HS_ENOENT;
  memcpy(r, records[i], 145);
  r[16] = 1;
  return 0;
}
int cordial_bonds_read(int type, const union ble_store_key *key,
                  union ble_store_value *value) {
  if (!security(type))
    return ble_store_config_read(type, key, value);
  unsigned skip = key->sec.idx;
  for (int i = 0; i < 8; i++) {
    const unsigned char *r = records[i], *p = r + offset(type);
    if (!r[8] || !(p[0] & 7) ||
        (ble_addr_cmp(&key->sec.peer_addr, BLE_ADDR_ANY) != 0 &&
         !match(r, &key->sec.peer_addr)))
      continue;
    if (skip) {
      skip--;
      continue;
    }
    memset(value, 0, sizeof(*value));
    struct ble_store_value_sec *v = &value->sec;
    v->peer_addr.type = r[9];
    reverse(v->peer_addr.val, r + 10, 6);
    v->ltk_present = !!(p[0] & 1);
    v->irk_present = !!(p[0] & 2);
    v->csrk_present = !!(p[0] & 4);
    v->authenticated = !!(p[0] & 8);
    v->sc = !!(p[0] & 32);
    v->key_size = p[1];
    v->ediv = p[2] | p[3] << 8;
    for (unsigned j = 0; j < 8; j++)
      v->rand_num = (v->rand_num << 8) | p[4 + j];
    reverse(v->ltk, p + 12, 16);
    reverse(v->irk, p + 28, 16);
    reverse(v->csrk, p + 44, 16);
    for (unsigned j = 0; j < 4; j++)
      v->sign_counter |= (uint32_t)p[60 + j] << (8 * j);
    return 0;
  }
  return BLE_HS_ENOENT;
}
int cordial_bonds_write(int type, const union ble_store_value *value) {
  if (!security(type))
    return ble_store_config_write(type, value);
  const struct ble_store_value_sec *v = &value->sec;
  int i = find(&v->peer_addr);
  if (i < 0)
    i = free_record();
  if (i < 0)
    return BLE_HS_ENOMEM;
  unsigned char *r = records[i], *p = r + offset(type);
  uint8_t irk[16];
  reverse(irk, v->irk, 16);
  if (type == BLE_STORE_OBJ_TYPE_PEER_SEC && (p[0] & 2) &&
      (!v->irk_present || memcmp(p + 28, irk, 16))) {
    int status = cordial_bonds_mark_dirty(&v->peer_addr);
    if (status)
      return status;
  }
  r[8] = 2;
  r[9] = v->peer_addr.type;
  reverse(r + 10, v->peer_addr.val, 6);
  p[0] = (v->ltk_present ? 1 : 0) | (v->irk_present ? 2 : 0) |
         (v->csrk_present ? 4 : 0) | (v->authenticated ? 8 : 0) |
         (v->sc ? 32 : 0);
  p[1] = v->key_size;
  p[2] = v->ediv;
  p[3] = v->ediv >> 8;
  for (unsigned j = 0; j < 8; j++)
    p[4 + j] = v->rand_num >> (8 * (7 - j));
  reverse(p + 12, v->ltk, 16);
  reverse(p + 28, v->irk, 16);
  reverse(p + 44, v->csrk, 16);
  for (unsigned j = 0; j < 4; j++)
    p[60 + j] = v->sign_counter >> (8 * j);
  return 0;
}
int cordial_bonds_delete(int type, const union ble_store_key *key) {
  if (!security(type))
    return ble_store_config_delete(type, key);
  int i = find(&key->sec.peer_addr);
  if (i < 0 || !(records[i][offset(type)] & 7))
    return BLE_HS_ENOENT;
  memset(records[i] + offset(type), 0, 64);
  if (((records[i][17] | records[i][81]) & 7) == 0)
    memset(records[i], 0, 145);
  return 0;
}

int cordial_bonds_peers(ble_addr_t *out, int *count, int capacity) {
  *count = 0;
  for (unsigned i = 0; i < 8; i++) {
    if (!records[i][8])
      continue;
    if (*count >= capacity)
      return BLE_HS_ENOMEM;
    out[*count].type = records[i][9];
    reverse(out[*count].val, records[i] + 10, 6);
    (*count)++;
  }
  return 0;
}

extern int cordial_ble_resolve_key(const uint8_t *irk, const uint8_t *rpa);
int cordial_bonds_resolve_rpa(const uint8_t *rpa, uint8_t *identity, uint8_t *type) {
  if ((rpa[5] & 0xc0) != 0x40)
    return 0;
  for (unsigned i = 0; i < 8; i++) {
    if (records[i][8] && (records[i][81] & 2) &&
        cordial_ble_resolve_key(records[i] + 109, rpa)) {
      *type = records[i][9];
      reverse(identity, records[i] + 10, 6);
      return 1;
    }
  }
  return 0;
}

int cordial_bonds_same_privacy(const uint8_t *r, unsigned size) {
  if (size != 145 || r[8] != 2)
    return 0;
  ble_addr_t addr = {.type = r[9]};
  reverse(addr.val, r + 10, 6);
  int i = find(&addr);
  if (i < 0)
    return 0;
  return (records[i][81] & 2) == (r[81] & 2) &&
         (!(r[81] & 2) || !memcmp(records[i] + 109, r + 109, 16));
}

// ble_gap_unpair only removes a controller entry when a peer IRK is present
// in its host view. Supply that flag even if repeat-pair deleted the old keys
// and the replacement distributed no IRK. This transient record is never
// exported.
int cordial_bonds_prepare_unpair(const ble_addr_t *addr) {
  if (!cordial_bonds_dirty(addr))
    return 0;
  int i = find(addr);
  if (i < 0)
    i = free_record();
  if (i < 0)
    return BLE_HS_ENOMEM;
  records[i][8] = 2;
  records[i][9] = addr->type;
  reverse(records[i] + 10, addr->val, 6);
  records[i][81] |= 2;
  return 0;
}
