// Boots an external kernel under libkrun 2.0 with a vhost-user-fs device (virtio type 26).
// usage: launch KERNEL INITRD CMDLINE SOCKET
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>
#include <libkrun.h>

static bool push_to_stderr(void *u, KrunStr s) { (void)u; fwrite(s.data, 1, s.len, stderr); return true; }
static KrunVtableHandle w = KRUN_VTABLE_HANDLE(KRUN_PUSH_STR_TYPE_TAG,
    ((KrunPushStrVtable){ .drop = NULL, .push = push_to_stderr }), NULL);
#define CHECK(call) krun_err = NULL; call; if (krun_err) { fprintf(stderr, "%s failed: ", #call); \
    krun_error_message(krun_err, &w); fputc('\n', stderr); return 1; }

int main(int argc, char **argv) {
    if (argc != 5) { fprintf(stderr, "usage: %s KERNEL INITRD CMDLINE SOCKET\n", argv[0]); return 2; }
    KrunError krun_err;
    const char *lvl = getenv("KRUN_LOG");
    CHECK(krun_init_log(-1, lvl ? atoi(lvl) : KRUN_LOG_LEVEL_WARN, KRUN_LOG_STYLE_AUTO, 0, &krun_err));
    KrunMmioDeviceManager devices = krun_mmio_device_manager_new();
    {
        KrunConsoleBuilder cb = krun_console_device_builder();
        CHECK(krun_console_builder_add_default_console(cb, STDIN_FILENO, STDOUT_FILENO, STDERR_FILENO, &krun_err));
        KrunConsoleDevice console = krun_console_builder_build(cb, &krun_err);
        krun_mmio_device_manager_add(devices, console);
    }
    {
        uint16_t sizes[] = {1024, 1024}; // hiprio + one request queue
        CHECK(KrunVhostUserDevice fs = krun_vhost_user_device_new(26, KRUN_STR(argv[4]),
            KRUN_STR("vhost-user-fs"), 2, sizes, 2, &krun_err));
        krun_mmio_device_manager_add(devices, fs);
    }
    CHECK(KrunPayload payload = krun_payload_load_external(KRUN_STR(argv[1]), KRUN_KERNEL_FORMAT_ELF,
        KRUN_STR(argv[2]), KRUN_STR(argv[3]), &krun_err));
    KrunVmmBuilder b = krun_vmm_builder_new();
    CHECK(krun_vmm_builder_vcpus(&b, 2, &krun_err));
    CHECK(krun_vmm_builder_ram_mib(&b, 1024, &krun_err));
    krun_vmm_builder_payload(&b, payload);
    krun_vmm_builder_devices(&b, devices);
    CHECK(KrunVmm vmm = krun_vmm_builder_build(&b, &krun_err));
    krun_vmm_run(vmm);
    return 0;
}
