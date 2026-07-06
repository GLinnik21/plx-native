// Link-time stub for the TV's real libavutil.so.55 (FFmpeg 3.4). SONAME
// libavutil.so.55. Real bodies come from the device at runtime. Packet/dict/mem
// helpers used by the demuxer.
unsigned avutil_version(void) { return 0; }
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
