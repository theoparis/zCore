/*
 * AArch64 Linux Image Header & Boot Stub for zCore on Apple Silicon
 * Compatible with the standard arm64 Linux boot protocol
 * (m1n1 linux.py / chainload.py / run_guest.py).
 *
 * The kernel is linked at 0xffff800040080000 but m1n1 loads it at an arbitrary
 * physical address, so this stub:
 *   1. establishes a stack inside the loaded image,
 *   2. builds early 16K-granule page tables (Apple cores are used with a 16K
 *      translation granule; MMU-off execution is not viable because all data
 *      accesses are then Device-typed and exclusives fault with
 *      DFSC=0b110101 "unsupported atomic access", which is what killed the
 *      first `spin::Once`),
 *      - TTBR0: identity map (RAM as Normal, low 64GB as Device MMIO),
 *      - TTBR1: kernel image window at its link address, plus the UART,
 *   3. enables the MMU and enters `rust_entry` at its *virtual* address with
 *      x0 = boot argument pointer (physical) and x1 = virt-minus-phys offset.
 */

/* 16K granule descriptors: AttrIndx=1 Normal WB, SH=Inner, AF=1 */
.set DESC_BLOCK_NORMAL, 0x705
.set DESC_BLOCK_DEVICE, 0x701
.set DESC_PAGE_NORMAL,  0x707
.set DESC_PAGE_DEVICE,  0x703
.set DESC_TABLE,        0x3
.set EARLY_HI_SLOTS,    16      /* 16 * 32MB = 256MB early kernel window */
.set EARLY_TABLES_SIZE, 0x28000 + EARLY_HI_SLOTS * 0x4000

.section .text.entry, "ax"
.global _start
.type _start, @function

_start:
    /*
     * Linux arm64 Image Header (64 bytes)
     */
    b       primary_entry                   /* code0: branch to kernel start */
    nop                                     /* code1 */
    .quad   0x00000000                      /* text_offset */
    .quad   _kernel_size                    /* image_size */
    .quad   0x0c                            /* flags: LE, 16K pages, physical placement */
    .quad   0                               /* reserved */
    .quad   0                               /* reserved */
    .quad   0                               /* reserved */
    .ascii  "ARM\x64"                       /* magic */
    .long   0                               /* pe_header offset: none */
    .long   0                               /* padding */

    /* Pad to 0x800: m1n1's raw chainload entry point (-E 0x800 default) */
    .fill   (0x800 - 68) / 4, 4, 0xd503201f /* nop */

/*
 * Primary entry point.
 *   x0 = physical address of the FDT/DTB or Apple BootArgs
 */
primary_entry:
    /* Use SP_EL1; m1n1's hypervisor enters the guest with SPSel=0 */
    msr     spsel, #1

    adrp    x19, early_boot_stack_top
    add     x19, x19, #:lo12:early_boot_stack_top
    mov     sp, x19

    mov     x21, x0                         /* save boot argument */
    msr     daifset, #0xf                   /* mask D, A, I, F */

    adr     x2, .Lhello_str
    bl      .Lputs

    /* If entered at EL2 (m1n1 bare), configure and drop to EL1h */
    mrs     x9, CurrentEL
    lsr     x9, x9, #2
    cmp     x9, #2
    b.lt    .Lsetup_mmu

    mov     x9, #(1 << 31)                  /* HCR_EL2.RW = 1 (AArch64 EL1) */
    msr     hcr_el2, x9
    mov     x9, #3
    msr     cnthctl_el2, x9                 /* EL1 timer access */
    msr     cntvoff_el2, xzr
    mov     x9, #0x3c5                      /* SPSR_EL2: EL1h, DAIF masked */
    msr     spsr_el2, x9
    adr     x9, .Lsetup_mmu
    msr     elr_el2, x9
    eret

.Lsetup_mmu:
    adrp    x19, early_boot_stack_top
    add     x19, x19, #:lo12:early_boot_stack_top
    mov     sp, x19

    /* x24 = offset = link address - load address */
    adr     x16, _start
    ldr     x17, =_start
    sub     x24, x17, x16

    /* Table pointers (physical: MMU is still off) */
    adrp    x25, early_l0_lo
    add     x25, x25, #:lo12:early_l0_lo
    adrp    x26, early_l1_lo
    add     x26, x26, #:lo12:early_l1_lo
    adrp    x27, early_l2_dev
    add     x27, x27, #:lo12:early_l2_dev
    adrp    x28, early_l2_ram
    add     x28, x28, #:lo12:early_l2_ram

    /* Zero all early tables (8 * 16K) */
    adrp    x10, early_l0_lo
    add     x10, x10, #:lo12:early_l0_lo
    mov     x11, xzr
    ldr     x12, =EARLY_TABLES_SIZE
