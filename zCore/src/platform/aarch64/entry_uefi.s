/*
 * Generic AArch64 entry for zCore booted by rboot (UEFI).
 * The firmware has already enabled the MMU and mapped the kernel at its link
 * address, and passes `x0` = &Aarch64BootInfo.
 */

.section .text.entry, "ax"
.global _start
.type _start, @function

_start:
    adrp    x19, boot_stack_top
    add     x19, x19, #:lo12:boot_stack_top
    mov     sp, x19
    b       rust_entry
