#include <stdint.h>

// Rust main runs on Vita's kernel-created main thread. Its nested VM, UI and
// save frames use the usual 2 MiB Rust stack allocation. vita-elf-create reads
// this retained symbol into the process parameters before VPK packaging.
const uint32_t sceUserMainThreadStackSize = 2u * 1024u * 1024u;

// The official PVR modules allocate their context state through SceLibc.
// Match the driver's GLES2 sample and KRKR host configuration; this heap is
// separate from the VitaSDK newlib heap used by Rust's allocator.
uint32_t sceLibcHeapSize = 16u * 1024u * 1024u;

// VitaSDK reserves this MAIN block for Rust's malloc-backed allocator. The
// default 128 MiB leaves too little headroom for complete fonts, VM state,
// media and frame data; GPU driver allocations use their separate native heaps.
uint32_t _newlib_heap_size_user = 160u * 1024u * 1024u;
