ENTRY(_start)
BASE_ADDRESS = 0xffff800040080000;

SECTIONS
{
    . = BASE_ADDRESS;
    skernel = .;

    .text : {
        stext = .;
        *(.text.entry)
        *(.text .text.*)
        . = ALIGN(64K);
        etext = .;
    }

    .rodata : {
        srodata = .;
        *(.rodata .rodata.*)
        *(.srodata .srodata.*)
        . = ALIGN(64K);
        erodata = .;
    }

    /* `.data.memory` holds the initial kernel heap. It must live in a loaded
     * (file-backed) section so that the flat `zcore.bin` handed to m1n1
     * contains it: m1n1 only maps the bytes it loads. */
    .data : {
        sdata = .;
        *(.data.memory)
        *(.data .data.*)
        *(.sdata .sdata.*)
        . = ALIGN(64K);
        edata = .;
    }

    .bss : {
        boot_stack = .;
        *(.bss.stack)
        . = ALIGN(64K);
        boot_stack_top = .;

        sbss = .;
        *(.bss .bss.*)
        *(.sbss .sbss.*)
        . = ALIGN(64K);
        ebss = .;
    }

    ekernel = .;
    _kernel_size = ekernel - skernel;

    /DISCARD/ : {
        *(.eh_frame) *(.eh_frame_hdr)
    }
}
