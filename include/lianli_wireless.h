/* C interface of lianli_wireless.dll: drives Lian Li wireless fans
 * through the RF dongle. Every function returns 0 on success or a
 * negative code; lianli_last_error describes the last failure on the
 * calling thread. */

#ifndef LIANLI_WIRELESS_H
#define LIANLI_WIRELESS_H

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

#define LIANLI_ABI_VERSION 1u
#define LIANLI_MAX_GROUPS 16

#define LIANLI_OK 0
#define LIANLI_ERR_ARGUMENT (-1)
#define LIANLI_ERR_DONGLE (-2)
#define LIANLI_ERR_LCONNECT (-3)
#define LIANLI_ERR_PANIC (-4)
#define LIANLI_ERR_SIZE (-5)
#define LIANLI_ERR_WINDOWS (-6)

typedef struct lianli_handle lianli_handle;

typedef struct lianli_group {
    uint8_t mac[6];
    uint8_t receiver;
    uint8_t fan_count;
    uint8_t model;
    uint8_t online;
    uint8_t acknowledged;
    uint8_t has_target;
    uint32_t unacknowledged;
    uint16_t rpm[4];
    uint8_t duty[4];
    uint8_t target[4];
} lianli_group;

typedef struct lianli_state {
    uint32_t size;      /* set to sizeof(lianli_state) before reading */
    uint32_t version;   /* LIANLI_ABI_VERSION of the library */
    uint8_t master_mac[6];
    uint8_t channel;
    uint8_t alarm;      /* 1 while the failsafe is in force */
    uint64_t ticks;
    uint64_t polls;
    uint64_t poll_failures;
    uint32_t group_count;
    lianli_group groups[LIANLI_MAX_GROUPS];
} lianli_state;

uint32_t lianli_version(void);
int32_t lianli_open(lianli_handle **out);
int32_t lianli_close(lianli_handle *handle);
int32_t lianli_set_percent(const lianli_handle *handle, const uint8_t mac[6], uint8_t percent);
int32_t lianli_read_state(const lianli_handle *handle, lianli_state *state);
int32_t lianli_take_log(const lianli_handle *handle, char *buffer, size_t length);
int32_t lianli_last_error(char *buffer, size_t length);

#ifdef __cplusplus
}
#endif

#endif
