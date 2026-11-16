#ifndef PLX_ASS_COMPOSITE_H
#define PLX_ASS_COMPOSITE_H

#include <stdint.h>
#include <string.h>

/* Two independent 16-bit lanes. Each numerator is at most 255*255+128,
 * so adding the high byte cannot carry into its neighbour. For 0 <= n <= 65025,
 * (n+128+((n+128)>>8))>>8 is exactly (n+127)/255. */
static inline uint32_t ass_div255_pair(uint32_t rounded)
{
    return ((rounded + ((rounded >> 8) & 0x00ff00ffu)) >> 8) & 0x00ff00ffu;
}

/* RGBA byte order expressed as integers, independent of host endianness. */
static inline uint32_t ass_load_rgba(const uint8_t *rgba)
{
    uint32_t word;
    memcpy(&word, rgba, sizeof word);
#if __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
    word = __builtin_bswap32(word);
#endif
    return word;
}

static inline void ass_store_rgba(uint8_t *rgba, uint32_t word)
{
#if __BYTE_ORDER__ == __ORDER_BIG_ENDIAN__
    word = __builtin_bswap32(word);
#endif
    memcpy(rgba, &word, sizeof word);
}

/* Source has opaque alpha; coverage supplies its opacity. Destination is
 * premultiplied RGBA. Keep the old scalar rounding for every channel, including
 * alpha, while doing red/blue and green/alpha together on 32-bit ARM. */
static inline uint32_t ass_blend_pixel(uint32_t dst, uint32_t source, unsigned a)
{
    const unsigned inverse = 255 - a;
    uint32_t rb = (source & 0x00ff00ffu) * a +
                  (dst & 0x00ff00ffu) * inverse + 0x00800080u;
    uint32_t ga = ((source >> 8) & 0x00ff00ffu) * a +
                  ((dst >> 8) & 0x00ff00ffu) * inverse + 0x00800080u;
    return ass_div255_pair(rb) | (ass_div255_pair(ga) << 8);
}

/* NEON: eight pixels at once, on every target that has it (32-bit ARM TVs, and arm64 hosts, where the
 * same intrinsics exist). PLX_ASS_NO_NEON keeps the scalar row path compiled on such a host, so the
 * tests can run both. The arithmetic is the scalar blend's, lane by lane, so the output is
 * bit-identical: `ci/test_ass_composite.py` runs every (coverage, source, destination) triple through
 * it. A pixel whose coverage is 0 or 255 needs no branch here: the formula leaves the destination or
 * stores the source exactly (255*d and 255*s are multiples of 255). */
#if defined(__ARM_NEON) && !defined(PLX_ASS_NO_NEON)
#define PLX_ASS_NEON 1
#include <arm_neon.h>

/* ass_div255_pair for eight 16-bit lanes: n -> (n + 128 + ((n + 128) >> 8)) >> 8, which is
 * (n + 127) / 255 for 0 <= n <= 65025. vrsraq adds the rounded high byte (n + ((n + 128) >> 8)) and
 * vrshrn adds the 128 and shifts, narrowing: both round in full width, and the largest intermediate,
 * 65025 + 254 = 65279 (+128 = 65407), stays below 2^16. */
static inline uint8x8_t ass_div255_lanes(uint16x8_t n)
{
    return vrshrn_n_u16(vrsraq_n_u16(n, n, 8), 8);
}

/* The four bytes of an RGBA colour (byte 0 in the low bits, as ass_load_rgba reads it), one per lane. */
typedef struct { uint8x8_t r, g, b, a; } AssSource8;

static inline AssSource8 ass_source8(uint32_t color)
{
    return (AssSource8){ vdup_n_u8((uint8_t)color), vdup_n_u8((uint8_t)(color >> 8)),
                         vdup_n_u8((uint8_t)(color >> 16)), vdup_n_u8((uint8_t)(color >> 24)) };
}

/* Eight pixels of one image onto the premultiplied destination `dst` (eight RGBA pixels): `mask` is
 * libass's coverage of them, `opacity` the image's opacity in every lane. The coverage is what the
 * scalar code reads from its table, (mask * opacity + 127) / 255, computed here instead of looked up:
 * a lookup per lane is eight scalar loads and eight stores into a vector. */
static inline void ass_blend8(uint8_t *dst, uint8x8_t mask, uint8x8_t opacity, AssSource8 source)
{
    const uint8x8_t a = ass_div255_lanes(vmull_u8(mask, opacity));
    const uint8x8_t inverse = vmvn_u8(a); /* 255 - a */
    uint8x8x4_t d = vld4_u8(dst);         /* planar: val[c] is channel c of the eight pixels */
    d.val[0] = ass_div255_lanes(vmlal_u8(vmull_u8(d.val[0], inverse), source.r, a));
    d.val[1] = ass_div255_lanes(vmlal_u8(vmull_u8(d.val[1], inverse), source.g, a));
    d.val[2] = ass_div255_lanes(vmlal_u8(vmull_u8(d.val[2], inverse), source.b, a));
    d.val[3] = ass_div255_lanes(vmlal_u8(vmull_u8(d.val[3], inverse), source.a, a));
    vst4_u8(dst, d);
}
#endif

#endif
