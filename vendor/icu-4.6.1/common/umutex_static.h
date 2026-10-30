/* Private layout for the five named mutexes in the managed entry closure. */
#ifndef UMUTEX_STATIC_H
#define UMUTEX_STATIC_H

#include "umutex.h"
#include "umapfile.h"

#if ICU_USE_THREADS != 1 || UCLN_NO_AUTO_CLEANUP != 1
#error Static managed mutexes require threads and no automatic ICU cleanup
#endif
#ifdef U_DEBUG_FAKETIME
#error The fake-clock mutex is outside the managed entry closure
#endif
#ifdef HAVE_CONFIG_H
#error An external ICU configuration requires a new managed source closure review
#endif

#ifdef U_WINDOWS
#include <windows.h>
#if MAP_IMPLEMENTATION != MAP_WIN32
#error Windows managed mutex closure requires MAP_WIN32
#endif
#define UMTX_LEGACY_TYPE CRITICAL_SECTION
#define UMTX_STATIC_INITIALIZER {0}, SRWLOCK_INIT
#else
#include <pthread.h>
#if MAP_IMPLEMENTATION != MAP_POSIX
#error POSIX managed mutex closure requires MAP_POSIX
#endif
#define UMTX_LEGACY_TYPE pthread_mutex_t
#define UMTX_STATIC_INITIALIZER PTHREAD_MUTEX_INITIALIZER
#endif

typedef struct ICUMutex ICUMutex;
struct ICUMutex {
    UMTX *owner;
    UBool heapAllocated;
    ICUMutex *next;
    int32_t recursionCount;
    UMTX_LEGACY_TYPE platformMutex;
#ifdef U_WINDOWS
    /* The legacy heap path retains its original critical-section ABI. */
    SRWLOCK staticPlatformMutex;
#endif
    UMTX userMutex;
    UBool userInitialized;
};

/* Each storage symbol denotes one actual source identity, never a pool slot. */
U_CFUNC ICUMutex umtx_globalStatic_46;
U_CFUNC ICUMutex umtx_resourceStatic_46;
U_CFUNC ICUMutex umtx_converterStatic_46;
U_CFUNC ICUMutex umtx_incdecStatic_46;
U_CFUNC ICUMutex umtx_initStatic_46;

#define UMTX_STATIC(name, storage) \
    static UMTX name; \
    ICUMutex storage = {&name, FALSE, NULL, 0, UMTX_STATIC_INITIALIZER, NULL, FALSE}; \
    static UMTX name = &storage

#endif
