/* Scalar ABI query only. This translation unit installs no allocation hooks. */
#include "managed_memory.h"
#include <stddef.h>
struct UManagedAligned46 { char prefix; UManagedAlignment46 payload; };
U_CAPI size_t U_EXPORT2 managed_memory_alignment_46(void) {
    return offsetof(struct UManagedAligned46, payload);
}
