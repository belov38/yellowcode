/* Private ownership ABI. No allocator hooks are enabled by this header. */
#ifndef ICU46_MANAGED_MEMORY_H
#define ICU46_MANAGED_MEMORY_H
#include "unicode/utypes.h"
#include <stddef.h>
#include <limits.h>

/* Plain char is reserved for C text. Roles, sites and status never use char. */
typedef struct {
    int32_t role;
    int32_t site;
} UManagedOwner46;

/* Maximal fundamental alignment used by this pinned C/C++98 build.
 * Extended compiler-specific over-aligned types are not an exported ICU ABI. */
typedef union {
    long double wide;
    double real;
    int64_t integer;
    void *pointer;
    void (*function)(void);
} UManagedAlignment46;

typedef char UManagedByteWidth46[(CHAR_BIT == 8) ? 1 : -1];
typedef char UManagedIntWidth46[(sizeof(int32_t) == 4 && (int32_t)-1 < 0) ? 1 : -1];
typedef char UManagedStatusWidth46[(sizeof(UErrorCode) == 4 && U_USING_FALLBACK_WARNING < 0) ? 1 : -1];

U_CAPI size_t U_EXPORT2 managed_memory_alignment_46(void);

/* Enabled only by the product `managed` build (-DU_MANAGED_MEMORY_ENABLED_46=1).
 * Without it, managed_malloc_46 is plain uprv_malloc and no owner hook exists. */
#ifndef U_MANAGED_MEMORY_ENABLED_46
#define U_MANAGED_MEMORY_ENABLED_46 0
#endif

/* Explicit field ownership; ordinary malloc remains Operation. */
typedef void * U_CALLCONV UManagedAllocFn46(const void *, UManagedOwner46, size_t);
typedef UBool U_CALLCONV UManagedFailedFn46(const void *);
U_CAPI void * U_EXPORT2 managed_malloc_46(size_t size, int32_t role, int32_t site);
U_CAPI UBool U_EXPORT2 managed_failed_46(void);
#if U_MANAGED_MEMORY_ENABLED_46 || defined(U_MANAGED_MEMORY_TESTING_46)
/* Installs the owned allocation and failure callbacks once, before ICU has
 * allocated and after u_setMemoryFunctions; otherwise U_INVALID_STATE_ERROR. */
U_CAPI void U_EXPORT2 managed_owned_install_46(UManagedAllocFn46 *alloc,
        UManagedFailedFn46 *failed, int32_t *status);
#endif
U_CAPI void U_EXPORT2 locale_set_default_checked_46(const char *, UErrorCode *);
U_CAPI const void * U_EXPORT2 locale_english_checked_46(UErrorCode *);
#endif
