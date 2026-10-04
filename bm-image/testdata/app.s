/* A stand-in for an application image: a vector table at the start of
   slot 1's body, bm_protocol's version note at the C build's address, code,
   initialised data loaded from flash, and RAM that is not loaded. */
	.syntax unified

	.section .isr_vector, "a", %progbits
	.word	0x200C0000
	.word	reset + 1
	.word	0x11111111, 0x22222222, 0x33333333

	/* versionNote_t, bm_protocol src/lib/common/version.h. */
	.section .note.sofar.version, "a", %note
	.word	8			/* namesz */
	.word	118			/* descsz */
	.word	0x10			/* type */
	.asciz	"VERSION"
	.quad	0xDF7F9AFDEC06627C	/* magic */
	.word	0x62d8b5d0		/* gitSHA */
	.byte	0, 13, 12		/* maj, min, rev */
	.byte	0			/* hwVersion */
	.word	0x1			/* flags: ENG */
	.short	8			/* versionStrLen */
1:	.ascii	"v0.13.12"
	.space	96 - (. - 1b)

	.text
	.thumb_func
reset:	b	reset
	.byte	0xA5

	.data
	.word	0xCAFEF00D
	.byte	1, 2, 3

	.bss
	.space	64

	.section .noinit, "aw", %nobits
	.space	32
