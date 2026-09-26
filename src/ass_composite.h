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

/* The numerator is below 65536. Multiplication by floor(65536/alpha)
 * underestimates its quotient by at most one; the remainder supplies the exact
 * correction. This replaces ARMv7's software divide without changing a pixel. */
static inline uint8_t ass_straight_channel(unsigned premultiplied, unsigned alpha,
                                          unsigned reciprocal)
{
    const unsigned n = premultiplied * 255u + alpha / 2;
    unsigned q = (n * reciprocal) >> 16;
    q += n - q * alpha >= alpha;
    return (uint8_t)(q > 255 ? 255 : q);
}

#endif
