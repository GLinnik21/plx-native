// Link-time stub for the TV's real libavutil.so.55 (FFmpeg 3.4). SONAME
// libavutil.so.55. Real bodies come from the device at runtime. Packet/dict/mem
// helpers used by the demuxer.
unsigned avutil_version(void) { return 0; }
void *av_malloc(void) { return 0; }
void *av_packet_alloc(void) { return 0; }
void av_packet_free(void **pkt) { (void)pkt; }
void av_packet_unref(void *pkt) { (void)pkt; }
int av_packet_ref(void *dst, const void *src) { (void)dst; (void)src; return 0; }
void av_init_packet(void *pkt) { (void)pkt; }
int av_dict_set(void **pm, const char *key, const char *value, int flags) { (void)pm; (void)key; (void)value; (void)flags; return 0; }
void av_dict_free(void **m) { (void)m; }
void av_free(void *ptr) { (void)ptr; }
void av_freep(void *ptr) { (void)ptr; }
long long av_rescale_q(long long a, void *bq, void *cq) { (void)a; (void)bq; (void)cq; return 0; }
void av_log_set_level(int level) { (void)level; }

/* Dev capture stream: MPEG1 encode input frames + option/pixfmt helpers. */
void *av_frame_alloc(void) { return 0; }
void av_frame_free(void **frame) { (void)frame; }
int av_frame_get_buffer(void *frame, int align) { (void)frame; (void)align; return 0; }
int av_frame_make_writable(void *frame) { (void)frame; return 0; }
int av_opt_set(void *obj, const char *name, const char *val, int search_flags) { (void)obj; (void)name; (void)val; (void)search_flags; return 0; }
int av_get_pix_fmt(const char *name) { (void)name; return 0; }
