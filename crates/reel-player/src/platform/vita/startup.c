#include <psp2/rtc.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>

// Rust main runs on Vita's kernel-created main thread. Its nested VM, UI and
// save frames use the usual 2 MiB Rust stack allocation. vita-elf-create reads
// this retained symbol into the process parameters before VPK packaging.
const uint32_t sceUserMainThreadStackSize = 2u * 1024u * 1024u;

// Publish the console's local offset before Jiff first resolves the process
// timezone. Keep stored Unix timestamps in UTC.
__attribute__((constructor))
static void init_timezone(void) {
    SceRtcTick utc;
    SceRtcTick local;
    if (sceRtcGetCurrentTick(&utc) < 0 ||
        sceRtcConvertUtcToLocalTime(&utc, &local) < 0) {
        return;
    }

    // POSIX TZ offsets count west of UTC. RTC ticks count microseconds.
    int64_t west = ((int64_t)utc.tick - (int64_t)local.tick) / 1000000;
    unsigned seconds = (unsigned)(west < 0 ? -west : west);
    char zone[32];
    snprintf(zone, sizeof(zone), "VITA%c%u:%02u:%02u", west < 0 ? '-' : '+',
             seconds / 3600, (seconds / 60) % 60, seconds % 60);
    setenv("TZ", zone, 1);
}

// vita-native's retained process-heap anchor supplies the complete C/newlib
// allocation family before CRT initialization. MAIN arenas grow on demand.
