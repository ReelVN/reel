#include <stdint.h>

// Rust main runs on Vita's kernel-created main thread. Its nested VM, UI and
// save frames use the usual 2 MiB Rust stack allocation. vita-elf-create reads
// this retained symbol into the process parameters before VPK packaging.
const uint32_t sceUserMainThreadStackSize = 2u * 1024u * 1024u;

// vita-native's retained process-heap anchor supplies the complete C/newlib
// allocation family before CRT initialization. MAIN arenas grow on demand.
