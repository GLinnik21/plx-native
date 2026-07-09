// Link-time stub for the TV's real libavcodec.so.57 (FFmpeg 3.4). SONAME
// libavcodec.so.57. Real bodies come from the device at runtime. We use libavcodec
// only for the bitstream filters (hevc/h264_mp4toannexb -> Annex-B for Starfish) and
// codec metadata; Starfish does the actual decoding, so no decoders are called here.
unsigned avcodec_version(void) { return 0; }
const void *av_bsf_get_by_name(const char *name) { (void)name; return 0; }
int av_bsf_alloc(const void *filter, void **ctx) { (void)filter; (void)ctx; return 0; }
int av_bsf_init(void *ctx) { (void)ctx; return 0; }
int av_bsf_send_packet(void *ctx, void *pkt) { (void)ctx; (void)pkt; return 0; }
int av_bsf_receive_packet(void *ctx, void *pkt) { (void)ctx; (void)pkt; return 0; }
void av_bsf_free(void **ctx) { (void)ctx; }
int avcodec_parameters_copy(void *dst, const void *src) { (void)dst; (void)src; return 0; }
const char *avcodec_get_name(int id) { (void)id; return "?"; }  /* real body from the TV's libavcodec */

/* Image-subtitle (PGS/VobSub/DVB) software decode: we DO call these decoders for
 * bitmap subtitles (client-render #44). Empty bodies here; real ones from the TV. */
void *avcodec_find_decoder(int id) { (void)id; return 0; }
void *avcodec_alloc_context3(const void *codec) { (void)codec; return 0; }
int avcodec_parameters_to_context(void *ctx, const void *par) { (void)ctx; (void)par; return 0; }
int avcodec_open2(void *ctx, const void *codec, void **opts) { (void)ctx; (void)codec; (void)opts; return 0; }
int avcodec_decode_subtitle2(void *ctx, void *sub, int *got, void *pkt) { (void)ctx; (void)sub; (void)got; (void)pkt; return 0; }
void avsubtitle_free(void *sub) { (void)sub; }
void avcodec_free_context(void **ctx) { (void)ctx; }
