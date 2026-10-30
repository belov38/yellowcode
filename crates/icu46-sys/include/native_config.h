/* Compiled in every private C/C++ unit, including stubdata and test wrappers.
 * Changing this configuration requires a new source/entry closure review. */
#ifndef ICU46_NATIVE_CONFIG_H
#define ICU46_NATIVE_CONFIG_H
#if defined(HAVE_CONFIG_H) || defined(U_LOCAL_SERVICE_HOOK) || defined(U_DEBUG_FAKETIME)
#error External ICU configuration or callbacks invalidate the five-mutex closure
#endif
#include "unicode/utypes.h"
#include "umapfile.h"
#if defined(U_LOCAL_SERVICE_HOOK) || defined(U_DEBUG_FAKETIME) || U_DISABLE_RENAMING != 0
#error ICU46 requires the reviewed private API and callback configuration
#endif
#if ICU_USE_THREADS != 1 || UCLN_NO_AUTO_CLEANUP != 1
#error ICU46 requires threads and disabled automatic cleanup
#endif
#if U_ENABLE_DYLOAD != 0 || U_ENABLE_TRACING != 0
#error ICU46 plugin loading and tracing must be compiled out
#endif
#if UCONFIG_NO_SERVICE != 0 || UCONFIG_NO_CONVERSION != 0 || UCONFIG_NO_REGULAR_EXPRESSIONS != 0
#error ICU46 source closure requires the reviewed service/conversion/regex configuration
#endif
#ifdef U_WINDOWS
#if MAP_IMPLEMENTATION != MAP_WIN32
#error ICU46 Windows source closure requires MAP_WIN32
#endif
#else
#if MAP_IMPLEMENTATION != MAP_POSIX
#error ICU46 POSIX source closure requires MAP_POSIX
#endif
#endif
#endif
