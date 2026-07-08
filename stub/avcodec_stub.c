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
