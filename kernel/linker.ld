OUTPUT_FORMAT(elf64-x86-64)
OUTPUT_ARCH(i386:x86-64)
ENTRY(_start)

PHDRS
{
    text PT_LOAD FLAGS(5);   /* RX */
    rodata PT_LOAD FLAGS(4); /* R  */
    data PT_LOAD FLAGS(6);   /* RW */
}

SECTIONS
{
    . = 1M;

    .text ALIGN(4K) :
    {
        __text_start = .;
        *(.text.boot)
        *(.text .text.*)
        __text_end = .;
    } :text

    .rodata ALIGN(4K) :
    {
        __rodata_start = .;
        *(.rodata .rodata.*)
        __rodata_end = .;
    } :rodata

    .data ALIGN(4K) :
    {
        __data_start = .;
        *(.data .data.*)
        __data_end = .;
    } :data

    .bss ALIGN(4K) :
    {
        __bss_start = .;
        *(COMMON)
        *(.bss .bss.*)
        __bss_end = .;
    } :data

    /DISCARD/ : { *(.eh_frame) *(.comment) }

    __kernel_start = ADDR(.text);
    __kernel_end = .;
}