.Lzero_tables:
    str     xzr, [x10, x11]
    add     x11, x11, #8
    cmp     x11, x12
    b.ne    .Lzero_tables

    /*
     * TTBR0 identity map.
     * 16K granule, 48-bit VA: L0 index = VA[47], L1 = VA[46:36] (64GB each),
     * L2 = VA[35:25] (32MB blocks), L3 = VA[24:14] (16K pages).
     */
    orr     x11, x26, #DESC_TABLE
    str     x11, [x25]                      /* l0_lo[0] -> l1_lo */

    orr     x11, x27, #DESC_TABLE
    str     x11, [x26]                      /* l1_lo[0] -> l2_dev (0..64GB) */

    /* l1_lo[PA(_start) >> 36] -> l2_ram */
    lsr     x12, x16, #36
    mov     x13, #0x7ff
    and     x12, x12, x13
    orr     x11, x28, #DESC_TABLE
    str     x11, [x26, x12, lsl #3]

    /* Fill l2_dev: 2048 Device blocks covering 0..64GB */
    mov     x23, #DESC_BLOCK_DEVICE
    mov     x10, xzr
.Lfill_dev:
    lsl     x11, x10, #25
    orr     x11, x11, x23
    str     x11, [x27, x10, lsl #3]
    add     x10, x10, #1
    cmp     x10, #2048
    b.ne    .Lfill_dev

    /* Fill l2_ram: 2048 Normal blocks covering the 64GB region holding RAM */
    lsl     x14, x12, #36                   /* region base */
    mov     x23, #DESC_BLOCK_NORMAL
    mov     x10, xzr
.Lfill_ram:
    lsl     x11, x10, #25
    add     x11, x11, x14
    orr     x11, x11, x23
    str     x11, [x28, x10, lsl #3]
    add     x10, x10, #1
    cmp     x10, #2048
    b.ne    .Lfill_ram

    /*
     * TTBR1 map of the kernel image at its link address.
     *
     * `EARLY_HI_SLOTS` 32MB L2 slots of 16K pages (256MB) starting at the 64MB
     * aligned window below the link address. This must cover the kernel image
     * *and* the first free physical region handed to the allocator, because
     * `memory::insert_regions` writes buddy metadata into that region before
     * `vm::init` installs the full kernel page table.
     */
    adrp    x25, early_l0_hi
    add     x25, x25, #:lo12:early_l0_hi
    adrp    x26, early_l1_hi
    add     x26, x26, #:lo12:early_l1_hi
    adrp    x27, early_l2_hi
    add     x27, x27, #:lo12:early_l2_hi
    adrp    x28, early_l3_hi
    add     x28, x28, #:lo12:early_l3_hi

    orr     x11, x26, #DESC_TABLE
    str     x11, [x25, #8]                  /* l0_hi[1] -> l1_hi (VA[47]=1) */

    /* x15 = 64MB-aligned virtual window base containing the image */
    mov     x9, #0x3ffffff
    bic     x15, x17, x9

    lsr     x12, x15, #36
    mov     x13, #0x7ff
    and     x12, x12, x13
    orr     x11, x27, #DESC_TABLE
    str     x11, [x26, x12, lsl #3]         /* l1_hi[idx] -> l2_hi */

    /* l2_hi[idx + i] -> l3_hi + i*16K, for each 32MB slot */
    lsr     x12, x15, #25
    and     x12, x12, x13
    mov     x10, xzr
.Lfill_l2_hi:
    add     x11, x28, x10, lsl #14
    orr     x11, x11, #DESC_TABLE
    add     x9, x12, x10
    str     x11, [x27, x9, lsl #3]
    add     x10, x10, #1
    cmp     x10, #EARLY_HI_SLOTS
    b.ne    .Lfill_l2_hi

    /* Fill the page entries: VA = window + i*16K, PA = VA - offset */
    mov     x23, #DESC_PAGE_NORMAL
    mov     x10, xzr
    ldr     x9, =(EARLY_HI_SLOTS * 2048)
.Lfill_pages:
    lsl     x11, x10, #14
    add     x11, x11, x15                   /* VA */
    sub     x11, x11, x24                   /* PA */
    orr     x11, x11, x23
    str     x11, [x28, x10, lsl #3]
    add     x10, x10, #1
    cmp     x10, x9
    b.ne    .Lfill_pages

    /*
     * Map the UART MMIO block at its direct-map address so the kernel's
     * `phys_to_virt(uart_base)` accesses resolve before `vm::init` runs.
     *
     * With T1SZ=16 the TTBR1 region spans 0xffff_0000_0000_0000.., so its L0
     * has *two* live entries: VA[47]=1 (the kernel window) and VA[47]=0 (where
     * `uart_pa + offset` lands). Each gets its own L1 table.
     */
    movz    x20, #0x9b20, lsl #16
    movk    x20, #0x0003, lsl #32           /* UART physical base */
    add     x9, x20, x24                    /* UART virtual address */

    adrp    x26, early_l1_hi0
    add     x26, x26, #:lo12:early_l1_hi0
    orr     x11, x26, #DESC_TABLE
    str     x11, [x25]                      /* l0_hi[0] -> l1_hi0 (VA[47]=0) */

    adrp    x28, early_l2_uart
    add     x28, x28, #:lo12:early_l2_uart
    lsr     x12, x9, #36
    and     x12, x12, x13
    orr     x11, x28, #DESC_TABLE
    str     x11, [x26, x12, lsl #3]         /* l1_hi0[idx] -> l2_uart */

    /*
     * A 32MB block cannot be used here: `offset` is not 32MB aligned, so the
     * VA's low bits differ from the PA's and the walk would resolve
     * `block_base | (VA & 0x1ffffff)` — a bogus MMIO address that the L2 cache
     * reports as an asynchronous bus error (SERROR). Map a single 16K page.
     */
    adrp    x22, early_l3_uart
    add     x22, x22, #:lo12:early_l3_uart
    lsr     x12, x9, #25
    and     x12, x12, x13
    orr     x11, x22, #DESC_TABLE
    str     x11, [x28, x12, lsl #3]         /* l2_uart[idx] -> l3_uart */

    lsr     x12, x9, #14
    and     x12, x12, x13
    mov     x11, #0x3fff
    bic     x11, x20, x11                   /* 16K-aligned UART page */
    mov     x23, #DESC_PAGE_DEVICE
    orr     x11, x11, x23
    str     x11, [x22, x12, lsl #3]

    /*
     * The tables were written with the MMU off (Device, non-cacheable), so
     * clean them to the PoC before the page-table walker reads them cached.
     */
    adrp    x10, early_l0_lo
    add     x10, x10, #:lo12:early_l0_lo
    mov     x11, xzr
    ldr     x12, =EARLY_TABLES_SIZE
.Lclean_tables:
    add     x9, x10, x11
    dc      cvac, x9
    add     x11, x11, #64
    cmp     x11, x12
    b.ne    .Lclean_tables
    dsb     sy

    /* MAIR: Attr0 = Device-nGnRE, Attr1 = Normal WB/WA cacheable */
    movz    x9, #0xff04
    msr     mair_el1, x9

    /*
     * TCR_EL1: T0SZ = T1SZ = 16 (48-bit VA), TG0 = 16K, TG1 = 16K,
     * SH0 = SH1 = Inner, WB/WA cacheable walks; IPS from ID_AA64MMFR0_EL1.
     */
    ldr     x9, =0x7510b510
    mrs     x10, id_aa64mmfr0_el1
    and     x10, x10, #0xf                  /* PARange */
    lsl     x10, x10, #32                   /* TCR_EL1.IPS */
    orr     x9, x9, x10
    msr     tcr_el1, x9
    isb

    adrp    x9, early_l0_lo
    add     x9, x9, #:lo12:early_l0_lo
    msr     ttbr0_el1, x9
    adrp    x9, early_l0_hi
    add     x9, x9, #:lo12:early_l0_hi
    msr     ttbr1_el1, x9
    isb

    tlbi    vmalle1
    dsb     sy
    isb

    mrs     x9, sctlr_el1
    orr     x9, x9, #0x1000                 /* I-cache */
    orr     x9, x9, #0x0004                 /* D-cache */
    orr     x9, x9, #0x0001                 /* MMU enable */
    msr     sctlr_el1, x9
    isb

    /* Running virtually now: switch stack, vectors and PC to link addresses */
    ldr     x19, =early_boot_stack_top
    mov     sp, x19
    ldr     x9, =early_vectors
    msr     vbar_el1, x9
    isb

    adr     x2, .Lmmu_str
    bl      .Lputs

    mov     x0, x21                         /* boot argument (physical) */
    mov     x1, x24                         /* virt - phys offset */
    ldr     x20, =rust_entry
    br      x20

/*
 * Prints the NUL-terminated string at x2 to the UART. Clobbers x1, x2, w3.
 * Usable both before and after the MMU is enabled: before, the identity map
 * is not yet active but the MMU is off; after, the UART is mapped low by
 * TTBR0's Device map.
 */
.Lputs:
    movz    x1, #0x9b20, lsl #16
    movk    x1, #0x0003, lsl #32
1:
    ldrb    w3, [x2], #1
    cbz     w3, 2f
    str     w3, [x1, #0x20]
    b       1b
2:
    ret

/* Prints x5 as 16 hex digits. Clobbers x1, x6, x7. */
.Lputhex:
    movz    x1, #0x9b20, lsl #16
    movk    x1, #0x0003, lsl #32
    mov     x6, #60
1:
    lsr     x7, x5, x6
    and     x7, x7, #0xf
    cmp     x7, #9
    b.hi    2f
    add     w7, w7, #48                     /* '0' */
    b       3f
2:
    add     w7, w7, #87                     /* 'a' - 10 */
3:
    str     w7, [x1, #0x20]
    subs    x6, x6, #4
    b.ge    1b
    ret

/*
 * Early fault reporter: prints the vector index, ESR, ELR, FAR and LR, then
 * parks. Replaced by `trapframe`'s vectors once kernel-hal initializes.
 */
early_fault:
    ldr     x19, =early_boot_stack_top
    mov     sp, x19
    mov     x23, x30

    adr     x2, .Lexc_str
    bl      .Lputs
    mov     x5, x22
    bl      .Lputhex
    adr     x2, .Lesr_str
    bl      .Lputs
    mrs     x5, esr_el1
    bl      .Lputhex
    adr     x2, .Lelr_str
    bl      .Lputs
    mrs     x5, elr_el1
    bl      .Lputhex
    adr     x2, .Lfar_str
    bl      .Lputs
    mrs     x5, far_el1
    bl      .Lputhex
    adr     x2, .Llr_str
    bl      .Lputs
    mov     x5, x23
    bl      .Lputhex
    adr     x2, .Lnl_str
    bl      .Lputs
1:
    wfi
    b       1b

.macro EARLY_VEC index
    .align  7
    mov     x22, #\index
    b       early_fault
.endm

.align 11
early_vectors:
    EARLY_VEC 0                             /* Current EL, SP0 */
    EARLY_VEC 1
    EARLY_VEC 2
    EARLY_VEC 3
    EARLY_VEC 4                             /* Current EL, SPx */
    EARLY_VEC 5
    EARLY_VEC 6
    EARLY_VEC 7
    EARLY_VEC 8                             /* Lower EL, AArch64 */
    EARLY_VEC 9
    EARLY_VEC 10
    EARLY_VEC 11
    EARLY_VEC 12                            /* Lower EL, AArch32 */
    EARLY_VEC 13
    EARLY_VEC 14
    EARLY_VEC 15

.Lhello_str:
    .asciz  "HVLOG: Hello from zCore entry!\r\n"
.Lmmu_str:
    .asciz  "HVLOG: MMU on, entering rust_entry\r\n"
.Lexc_str:
    .asciz  "HVLOG: early fault, vec="
.Lesr_str:
    .asciz  " esr="
.Lelr_str:
    .asciz  " elr="
.Lfar_str:
    .asciz  " far="
.Llr_str:
    .asciz  " lr="
.Lnl_str:
    .asciz  "\r\n"

/*
 * The early stack and page tables must live in a *writable* and *file-backed*
 * section: `vm::init` maps `.text` read-only/executable, so a stack in
 * `.text.entry` faults the moment the kernel page table is activated, and
 * `.bss` is absent from the flat `zcore.bin` that m1n1 loads.
 */
.section .data.early, "aw"

.align 16
early_boot_stack:
    .space  0x10000                         /* 64 KB */
early_boot_stack_top:

/* Early translation tables: 8 * 16K, 16K-aligned and contiguous. */
.align 14
early_l0_lo:
    .space  0x4000
early_l1_lo:
    .space  0x4000
early_l2_dev:
    .space  0x4000
early_l2_ram:
    .space  0x4000
early_l0_hi:
    .space  0x4000
early_l1_hi:
    .space  0x4000
early_l2_hi:
    .space  0x4000
early_l2_uart:
    .space  0x4000
early_l1_hi0:
    .space  0x4000
early_l3_uart:
    .space  0x4000
early_l3_hi:
    .space  EARLY_HI_SLOTS * 0x4000         /* one 16K L3 table per 32MB slot */
